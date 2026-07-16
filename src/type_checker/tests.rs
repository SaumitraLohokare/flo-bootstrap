use super::TypeChecker;
use crate::{errors::FloErr, parser::Parser, tokenizer::Tokenizer};

/// Run the whole pipeline (tokenize → parse → type-check + specialize) and,
/// on success, return the monomorphized functions as sorted `fn … = …;` lines.
/// Sorting makes the result independent of the `funcs` HashMap's iteration
/// order so tests can compare against a fixed set.
fn compile(src: &str) -> Result<Vec<String>, Vec<FloErr>> {
    let src = src.to_string();
    let tokens = Tokenizer::new(&src).tokenize();
    let mut module = Parser::new(tokens).parse().map_err(|e| vec![e])?;

    let resolved = TypeChecker::new(&mut module).check()?;

    let dump = format!("{resolved:?}");
    let mut lines: Vec<String> = dump
        .lines()
        .filter(|l| l.starts_with("fn "))
        .map(str::to_string)
        .collect();
    lines.sort();
    Ok(lines)
}

/// Assert that `src` type-checks and produces exactly `expected` (order
/// insensitive).
fn assert_funcs(src: &str, expected: &[&str]) {
    let got = compile(src).expect("expected a successful type check");
    let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
}

/// Like `compile`, but keeps each function's full (possibly multi-line) body
/// intact — a scope body prints across several lines, so the `starts_with("fn ")`
/// filter in `compile` would drop everything but the first line. Splits the dump
/// into per-function blocks (a block starts at a `fn ` line and runs until the
/// next one) and sorts them, again so the result is HashMap-order independent.
fn compile_blocks(src: &str) -> Result<Vec<String>, Vec<FloErr>> {
    let src = src.to_string();
    let tokens = Tokenizer::new(&src).tokenize();
    let mut module = Parser::new(tokens).parse().map_err(|e| vec![e])?;

    let resolved = TypeChecker::new(&mut module).check()?;

    let dump = format!("{resolved:?}");
    let mut blocks: Vec<String> = Vec::new();
    for line in dump.lines() {
        if line.starts_with("fn ") {
            blocks.push(line.to_string());
        } else if let Some(last) = blocks.last_mut() {
            // Body / closing-brace continuation of the current function.
            last.push('\n');
            last.push_str(line);
        }
        // The leading "Module:" header (before any `fn`) is ignored.
    }
    blocks.sort();
    Ok(blocks)
}

