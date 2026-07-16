# Type checking flo — an implementation guide

This is a plain-worded, in-depth walkthrough of how to type-check the **core** of
flo: number/bool/char literals, unary and binary operators, blocks, `if`/`while`,
`let`/`var`, and functions (including overloading, generic inference,
specialization, defaulting, and recursion).

It's meant to be read top to bottom before you start coding. It explains not just
*what* to do but *why* each step exists and *when* it fires. The worked examples in
[foo.flo](foo.flo) are the ground truth; this document is the prose behind them.

Three design questions were settled while writing this:

1. **Overload specificity** — if a call matches both a generic overload and a
   concrete one, that is an **error** (ambiguous). We do *not* prefer the more
   specific overload.
2. **Numeric defaulting** — a numeric literal defaults to `i32`, applied as a
   **last resort tiebreak**: overload resolution runs first on real (concrete)
   type information, and only when it *stalls* on an ambiguity do the still-free
   numeric variables default to `i32` and resolution **retry** (the
   **resolve → default → resolve** loop). Real constraints therefore always win —
   a `u8`-typed context pins the literal before defaulting can fire — but a call
   with no other information, like `f(0)` against `f(i32)`/`f(u8)` (or a bare
   `1 + 2`), defaults the literal to `i32` and selects the `i32` overload rather
   than erroring. Defaulting still never *inspects* the overload set to pick a
   convenient value; it unconditionally freezes free numerics to `i32`, and the
   retry then prunes. A value with no numeric bound (e.g. a return-only-overloaded
   `make()` whose result var carries no kind) is untouched by defaulting and stays
   ambiguous. (An earlier revision left `f(0)`/`1 + 2` ambiguous; defaulting-as-
   tiebreak was adopted so bare arithmetic type-checks without annotation.)
3. **No unit type** — there is no `()`/void value. `if`-without-`else`, `while`,
   and `;`-terminated blocks are legal *only in statement position*. Using one
   where a value is expected is a kind/position error, not a type-unit.

---

## 1. The mental model

flo's checker is a **constraint-based** inferencer, in the Hindley–Milner family
but bent to support ad-hoc overloading and whole-program monomorphization.

The core idea: you never try to figure out a type by looking at an expression in
isolation. Instead you invent a fresh **type variable** for every expression whose
type you don't yet know, and you emit **constraints** that relate those variables
to each other and to concrete types. Then a separate **solver** grinds the
constraints down until types fall out (or a contradiction appears).

There are two "unknowns" the checker resolves:

- **What type is this expression?** — handled by ordinary type variables and
  equality constraints.
- **Which function does this call actually invoke?** — handled by *overload sets*
  attached to call/operator constraints, narrowed as argument types become known.

And there are two whole-program phases:

- **Bottom-up pass** — computes each function's most-general (polymorphic) *schema*,
  processing callees before callers.
- **Specialize pass** — starting from `main`, pushes concrete types downward and
  produces one monomorphized *instance* per (function, concrete-argument-types)
  combination.

---

## 2. The type language

Types the core deals with:

- **Concrete scalar types**: `i8 i16 i32 i64`, `u8 u16 u32 u64`, `bool`, `char`.
- **Type variables**: `t0, t1, …, tn, tx, tret, …` — placeholders the solver fills
  in. Naming is irrelevant; identity is what matters.
- **Kinds** (a.k.a. type classes / bounds): right now just `Numeric` = the set of
  the integer (and later float) types. A kind is a *constraint on a variable*, not
  a type itself — you can't have a value "of type Numeric".

There is **no unit type**. See §6 for how the position rule replaces it.

A **schema** is a function's inferred signature plus any leftover constraints that
couldn't be discharged without knowing the caller's types. For example
`id`'s schema is `(tx) -> tx` — fully polymorphic, no residual constraints —
while `main`'s during inference might be `() -> tret` with a residual
`IsKind(tret, Numeric)`.

---

## 3. The constraint language

Four constraint forms cover the core. Each is a small record you push onto a list
while walking the AST.

### `IsEqual(a, b)`
"These two types are the same." `a` and `b` are each a type variable or a concrete
type. This is the workhorse — it's how a `let` binding ties a name to its
initializer, how both branches of an `if` are forced to agree, how a call's
argument ties to a parameter, and so on.

### `IsKind(t, Numeric)`
"`t` must be one of the numeric types." Emitted for every numeric literal and for
the operands/result of arithmetic operators. It does two jobs: it rejects
nonsense (`true + 1`), and it tells the defaulter "if this variable is still free
at the end, give it `i32`."

