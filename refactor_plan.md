# Refactor plan: records & sums, `.`-prefixed literals, no types in expressions

> **Status: done.** Phases A–D below are implemented and the suite is green (352
> tests). The old syntax is a parse error. §6 records what was decided; the
> ambiguity review in §2 is kept as the rationale for the rules that landed, with
> the resolutions actually taken marked inline. Phase E (`match`) is not started.

Target syntax is the one sketched in [test.flo](test.flo). This document is
three things:

1. **§1–§2** the target grammar, and an ambiguity review of it — what is fine,
   what needs a disambiguation rule, what has to change.
2. **§3–§4** the composability rules for user types, and the type lattice
   (`SomeRecord → AnonRecord | NamedRecord`, same for sums) with the
   zero-initialization rule.
3. **§5** the staged refactor, file by file, ordered so the 329-test suite is a
   regression net through the risky part instead of being red for the duration.

§6 lists the decisions that are genuinely yours to make; every one of them has a
recommendation, and the plan below assumes the recommendation.

---

## 1. Target grammar

### 1.1 Types

```
type        := prim | path type_args? | anon_record | anon_sum
             | '*' type | '[' N? ']' type            -- when pointers/arrays land
anon_record := '{' (field_decl (',' field_decl)* ','?)? '}'
field_decl  := ident ':' type                        -- named field
             | type                                  -- positional field
anon_sum    := case_decl ('|' case_decl)+            -- at least one '|'
case_decl   := ident anon_record?                    -- payload is a record, or absent
```

### 1.2 Declarations

```
type_decl := 'type' ident type_params? '=' decl_body ';'
decl_body := anon_record                             -- a RECORD type
           | case_decl ('|' case_decl)*              -- a SUM type
```

`=` followed by `{` is a record; `=` followed by an identifier is a sum. The old
one-case shorthand (`type Foo = Foo { .. }` ≡ `type Foo = { .. }`) **goes away**:
those are now two different types. `type Foo = { .. }` is a record and supports
`.field`; `type Foo = Foo { .. }` is a one-case sum and does not.

### 1.3 Expressions

```
atom        := ...
             | '.' record_lit                        -- .{ x: 0 }
             | '.' ident ('.' record_lit)?           -- .Some, .Some .{ 0 }
             | path '.' record_lit                   -- Vec.{ .. }      (qualified)
             | path '.' ident ('.' record_lit)?      -- Option.Some.{ .. }
record_lit  := '{' (field_init (',' field_init)* ','?)? '}'
field_init  := ident ':' expr | expr                 -- named or positional
postfix     := atom ('.' ident)*                     -- field access, records only
```

No type may be written inside an expression. The only exceptions stay
`@cast(T) x`, `@sizeof(T)`, `@alignof(T)`. The turbofish is gone; a generic's
arguments are inferred, and the way to force them is to annotate the binding
(`let v: Vec<u8> = ...`) or the signature it flows into.

`use` is gone with it: a bare case name never appears, so nothing needs bringing
into scope. `::` survives only as the future module separator (`foo::Option`).

---

## 2. Ambiguity review

Verdicts: ✅ unambiguous as written · ⚠️ needs a rule (given) · ❌ must change.

### ✅ A1 — `.Dead` is known to be a case name

