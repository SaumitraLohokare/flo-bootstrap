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
fn ambiguous_literal_overload_is_an_error() {
    // A bare numeric literal doesn't disambiguate between the `i32` and `u8`
    // overloads (resolution happens before defaulting), so the call is ambiguous.
    let errs = compile(
        "fn f(x: i32) = x;
         fn f(x: u8) = x;
         fn main() = f(0);",
    )
    .expect_err("literal matches both overloads");
    assert!(matches!(errs[0], FloErr::AmbiguousCall { .. }));
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
fn kind_violation_is_an_error() {
    // `0` is `Integral`, but the parameter it flows into is `void`.
    let errs = compile(
        "fn takes_void(x: void) = x;
         fn main() = takes_void(0);",
    )
    .expect_err("integral literal used where void required");
    assert!(matches!(errs[0], FloErr::UnsatisfiedTypeKind { .. }));
}