> Note: foo.flo occasionally writes `IsEqual(t, Numeric)` — that's a typo for
> `IsKind`. `Numeric` is a kind, never the right-hand side of an equality.

### `Call(name, [arg_t, …], ret_t, [cand, …])`
"There is a call to a function named `name`, with these argument-type variables and
this result-type variable, and these are the candidate overloads it might resolve
to." `cand` entries are overload ids like `@1`, `@2` (foo.flo's notation for "the
first/second declaration of that name"). A non-overloaded function starts with a
one-element candidate list.

### `Op(op, [arg_t, …], ret_t, [cand, …])`
Exactly like `Call`, but for built-in operators. The candidate list is the
built-in overload table for that operator, e.g. for `+`:
`[@1:(+,[i32,i32],i32), @2:(+,[u8,u8],u8), …]`. Comparison operators (`==`, `<`,
…) have result type `bool` in every candidate.

`Call` and `Op` are the *only* constraints that carry an overload set, and they are
the only ones that can trigger specialization.

---

## 4. Generating constraints (walking the AST)

You walk each function body once, bottom-up over the expression tree, returning
"the type variable that stands for this sub-expression's value" and emitting
constraints as a side effect. Every node that has a value gets a fresh variable.

An important distinction runs through everything: **value position vs. statement
position.** A value-position expression must produce a value; a statement-position
one may or may not. This is what replaces a unit type (§6).

Here's each core form. `fresh()` mints a new type variable.

- **Number literal** `n` → `t = fresh(); emit IsKind(t, Numeric); return t`.
- **Bool literal** `true/false` → return the concrete type `bool` (no variable
  needed, but using a fresh var equated to `bool` is fine too).
- **Char literal** `'a'` → return `char`.
- **Unary op** `!e` → operand must be `bool`, result `bool`. `-e` → `Op(-, [te],
  tr, …)` with the unary-minus candidates; emits `IsKind` as needed.
- **Binary op** `a ⊕ b` → `ta = walk(a); tb = walk(b); tr = fresh();
  emit Op(⊕, [ta, tb], tr, candidates(⊕)); return tr`. For arithmetic ops also
  emit `IsKind(ta, Numeric)` etc. (the operator's candidate table already encodes
  the legal combinations, but the kind constraints drive defaulting).
- **Name reference** `x` → return the variable that `x` was bound to (from the
  environment; see `let`/`var`).
- **Call** `f(a, b)` → walk the args to get `[ta, tb]`; `tr = fresh();
  emit Call("f", [ta, tb], tr, overloads_of("f")); return tr`.
- **Block** `{ s1; …; sn; e }` → walk each `si` in statement position (constraints
  emitted, value ignored). If there is a trailing value expression `e`, the block's
  type is `walk(e)`. If the block ends in `;` (no trailing `e`), the block produces
  no value — it is only legal in statement position, and returns "no value" (§6).
- **`if c { a } else { b }`** in value position → `emit IsEqual(tc, bool)`; `ta =
  walk(a); tb = walk(b); tr = fresh(); emit IsEqual(ta, tr); emit IsEqual(tb, tr);
  return tr`. (This is exactly the `fact` example's
  `IsEqual(t2,t7), IsEqual(t6,t7), IsEqual(t7,tret)` shape.)
- **`if c { a }`** (no else) → statement position only; `emit IsEqual(tc, bool)`;
  walk `a` in statement position; no value.
- **`while c { … }`** → statement position only; `emit IsEqual(tc, bool)`; walk
  body in statement position; no value.
- **`let x = e;`** → `tx = fresh(); te = walk(e); emit IsEqual(tx, te)`; bind `x ↦
  tx` in the environment. `let x: T = e;` additionally `emit IsEqual(tx, T)`.
  `var` is identical except `x` is marked mutable.
- **Assignment `x = e;`** → statement position only; requires `x` mutable;
  `emit IsEqual(tx, te)`; no value.
- **`return e;`** → `emit IsEqual(te, tret_of_current_function)`; the expression
  diverges, so it has no value at its own position.
- **Function body** `fn f(params) -> R = body` → give each parameter a fresh
  variable and bind it; if a parameter is annotated `p: T`, `emit IsEqual(tp, T)`.
  Walk `body` in value position to get `tbody`; `emit IsEqual(tbody, tret)`. If the
  return type is annotated `-> R`, `emit IsEqual(tret, R)`. The schema starts as
  `(tp1, …) -> tret`.

