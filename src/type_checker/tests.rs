//! Type checker tests.
//!
//! These run the full front-end pipeline (tokenize -> parse -> type check) on
//! small Flo programs and assert on the resolved output or the reported errors.
//! Because Flo is a binary crate (no `lib.rs`), these live in-crate as a
//! `#[cfg(test)]` module rather than under `tests/`.

use super::{TypeChecker, mangle_name};
use crate::ast::{Expr, ExprKind, Func, Module};
use crate::errors::FloErr;
use crate::parser::Parser;
use crate::tokenizer::Tokenizer;
use crate::types::Type;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Run the whole front end on `src` and return the type checker's result.
fn check(src: &str) -> Result<Module, Vec<FloErr>> {
    let tokens = Tokenizer::new(src).tokenize();
    let module = Parser::new(tokens)
        .parse()
        .expect("test source should parse without errors");
    TypeChecker::new().check(module)
}

/// Expect type checking to succeed and return the resolved module.
fn check_ok(src: &str) -> Module {
    match check(src) {
        Ok(module) => module,
        Err(errs) => panic!("expected type check to succeed, got errors: {errs:?}"),
    }
}

/// Expect type checking to fail and return the errors.
fn check_err(src: &str) -> Vec<FloErr> {
    match check(src) {
        Ok(module) => panic!("expected type check to fail, but it succeeded:\n{module:?}"),
        Err(errs) => errs,
    }
}

fn fn_ty(args: Vec<Type>, ret: Type) -> Type {
    Type::Fn(args, Box::new(ret))
}

/// Look up a resolved (mangled) function by name.
fn func<'a>(module: &'a Module, mangled: &str) -> &'a Func {
    match module.funcs.get(mangled) {
        Some(funcs) => &funcs[0],
        None => panic!(
            "no function `{mangled}`; available: {:?}",
            module.funcs.keys().collect::<Vec<_>>()
        ),
    }
}

/// The resolved (mangled) callee name of a call expression.
fn resolved_call_name(expr: &Expr) -> &str {
    match &expr.kind {
        ExprKind::Call(_, _, Some(name)) => name.as_str(),
        ExprKind::Call(orig, _, None) => panic!("call `{orig}` was left unresolved"),
        other => panic!("expected a call expression, got {other:?}"),
    }
}

/// The argument expressions of a call.
fn call_args(expr: &Expr) -> &[Expr] {
    match &expr.kind {
        ExprKind::Call(_, args, _) => args,
        other => panic!("expected a call expression, got {other:?}"),
    }
}

/// Assert that at least one of the errors matches the given predicate.
macro_rules! assert_err {
    ($errs:expr, $pat:pat) => {{
        let errs = &$errs;
        assert!(
            errs.iter().any(|e| matches!(e, $pat)),
            "expected an error matching `{}`, got: {errs:?}",
            stringify!($pat),
        );
    }};
}

// --------------------------------------------------------------------------
// Basic inference & defaulting
// --------------------------------------------------------------------------

#[test]
fn literal_body_defaults_to_i32() {
    let module = check_ok("fn main() -> i32 = 0;");
    let main = func(&module, "main___i32");
    assert_eq!(main.ty, fn_ty(vec![], Type::I32));
    assert_eq!(main.body.ty, Type::I32);
    assert!(matches!(main.body.kind, ExprKind::Num(0)));
}

#[test]
fn return_type_propagates_to_literal_i8() {
    let module = check_ok("fn main() -> i8 = 42;");
    let main = func(&module, "main___i8");
    assert_eq!(main.ty, fn_ty(vec![], Type::I8));
    assert_eq!(main.body.ty, Type::I8);
}

#[test]
fn return_type_propagates_to_literal_u64() {
    let module = check_ok("fn main() -> u64 = 7;");
    let main = func(&module, "main___u64");
    assert_eq!(main.body.ty, Type::U64);
}

