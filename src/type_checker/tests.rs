//! Type checker tests.
//!
//! These run the full front-end pipeline (tokenize -> parse -> type check) on
//! small Flo programs and assert on the resolved output or the reported errors.
//! Because Flo is a binary crate (no `lib.rs`), these live in-crate as a
//! `#[cfg(test)]` module rather than under `tests/`.
//!
//! Expected mangled names are never hard-coded: they are computed with the
//! checker's own [`mangle_name`] via the [`m`] / [`func_sig`] helpers. That way
//! changing the mangling scheme or adding new operators/overloads can't break
//! these tests — only a genuine change in *resolution* behavior can.

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

/// The mangled name for a `(name, args, ret)` signature, produced by the
/// checker's own scheme. Using this instead of a string literal keeps tests
/// correct when the mangling format changes.
fn m(name: &str, args: Vec<Type>, ret: Type) -> String {
    mangle_name(&fn_ty(args, ret), name)
}

/// Look up a resolved function by its `name`, argument types and return type.
fn func_sig<'a>(module: &'a Module, name: &str, args: Vec<Type>, ret: Type) -> &'a Func {
    func(module, &m(name, args, ret))
}

/// Look up a resolved (mangled) function by its already-mangled name.
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
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.ty, fn_ty(vec![], Type::I32));
    assert_eq!(main.body.ty, Type::I32);
    assert!(matches!(main.body.kind, ExprKind::Num(0)));
}

#[test]
fn return_type_propagates_to_literal_i8() {
    let module = check_ok("fn main() -> i8 = 42;");
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(main.ty, fn_ty(vec![], Type::I8));
    assert_eq!(main.body.ty, Type::I8);
}

#[test]
fn return_type_propagates_to_literal_u64() {
    let module = check_ok("fn main() -> u64 = 7;");
    let main = func_sig(&module, "main", vec![], Type::U64);
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
    let id = func_sig(&module, "id", vec![Type::I32], Type::I32);
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
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(main.ty, fn_ty(vec![], Type::Void));
    assert_eq!(resolved_call_name(&main.body), m("a", vec![], Type::Void));
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
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("id", vec![Type::I32], Type::I32)
    );
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
    let main = func_sig(&module, "main", vec![], Type::I32);
    let a = &main.body;
    assert_eq!(resolved_call_name(a), m("a", vec![Type::I32], Type::I32));
    let b = &call_args(a)[0];
    assert_eq!(resolved_call_name(b), m("b", vec![Type::I32], Type::I32));
    let c = &call_args(b)[0];
    assert_eq!(resolved_call_name(c), m("c", vec![Type::I32], Type::I32));
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
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(
        resolved_call_name(&main.body),
        m("take", vec![Type::I8], Type::I8)
    );
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
    let main = func_sig(&module, "main", vec![], Type::I64);
    assert_eq!(
        resolved_call_name(&main.body),
        m("f", vec![Type::I8, Type::U16, Type::I64], Type::I64)
    );
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
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(
        resolved_call_name(&main.body),
        m("id", vec![Type::I8], Type::I8)
    );
    // Both overloads are still emitted (each is independently well typed).
    func_sig(&module, "id", vec![Type::I8], Type::I8);
    func_sig(&module, "id", vec![Type::I32], Type::I32);
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
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("foo", vec![Type::I32], Type::I32)
    );
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
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("foo", vec![Type::I32, Type::U8], Type::I32)
    );
    let args = call_args(&main.body);
    assert_eq!(
        resolved_call_name(&args[0]),
        m("id", vec![Type::I32], Type::I32)
    );
    assert_eq!(
        resolved_call_name(&args[1]),
        m("id", vec![Type::U8], Type::U8)
    );
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
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("outer", vec![Type::I32], Type::I32)
    );
    assert_eq!(
        resolved_call_name(&call_args(&main.body)[0]),
        m("inner", vec![], Type::I32)
    );
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
// Floats & decimal defaulting
// --------------------------------------------------------------------------

#[test]
fn float_literal_propagates_from_return_type_f32() {
    let module = check_ok("fn main() -> f32 = 1.5;");
    let main = func_sig(&module, "main", vec![], Type::F32);
    assert_eq!(main.ty, fn_ty(vec![], Type::F32));
    assert_eq!(main.body.ty, Type::F32);
    assert!(matches!(main.body.kind, ExprKind::Flt(_)));
}

#[test]
fn float_literal_propagates_from_return_type_f64() {
    let module = check_ok("fn main() -> f64 = 3.25;");
    let main = func_sig(&module, "main", vec![], Type::F64);
    assert_eq!(main.body.ty, Type::F64);
}

#[test]
fn decimal_literal_defaults_to_f32() {
    // Neither float literal is pinned to a concrete width by the `<` operator
    // (its float overloads accept f32 and f64), so both default to f32.
    let module = check_ok("fn main() -> bool = 1.5 < 2.5;");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("<", vec![Type::F32, Type::F32], Type::Bool)
    );
    let args = call_args(&main.body);
    assert_eq!(args[0].ty, Type::F32);
    assert_eq!(args[1].ty, Type::F32);
}

#[test]
fn float_argument_coerces_literal_to_param_type() {
    // A {decimal} literal argument is pinned to the concrete parameter type,
    // mirroring the {integer} case in `literal_argument_takes_narrow_param_type`.
    let module = check_ok(
        "
        fn main() -> f64 = take(2.0);
        fn take(a: f64) -> f64 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::F64);
    assert_eq!(
        resolved_call_name(&main.body),
        m("take", vec![Type::F64], Type::F64)
    );
    assert_eq!(call_args(&main.body)[0].ty, Type::F64);
}