That's the whole generator. Everything else is solving.

---

## 5. The solver

The solver consumes the constraint list and mutates a shared store until nothing
more can be done. It is *not* a single left-to-right pass — it's a **worklist that
runs to a fixed point**, because resolving one constraint can unlock another.

Two pieces of state:

- A **union-find** over type variables. Each set has a *representative*, which is
  either a concrete type or "still unknown", plus any **kind bounds** (`Numeric`)
  accumulated on that set.
- The **constraint list**, from which solved constraints are removed and into which
  new ones are pushed as overloads resolve.

### Handling each constraint

**`IsEqual(a, b)`** — unify:
- both concrete → they must be identical, else **type error**.
- one concrete, one variable → point the variable's set at the concrete type; check
  any kind bounds on that set are satisfied (e.g. a `Numeric`-bound set can't become
  `bool`).
- both variables → union the two sets; merge their kind bounds. If the sets already
  resolved to conflicting concrete types → **error**.

(For compound types — function types, pointers, arrays later — unification is
structural: `(A)->B` unifies with `(C)->D` by unifying `A~C` and `B~D`. The core is
all scalars, but build the union-find so this extends cleanly.)

**`IsKind(t, Numeric)`** — record the bound on `t`'s set. If the set is already a
concrete type, check membership now (numeric ok; `bool`/`char` → error). If still
free, the bound just sits there until either an `IsEqual` resolves it or the
defaulter picks it up.

**`Call` / `Op`** — this is the interesting one. On each visit:
1. **Prune candidates.** For every candidate whose *known* parameter types conflict
   with the *known* argument types, drop it. "Known" means the arg's union-find set
   resolved to a concrete type. A candidate whose parameter is a type variable
   (a generic overload) matches *anything*, so it's never pruned by a known arg —
   this is the root of the ambiguity rule below.
   - **0 candidates remain → error** ("no matching overload / bad argument types").