#[test]
fn argument_variable_keeps_its_type() {
    let module = check_ok(
        "
        fn main() -> i32 = id(9);
        fn id(a: i32) -> i32 = a;
        ",
    );
    let id = func(&module, "id__i32_i32");
    assert_eq!(id.ty, fn_ty(vec![Type::I32], Type::I32));
    assert_eq!(id.body.ty, Type::I32);
    assert!(matches!(id.body.kind, ExprKind::Var(_)));
}

#[test]
fn void_function_resolves() {
    // The only way to produce a `void` value is to call a void function, so a
    // self-recursive void function is the smallest example.
    let module = check_ok(
        "
        fn main() = a();
        fn a() = a();
        ",
    );
    let main = func(&module, "main___void");
    assert_eq!(main.ty, fn_ty(vec![], Type::Void));
    assert_eq!(resolved_call_name(&main.body), "a___void");
}

// --------------------------------------------------------------------------
// Call resolution
// --------------------------------------------------------------------------

#[test]
fn single_call_gets_resolved_and_mangled() {
    let module = check_ok(
        "
        fn main() -> i32 = id(1);
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func(&module, "main___i32");
    assert_eq!(resolved_call_name(&main.body), "id__i32_i32");
    // The literal argument was coerced to the parameter type.
    assert_eq!(call_args(&main.body)[0].ty, Type::I32);
}

#[test]
fn chained_calls_resolve() {
    let module = check_ok(
        "
        fn main() -> i32 = a(b(c(0)));
        fn a(x: i32) -> i32 = x;
        fn b(x: i32) -> i32 = x;
        fn c(x: i32) -> i32 = x;
        ",
    );
    let main = func(&module, "main___i32");
    let a = &main.body;
    assert_eq!(resolved_call_name(a), "a__i32_i32");
    let b = &call_args(a)[0];
    assert_eq!(resolved_call_name(b), "b__i32_i32");
    let c = &call_args(b)[0];
    assert_eq!(resolved_call_name(c), "c__i32_i32");
    assert_eq!(call_args(c)[0].ty, Type::I32);
}

#[test]
fn literal_argument_takes_narrow_param_type() {
    let module = check_ok(
        "
        fn main() -> i8 = take(5);
        fn take(a: i8) -> i8 = a;
        ",
    );
    let main = func(&module, "main___i8");
    assert_eq!(resolved_call_name(&main.body), "take__i8_i8");
    assert_eq!(call_args(&main.body)[0].ty, Type::I8);
}

#[test]
fn multi_arg_mangling_and_per_arg_coercion() {
    let module = check_ok(
        "
        fn main() -> i64 = f(1, 2, 3);
        fn f(a: i8, b: u16, c: i64) -> i64 = c;
        ",
    );
    let main = func(&module, "main___i64");
    assert_eq!(resolved_call_name(&main.body), "f__i8_u16_i64_i64");
    let args = call_args(&main.body);
    assert_eq!(args[0].ty, Type::I8);
    assert_eq!(args[1].ty, Type::U16);
    assert_eq!(args[2].ty, Type::I64);
}

// --------------------------------------------------------------------------
// Overload resolution
// --------------------------------------------------------------------------

#[test]
fn overload_selected_by_return_type() {
    let module = check_ok(
        "
        fn main() -> i8 = id(5);
        fn id(a: i8) -> i8 = a;
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func(&module, "main___i8");
    assert_eq!(resolved_call_name(&main.body), "id__i8_i8");
    // Both overloads are still emitted (each is independently well typed).
    func(&module, "id__i8_i8");
    func(&module, "id__i32_i32");
}

#[test]
fn overload_selected_by_arity() {
    let module = check_ok(
        "
        fn main() -> i32 = foo(1);
        fn foo(a: i32) -> i32 = a;
        fn foo(a: i32, b: i32) -> i32 = a;
        ",
    );
    let main = func(&module, "main___i32");
    assert_eq!(resolved_call_name(&main.body), "foo__i32_i32");
}

#[test]
fn nested_overloads_resolve_via_parent_params() {
    // `foo`'s parameter types pin each `id(..)` call to a different overload.
    let module = check_ok(
        "
        fn main() -> i32 = foo(id(0), id(1));
        fn foo(a: i32, b: u8) -> i32 = a;
        fn id(a: i32) -> i32 = a;
        fn id(a: u8) -> u8 = a;
        ",
    );
    let main = func(&module, "main___i32");
    assert_eq!(resolved_call_name(&main.body), "foo__i32_u8_i32");
    let args = call_args(&main.body);
    assert_eq!(resolved_call_name(&args[0]), "id__i32_i32");
    assert_eq!(resolved_call_name(&args[1]), "id__u8_u8");
}

#[test]
fn parent_overload_disambiguated_by_child_return() {
    // The fixpoint case: `outer` can only be chosen after `inner`'s return
    // type (i32) is known, which requires resolving `inner` first.
    let module = check_ok(
        "
        fn main() -> i32 = outer(inner());
        fn inner() -> i32 = 0;
        fn outer(x: i32) -> i32 = x;
        fn outer(x: u8) -> i32 = 0;
        ",
    );
    let main = func(&module, "main___i32");
    assert_eq!(resolved_call_name(&main.body), "outer__i32_i32");
    assert_eq!(resolved_call_name(&call_args(&main.body)[0]), "inner___i32");
}

// --------------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------------

#[test]
fn undefined_function_is_an_error() {
    let errs = check_err("fn main() -> i32 = ghost(0);");
    assert_err!(errs, FloErr::UndefinedFunction { .. });
}

#[test]
fn too_few_arguments_is_an_error() {
    let errs = check_err(
        "
        fn main() -> i32 = foo(1);
        fn foo(a: i32, b: i32) -> i32 = a;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn too_many_arguments_is_an_error() {
    let errs = check_err(
        "
        fn main() -> i32 = foo(1, 2);
        fn foo(a: i32) -> i32 = a;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn no_overload_matches_expected_return_type() {
    // `foo` returns i8, but the call site needs i32.
    let errs = check_err(
        "
        fn main() -> i32 = foo(0);
        fn foo(a: i8) -> i8 = a;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn argument_type_incompatible_is_an_error() {
    // `bar()` yields i32 which cannot be passed to an i8 parameter.
    let errs = check_err(
        "
        fn main() -> i32 = foo(bar());
        fn foo(a: i8) -> i32 = 0;
        fn bar() -> i32 = 0;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn return_type_mismatch_in_body() {
    // Body has type i32 but the declared return type is i8.
    let errs = check_err(
        "
        fn main() -> i32 = 0;
        fn bad(a: i32) -> i8 = a;
        ",
    );
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn void_return_with_integer_body_is_an_error() {
    let errs = check_err("fn main() = 0;");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn integer_literal_matching_multiple_narrow_overloads_defaults_and_misses() {
    // KNOWN LIMITATION: `0` fits both i8 and u8, so neither can be chosen; the
    // literal then defaults to i32, which matches neither overload, yielding
    // "no possible overloads". Documents current defaulting-eagerness behavior.
    let errs = check_err(
        "
        fn main() -> i32 = foo(0);
        fn foo(a: i8) -> i32 = 0;
        fn foo(a: u8) -> i32 = 0;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

// --------------------------------------------------------------------------
// Name mangling unit test
// --------------------------------------------------------------------------

#[test]
fn mangle_name_format() {
    assert_eq!(
        mangle_name(&fn_ty(vec![], Type::I32), "main"),
        "main___i32"
    );
    assert_eq!(
        mangle_name(&fn_ty(vec![Type::I32], Type::I32), "id"),
        "id__i32_i32"
    );
    assert_eq!(
        mangle_name(&fn_ty(vec![Type::I32, Type::U8], Type::I32), "foo"),
        "foo__i32_u8_i32"
    );
    assert_eq!(
        mangle_name(&fn_ty(vec![], Type::Void), "a"),
        "a___void"
    );
}
