# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A bootstrap compiler for **Flo**, a statically typed language, written in Rust (edition 2024) with **zero dependencies**. Only the front end exists today: tokenize → parse → type check. Lowering to IR and codegen are not implemented (`src/lower/` was removed; see git status).

## Commands

```sh
cargo build
cargo run                       # compiles the source string hardcoded in main.rs
cargo test                      # 289 tests, all in src/type_checker/tests.rs
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
- Tracks scopes, `use`d case names (so a bare `Some` parses as a literal, not an unknown ident), `loop_depth` (rejects `break`/`continue` outside a loop), and duplicate declarations.
- Sets fixed types where they are structural, not inferred: `while` is `Void`, `return`/`break`/`continue` are `Never`.

### Type representation ([types.rs](src/types.rs))

The three-way split for user types is the heart of the design:

- `User(name, args)` — **nominal**; same declaration or not the same type.
- `SomeType(cases)` — the **open** type of a type literal. A literal names only a *case*, and many types may have a case by that name, so it does not pick one. Unification narrows it: meeting a `User` validates its cases against that declaration and becomes it; meeting another `SomeType` merges the sets. Never `is_known`.
- `Anon(cases)` — a `SomeType` that never met a declaration, closed over exactly the cases it had. **Structural**, and never equal to a `User` of the same shape.

Also: `Never` is the bottom type (`return`, `break`, `continue`) and is deliberately *not* propagated through unification. `Integer` / `Decimal` are unresolved literal types, defaulted to `i32` / `f32`.

`Type::satisfies_type` prunes overload candidates and **must stay monotone** — as bindings accumulate an answer may go true → false, never the reverse, or a viable overload could be pruned before it was needed.

### Type checker ([src/type_checker/](src/type_checker/))

- [decls.rs](src/type_checker/decls.rs) — whole-program checks on `type` declarations (names exist, arity matches, no infinitely sized type). Runs first and alone: nothing later says anything sensible about a type that doesn't exist.
- [mod.rs](src/type_checker/mod.rs) — a **worklist**, not a loop. Non-generic functions seed the queue; a generic function is never checked as written (it has no single type) but once per instantiation, after `Func::instantiate` substitutes its type params away. Checking one function discovers instantiations that need checking. `MONOMORPHIZATION_LIMIT` (256) is the runaway backstop.
- Per function: collect every `Constraint` in one AST pass (children before parents), then `solve`, then `Func::resolve` rebuilds the tree with concrete types and mangled call names.
- `solve` is staged and each stage can unblock the next: `reduce` to fixpoint → `default_types()` → `reduce` → `close_some_types()` → `reduce` → report. `reduce` retries every pending call and field access until a round commits nothing new, because committing one binds variables that can narrow a neighbour.
- A call commits only when **exactly one** candidate survives pruning. Candidates are instantiated once per call site and reused across rounds — re-instantiating would throw away everything the solver learned about their fresh variables and the fixpoint would never converge.
- [replace_set.rs](src/type_checker/replace_set.rs) — union-find over type variables with the bindings attached to roots.
- Output functions are keyed by **mangled name**: `name__arg1_arg2__ret`, built from the `Debug` of each `Type`. Two overloads may legitimately share one; that's reported at the call site, not the declaration.

## Conventions

- **Comments explain why, not what.** The codebase is unusually heavily commented with design rationale — invariants, why a constraint is a no-op, why an order matters. Match that density and register in new code; it is how the non-obvious parts of the solver are documented.
- **Tests never hardcode mangled names.** Use the `m` / `func_sig` / `func` helpers at the top of [tests.rs](src/type_checker/tests.rs), which compute them via the checker's own `mangle_name`, so changing the mangling scheme can't break a test — only a real change in resolution behavior can.
- Test sources are small Flo programs run through the full front end via `check` / `check_ok` / `check_err` / `parse_err`.
- Every error variant carries a `Loc` and gets a rendering arm in [errors.rs](src/errors.rs).
- Known gaps are tracked as `// FIXME:` comments at the top of [main.rs](src/main.rs), followed by the planned feature order.