2. **If exactly 1 candidate remains, resolve it.** Instantiate that callee's
   current schema (see §5.1 — *fresh copy of its variables*), then emit
   `IsEqual(arg_i, param_i)` for each argument and `IsEqual(ret_t, schema_ret)`.
   This "pulls in" the callee's constraints. Remove the `Call`/`Op` from the list
   (it's now just those equalities). Keep solving — the new equalities may resolve
   more variables and unlock more calls.
3. **If ≥2 candidates remain, leave it** and move on; a later resolution might make
   another argument concrete and prune further.

**Reaching a fixed point.** Keep cycling the worklist until a full pass resolves
nothing new. What's left over is "stuck": free variables (some with kind bounds)
and calls with more than one live candidate. That residue is either resolved later
by defaulting (§7) or by the caller's concrete types (§8), or it's a genuine
ambiguity error once no more information can arrive.

### 5.1 Instantiation: freshen the callee's variables (mandatory)

When you pull in a callee's schema at a call site, you must **make a fresh copy of
all the schema's type variables** first. This is not optional and it's easy to get
wrong.

Why: a polymorphic function like `fn id(x) = x` has schema `(tx) -> tx`. If `main`
calls `id(0)` and `id(true)`, both call sites share the *name* `tx`. If you reuse
the same `tx` variable, the first call unifies `tx = i32`, the second unifies
`tx = bool`, and you get a bogus `i32 = bool` conflict — even though `id` is
perfectly happy being called at two types. Freshening means call site A gets
`(tx') -> tx'` and call site B gets `(tx'') -> tx''`, independent.

Concretely: when instantiating, build a substitution mapping each of the schema's
*own* variables to a brand-new variable, apply it to the parameter types, return
type, and any residual constraints in the schema, and emit *those* copies.
Variables that the schema resolved to concrete types stay concrete (no copy
needed).

This is the same mechanism as HM let-generalization/instantiation; flo just applies
it per function schema rather than per `let`.

---

## 6. No unit type — the position rule

Because there's no unit value, the checker (or the resolver feeding it) tracks
whether each expression sits in **value position** or **statement position**:

- **Value position**: the last expression of a block, an `if`/`else` branch used as
  a value, a `let` initializer, a call argument, an operator operand, a function
  body. Must produce a value → must have a type.
- **Statement position**: every non-final item in a block, the body of a `while`,
  the discarded branch of a statement-`if`. May be value-producing (value discarded)
  or non-value-producing.

The non-value-producing forms — `while`, `if` without `else`, assignment, `let`,
`return`, and a `{ … ; }` block that ends in a semicolon — are **only legal in
statement position**. If one appears in value position, that's a **position error**
raised before/independently of type inference (it's structural, not a type
mismatch). This is why the constraint generator in §4 never needs to invent a type
for them.

Practical consequence: `if`/`else` gets the branch-equality constraint *only in
value position*. In statement position both branch values are discarded, so no
`IsEqual(ta, tb)` is emitted (the condition is still forced to `bool`).

---

## 7. Defaulting

After the bottom-up pass, and again inside the specialize pass, some variables are
still free but carry a `Numeric` kind bound — the classic "what type is the literal
`0`?" situation. **Defaulting** resolves them: every still-free `Numeric` set
becomes **`i32`**. (Later, a float kind would default to `f64`.)

Timing and the decided behavior (question 2):

- Defaulting happens **after** ordinary solving is stuck, so real information always
  wins over the default. In `id(true); id(0)`, the `bool` call resolves from known
  types *before* defaulting ever touches the `0`.
- Defaulting is **unconditional**: a free numeric variable becomes `i32` even if
  that variable also feeds an overloaded call whose candidates don't include `i32`.
  If that later prunes the overload set to empty → **error**, and the user must add
  an annotation. We deliberately do *not* look at the overload set to pick a
  "convenient" default.
- After defaulting, **solve again**. Freezing those variables to `i32` typically
  prunes overload sets down to one candidate and lets the remaining calls resolve
  (and specialize). This is the `1 + 1` example: default `t0,t1 → i32`, which prunes
  `+`'s candidates to the `i32` overload, which then fixes `tret = i32`.

---

## 8. The two passes over the program

### 8.1 The call graph and processing order

Build a directed graph: an edge from `f` to `g` whenever `f`'s body contains a call
whose candidate set includes `g`. Collapse it into **strongly connected
components** (SCCs) — each SCC is a set of mutually recursive functions (a
self-recursive function is a singleton SCC that has a self-edge). Topologically sort
the SCCs.

- **Bottom-up pass** processes SCCs in **reverse topological order** (callees
  first). Reason: to instantiate a callee's schema at a call site, that schema must
  already exist.
- **Specialize pass** processes **top-down from `main`** (callers first). Reason:
  concrete types are only known at the entry point and flow downward through
  argument positions.

foo.flo's `fact`/`main` example shows the SCC grouping: `[["fact"], "main"]` — the
inner list `["fact"]` is fact's self-recursive SCC, processed before `main`.

### 8.2 Bottom-up pass — compute each function's schema

For each SCC, in reverse-topo order, **repeat until no schema in the SCC changes**
(the fixpoint loop matters only for recursive SCCs; a non-recursive singleton
settles in one iteration):

For each function `f` in the SCC:

1. Start a fresh schema: fresh vars for params (annotated params also get an
   `IsEqual` to their annotation), fresh var for the return (likewise if annotated).
2. Generate `f`'s body constraints (§4) and add the schema's own constraints.
3. For every call in the body that already has exactly one candidate, instantiate
   (§5.1) the callee's *current* schema and pull in its equalities. (Inside a
   recursive SCC, "current schema" means the schema from the previous fixpoint
   iteration — initially the maximally-general fresh one.)
4. Solve to a fixed point (§5). If solving prunes any call to a single candidate,
   pull that callee in and keep solving.
5. Opportunistically **specialize** any call whose args *and* result are already
   fully concrete (rare in this pass, since nothing is defaulted yet — but it
   happens when annotations make everything concrete, as in the `id(x) -> u8`
   example).
6. Read off `f`'s updated schema: the representatives of the param vars and the
   return var, plus any residual constraints (kind bounds, still-ambiguous calls).

The output of this pass is, for every function, its **most-general schema** — e.g.
`id : (tx) -> tx`, `foo : (tx) -> tx`, `main : () -> tret` with residual
`IsKind(tret, Numeric)`.

Nothing is defaulted and nothing is monomorphized yet (except where annotations
already forced concreteness). The point of this pass is purely to learn each
function's polymorphic shape so callers can use it.

### 8.2.1 How the bottom-up pass is actually implemented

The prose above describes the algorithm abstractly; here's how it maps onto the
code in [src/type_checker/](src/type_checker/) — chiefly [mod.rs](src/type_checker/mod.rs)
(the pass driver), [infer.rs](src/type_checker/infer.rs) (constraint generation +
the solver), [subst.rs](src/type_checker/subst.rs) (freshening/canonicalization),
and [unify.rs](src/type_checker/unify.rs) (the union-find) — including a few
decisions that aren't forced by the algorithm but keep the implementation simple.

