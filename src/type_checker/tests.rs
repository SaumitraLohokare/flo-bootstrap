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
