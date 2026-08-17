# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A bootstrap compiler for **Flo**, a statically typed language, written in Rust (edition 2024) with **zero dependencies**. Only the front end exists today: tokenize → parse → type check. Lowering to IR and codegen are not implemented (`src/lower/` was removed; see git status).

## Commands

```sh
cargo build
cargo run                       # compiles the source string hardcoded in main.rs
cargo test                      # 352 tests, all in src/type_checker/tests.rs
cargo test literal_body_defaults_to_i32   # single test by name
cargo test -- --nocapture       # see printed output
```

There is **no CLI file argument**. To try a Flo program, edit the `src` string literal in [main.rs:28](src/main.rs#L28). The `.flo` files in the repo root are specs and scratch programs the compiler never reads.

Flo is a binary crate with no `lib.rs`, so tests cannot live under `tests/` — they are an in-crate `#[cfg(test)] mod tests` under the type checker.

## Language spec

[first_spec.flo](first_spec.flo) is the authoritative spec, written as a commented Flo file. **Section 7 (Implementation Status)** lists exactly what is implemented and what isn't — read it before assuming a feature exists or is missing. [implementation.md](implementation.md) is an older, coarser staging plan and is partly out of date.

## Pipeline

`Tokenizer` → `Parser` → `check_entry_point` → `check_type_decls` → `TypeChecker` → typed `Module`

Each stage in [main.rs](src/main.rs) prints and exits on error; errors are `FloErr` values carrying a `Loc`, rendered by `err.pretty_print(&src)`.

### The parser does more than parse

[parser.rs](src/parser.rs) resolves names and shapes the whole AST for the checker:

- Hands out **variable ids** and **type variable ids** (via `util::Iota`); the counts land in `Module::var_count` / `type_var_count` so later passes can mint non-colliding ids with `Iota::seeded`.
- Every `Expr` leaves the parser with `ty` already set — usually a fresh `Type::T(n)`. The checker *asserts* on this: `Call` and `Field` expressions must carry a `Type::T`, since its id is the key their constraints are recorded under.
- Tracks scopes (variables only — a case name is only ever written after a `.`, so nothing needs bringing into scope), `loop_depth` (rejects `break`/`continue` outside a loop), and duplicate declarations.
- Sets fixed types where they are structural, not inferred: `while` is `Void`, `return`/`break`/`continue` are `Never`, `@sizeof`/`@alignof` are `U64`, and a `@cast`'s type is the one written into it.
- Joins `<<` and `>>` itself, from two adjacent `<`/`>` tokens (`Parser::peek_shift`). Neither can be a token, because `View<View<i32>>` closes two argument lists with two `>` in a row.
- Decides which *kind* each `type` declaration is from what follows the `=` (a `{` is a record, a name is a sum), and whether each field is named or positional (an identifier followed by `:` is named). It does **not** decide whether a literal's qualifier names a real type — the declaration may be further down the file, so `foo.bar` parses as a qualified literal and `check_qualifiers` reports it.
- `Ident .` is field access when a variable of that name is in scope and a literal's qualifier when one is not. The variable always wins.
- A block-shaped expression is never the receiver of `.` (`Parser::parse_postfix`). Since every literal *begins* with `.`, gluing one on would silently swallow the next statement; stopping there turns it into the missing-`;` error it is. `({ .. }).x` still works — the parenthesized form runs its own field chain.

### Type representation ([types.rs](src/types.rs))

The lattice for user types is the heart of the design. Records and sums are separate kinds, and each has an open form that inference narrows:

- `User(name, args)` — **nominal**; same declaration or not the same type. Whether it is a record or a sum is *not* recorded here: a name resolves to one declaration, so that question goes to the `TypeTable` (`TypeDecl::kind`), which unification and pruning both have to hand.
- `SomeRecord(Record)` — the **open** type of a record literal, and the only type that can still **grow**. `.{ x: 0 }` says only "something with an `x`". Meeting a concrete record checks these fields against it; meeting another open record takes the **union** of the two field sets.
- `SomeSum(cases)` — the open type of a case literal. A literal names only a *case*, and many sums may have one by that name, so it does not pick one.
- `AnonRecord(Record)` / `AnonSum(cases)` — an open type that never met a concrete one, closed over exactly what it had; also what an anonymous type *written down* means, because written means concrete. **Structural**, and never equal to a `User` of the same shape.

A `Record` is `Named` (sorted by name, so order is not identity) or `Pos` (index *is* the name, `_0`/`_1`, so order is). The two never unify. A `SumCase`'s payload is a record, or absent — and absent is not the same as an empty one.

**Zero initialization is not in the type.** Unifying an open record with a concrete one only ever checks that its fields are a *subset*; nothing records what was missing. Filling in is elaboration on record *literals*, done in `Expr::resolve` via `fill_record`, because which fields a literal left out is a property of that literal and not of the type several of them share. A value whose type merely wasn't known yet needs no filling.

Also: `Never` is the bottom type (`return`, `break`, `continue`) and is deliberately *not* propagated through unification. `Integer` / `Decimal` are unresolved literal types, defaulted to `i32` / `f32`.

`Type::satisfies_type` prunes overload candidates and **must stay monotone** — as bindings accumulate an answer may go true → false, never the reverse, or a viable overload could be pruned before it was needed. Every "is this a subset of that" arm is monotone for the same reason: an open record only gains fields, an open sum only gains cases.

### Type checker ([src/type_checker/](src/type_checker/))

- [decls.rs](src/type_checker/decls.rs) — whole-program checks on `type` declarations (names exist, arity matches, every qualifier names a type, no infinitely sized type). Runs first and alone: nothing later says anything sensible about a type that doesn't exist. Both walks recurse *through* anonymous records and sums — a cycle is a cycle whether or not every step has a name.
- [mod.rs](src/type_checker/mod.rs) — a **worklist**, not a loop. Non-generic functions seed the queue; a generic function is never checked as written (it has no single type) but once per instantiation, after `Func::instantiate` substitutes its type params away. Checking one function discovers instantiations that need checking. `MONOMORPHIZATION_LIMIT` (256) is the runaway backstop.
- Per function: collect every `Constraint` in one AST pass (children before parents), then `solve`, then `Func::resolve` rebuilds the tree with concrete types, mangled call names, and record literals filled out to every field.
- `TypeChecker::solve_constraint` hands two types with no variable at the root to the unifier rather than comparing them. Comparing would be wrong as well as unhelpful: two records can differ by an `{integer}` in a field alone.
- `solve` is staged and each stage can unblock the next: `reduce` to fixpoint → `default_types()` → `reduce` → `close_some_types()` → `reduce` → report. `reduce` retries every pending call and field access until a round commits nothing new, because committing one binds variables that can narrow a neighbour.
- A call commits only when **exactly one** candidate survives pruning. Candidates are instantiated once per call site and reused across rounds — re-instantiating would throw away everything the solver learned about their fresh variables and the fixpoint would never converge.
- Before that, `bind_agreed_params` binds any argument whose type *every* surviving candidate agrees on — sound because the set only shrinks. Without it a wide overload set (the 64 `<<` builtins, whose operands may differ in width) would still be waiting when `default_types()` ran, and defaulting would answer `i32` however the context was annotated.
- [replace_set.rs](src/type_checker/replace_set.rs) — union-find over type variables with the bindings attached to roots.
- Output functions are keyed by **mangled name**: `name__arg1_arg2__ret`, built from the `Debug` of each `Type`. Two overloads may legitimately share one; that's reported at the call site, not the declaration.

## Conventions

- **Comments explain why, not what.** The codebase is unusually heavily commented with design rationale — invariants, why a constraint is a no-op, why an order matters. Match that density and register in new code; it is how the non-obvious parts of the solver are documented.
- **Tests never hardcode mangled names.** Use the `m` / `func_sig` / `func` helpers at the top of [tests.rs](src/type_checker/tests.rs), which compute them via the checker's own `mangle_name`, so changing the mangling scheme can't break a test — only a real change in resolution behavior can.
- Test sources are small Flo programs run through the full front end via `check` / `check_ok` / `check_err` / `parse_err`.
- Every error variant carries a `Loc` and gets a rendering arm in [errors.rs](src/errors.rs).
- Known gaps are tracked as `// FIXME:` comments at the top of [main.rs](src/main.rs), followed by the planned feature order.