**Where the schema lives — there is no `Schema` struct.** A schema is "signature +
residual constraints" (§2), but the signature half already exists: it's an
overload's `func.ty` (a `Type::Fn(params, ret)`) sitting in the `Module`. Because a
name can be **overloaded**, `module.funcs` maps each name to a **`Vec<Func>`** — one
entry per overload — and the bottom-up pass **mutates each overload's `func.ty` and
`func.body` in place**, so once processed the module *is* the source of truth for
that overload's signature. The only thing `Type::Fn` can't represent is a leftover
**kind bound on a still-free variable** (e.g. `fn bar() -> 'a = 0` has signature
`() -> t0` but `t0` must be `Numeric`). Those go in one side-map on the checker,
keyed the same way — a parallel `Vec` per name, one residual list per overload:

```
residuals: HashMap<String, Vec<Vec<(usize, TypeKind, Loc)>>>  // name -> per-overload [(var id, kind, where)]
```

So overload `i`'s full schema is `module.funcs[name][i].ty` **plus**
`residuals[name][i]`. When a caller resolves a call it reads *both* straight out of
the module/side-map — processing callees before callers (reverse-topo) guarantees
they're already final. Freshening (§5.1) is applied to the chosen overload's
signature **and** its residual entries at each call site.

**One id space, two conventions.** Type-variable ids come from three places and
must never collide:
- ids the parser baked into the module,
- fresh ids minted while instantiating a callee's schema,
- canonical ids used to name a finished schema's own variables.

The pass keeps a single monotonic `fresh` counter, seeded to `max_var_id(module) +
1`, so every freshened id is above every parser id. After solving a function its
variables are **canonicalized** — renumbered from `0` in order of appearance
(signature first, then body). Canonical ids are therefore always *below* the
`fresh` counter and never collide with freshened ids. Canonicalization exists for
one concrete reason: schema equality must be checked across fixpoint iterations
(§8.2 "repeat until no schema changes"), and that comparison has to be stable
regardless of which transient ids the solver happened to hand out — renumbering
makes two structurally-equal schemas compare equal.

**Fixpoint convergence vs. writing back.** The SCC fixpoint loop stops when no
member's **signature or residuals** change (that's all a caller can observe). But
the resolved **body** is written back on *every* successful solve, not only when
the signature changed — a function like `fn foo() -> i32 = 0` has a fixed signature
yet its body literal still needs resolving from `t2` to `i32`. Gating the body
write on signature-change would leave those internal types unresolved.

**The overload worklist (§5).** Constraint generation (`gen_expr`) no longer pulls
callees in directly; instead every call becomes an **`Obligation`** (name, argument
types, result type, loc) alongside the equality/kind constraints. `solve` then
unifies all equalities and applies all kind bounds, and runs the overload worklist
to a fixed point. Each round, for every unresolved obligation it resolves the
argument/result types to their union-find heads and computes the **live candidates**
(`candidates`): overloads whose arity matches and whose concrete parameter/return
types — and residual kind bounds — don't contradict the *already-concrete*
argument/result types (`compat`). Then:

- **exactly one candidate** → `commit` it: freshen that overload's signature +
  residuals and unify `arg_i ~ param_i`, `result ~ ret`, adding its kind bounds.
  This can make more variables concrete and unlock other obligations, so the loop
  runs again.
- **zero candidates** → error. Pruning is monotonic (unification only ever makes
  types *more* concrete, which only removes candidates), so zero now means zero
  forever — reported as `NoMatchingOverload`, or the sharper `CallArityMismatch` /
  `UndefinedFunction` when that's the cause.
- **two or more** → depends on the mode (below).

**Two modes: `strict`.** The bottom-up pass solves **non-strict**: a call still
sitting on ≥2 candidates when the loop stalls is *left unresolved*, not errored —
the function stays polymorphic there, and the specialize pass retries it once
concrete argument types arrive. The specialize pass solves **strict**: when the
worklist stalls on a ≥2-candidate call it defaults the free numerics and retries
(the resolve → default → resolve loop below); a call still stalled after defaulting
can no longer make progress and is a genuine `AmbiguousCall` error.