#[test]
fn float_literal_for_integer_return_is_an_error() {
    let errs = check_err("fn main() -> i32 = 1.5;");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

// --------------------------------------------------------------------------
// Bools
// --------------------------------------------------------------------------

#[test]
fn bool_literal_resolves() {
    let module = check_ok("fn main() -> bool = true;");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(main.ty, fn_ty(vec![], Type::Bool));
    assert_eq!(main.body.ty, Type::Bool);
    assert!(matches!(main.body.kind, ExprKind::Bool(true)));
}

#[test]
fn bool_argument_keeps_its_type() {
    let module = check_ok(
        "
        fn main() -> bool = negate(false);
        fn negate(a: bool) -> bool = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("negate", vec![Type::Bool], Type::Bool)
    );
    assert_eq!(call_args(&main.body)[0].ty, Type::Bool);
}

#[test]
fn bool_body_for_integer_return_is_an_error() {
    let errs = check_err("fn main() -> i32 = true;");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

// --------------------------------------------------------------------------
// Binary operators: resolution, result types & mangling
// --------------------------------------------------------------------------

#[test]
fn arithmetic_operator_resolves_to_builtin() {
    let module = check_ok("fn main() -> i32 = 1 + 2;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::I32, Type::I32], Type::I32)
    );
    assert_eq!(main.body.ty, Type::I32);
    let args = call_args(&main.body);
    assert_eq!(args[0].ty, Type::I32);
    assert_eq!(args[1].ty, Type::I32);
}

#[test]
fn arithmetic_operator_takes_narrow_return_type() {
    // The i8 return type flows down into both operands and picks the i8 overload.
    let module = check_ok("fn main() -> i8 = 1 + 2;");
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::I8, Type::I8], Type::I8)
    );
    assert_eq!(main.body.ty, Type::I8);
}

#[test]
fn arithmetic_operator_over_variables() {
    let module = check_ok(
        "
        fn main() -> i32 = add(1, 2);
        fn add(a: i32, b: i32) -> i32 = a * b;
        ",
    );
    let add = func_sig(&module, "add", vec![Type::I32, Type::I32], Type::I32);
    assert_eq!(
        resolved_call_name(&add.body),
        m("*", vec![Type::I32, Type::I32], Type::I32)
    );
    assert_eq!(add.body.ty, Type::I32);
}

#[test]
fn all_arithmetic_operators_resolve() {
    for op in ["+", "-", "*", "/", "%"] {
        let src = format!("fn main() -> i32 = 6 {op} 3;");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::I32);
        assert_eq!(
            resolved_call_name(&main.body),
            m(op, vec![Type::I32, Type::I32], Type::I32)
        );
    }
}

#[test]
fn float_arithmetic_operator_resolves() {
    let module = check_ok("fn main() -> f32 = 1.0 + 2.0;");
    let main = func_sig(&module, "main", vec![], Type::F32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::F32, Type::F32], Type::F32)
    );
    assert_eq!(main.body.ty, Type::F32);
}

#[test]
fn comparison_operators_yield_bool() {
    for op in ["==", "!=", "<", ">", "<=", ">="] {
        let src = format!("fn main() -> bool = 1 {op} 2;");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::Bool);
        assert_eq!(
            resolved_call_name(&main.body),
            m(op, vec![Type::I32, Type::I32], Type::Bool)
        );
        // Operands defaulted to i32, but the result is bool.
        assert_eq!(main.body.ty, Type::Bool);
        assert_eq!(call_args(&main.body)[0].ty, Type::I32);
    }
}

#[test]
fn equality_operator_works_on_bools() {
    let module = check_ok("fn main() -> bool = true == false;");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("==", vec![Type::Bool, Type::Bool], Type::Bool)
    );
    assert_eq!(main.body.ty, Type::Bool);
}

#[test]
fn logical_operators_require_bools() {
    for op in ["&&", "||"] {
        let src = format!("fn main() -> bool = true {op} false;");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::Bool);
        assert_eq!(
            resolved_call_name(&main.body),
            m(op, vec![Type::Bool, Type::Bool], Type::Bool)
        );
    }
}

#[test]
fn bitwise_operators_resolve_on_integers() {
    for op in ["&", "|", "^"] {
        let src = format!("fn main() -> i32 = 6 {op} 3;");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::I32);
        assert_eq!(
            resolved_call_name(&main.body),
            m(op, vec![Type::I32, Type::I32], Type::I32)
        );
        assert_eq!(main.body.ty, Type::I32);
    }
}

#[test]
fn bitwise_operators_resolve_on_bools() {
    // `&`, `|` and `^` also have bool overloads (unlike `&&`/`||`).
    for op in ["&", "|", "^"] {
        let src = format!("fn main() -> bool = true {op} false;");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::Bool);
        assert_eq!(
            resolved_call_name(&main.body),
            m(op, vec![Type::Bool, Type::Bool], Type::Bool)
        );
    }
}

// --------------------------------------------------------------------------
// Binary operator errors
// --------------------------------------------------------------------------