/// Like `assert_funcs`, but block-aware (see `compile_blocks`) so `expected`
/// entries may be multi-line scope bodies.
fn assert_blocks(src: &str, expected: &[&str]) {
    let got = compile_blocks(src).expect("expected a successful type check");
    let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn numeric_literal_defaults_to_i32() {
    // Nothing constrains the literal, so defaulting (the last resort) picks i32.
    assert_funcs("fn main() = 0;", &["fn main() -> i32 = 0:i32;"]);
}

#[test]
fn polymorphic_passthrough_chain() {
    // `bar` is monomorphized at i32; the annotation on `foo` pins the literal.
    // Every non-`main` instance is mangled with its argument types and return type.
    assert_funcs(
        "fn main() = foo();
         fn foo() -> i32 = bar(0);
         fn bar(x: 'a) -> 'a = x;",
        &[
            "fn main() -> i32 = foo$i32():i32;",
            "fn foo$i32() -> i32 = bar$i32$i32(0:i32):i32;",
            "fn bar$i32$i32(i32) -> i32 = var_0:i32;",
        ],
    );
}

#[test]
fn self_recursion_terminates() {
    // The recursive call resolves to the in-progress `fact$i32$i32` instance
    // instead of specializing forever.
    assert_funcs(
        "fn main() = fact(id(0));
         fn fact(n: i32) -> i32 = fact(n);
         fn id(x: 'a) -> 'a = x;",
        &[
            "fn main() -> i32 = fact$i32$i32(id$i32$i32(0:i32):i32):i32;",
            "fn fact$i32$i32(i32) -> i32 = fact$i32$i32(var_0:i32):i32;",
            "fn id$i32$i32(i32) -> i32 = var_0:i32;",
        ],
    );
}

#[test]
fn caller_context_beats_defaulting() {
    // Regression: `zero`'s free numeric return must adopt the `u8` demanded by
    // `foo`'s parameter, *not* default to i32. With no arguments its instance is
    // distinguished solely by that return type (`zero$u8`).
    assert_funcs(
        "fn zero() = 0;
         fn foo(x: u8) = x;
         fn main() = foo(zero());",
        &[
            "fn main() -> u8 = foo$u8$u8(zero$u8():u8):u8;",
            "fn foo$u8$u8(u8) -> u8 = var_0:u8;",
            "fn zero$u8() -> u8 = 0:u8;",
        ],
    );
}

#[test]
fn unconstrained_return_polymorphic_defaults() {
    // With no caller context, `zero`'s return falls back to the i32 default.
    assert_funcs(
        "fn main() = zero();
         fn zero() = 0;",
        &[
            "fn main() -> i32 = zero$i32():i32;",
            "fn zero$i32() -> i32 = 0:i32;",
        ],
    );
}

#[test]
fn undefined_function_is_an_error() {
    let errs = compile("fn main() = nope();").expect_err("call to unknown fn");
    assert!(matches!(errs[0], FloErr::UndefinedFunction { .. }));
}

#[test]
fn call_arity_mismatch_is_an_error() {
    let errs = compile(
        "fn main() = foo(0);
         fn foo() = 0;",
    )
    .expect_err("too many arguments");
    assert!(matches!(errs[0], FloErr::CallArityMismatch { .. }));
}

#[test]
fn overload_selected_by_argument_type() {
    // Two overloads of `f` differ by parameter type. The concrete `u8` flowing in
    // from `use_it` selects the `u8` overload; the `i32` overload is unreachable
    // from `main` and never emitted.
    assert_funcs(
        "fn f(x: i32) -> i32 = x;
         fn f(x: u8) -> u8 = x;
         fn use_it(a: u8) = f(a);
         fn main() = use_it(0);",
        &[
            "fn main() -> u8 = use_it$u8$u8(0:u8):u8;",
            "fn use_it$u8$u8(u8) -> u8 = f$u8$u8(var_0:u8):u8;",
            "fn f$u8$u8(u8) -> u8 = var_0:u8;",
        ],
    );
}

#[test]
fn overload_selected_by_arity() {
    // `f` is overloaded on arity. Both overloads are reachable from the single
    // `main` expression and both get monomorphized.
    assert_funcs(
        "fn f() -> u8 = 0;
         fn f(x: u8) -> u8 = x;
         fn main() = f(f());",
        &[
            "fn main() -> u8 = f$u8$u8(f$u8():u8):u8;",
            "fn f$u8() -> u8 = 0:u8;",
            "fn f$u8$u8(u8) -> u8 = var_0:u8;",
        ],
    );
}

#[test]
fn return_type_overload_resolved_by_context() {
    // `make` is overloaded only on return type. The `u8` parameter of `take`
    // demands `u8`, which disambiguates the call — the `i32` overload is pruned.
    assert_funcs(
        "fn make() -> i32 = 0;
         fn make() -> u8 = 0;
         fn take(x: u8) = x;
         fn main() = take(make());",
        &[
            "fn main() -> u8 = take$u8$u8(make$u8():u8):u8;",
            "fn take$u8$u8(u8) -> u8 = var_0:u8;",
            "fn make$u8() -> u8 = 0:u8;",
        ],
    );
}

#[test]
fn literal_overload_resolved_by_defaulting() {
    // When resolution stalls, a still-free numeric literal defaults to i32 and the
    // solver retries (resolve → default → resolve), selecting the i32 overload.
    // The u8 overload is never reached from main and is not emitted.
    assert_funcs(
        "fn f(x: i32) = x;
         fn f(x: u8) = x;
         fn main() = f(0);",
        &[
            "fn main() -> i32 = f$i32$i32(0:i32):i32;",
            "fn f$i32$i32(i32) -> i32 = var_0:i32;",
        ],
    );
}

#[test]
fn ambiguous_return_type_overload_is_an_error() {
    // With no caller context to pin the return type, the two return-type-only
    // overloads of `make` can't be told apart.
    let errs = compile(
        "fn make() -> i32 = 0;
         fn make() -> u8 = 0;
         fn main() = make();",
    )
    .expect_err("return type is unconstrained");
    assert!(matches!(errs[0], FloErr::AmbiguousCall { .. }));
}

#[test]
fn no_matching_overload_is_an_error() {
    // `g` passes a `void` to `f`, but neither overload of `f` accepts `void`.
    let errs = compile(
        "fn f(x: i32) -> i32 = x;
         fn f(x: u8) -> u8 = x;
         fn g(y: void) = f(y);
         fn main() = 0;",
    )
    .expect_err("no overload accepts void");
    assert!(matches!(errs[0], FloErr::NoMatchingOverload { .. }));
}

#[test]
fn overloaded_main_is_an_error() {
    let errs = compile(
        "fn main() = 0;
         fn main() = 1;",
    )
    .expect_err("main cannot be overloaded");
    assert!(matches!(errs[0], FloErr::MultipleMainDefinitions { .. }));
}

#[test]
fn binary_operator_desugars_to_builtin_call() {
    // `+` desugars to a call against the built-in overload, monomorphized at i32
    // and mangled like any other function; its body is the intrinsic sentinel.
    // The free operands default to i32 (resolve → default → resolve), selecting
    // the i32 overload.
    assert_funcs(
        "fn main() = 1 + 2;",
        &[
            "fn main() -> i32 = +$i32$i32$i32(1:i32, 2:i32):i32;",
            "fn +$i32$i32$i32(i32, i32) -> i32 = <intrinsic>:i32;",
        ],
    );
}

#[test]
fn operator_precedence_is_respected() {
    // `*` binds tighter than `+`, so this is `1 + (2 * 3)`.
    assert_funcs(
        "fn main() = 1 + 2 * 3;",
        &[
            "fn main() -> i32 = +$i32$i32$i32(1:i32, *$i32$i32$i32(2:i32, 3:i32):i32):i32;",
            "fn +$i32$i32$i32(i32, i32) -> i32 = <intrinsic>:i32;",
            "fn *$i32$i32$i32(i32, i32) -> i32 = <intrinsic>:i32;",
        ],
    );
}

#[test]
fn binary_operators_are_left_associative() {
    // `1 - 2 - 3` parses as `(1 - 2) - 3`, not `1 - (2 - 3)`.
    assert_funcs(
        "fn main() = 1 - 2 - 3;",
        &[
            "fn main() -> i32 = -$i32$i32$i32(-$i32$i32$i32(1:i32, 2:i32):i32, 3:i32):i32;",
            "fn -$i32$i32$i32(i32, i32) -> i32 = <intrinsic>:i32;",
        ],
    );
}

#[test]
fn unary_minus_desugars_to_arity_one_call() {
    // Unary `-` shares the `-` name with binary `-`; arity distinguishes the
    // one-argument overload, which mangles with a single argument tag.
    assert_funcs(
        "fn main() = -5;",
        &[
            "fn main() -> i32 = -$i32$i32(5:i32):i32;",
            "fn -$i32$i32(i32) -> i32 = <intrinsic>:i32;",
        ],
    );
}

#[test]
fn context_still_beats_defaulting_for_operators() {
    // Defaulting is only a last resort: a `u8`-typed operand pins the operator
    // before the stall triggers defaulting, so this resolves to the u8 overload
    // rather than the i32 default.
    assert_funcs(
        "fn add(a: u8) -> u8 = a + 1;
         fn main() = add(2);",
        &[
            "fn main() -> u8 = add$u8$u8(2:u8):u8;",
            "fn add$u8$u8(u8) -> u8 = +$u8$u8$u8(var_0:u8, 1:u8):u8;",
            "fn +$u8$u8$u8(u8, u8) -> u8 = <intrinsic>:u8;",
        ],
    );
}

#[test]
fn operator_overload_selected_by_operand_type() {
    // The `u8` parameters flow into `+`, selecting its `u8` overload; the i32
    // overload is never reached and never emitted.
    assert_funcs(
        "fn add(a: u8, b: u8) -> u8 = a + b;
         fn main() = add(1, 2);",
        &[
            "fn main() -> u8 = add$u8$u8$u8(1:u8, 2:u8):u8;",
            "fn add$u8$u8$u8(u8, u8) -> u8 = +$u8$u8$u8(var_0:u8, var_1:u8):u8;",
            "fn +$u8$u8$u8(u8, u8) -> u8 = <intrinsic>:u8;",
        ],
    );
}

#[test]
fn operator_on_non_numeric_operand_is_an_error() {
    // No built-in `+` overload accepts `void`, so the desugared call matches no
    // overload — reported (as an operator) via `NoMatchingOverload`.
    let errs = compile(
        "fn bad(x: void) = x + 1;
         fn main() = 0;",
    )
    .expect_err("+ has no void overload");
    assert!(matches!(errs[0], FloErr::NoMatchingOverload { .. }));
}

#[test]
fn kind_violation_is_an_error() {
    // `0` is `Integral`, but the parameter it flows into is `void`.
    let errs = compile(
        "fn takes_void(x: void) = x;
         fn main() = takes_void(0);",
    )
    .expect_err("integral literal used where void required");
    assert!(matches!(errs[0], FloErr::UnsatisfiedTypeKind { .. }));
}

// -------------------------------------------------------------------------
// Booleans
// -------------------------------------------------------------------------

#[test]
fn bool_literal_is_bool() {
    // A `bool` literal is concretely typed — no defaulting is involved (unlike a
    // numeric literal, whose type is only a kind bound until defaulting runs).
    assert_funcs("fn main() = true;", &["fn main() -> bool = true:bool;"]);
}

#[test]
fn logical_and_desugars_to_builtin_call() {
    // `&&` desugars to a call against the built-in bool overload, monomorphized and
    // mangled like any other function; its body is the intrinsic sentinel.
    assert_funcs(
        "fn main() = true && false;",
        &[
            "fn main() -> bool = &&$bool$bool$bool(true:bool, false:bool):bool;",
            "fn &&$bool$bool$bool(bool, bool) -> bool = <intrinsic>:bool;",
        ],
    );
}

#[test]
fn logical_or_desugars_to_builtin_call() {
    assert_funcs(
        "fn main() = true || false;",
        &[
            "fn main() -> bool = ||$bool$bool$bool(true:bool, false:bool):bool;",
            "fn ||$bool$bool$bool(bool, bool) -> bool = <intrinsic>:bool;",
        ],
    );
}

#[test]
fn logical_not_desugars_to_arity_one_call() {
    assert_funcs(
        "fn main() = !true;",
        &[
            "fn main() -> bool = !$bool$bool(true:bool):bool;",
            "fn !$bool$bool(bool) -> bool = <intrinsic>:bool;",
        ],
    );
}

#[test]
fn bool_flows_through_a_parameter() {
    // A `bool` argument selects the (only) instance and pins the passthrough's
    // parameter and return, mangling with the `bool` tag.
    assert_funcs(
        "fn id(x: bool) -> bool = x;
         fn main() = id(true);",
        &[
            "fn main() -> bool = id$bool$bool(true:bool):bool;",
            "fn id$bool$bool(bool) -> bool = var_0:bool;",
        ],
    );
}

#[test]
fn numeric_literal_where_bool_required_is_an_error() {
    // The numeric literal `0` is `Integral`, but `&&`'s only overload takes `bool`;
    // pinning the literal to `bool` violates its kind bound.
    let errs = compile("fn main() = true && 0;").expect_err("0 is not a bool");
    assert!(matches!(errs[0], FloErr::UnsatisfiedTypeKind { .. }));
}

// -------------------------------------------------------------------------
// Scopes
// -------------------------------------------------------------------------

#[test]
fn empty_scope_is_void() {
    // Regression: an empty scope has no tail, so its type is `void`. Without the
    // scope-type constraint the scope variable would float free and fail to
    // resolve (it has no kind bound, so defaulting can't touch it).
    assert_blocks("fn main() = {};", &["fn main() -> void = {\n\n};"]);
}

#[test]
fn scope_evaluates_to_its_tail_expression() {
    // Regression for the reported bug: a scope's type is its tail expression's
    // type. `main`'s block ends in `foo()`, so `main` returns i32; the leading
    // statements are still type-checked and monomorphized.
    assert_blocks(
        "fn main() = {
             nop();
             foo();
             foo()
         };
         fn nop() -> void = {};
         fn foo() = 0;",
        &[
            "fn main() -> i32 = {\n  nop$void():void;\n  foo$i32():i32;\n  foo$i32():i32\n};",
            "fn nop$void() -> void = {\n\n};",
            "fn foo$i32() -> i32 = 0:i32;",
        ],
    );
}

#[test]
fn scope_tail_can_be_bool() {
    // The tail's type flows out as the scope's type regardless of what that type
    // is — here a `bool`, so `main` returns `bool`.
    // A tail-only scope prints a blank line after `{` (it has no leading
    // statements), and a scope expression carries no `:type` suffix of its own.
    assert_blocks(
        "fn main() = { true };",
        &["fn main() -> bool = {\n\n  true:bool\n};"],
    );
}

#[test]
fn nested_scope_type_propagates_outward() {
    // The inner scope's tail (`0`) types the inner scope, which is the outer
    // scope's tail, which types `main` — i32 all the way up.
    assert_blocks(
        "fn main() = { { 0 } };",
        &["fn main() -> i32 = {\n\n  {\n\n    0:i32\n}\n};"],
    );
}

#[test]
fn scope_can_be_a_call_argument() {
    // A scope is an ordinary expression, so it may appear as a call argument; its
    // tail type (i32) is what flows into the call.
    assert_blocks(
        "fn id(x: i32) -> i32 = x;
         fn main() = id({ 0 });",
        &[
            "fn main() -> i32 = id$i32$i32({\n\n  0:i32\n}):i32;",
            "fn id$i32$i32(i32) -> i32 = var_0:i32;",
        ],
    );
}