**Resolve → default → resolve; free vars are wildcards when pruning.** Overload
resolution runs on concrete types first: an argument whose type is still a free
variable — an un-pinned numeric literal like `0` — is treated *permissively* by
`candidates` (it neither prunes a candidate nor commits one), so only concrete
types prune. When the strict worklist stalls with everything it can resolve from
real information already resolved, `solve` calls `default_free` (§7) — freezing
every still-free numeric variable to `i32` — and loops again; freezing `0` (or a
`1 + 2`'s operands) to `i32` prunes the surviving overloads down to the `i32` one,
which then commits. `default_free` reports whether it bound anything, so the loop
runs only while defaulting makes progress and reports `AmbiguousCall` once nothing
free remains yet candidates still tie (e.g. two overloads matching a value that has
no numeric bound to default). Defaulting never *inspects* the overload set to pick a
convenient value — it is unconditional, and the retry does the pruning. A single
non-overloaded call still reports its true kind/type error (rather than "no matching
overload"), because a lone candidate is committed and the real unification surfaces
the mismatch.

**Errors don't wedge the loop.** If solving a function errors (type mismatch, kind
violation, arity/undefined-call), the error is recorded and that function is marked
errored and skipped for the rest of its SCC, so a genuine error can't spin the
fixpoint forever.

### 8.3 Specialize pass — monomorphize from `main` down

This pass turns the polymorphic schemas into concrete **instances**, one per
distinct set of concrete argument types. Maintain a `specialized_schemas` map keyed
by `(function, overload-id, [concrete arg types])`.

Start with `main`:

1. Take `main`'s schema and residual constraints. `main` takes no arguments, so no
   external constraints are added.
2. Solve (usually already stuck).
3. **Default** the free numeric vars to `i32` (§7).
4. **Solve again.** Now `main`'s calls have concrete argument types, so they prune
   to a single candidate and can be specialized.
5. For each such fully-concrete call to `g` with concrete arg types `A`: **specialize
   `g` at `A`** (below), which yields `g`'s concrete return type; feed that back as
   `IsEqual(call_ret, that)` and keep solving.

**Specializing `g` at concrete arg types `A`** (this recurses):

1. **Memoize first (mandatory).** If `(g, overload, A)` is already in
   `specialized_schemas` — *or is currently being built* — return that instance's
   signature instead of re-doing the work. Register an in-progress marker *before*
   step 2. This is what makes recursion terminate: `fact` specialized at `i32` calls
   `fact(n-1)`, also `i32`; the recursive call finds the in-progress `(fact, i32)`
   instance and reuses it instead of specializing forever.
2. Instantiate `g`'s polymorphic schema (§5.1) and its residual constraints, and add
   `IsEqual(param_i, A_i)` for each argument.
3. Solve; **default** any still-free numeric vars; solve again.
4. Every call inside `g` is now concrete → recursively specialize *its* callees
   (step 1 of this list), wiring their concrete return types back in.
5. Record the finished instance signature `(A) -> concrete_ret` in
   `specialized_schemas`.

When this settles, you have `main` plus a concrete instance for every reachable
(function, argument-types) pair — ready for code generation. Functions that are
never reached from `main` are never specialized (they stay polymorphic; you simply
don't emit code for them).

### 8.3.1 How the specialize pass is actually implemented

This maps §8.3 onto the `Specializer` in
[src/type_checker/specialize.rs](src/type_checker/specialize.rs). It runs only after
a clean bottom-up pass (if that pass errored, the schemas can't be trusted, so
specialize is skipped).

**Picking the overload (`select_overload`).** `specialize(name, args, expected_ret,
loc)` first chooses *which* overload it is specializing. By the time a call is
reached, `args` — and, at every real call site, `expected_ret` — are concrete, so
`candidates` must return exactly one overload: the same one the strict solve already
committed to. Zero or several here is an error (`NoMatchingOverload` /
`AmbiguousCall`). The root `main` passes `expected_ret = None` (permissive) and,
being unoverloadable (the parser rejects a second `main`), always has a single
candidate.

**The memo table replaces `specialized_schemas`.** Instances live in
`instances: HashMap<InstanceKey, Option<Func>>` where the key is `(function name,
concrete arg types, concrete return type)`. No separate overload-id is needed — the
resolved `(args, ret)` already pin down which overload this is — and the return type
*is* part of the key (see the return-polymorphism note below). The value is `None`
while the instance is being built (the in-progress marker) and `Some(func)` once the
concrete body is done; the return type isn't stored again in the value since it's
already the third element of the key.

**The caller's expected return type is threaded in — this is the subtle part.**
§8.3 keys instances by argument types alone, which quietly assumes a function's
return type is determined by its arguments. That breaks for a **return-polymorphic**
function: `fn zero() = 0` has schema `() -> t0` with `IsKind(t0, Numeric)`, and its
result is fixed by *where it's called*, not by its (empty) argument list. In
`fn foo(x: u8) = x; fn main() = foo(zero())` the `u8` parameter demands
`zero() : u8`; specializing `zero` in isolation would instead *default* its free
return to `i32` and produce an instance whose type contradicts the call site.

So `specialize(name, args, expected_ret)` takes the return type the caller demands.
`resolve_and_specialize` already resolved each call's result type against the
caller's union-find, so it passes that as `Some(ret)`; only the root `main` passes
`None` (its return is fixed by its own body). Inside `specialize` the expected
return is unified in as `IsEqual(fret, expected_ret)` *before* `default_free` runs,
so real information wins and `zero` becomes `() -> u8` here. Two call sites at
different types therefore yield two genuinely distinct instances — which is exactly
why the return type is in the key and in the **mangled name**. `mangle` appends
every argument type tag *and* the return tag to the name (only `main` is exempt and
stays `main`), so instances that differ in either arguments or return can't collide:
`bar$i32$i32`, `foo$u8$u8`, and the nullary `zero$u8`. With overloading this return
tag is essential — two overloads that differ *only* in return type (`make() -> i32`
vs `make() -> u8`) resolve to `make$i32` and `make$u8`.

**Return type is registered before the body — that's what terminates recursion.**
The mandatory in-progress marker (§8.3 step 1) works because the concrete return
type is known once `unify(params, A)` + the demanded return + defaulting have run —
before the body is walked. `specialize` inserts `Instance { ret, func: None }` and
only *then* descends. A recursive self-call (e.g. `fact$i32` calling `fact$i32`)
hits the memo, reads `ret`, and returns — it never re-enters the body. When the
caller pinned the return type there's also a fast-path memo check up front, since
the full key is known before any solving.

**One solve, then a resolve-and-rewrite walk.** The abstract description's
"solve / default / solve again" loop lives *inside* `solve` when `strict = true`:
the implementation instantiates the chosen overload's schema (freshen signature,
body, and residuals through one shared map so linked variables stay linked), pins
`param_i = A_i` and `fret = expected_ret`, regenerates the body's constraints with
the same `gen_expr` the bottom-up pass uses, then hands the obligations to `solve`
with `strict = true`. When that worklist stalls it defaults free numerics and
retries internally (§8.2.1), so a single `solve` call already does resolve → default
→ resolve; the trailing `default_free` in `specialize` only mops up leftover free
numerics in bodies with no calls to stall on (e.g. `fn main() = 0;`). The worklist
resolves every body call against the now-concrete argument types; because `commit`
re-links each call's argument/return to the callee's schema, each call's result type
is already concrete afterwards — no need to explicitly feed callee return types back
in. A second pass
(`resolve_and_specialize`) then walks the concrete body: it resolves every type and,
at each call, recurses into `specialize` for the callee (passing the call's concrete
result as the expected return) and rewrites the call target to the callee instance's
mangled name.

**Defaulting is genuinely last (§7).** `default_free` runs *after* unification —
including the caller's demanded return type — and only binds variables that are
still free *and* carry a kind bound, to that kind's default (`Numeric → i32`).
Anything real information pinned down is untouched. A variable still free with *no*
bound after defaulting is a real `UnresolvedType` error, not a panic.

**The output is a `ResolvedModule`.** `into_module` collapses the memo table into a
`HashMap<String, Func>` keyed by mangled name (`main` stays `main`; every other name
carries its argument tags and return tag). `check` wraps it in a **`ResolvedModule`**
— a type distinct from `Module`, whose `funcs` is `HashMap<String, Func>` (one
concrete function per mangled name) rather than `Module`'s `HashMap<String,
Vec<Func>>` (overload sets). `check`'s signature is therefore
`Result<ResolvedModule, Vec<FloErr>>`, and it `assert!`s the invariant that every
instance is fully monomorphic — no unresolved type variable survives anywhere in a
resolved signature. Unreachable functions never entered the memo, so they simply
vanish — exactly the "don't emit code for them" behavior the guide calls for.

---

## 9. Overloading and ambiguity, precisely

Pulling the overload rules together, since they're the subtle part:

- A candidate is **pruned** when a *known* (concrete) argument type disagrees with
  that candidate's parameter type at the same position.
- A candidate with a **type-variable parameter** (a generic overload) is compatible
  with any argument, so known arguments never prune it.
- **0 live candidates** → error (no overload accepts these arguments).
- **1 live candidate** → resolve and pull it in (§5 step 2).
- **≥2 live candidates once the worklist stalls** → default free numerics and
  retry (strict pass); left unresolved in the non-strict (bottom-up) pass. Only a
  stall that *defaulting can't break* is an **ambiguity error**. Question 1 still
  holds — we do **not** implement "most specific wins." When both `fn id(x)` and
  `fn id(x: i32)` are in scope and `id` is called with an `i32`-typed value, the
  generic overload accepts anything and the `i32` overload accepts `i32`; both
  survive on *concrete* information, defaulting has nothing free to bind, so two
  candidates remain — an error.

**Ordering (as implemented).** Resolution runs on concrete information first; an
argument still on a free type variable is treated permissively (it never prunes),
so real types decide first. Only when the strict worklist stalls does defaulting
freeze the still-free numerics to `i32` and drive a retry (resolve → default →
resolve). Concretely, with the implemented type set (`i32`/`u8`, both `Numeric`):

- `fn id(x: i32)` + `fn id(x: u8)`, call `id(0)`: no concrete info prunes, the
  worklist stalls, so `0` defaults to `i32` and the retry selects the `i32`
  overload. (The `i32`/generic case above is different: there defaulting has
  nothing free to bind, so it stays ambiguous.)
- Context still wins when it exists — if `id(0)`'s result flows into a parameter of
  known type the argument is concrete *before* the worklist stalls, so no defaulting
  is needed (see the `add(id(1), id(1))` example, where `add`'s `i32`/`u8`
  parameters select the two `id` overloads).

---

## 10. Recursion, in one place

Two independent mechanisms handle recursion; don't conflate them:

- **Bottom-up (learning the schema):** a recursive SCC is iterated to a fixed point.
  Each iteration recomputes every function in the SCC using the previous iteration's
  schemas for the intra-SCC calls, starting from the maximally-general fresh schema.
  Stop when an iteration changes no schema. (foo.flo: "In an SCC if a schema
  changes, we iterate over it again until no schema changes.")
- **Specialize (monomorphizing):** recursion is handled by the memo table. Register
  the in-progress `(function, arg-types)` instance *before* solving its body, so a
  recursive call resolves to that same instance instead of triggering an infinite
  chain of specializations.

---

## 11. Error conditions to report

- **Type mismatch** — `IsEqual` between two conflicting concrete types
  (`i32` vs `bool`).
- **Kind violation** — a `Numeric`-bound variable forced to a non-numeric type
  (`true + 1`).
- **No matching overload** — a `Call`/`Op` pruned to 0 candidates (`NoMatchingOverload`;
  the sharper `CallArityMismatch` / `UndefinedFunction` when that's the cause).
- **Ambiguous call** — a `Call`/`Op` still on ≥2 candidates after the strict
  worklist has stalled *and* defaulting has run without breaking the tie
  (`AmbiguousCall`). An un-pinned numeric literal no longer triggers this (it
  defaults to `i32` and resolves); the remaining cases are ties defaulting can't
  help — e.g. a value with no numeric bound matching two overloads, or a concrete
  value matching both a generic and a concrete overload. The fix is an annotation
  or a more specific type.
- **Multiple `main` definitions** — `main` is the single specialize root and can't
  be overloaded (`MultipleMainDefinitions`).
- **Position error** — a non-value-producing form (`while`, `if`-without-`else`,
  `;`-terminated block, assignment) used in value position. (Structural; can be
  caught before inference.)
- **Unbound name / immutable assignment** — resolver-level, but worth checking
  alongside.

---

## 12. Suggested order of attack for the implementation

1. **AST + resolver** — names bound, value/statement positions marked, overload
   sets gathered per call. Get the position rule (§6) enforced here.
2. **Union-find with kind bounds** — the `IsEqual` + `IsKind` core. Test it in
   isolation on straight-line arithmetic (`1 + 1`, annotations).
3. **Constraint generator** (§4) — one function body → a constraint list.
4. **Solver worklist** (§5) with `Call`/`Op` pruning and **freshened**
   instantiation (§5.1). Test on single non-recursive functions.
5. **Call graph + SCCs + bottom-up pass** (§8.2). Test on the `main`/`foo`/`id`
   chain from foo.flo.
6. **Defaulting** (§7).
7. **Specialize pass with memoization** (§8.3, §10). Test on `fact`.
8. **Overload + ambiguity cases** (§9) last — they're the fiddly bits and having
   everything else solid makes them tractable.

Each numbered step maps onto a block in foo.flo you can use as a regression test.