Answers the question at [test.flo:62](test.flo#L62). A leading `.` can only start
a case literal or a record literal, so *syntactically* there is nothing to
decide. Which **type** it belongs to is inference's job, exactly as it is today:
`.Dead` yields the open type "some sum that has a case `Dead`", and the
parameter type of `anon_enum` narrows it. This is the whole payoff of the `.`
prefix — it is what removes `use` and the "is this bare name a variable or a
case?" rule from the parser.

### ⚠️ A2 — `Type.Case` vs `expr.field`

`Option.Some` and `player.pos` are the same token shape. Resolution order at
`Ident .`:

1. a variable in scope → field access (variable always wins, as today),
2. else a declared type name → qualified literal,
3. else unknown identifier.

Step 2 needs the parser to know every type name **before** it parses any body,
since a `type` may be declared further down the file. Add a
`prescan_type_names` pass — the same shape as the `prescan_file_uses` this
refactor deletes, scanning for `type <Ident>` at brace depth zero.

Consequence to document: a local named `Option` shadows the type qualifier.
(Alternative: keep `::` for qualification — `Option::Some.{ 0 }` — which needs no
prescan at all. See §6, Q1.)

Distinguishing the two qualified forms is one token of lookahead after the dot:
`{` → record literal, `Ident` → case name.

### ⚠️ A3 — a `.` literal glues onto the block before it — *fixed, two ways*

```
let x = {
    if a { f(); }
    .Alive              -- parses as (if a { f(); }).Alive
};
```

`parse_postfix` sees `.` and takes it as field access; the implicit `;` after a
block-shaped statement never gets a chance to fire.

**Rule:** a block-like expression (`{ .. }`, `if`, `while`, `match`) may not be
the receiver of `.`. Parenthesize to access a field of one: `({ .. }).x`. The
postfix loop stops there, the implicit `;` fires, and the snippet above means
what it looks like. This costs nothing real — nobody writes `if c {a} else {b}.x`.

### ⚠️ A4 — an unbraced `if`/`while` body starting with `.` — *avoided: bodies stay scopes*

Same root cause, but the receiver is an ordinary name, so A3's rule does not
help:

```
if a .Alive else .Dead      -- condition eats it: (a.Alive), then `else` is a parse error
if a .Alive;                -- worse: silently a field access, evaluated and discarded
```

This is the price of adopting the unbraced-body form from
[test.flo:101](test.flo#L101). Three ways out:

- **(a)** keep bodies as scopes (status quo) — loses `if foo() bar()`.
- **(b) recommended** — allow unbraced bodies, but a `.`-prefixed literal is not
  one of them: it must be braced or parenthesized. Backed by a targeted
  diagnostic: if the condition *ends* in a field access whose `.` was not
  adjacent to its receiver, report "did you mean this as the body? brace it".
  The `Loc`-adjacency test is already in the parser for `<<` (`peek_shift`).
- **(c)** make field-access `.` adjacency-sensitive language-wide: `a.b` is field
  access, `a .b` is two expressions. Systematic — it fixes A3, A4 and A5 at once,
  and it matches how test.flo is already written (`.Some .{ 0 }` spaced,
  `vec_1.data` not). But whitespace-significant field access is a real
  commitment. See §6, Q2.

### ❌ A5 — match arms need a separator

[test.flo](test.flo) is inconsistent: `anon_enum` separates arms with `,`,
`match_expr` does not. Without one, arm bodies run into the next pattern:

```
0 -> foo()  .Alive -> ...   -- foo().Alive
.A -> -1    .B -> 2         -- 1 .B
```

**Rule:** arms are comma-separated, the comma is optional after a block-like
body (mirroring the implicit `;`), and a trailing comma is allowed.

### ✅ A6 — named vs positional record fields

`{ T }` vs `{ x: T }`, and `.{ 0 }` vs `.{ x: 0 }`: one token of lookahead
(`Ident` followed by `:`). `::` is a distinct token so a module path in a
positional field (`.{ foo::BAR }`) does not collide.

Two things to nail down and document:

- A record is **all-named or all-positional**; mixing is an error.
- `.{ x }` is a positional field whose value is `x` — *not* shorthand for
  `x: x`. (In *patterns* a bare identifier does mean a binding; see A14.)

### ⚠️ A7 — bare `Ident` in a type position

`Foo` could be a named type or a one-case anonymous sum. **Rule:** a bare
identifier is always a named type reference; an anonymous sum needs at least one
`|`, i.e. two or more cases. So `Alive | Dead` works, and a lone
`Foo { x: i32 }` in a type position (identifier followed by `{`) is rejected
with "a single-case anonymous sum is a record — write `{ .. }`".

Implementation: parse the identifier, then look at the next token — `{` or `|`
starts a case chain, anything else makes it a named type. One token, no
backtracking.

### ✅ A8 — `|` in types vs bitwise-or

Type and expression positions never overlap, and every type position ends at a
hard token (`=`, `,`, `)`, `;`, `}`). `fn f() -> A | B = ...` terminates the sum
at `=`. `|>` is a single token, so `A |> B` cannot be misread as a sum.

### ✅ A9 — `|` in a field type vs the enclosing declaration's case separator

`type Foo = A { s: Alive | Dead } | B;` — inside a record body the sum ends at
`,` or `}`, so the outer `|` is unreachable from the inner one. Unambiguous.

### ✅ A10 — `if c {` no longer needs a scope-only body to be safe

Removing `Foo { .. }` as literal syntax means an identifier can never be
followed by a literal brace, so `if foo() {` can only be a scope. This is what
makes A4's unbraced bodies possible at all, and it retires the "the branch must
be a scope so `{` is never read as something else" rule from the current spec.
New consequence: a dangling `else` binds to the nearest `if`.

### ✅ A11 — `match <expr> {`

The scrutinee is parsed greedily and, per A10, cannot swallow the `{`.
`match Player.{ x: 0 } { .. }` parses: the literal closes, then the arms open.

### ⚠️ A12 — `.` and number literals

`.{ 0. }` is fine (`0.` lexes as a float). Two hazards:

- `.5` lexes as `.` then `5` and would parse as a case named `5`; make it an
  explicit error message rather than a confusing one.
- `v.0` lexes fine but `v.0.1` lexes as `v` `.` `0.1`. Which is one more reason
  for the recommendation in §6, Q3: **no positional field access.** Positional
  records exist to be sum-case payloads, and payloads are read by `match`.

### ⚠️ A13 — `-` and `--`

`1 - -4` needs the space; `1 --4` is a comment to end of line. Pre-existing, not
caused by this refactor, but [test.flo:66](test.flo#L66) leans on it — worth a
spec note.

### ⚠️ A14 — patterns: binding vs value

`.Some.{ v }` — `v` is a **binding** (Rust's rule); a literal like `.Some.{ 1 }`
is a value test. So a variable's value cannot be matched against; use a guard.

The wart: for a *named*-field record, `.{ x }` should reasonably mean "bind field
`x`", which is how `is Some { val }` reads in the current spec. That collides
with the positional reading. Resolution: the parser emits a neutral
`FieldPat::Bare(ident)` and the **checker** picks — positional binding for a
positional record, named-field shorthand for a named one. It knows the
scrutinee's type by then; the parser does not.

### ✅ A15 — `.{}`, and the empty record

`{}` as a type is the empty record and `.{}` its literal. No conflict with an
empty scope: type positions do not take scopes. Note that under §4's rule `.{}`
satisfies *every* record type (everything zero-filled), which makes it maximally
ambiguous across overloads — a consequence, not a bug.

### ✅ A16 — `Vec.{ .. }` on a generic type

No type arguments can be written (there is no turbofish), so the qualifier mints
a fresh variable per type parameter and inference fills them in — exactly what
[test.flo:14](test.flo#L14) describes. A type and a function may share the name
`Vec` because their namespaces are separate: `Vec()` is the call, `Vec.{ .. }`
the literal.

### ✅ A17 — no new token kinds

Worth stating: the whole surface change needs `.`, `{`, `}`, `|`, `,` — all of
which already lex. The tokenizer's only edits are the `match` keyword and
dropping `use`.

---

## 3. Composability of user types

The rule that makes `type Player` work: **anywhere a type may be written, any
type may be written.** Concretely,

- a record field's type may be a primitive, a named type, an anonymous record,
  or an anonymous sum, nested to any depth;
- a sum case's payload is an anonymous record written inline (`Some { T }`), or
  absent (`None`);
- an anonymous type written in a signature, a `let` annotation, or a field
  declaration is **concrete** — an `AnonRecord`/`AnonSum`, not something still
  being inferred;
- anonymous types are structural: two anonymous records with the same fields are
  the same type. Two `Alive | Dead` fields in different declarations have the
  same type;
- an anonymous type is **never** equal to a named type of the same shape
  (§6, Q4).

```flo
type Player = {
    pos: { x: i32, y: i32 },        -- anon record field
    dim: { w: i32, h: i32 },        -- (test.flo has `y` here; typo)
    status: Alive | Dead,           -- anon sum field
};
```

Two passes must learn to walk into anonymous types, or this silently
half-works:

- `decls.rs::check_type` — so a bogus named type nested inside
  `{ pos: { p: Nope } }` is reported.
- `decls.rs::visit` (the size/recursion check) — it currently only follows
  `Type::User` fields, so `type A = { b: { c: A } }` would slip through. Cycles
  can now run through anonymous types and through sum-case payloads.

---

## 4. The type lattice

### 4.1 Representation ([types.rs](src/types.rs))

```rust
enum Type {
    // ...
    User(String, Vec<Type>),   // nominal; record-or-sum is the declaration's business
    SomeRecord(Record),        // open, grows fields as inference proceeds
    AnonRecord(Record),        // closed, structural
    SomeSum(Vec<SumCase>),     // open, grows cases
    AnonSum(Vec<SumCase>),     // closed, structural
}

enum Record {
    Named(Vec<(String, Type)>),   // kept sorted by name; order is not identity
    Pos(Vec<Type>),               // index IS the name; never empty
}

struct SumCase { name: String, payload: Option<Type> }   // payload is a record type
```

`User` deliberately does **not** split into `NamedRecord`/`NamedSum`: a name
resolves to exactly one declaration, and which flavour it is lives on
`TypeDecl { kind: DeclKind::Record(Record) | DeclKind::Sum(Vec<SumCase>) }`.
`ReplaceSet` already holds the `TypeTable`, so unification can ask.

`Some*` are never `is_known`. The empty record is only ever `Named(vec![])`.

This is a smaller change than it looks: it is today's `SomeType`/`Anon`/`User`
triple, split into a record flavour and a sum flavour. Every invariant the
current design documents carries over.

### 4.2 Unification

Writing `R*`/`Ra`/`Rn` for some/anon/named record and `S*`/`Sa`/`Sn` for sums:

| meet | result |
| --- | --- |
| `R* ∪ R*` | union of fields, shared ones unified → `R*` |
| `R* ∪ Ra` | `R*`'s fields ⊆ `Ra`'s, types unified → `Ra` |
| `R* ∪ Rn` | declaration must be a record; `R*`'s fields ⊆ its fields at those args → `Rn` |
| `Ra ∪ Ra` | same field set exactly (structural) |
| `Ra ∪ Rn` | **error** — anonymous is never nominal |
| `Rn ∪ Rn` | same name, unify arguments (as today) |
| `S* ∪ S*` | union of cases, shared ones unified → `S*` |
| `S* ∪ Sa` | `S*`'s cases ⊆ `Sa`'s → `Sa` |
| `S* ∪ Sn` | cases ⊆ the declaration's → `Sn` |
| `Sa ∪ Sa` | same case set exactly |
| `Sa ∪ Sn` | **error** |
| record ∪ sum | **error** (`RecordVsSum`) |

`close_some_types` closes both: `R* → Ra`, `S* → Sa`.

Positional records unify by prefix and the longer length wins; the missing tail
is zero-filled (§4.3). Named and positional never unify with each other.

Case payloads: absent/absent unifies, present/present unifies the record types,
absent against present is an error ("case `Some` takes a payload").

`satisfies_type` mirrors this with "could still be made equal" semantics, and
**stays monotone** — the invariant the current code calls out. A `SomeRecord`
only ever gains fields, and gaining one can only turn a subset test from true to
false, never back.

### 4.3 Zero initialization

> If a `SomeRecord` has fewer fields than expected, zero-initialize those
> fields. More than expected, or an unexpected field, is an error.

Two halves, and keeping them apart is what makes this sound:

- **Types.** Unification only ever checks a *subset* relation (the table above).
  It records nothing about what was missing. That is correct not just for
  literals: if a variable's type is `SomeRecord{x}` and it meets
  `{ x: i32, y: i32 }`, the value already has a `y` at runtime — nothing needs
  filling, the type was merely not yet known.
- **Elaboration.** Filling in is purely a *record literal* concern, and happens
  in `Expr::resolve`, once the literal's type is concrete: compare the literal's
  fields against the resolved record, append an `ExprKind::Zeroed(ty)` init for
  each missing field, and reorder into declaration order so codegen can lay it
  out positionally. An extra field cannot reach here — unification rejected it.

Consequences to accept up front:

- Unifying two record literals zero-fills *both*: `.{ x: 1 }` meeting `.{ y: 2 }`
  gives `{x, y}`, and each literal gets the other's field zeroed. That is what
  "a `SomeRecord` can add more fields as inference goes on" means in practice.
- Overload resolution gets more ambiguous: `.{ x: 0 }` satisfies both
  `{ x: i32 }` and `{ x: i32, y: i32 }`, so an overload set that differs only in
  extra record fields is undecidable at a literal call site. `.{}` satisfies
  every record. Fine, but it will show up as `MultiplePossibleOverloads`.

### 4.4 Field access

`.field` is legal on records only: `Rn`, `Ra`, `R*`. Every sum is rejected —
including a **one-case** sum, which is a behaviour change from today's "rejected
when there is more than one case". Until `match` lands, a sum can be built but
not read, which is where the language already stands with `is` unimplemented.

`lookup_field` keeps its current shape (return `Ok(None)` while the receiver is
unknown, let the final pass report), and gains a slice arm later for
`slice.count` ([test.flo:196](test.flo#L196)).

Whether an *unknown* receiver should be driven to `SomeRecord{field: T}` — so
`let v; v.x = 1; v.y = 2;` infers an anonymous record — is §6 Q5. Recommended:
not yet.

---

## 5. Refactor phases

Phases B and C were planned as two steps, with B keeping the old surface syntax
alive behind temporary glue so the existing tests stayed green through the
unifier rewrite. **That glue was dropped**: the old syntax had to be invalid at
the end anyway, and carrying it through one step risked leaving some of it
behind. B and C landed together instead, and the test suite was swept in one go.

### Phase A — the spec

Rewrite [first_spec.flo](first_spec.flo); it is the contract everything else is
checked against. §2.6 becomes records + sums; §2.7 becomes `.{ .. }` / `.Case`;
§2.8 becomes records-only field access; §3.7 (`use`) is deleted; §4.8 (`is`)
becomes `match`; §5.3 loses the turbofish; §4.3/§4.4 gain unbraced bodies and
the A3/A4/A5 rules; §7 is refreshed. Add the A6/A7/A14 rules explicitly — they
are the ones a reader will otherwise get wrong.

Gated on §6.

### Phase B — the lattice, old syntax intact

- [types.rs](src/types.rs): `Record`, `SumCase`, `DeclKind`, the four new `Type`
  variants replacing `SomeType`/`Anon`. Update `is_known`, `substitute`,
  `satisfies_type`, `Debug`. `TypeDecl::case_at` → `payload_at` + `record_at`.
- [replace_set.rs](src/type_checker/replace_set.rs): the §4.2 table in
  `unify_types`; `close_some_types` closes both flavours; `occurs` walks records
  and payloads.
- [decls.rs](src/type_checker/decls.rs): `check_type` and `visit` recurse into
  anonymous types (§3). Record/sum kind on declarations.
- [parser.rs](src/parser.rs): `type Foo = { .. }` builds a record declaration,
  `type Foo = A { .. } | B` a sum. Anonymous records and sums in type positions.

### Phase C — the surface syntax

- [tokenizer.rs](src/tokenizer.rs): add `match`, drop `use`. Nothing else (A17).
- [ast.rs](src/ast.rs): `ExprKind::RecordLit(Option<String>, Vec<FieldInit>)`;
  `CaseLit(Option<String>, String, Option<Box<Expr>>)` where the payload is a
  `RecordLit`; `FieldInit.name: FieldName`. Delete `Call`'s `type_args`,
  `StmtKind::Use`, `UseDecl`, `Module::uses`.
- [parser.rs](src/parser.rs): leading-dot literals; `Type.Case` / `Type.{`
  with `prescan_type_names` (A2); anonymous types in type positions (A7);
  postfix rule (A3); unbraced `if`/`while`/`else` bodies + the A4 diagnostic;
  `match` with comma-separated arms and patterns (A5, A14).
  **Deletions:** `parse_turbofish`, `parse_use`, `prescan_file_uses`,
  `Scope::cases`/`add_case`/`knows_case`, the one-case declaration shorthand,
  and the "bare name might be a case" branch of `parse_atom`.
- [type_checker/mod.rs](src/type_checker/mod.rs): `RecordLit` collects a
  `SomeRecord` constraint, `CaseLit` a `SomeSum` with the payload's variable;
  the qualifier stays one more equality. `instantiate_candidates` loses its
  turbofish handling.
- [decls.rs](src/type_checker/decls.rs): drop `check_uses`; `walk_written_types`
  loses the turbofish arm.
- [errors.rs](src/errors.rs): delete `UseOutsideStatementPosition`; add
  `RecordVsSum`, `MixedFieldKinds`, `PositionalArityMismatch`,
  `PayloadMismatch`, `SingleCaseAnonSum`, `NotAReceiver`, and (Phase D)
  `UnexpectedField`.
- Sweep [tests.rs](src/type_checker/tests.rs): ~170 literal sites, 89 type
  declarations, 45 `use`s, 16 turbofishes. Mechanical. Plus the `src` string in
  [main.rs](src/main.rs#L28).

### Phase D — zero-init, and records-only field access

- Subset rule in unification, `ExprKind::Zeroed` + elaboration in
  `Expr::resolve` (§4.3), field order normalized to declaration order.
- `lookup_field` rejects every sum, accepts every record (§4.4).
- Tests: rewrite the 3 `WrongFields` cases; add zero-fill, positional records,
  mixed-kind rejection, `Anon` ≠ `Named`, record-vs-sum mismatch, and
  field-access-on-one-case-sum.

### Phase E — `match`

Its own project: patterns, bindings, guards, exhaustiveness. Until it lands,
sums are write-only — the same position `is` leaves the language in today, so
nothing regresses. Everything above is independent of it.

---

## 6. Decisions taken

**Q1 — qualification separator: `.`, with no prescan.** A2's prescan turned out
to be unnecessary: whether a name is a type is not decided while parsing at all.
`Ident .` is field access if a variable of that name is in scope and a qualified
literal otherwise, and `check_qualifiers` reports a qualifier that names no type
(`NotATypeOrVariable`). `::` is rejected in an expression with a
"module paths are not implemented" error, so old `Type::Case` and `f::<T>()`
spellings say what is wrong.

**Q2 — mandatory `;`, and no unbraced bodies.** There is no implicit `;` anywhere:
every statement ends in one, block-shaped ones included, and the tail is the one
expression that goes without. `if`/`while` bodies stay scopes, so A4 does not
arise. A3 is additionally closed by the rule that a block-shaped expression is
never the receiver of `.` — so a missing `;` before a `.` literal reports as a
missing `;`.

**Q3 — positional access is `._0`.** An ordinary identifier, so it needs no
tokenizer change and `v._0._1` chains. `_<digits>` is therefore reserved as a
field name in a named record (`ReservedFieldName`).

**Q4 — anonymous and named are peers.** An anonymous record or sum is never equal
to a declared type of the same shape. An open type pins straight to a declared
one, and closes to the anonymous form only when nothing pinned it.

**Q5 — field access does not grow its receiver.** An access waits for the
receiver's type, as before. Open records still grow by *merging*, which is what
makes `.{ x: 1 }` meeting `.{ y: 2 }` give `{ x, y }` with each literal zero
filling the other's field.

**Q6 — `match` is deferred (Phase E).** The keyword is reserved and reports
`NotImplemented`, so a program using it as a name breaks now rather than later.

---

## 7. Notes on test.flo itself

- [:38](test.flo#L38) `dim: { w: i32, y: i32 }` — `y` should be `h`.
- [:55](test.flo#L55) vs [:117](test.flo#L117) — arm separators are inconsistent
  (A5).
- [:66](test.flo#L66) `- -4` — the space is load-bearing (A13).
- Pointers, arrays, slices, strings and `nil` appear throughout and remain
  unimplemented; the programs using them are specification, not test input.