#[test]
fn operator_on_mixed_bool_and_int_is_an_error() {
    // No `+` overload accepts (bool, {integer}).
    let errs = check_err("fn main() -> i32 = true + 1;");
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn operator_on_mismatched_int_widths_is_an_error() {
    // There is no implicit widening: `+` only has same-width overloads.
    let errs = check_err(
        "
        fn main() -> i32 = add(1, 2);
        fn add(a: i8, b: i32) -> i32 = a + b;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn logical_operator_on_integers_is_an_error() {
    // `&&` only has a (bool, bool) overload.
    let errs = check_err("fn main() -> bool = 1 && 2;");
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn arithmetic_result_used_where_bool_expected_is_an_error() {
    // `1 + 2` can only be an integer type, never bool.
    let errs = check_err("fn main() -> bool = 1 + 2;");
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

// --------------------------------------------------------------------------
// Operator precedence (matches C's relative precedence ordering)
//
// Precedence is verified structurally: the operator that binds *tighter*
// ends up deeper in the resolved call tree. Helpers below name the operator
// at the root and at a given argument position.
// --------------------------------------------------------------------------

/// The (single) `main` function, whatever return type it was mangled with.
fn main_func(module: &Module) -> &Func {
    module
        .funcs
        .iter()
        .find(|(name, _)| name.starts_with("main__"))
        .map(|(_, funcs)| &funcs[0])
        .expect("no main function")
}

/// Root operator name and the operator name nested at argument `idx`.
fn op_and_nested(module: &Module, arg_idx: usize) -> (String, String) {
    let body = &main_func(module).body;
    let root = resolved_call_name(body).to_string();
    let nested = resolved_call_name(&call_args(body)[arg_idx]).to_string();
    (root, nested)
}

/// A binary operator's mangled name for `(lhs, rhs) -> ret`, all the same width.
fn op(name: &str, operand: Type, ret: Type) -> String {
    m(name, vec![operand.clone(), operand], ret)
}

#[test]
fn mul_binds_tighter_than_add_on_the_right() {
    // 1 + 2 * 3  ==  1 + (2 * 3)  -> `*` nested under the right arg of `+`.
    let module = check_ok("fn main() -> i32 = 1 + 2 * 3;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("+", Type::I32, Type::I32));
    assert_eq!(nested, op("*", Type::I32, Type::I32));
}

#[test]
fn mul_binds_tighter_than_add_on_the_left() {
    // 1 * 2 + 3  ==  (1 * 2) + 3  -> `*` nested under the left arg of `+`.
    let module = check_ok("fn main() -> i32 = 1 * 2 + 3;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("+", Type::I32, Type::I32));
    assert_eq!(nested, op("*", Type::I32, Type::I32));
}

#[test]
fn div_and_mod_bind_tighter_than_sub() {
    // 8 - 6 / 2  ==  8 - (6 / 2)
    let module = check_ok("fn main() -> i32 = 8 - 6 / 2;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("-", Type::I32, Type::I32));
    assert_eq!(nested, op("/", Type::I32, Type::I32));
}

#[test]
fn add_binds_tighter_than_comparison() {
    // 1 + 2 < 3  ==  (1 + 2) < 3
    let module = check_ok("fn main() -> bool = 1 + 2 < 3;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("<", Type::I32, Type::Bool));
    assert_eq!(nested, op("+", Type::I32, Type::I32));
}

#[test]
fn comparison_binds_tighter_than_equality() {
    // 1 < 2 == true  ==  (1 < 2) == true
    let module = check_ok("fn main() -> bool = 1 < 2 == true;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("==", Type::Bool, Type::Bool));
    assert_eq!(nested, op("<", Type::I32, Type::Bool));
}

#[test]
fn equality_binds_tighter_than_bitwise_and() {
    // true & false == true  ==  true & (false == true)
    let module = check_ok("fn main() -> bool = true & false == true;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("&", Type::Bool, Type::Bool));
    assert_eq!(nested, op("==", Type::Bool, Type::Bool));
}

#[test]
fn bitwise_and_binds_tighter_than_bitwise_xor() {
    // true ^ false & true  ==  true ^ (false & true)
    let module = check_ok("fn main() -> bool = true ^ false & true;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("^", Type::Bool, Type::Bool));
    assert_eq!(nested, op("&", Type::Bool, Type::Bool));
}

#[test]
fn bitwise_xor_binds_tighter_than_bitwise_or() {
    // true | false ^ true  ==  true | (false ^ true)
    let module = check_ok("fn main() -> bool = true | false ^ true;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("|", Type::Bool, Type::Bool));
    assert_eq!(nested, op("^", Type::Bool, Type::Bool));
}

#[test]
fn bitwise_or_binds_tighter_than_logical_and() {
    // true && false | true  ==  true && (false | true)
    let module = check_ok("fn main() -> bool = true && false | true;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("&&", Type::Bool, Type::Bool));
    assert_eq!(nested, op("|", Type::Bool, Type::Bool));
}

#[test]
fn logical_and_binds_tighter_than_logical_or() {
    // true || false && true  ==  true || (false && true)
    let module = check_ok("fn main() -> bool = true || false && true;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("||", Type::Bool, Type::Bool));
    assert_eq!(nested, op("&&", Type::Bool, Type::Bool));
}

#[test]
fn full_precedence_ladder_nests_deepest_operator_last() {
    // A chain touching every precedence level, associating rightward:
    //   a || b && c | d ^ e & f == g < h + i * j
    // Each operator binds tighter than the one to its left, so the tree is a
    // right-leaning spine ending in the `*` (tightest) node. The leaves are
    // chosen so every level type-checks: bool down to the `==`, then the `<`
    // compares integers (yielding the bool that `==` consumes).
    let module = check_ok(
        "fn main() -> bool =
            true || true && true | true ^ true & true == 2 < 3 + 4 * 5;",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    let expected = [
        op("||", Type::Bool, Type::Bool),
        op("&&", Type::Bool, Type::Bool),
        op("|", Type::Bool, Type::Bool),
        op("^", Type::Bool, Type::Bool),
        op("&", Type::Bool, Type::Bool),
        op("==", Type::Bool, Type::Bool),
        op("<", Type::I32, Type::Bool),
        op("+", Type::I32, Type::I32),
        op("*", Type::I32, Type::I32),
    ];
    let mut node = &main.body;
    for expected_name in expected {
        assert_eq!(resolved_call_name(node), expected_name);
        // Every level nests its tighter-binding neighbour in the right arg.
        node = &call_args(node)[1];
    }
}

// --------------------------------------------------------------------------
// Operator associativity
// --------------------------------------------------------------------------

#[test]
fn binary_operators_are_left_associative() {
    // Like C, operators of equal precedence associate left-to-right: the parser
    // recurses with `parse_expr(op_precedence + 1)`, so `1 - 2 - 3` parses as
    // `(1 - 2) - 3`. The nested subtraction therefore sits in the LEFT operand.
    let module = check_ok("fn main() -> i32 = 1 - 2 - 3;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        op("-", Type::I32, Type::I32)
    );
    let args = call_args(&main.body);
    assert_eq!(resolved_call_name(&args[0]), op("-", Type::I32, Type::I32));
    assert!(matches!(args[1].kind, ExprKind::Num(3)));
}

#[test]
fn division_is_left_associative() {
    // `16 / 4 / 2` == `(16 / 4) / 2` == 2, not `16 / (4 / 2)` == 8. This only
    // comes out right if `/` associates leftward (a right-assoc parse would
    // change the result), so it's a meaningful associativity guard.
    let module = check_ok("fn main() -> i32 = 16 / 4 / 2;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let args = call_args(&main.body);
    assert_eq!(resolved_call_name(&args[0]), op("/", Type::I32, Type::I32));
    assert!(matches!(args[1].kind, ExprKind::Num(2)));
}

// --------------------------------------------------------------------------
// Unary operators
//
// Only `+` and `-` are unary, and only over the numeric types (there is no
// unary bool overload). A unary op parses to a single-argument `Call`, so it
// resolves and mangles exactly like a one-arg function.
// --------------------------------------------------------------------------

#[test]
fn unary_minus_resolves_to_builtin() {
    let module = check_ok("fn main() -> i32 = -5;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("-", vec![Type::I32], Type::I32)
    );
    assert_eq!(main.body.ty, Type::I32);
    let args = call_args(&main.body);
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].ty, Type::I32);
}

#[test]
fn unary_plus_resolves_to_builtin() {
    let module = check_ok("fn main() -> i32 = +5;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::I32], Type::I32)
    );
    assert_eq!(main.body.ty, Type::I32);
}

#[test]
fn unary_minus_takes_narrow_return_type() {
    // The i8 return type flows into the operand and picks the i8 overload.
    let module = check_ok("fn main() -> i8 = -1;");
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(
        resolved_call_name(&main.body),
        m("-", vec![Type::I8], Type::I8)
    );
    assert_eq!(main.body.ty, Type::I8);
    assert_eq!(call_args(&main.body)[0].ty, Type::I8);
}

#[test]
fn unary_minus_on_float() {
    let module = check_ok("fn main() -> f32 = -1.5;");
    let main = func_sig(&module, "main", vec![], Type::F32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("-", vec![Type::F32], Type::F32)
    );
    assert_eq!(main.body.ty, Type::F32);
}

#[test]
fn unary_minus_over_a_variable() {
    let module = check_ok(
        "
        fn main() -> i32 = neg(5);
        fn neg(a: i32) -> i32 = -a;
        ",
    );
    let neg = func_sig(&module, "neg", vec![Type::I32], Type::I32);
    assert_eq!(
        resolved_call_name(&neg.body),
        m("-", vec![Type::I32], Type::I32)
    );
    assert_eq!(neg.body.ty, Type::I32);
    assert!(matches!(call_args(&neg.body)[0].kind, ExprKind::Var(_)));
}

#[test]
fn double_unary_minus_nests() {
    // `- -5` == `-(-5)`: an outer unary `-` wrapping an inner unary `-`.
    let module = check_ok("fn main() -> i32 = - -5;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("-", vec![Type::I32], Type::I32)
    );
    let inner = &call_args(&main.body)[0];
    assert_eq!(resolved_call_name(inner), m("-", vec![Type::I32], Type::I32));
    assert!(matches!(call_args(inner)[0].kind, ExprKind::Num(5)));
}

#[test]
fn unary_minus_binds_tighter_than_addition() {
    // `-1 + 2` == `(-1) + 2` -> unary `-` nested under the left arg of `+`.
    let module = check_ok("fn main() -> i32 = -1 + 2;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("+", Type::I32, Type::I32));
    assert_eq!(nested, m("-", vec![Type::I32], Type::I32));
}

#[test]
fn unary_operator_on_bool_is_an_error() {
    // There is no unary `-` overload for bool.
    let errs = check_err("fn main() -> bool = -true;");
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn unary_minus_result_where_bool_expected_is_an_error() {
    // `-1` can only be a numeric type, never bool.
    let errs = check_err("fn main() -> bool = -1;");
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

// --------------------------------------------------------------------------
// Function piping
//
// `x |> f(rest...)` desugars during parsing into `f(x, rest...)` — the
// left-hand side is inserted as the first argument. Because the desugaring
// happens in the parser, the type checker sees an ordinary call: these tests
// verify the resulting call resolves the way the equivalent direct call would.
// --------------------------------------------------------------------------

#[test]
fn pipe_becomes_first_argument() {
    let module = check_ok(
        "
        fn main() -> i32 = 5 |> id();
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("id", vec![Type::I32], Type::I32)
    );
    let args = call_args(&main.body);
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].ty, Type::I32);
    assert!(matches!(args[0].kind, ExprKind::Num(5)));
}

#[test]
fn pipe_without_parens_still_calls() {
    // The argument list is optional after `|>`; `5 |> id` is `id(5)`.
    let module = check_ok(
        "
        fn main() -> i32 = 5 |> id;
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("id", vec![Type::I32], Type::I32)
    );
    assert!(matches!(call_args(&main.body)[0].kind, ExprKind::Num(5)));
}

#[test]
fn pipe_prepends_to_existing_arguments() {
    // `1 |> add(2)` == `add(1, 2)`: the piped value goes *before* the written
    // arguments.
    let module = check_ok(
        "
        fn main() -> i32 = 1 |> add(2);
        fn add(a: i32, b: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("add", vec![Type::I32, Type::I32], Type::I32)
    );
    let args = call_args(&main.body);
    assert_eq!(args.len(), 2);
    assert!(matches!(args[0].kind, ExprKind::Num(1)));
    assert!(matches!(args[1].kind, ExprKind::Num(2)));
}

#[test]
fn chained_pipes_nest_left_to_right() {
    // `1 |> inc() |> inc()` == `inc(inc(1))`: the outer call is the last stage.
    let module = check_ok(
        "
        fn main() -> i32 = 1 |> inc() |> inc();
        fn inc(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let outer = &main.body;
    assert_eq!(resolved_call_name(outer), m("inc", vec![Type::I32], Type::I32));
    let inner = &call_args(outer)[0];
    assert_eq!(resolved_call_name(inner), m("inc", vec![Type::I32], Type::I32));
    assert!(matches!(call_args(inner)[0].kind, ExprKind::Num(1)));
}

#[test]
fn pipe_selects_overload_by_return_type() {
    let module = check_ok(
        "
        fn main() -> u8 = 3 |> id();
        fn id(a: i32) -> i32 = a;
        fn id(a: u8) -> u8 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::U8);
    assert_eq!(
        resolved_call_name(&main.body),
        m("id", vec![Type::U8], Type::U8)
    );
}

#[test]
fn pipe_binds_tighter_than_binary_operator() {
    // `1 |> inc() + 2` == `inc(1) + 2`: the pipe is consumed before the parser
    // considers the trailing `+`, so it does not pipe the whole `... + 2`.
    let module = check_ok(
        "
        fn main() -> i32 = 1 |> inc() + 2;
        fn inc(a: i32) -> i32 = a;
        ",
    );
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("+", Type::I32, Type::I32));
    assert_eq!(nested, m("inc", vec![Type::I32], Type::I32));
}

#[test]
fn pipe_into_undefined_function_is_an_error() {
    let errs = check_err("fn main() -> i32 = 5 |> ghost();");
    assert_err!(errs, FloErr::UndefinedFunction { .. });
}

#[test]
fn pipe_argument_type_incompatible_is_an_error() {
    // `true |> inc()` is `inc(true)`, but `inc` needs an i32.
    let errs = check_err(
        "
        fn main() -> bool = true |> inc();
        fn inc(a: i32) -> i32 = a;
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

/// The scope's statement expressions and its optional tail expression.
fn scope_parts(expr: &Expr) -> (&[Expr], Option<&Expr>) {
    match &expr.kind {
        ExprKind::Scope(exprs, tail) => (exprs, tail.as_deref()),
        other => panic!("expected a scope expression, got {other:?}"),
    }
}

/// The optional operand of a return expression.
fn return_value(expr: &Expr) -> Option<&Expr> {
    match &expr.kind {
        ExprKind::Return(value) => value.as_deref(),
        other => panic!("expected a return expression, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Scope expressions
//
// A `{ stmt; stmt; tail }` block is an expression. Its type is the type of the
// trailing (semicolon-less) expression, or `void` when there is no tail (empty
// block, or one ending in `;`). Statement-position expressions still have to be
// fully resolved even though their values are discarded — that is the property
// the call-resolution passes must recurse into.
// --------------------------------------------------------------------------

#[test]
fn empty_scope_is_void() {
    let module = check_ok("fn main() = {};");
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(main.ty, fn_ty(vec![], Type::Void));
    assert_eq!(main.body.ty, Type::Void);
    let (stmts, tail) = scope_parts(&main.body);
    assert!(stmts.is_empty());
    assert!(tail.is_none());
}

#[test]
fn scope_type_is_its_tail_type() {
    let module = check_ok("fn main() -> bool = { true };");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(main.body.ty, Type::Bool);
    let (stmts, tail) = scope_parts(&main.body);
    assert!(stmts.is_empty());
    assert!(matches!(tail.unwrap().kind, ExprKind::Bool(true)));
    assert_eq!(tail.unwrap().ty, Type::Bool);
}

#[test]
fn scope_tail_literal_defaults_to_i32() {
    let module = check_ok("fn main() -> i32 = { 0 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    let (_, tail) = scope_parts(&main.body);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn scope_return_type_propagates_into_tail_literal() {
    // The declared i8 return type flows through the scope into the tail literal.
    let module = check_ok("fn main() -> i8 = { 42 };");
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(main.body.ty, Type::I8);
    let (_, tail) = scope_parts(&main.body);
    assert_eq!(tail.unwrap().ty, Type::I8);
}

#[test]
fn scope_with_trailing_semicolon_is_void() {
    // A block ending in `;` has no tail, so it is `void` regardless of the last
    // statement's own type.
    let module = check_ok(
        "
        fn main() = { nop(); };
        fn nop() = {};
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(main.body.ty, Type::Void);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmts.len(), 1);
    assert!(tail.is_none());
}

#[test]
fn statement_position_calls_are_resolved() {
    // Regression: calls that sit in statement position (before the tail) must be
    // resolved by the call-resolution passes, not just the tail. Previously the
    // passes only recursed into `Call` args and skipped `Scope` bodies entirely,
    // leaving statement calls with unresolved type variables.
    let module = check_ok(
        "
        fn main() -> bool = {
            nop();
            true && false
        };
        fn nop() = {};
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    let (stmts, tail) = scope_parts(&main.body);

    assert_eq!(stmts.len(), 1);
    assert_eq!(resolved_call_name(&stmts[0]), m("nop", vec![], Type::Void));
    assert_eq!(stmts[0].ty, Type::Void);

    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("&&", vec![Type::Bool, Type::Bool], Type::Bool)
    );
}

#[test]
fn scope_resolves_statements_and_tail_calls_together() {
    // Several calls across statement and tail positions all resolve.
    let module = check_ok(
        "
        fn main() -> i32 = {
            id(1);
            id(2);
            id(3)
        };
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmts.len(), 2);
    for call in stmts.iter().chain(std::iter::once(tail.unwrap())) {
        assert_eq!(resolved_call_name(call), m("id", vec![Type::I32], Type::I32));
    }
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn nested_scopes_resolve() {
    let module = check_ok("fn main() -> i32 = { { 7 } };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    let (_, outer_tail) = scope_parts(&main.body);
    let inner = outer_tail.unwrap();
    assert_eq!(inner.ty, Type::I32);
    let (_, inner_tail) = scope_parts(inner);
    assert_eq!(inner_tail.unwrap().ty, Type::I32);
}

#[test]
fn scope_sees_enclosing_function_arguments() {
    // The block inherits the function's parameters, so `a` resolves inside it.
    let module = check_ok(
        "
        fn id(a: i32) -> i32 = { a };
        fn main() -> i32 = id(1);
        ",
    );
    let id = func_sig(&module, "id", vec![Type::I32], Type::I32);
    let (_, tail) = scope_parts(&id.body);
    assert!(matches!(tail.unwrap().kind, ExprKind::Var(_)));
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn scope_tail_type_mismatch_is_an_error() {
    // The tail is a bool but the declared return type is i32.
    let errs = check_err("fn main() -> i32 = { true };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn empty_scope_for_non_void_return_is_an_error() {
    // An empty block is `void`, which cannot satisfy an i32 return type.
    let errs = check_err("fn main() -> i32 = {};");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn unresolved_statement_call_is_an_error() {
    // A statement-position call to an unknown function is still reported.
    let errs = check_err(
        "
        fn main() -> bool = {
            ghost();
            true
        };
        ",
    );
    assert_err!(errs, FloErr::UndefinedFunction { .. });
}

/// The condition, `then` branch and optional `else` branch of an if expression.
fn if_parts(expr: &Expr) -> (&Expr, &Expr, Option<&Expr>) {
    match &expr.kind {
        ExprKind::If(cond, then, otherwise) => (cond, then, otherwise.as_deref()),
        other => panic!("expected an if expression, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// If expressions
//
// `if cond { then } else { else }` is an expression whose type is the shared
// type of its branches. Without an `else` it is `void` (and the `then` branch
// must then be `void` too). The condition must be `bool`. Crucially, the `if`
// expression's own type is tied to its branches, so a narrow return type flows
// down into branch literals and mismatched branches are rejected.
// --------------------------------------------------------------------------

#[test]
fn if_with_matching_branches_resolves() {
    let module = check_ok("fn main() -> i32 = if true { 1 } else { 2 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    let (cond, then, otherwise) = if_parts(&main.body);
    assert!(matches!(cond.kind, ExprKind::Bool(true)));
    assert_eq!(then.ty, Type::I32);
    assert_eq!(otherwise.unwrap().ty, Type::I32);
}

#[test]
fn if_return_type_propagates_into_branch_literals() {
    // Regression: the `if` expression's type must be linked to its branches, so
    // the declared i8 return type flows down into *both* branch literals. Before
    // the fix the branches defaulted to i32 while the `if` was independently i8.
    let module = check_ok("fn main() -> i8 = if true { 1 } else { 2 };");
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(main.body.ty, Type::I8);
    let (_, then, otherwise) = if_parts(&main.body);
    assert_eq!(then.ty, Type::I8);
    assert_eq!(otherwise.unwrap().ty, Type::I8);
}

#[test]
fn if_without_else_is_void() {
    let module = check_ok(
        "
        fn main() = if true { nop() };
        fn nop() = {};
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(main.body.ty, Type::Void);
    let (_, then, otherwise) = if_parts(&main.body);
    assert_eq!(then.ty, Type::Void);
    assert!(otherwise.is_none());
}

#[test]
fn if_branches_resolve_calls() {
    // Calls in either branch are resolved, and the branch/if types agree.
    let module = check_ok(
        "
        fn main() -> i32 = if true { id(1) } else { id(2) };
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    let (_, then, otherwise) = if_parts(&main.body);
    // Each branch is a `{ id(..) }` scope; the call is its tail.
    let then_call = scope_parts(then).1.unwrap();
    let else_call = scope_parts(otherwise.unwrap()).1.unwrap();
    assert_eq!(resolved_call_name(then_call), m("id", vec![Type::I32], Type::I32));
    assert_eq!(resolved_call_name(else_call), m("id", vec![Type::I32], Type::I32));
}

#[test]
fn if_selects_overload_by_expected_type() {
    // The i8 expected type flows through the `if` into the branches, picking the
    // i8 overload of `id` in both.
    let module = check_ok(
        "
        fn main() -> i8 = if true { id(1) } else { id(2) };
        fn id(a: i8) -> i8 = a;
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I8);
    let (_, then, otherwise) = if_parts(&main.body);
    let then_call = scope_parts(then).1.unwrap();
    let else_call = scope_parts(otherwise.unwrap()).1.unwrap();
    assert_eq!(resolved_call_name(then_call), m("id", vec![Type::I8], Type::I8));
    assert_eq!(resolved_call_name(else_call), m("id", vec![Type::I8], Type::I8));
}

#[test]
fn nested_if_resolves() {
    let module = check_ok("fn main() -> i32 = if true { 1 } else if false { 2 } else { 3 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    // The `else` branch is itself an `if`, also typed i32.
    let (_, _, otherwise) = if_parts(&main.body);
    let inner = otherwise.unwrap();
    assert_eq!(inner.ty, Type::I32);
    let (_, then, else2) = if_parts(inner);
    assert_eq!(then.ty, Type::I32);
    assert_eq!(else2.unwrap().ty, Type::I32);
}

#[test]
fn if_condition_must_be_bool() {
    // `1` is an integer, not a bool.
    let errs = check_err("fn main() -> i32 = if 1 { 1 } else { 2 };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn if_mismatched_branches_is_an_error() {
    // One branch is an integer, the other a bool.
    let errs = check_err("fn main() -> i32 = if true { 1 } else { false };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn if_branch_conflicting_with_return_type_is_an_error() {
    // Both branches agree (bool), but the declared return type is i32.
    let errs = check_err("fn main() -> i32 = if true { true } else { false };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn if_without_else_for_non_void_return_is_an_error() {
    // An else-less `if` is void, which cannot satisfy an i32 return type.
    let errs = check_err("fn main() -> i32 = if true { 1 };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

// --------------------------------------------------------------------------
// User-defined operator overloading
//
// `op <symbol>(params...) -> ret = body;` registers a new overload under the
// operator's own name, alongside the built-in overloads. Because it shares the
// operator's overload set, it participates in ordinary overload resolution:
// it is chosen when its signature is the unique match, and it collides with a
// built-in only if it mangles to an identical signature.
// --------------------------------------------------------------------------

#[test]
fn user_operator_overload_is_selected() {
    // There is no built-in `+` for bools, so this user overload is the only
    // candidate for `true + false`.
    let module = check_ok(
        "
        op +(a: bool, b: bool) -> bool = a;
        fn main() -> bool = true + false;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::Bool, Type::Bool], Type::Bool)
    );
    assert_eq!(main.body.ty, Type::Bool);
    let args = call_args(&main.body);
    assert_eq!(args[0].ty, Type::Bool);
    assert_eq!(args[1].ty, Type::Bool);
    // The overload itself is emitted as a resolved function.
    func_sig(&module, "+", vec![Type::Bool, Type::Bool], Type::Bool);
}

#[test]
fn user_operator_overload_coexists_with_builtins() {
    // Adding a bool overload for `+` leaves the built-in integer overloads
    // untouched: `1 + 2` still resolves to the i32 built-in.
    let module = check_ok(
        "
        op +(a: bool, b: bool) -> bool = a;
        fn main() -> i32 = 1 + 2;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::I32, Type::I32], Type::I32)
    );
}

#[test]
fn unary_user_operator_overload_resolves() {
    // A one-parameter `op` is a unary operator overload; there is no built-in
    // unary `-` for bool, so this is the sole candidate.
    let module = check_ok(
        "
        op -(a: bool) -> bool = a;
        fn main() -> bool = -true;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("-", vec![Type::Bool], Type::Bool)
    );
    assert_eq!(call_args(&main.body).len(), 1);
}

#[test]
fn user_operator_overload_without_return_type_is_void() {
    // Omitting `-> ret` makes the overload return `void`. It is picked over the
    // built-in `&&` (which returns bool) because the call site wants `void`.
    let module = check_ok(
        "
        op &&(a: bool, b: bool) = nop();
        fn nop() = {};
        fn main() = true && false;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(
        resolved_call_name(&main.body),
        m("&&", vec![Type::Bool, Type::Bool], Type::Void)
    );
    assert_eq!(main.body.ty, Type::Void);
}

#[test]
fn user_operator_overload_selected_by_return_type() {
    // Two `+` bool overloads differing only in return type; the call site's
    // expected type disambiguates.
    let module = check_ok(
        "
        op +(a: bool, b: bool) -> bool = a;
        op +(a: bool, b: bool) -> i32 = 0;
        fn main() -> i32 = true + false;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::Bool, Type::Bool], Type::I32)
    );
}

#[test]
fn user_operator_overload_body_type_mismatch_is_an_error() {
    // The body is a bool but the declared operator return type is i32.
    let errs = check_err(
        "
        op +(a: bool, b: bool) -> i32 = a;
        fn main() -> i32 = 0;
        ",
    );
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn user_operator_overload_duplicating_a_builtin_is_ambiguous() {
    // This mangles identically to the built-in `+__i32_i32__i32`, producing two
    // functions with the same signature.
    let errs = check_err(
        "
        op +(a: i32, b: i32) -> i32 = a;
        fn main() -> i32 = 0;
        ",
    );
    assert_err!(errs, FloErr::AmbiguousOverload { .. });
}

// --------------------------------------------------------------------------
// Name mangling unit test
//
// This is the one place that deliberately pins the exact mangled-name format,
// so it is the single test that must be updated if the scheme changes.
// --------------------------------------------------------------------------

#[test]
fn mangle_name_format() {
    assert_eq!(mangle_name(&fn_ty(vec![], Type::I32), "main"), "main____i32");
    assert_eq!(
        mangle_name(&fn_ty(vec![Type::I32], Type::I32), "id"),
        "id__i32__i32"
    );
    assert_eq!(
        mangle_name(&fn_ty(vec![Type::I32, Type::U8], Type::I32), "foo"),
        "foo__i32_u8__i32"
    );
    assert_eq!(mangle_name(&fn_ty(vec![], Type::Void), "a"), "a____void");
}

// --------------------------------------------------------------------------
// Return expressions & the NoReturn (bottom) type
//
// `return e` is an expression of type `NoReturn`. Its operand is constrained to
// the enclosing function's return type. `NoReturn` satisfies any other type, and
// it is absorbed by joins: a diverging `if` branch or scope statement never
// forces its non-diverging neighbours to `NoReturn`.
// --------------------------------------------------------------------------

#[test]
fn return_as_body_is_noreturn() {
    let module = check_ok("fn main() -> i32 = return 5;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::Never);
    let val = return_value(&main.body).expect("return should carry a value");
    assert_eq!(val.ty, Type::I32);
    assert!(matches!(val.kind, ExprKind::Num(5)));
}

#[test]
fn return_operand_takes_function_return_type() {
    // The declared i8 return type flows into the return's operand literal.
    let module = check_ok("fn main() -> i8 = return 5;");
    let main = func_sig(&module, "main", vec![], Type::I8);
    assert_eq!(return_value(&main.body).unwrap().ty, Type::I8);
}

#[test]
fn return_operand_type_mismatch_is_an_error() {
    // `return true` in an i32 function: the operand cannot match the return type.
    let errs = check_err("fn main() -> i32 = return true;");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn bare_return_in_void_function_is_ok() {
    let module = check_ok("fn main() = return;");
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(main.body.ty, Type::Never);
    assert!(return_value(&main.body).is_none());
}

#[test]
fn bare_return_in_non_void_function_is_an_error() {
    // A value-returning function cannot `return` without a value.
    let errs = check_err("fn main() -> i32 = return;");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn return_operand_call_is_resolved() {
    // Calls nested inside a return operand must still be resolved by the
    // call-resolution passes.
    let module = check_ok(
        "
        fn main() -> i32 = return id(0);
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let val = return_value(&main.body).unwrap();
    assert_eq!(resolved_call_name(val), m("id", vec![Type::I32], Type::I32));
}

#[test]
fn return_in_operand_position_resolves() {
    // `return` can appear anywhere an expression can. As an operand of `+` it is
    // `NoReturn`, which satisfies the parameter; the call still resolves.
    let module = check_ok("fn main() -> i32 = 1 + return 0;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("+", vec![Type::I32, Type::I32], Type::I32)
    );
    let args = call_args(&main.body);
    assert_eq!(args[0].ty, Type::I32);
    assert_eq!(args[1].ty, Type::Never);
}

#[test]
fn diverging_branch_does_not_poison_the_other() {
    // One branch diverges; the OTHER branch keeps its real type and drives the
    // `if`'s type. This is the core non-poisoning property.
    let module = check_ok(
        "
        fn main() -> i32 = cond(true);
        fn cond(b: bool) -> i32 = if b { return 0 } else { 1 };
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::I32);
    assert_eq!(cond.body.ty, Type::I32);
    let (_, then, otherwise) = if_parts(&cond.body);
    assert_eq!(then.ty, Type::Never);
    assert_eq!(otherwise.unwrap().ty, Type::I32);
}

#[test]
fn both_branches_diverging_makes_if_noreturn() {
    // When both branches diverge the whole `if` diverges; it still satisfies the
    // declared i32 return type.
    let module = check_ok(
        "
        fn main() -> i32 = cond(true);
        fn cond(b: bool) -> i32 = if b { return 0 } else { return 1 };
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::I32);
    let (_, then, otherwise) = if_parts(&cond.body);
    assert_eq!(then.ty, Type::Never);
    assert_eq!(otherwise.unwrap().ty, Type::Never);
}

#[test]
fn return_in_statement_position_marks_scope_noreturn() {
    // A `return` statement makes the whole scope NoReturn; the trailing `5` is
    // dead code but is still fully typed.
    let module = check_ok("fn main() -> i32 = { return 0; 5 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmts.len(), 1);
    assert_eq!(stmts[0].ty, Type::Never);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn scope_with_only_a_return_is_valid_for_any_return_type() {
    // `{ return 0; }` has no tail but diverges, so it satisfies i32 (it would be
    // `void` and fail without the divergence rule).
    let module = check_ok("fn main() -> i32 = { return 0; };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmts.len(), 1);
    assert_eq!(stmts[0].ty, Type::Never);
    assert!(tail.is_none());
}

#[test]
fn if_without_else_returning_does_not_diverge_the_scope() {
    // `if b { return 0 }` is void (control may skip it), so the scope falls
    // through to `5` and is i32 — a return in one arm does not poison the scope.
    let module = check_ok(
        "
        fn main() -> i32 = cond(true);
        fn cond(b: bool) -> i32 = { if b { return 0 }; 5 };
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::I32);
    assert_eq!(cond.body.ty, Type::I32);
    let (stmts, tail) = scope_parts(&cond.body);
    assert_eq!(stmts.len(), 1);
    assert_eq!(stmts[0].ty, Type::Void);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn calls_in_if_branches_resolve() {
    // Regression guard: the call-resolution passes must recurse into both `if`
    // branches (relevant now that a branch may hold a diverging expression).
    let module = check_ok(
        "
        fn main() -> i32 = cond(true);
        fn cond(b: bool) -> i32 = if b id(0) else id(1);
        fn id(a: i32) -> i32 = a;
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::I32);
    let (_, then, otherwise) = if_parts(&cond.body);
    assert_eq!(resolved_call_name(then), m("id", vec![Type::I32], Type::I32));
    assert_eq!(
        resolved_call_name(otherwise.unwrap()),
        m("id", vec![Type::I32], Type::I32)
    );
}
