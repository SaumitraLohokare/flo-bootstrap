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
use crate::ast::{Expr, ExprKind, FieldInit, Func, Module, Op, Statement, StmtKind, TypeQuery};
use crate::errors::FloErr;
use crate::parser::{Parser, check_entry_point};
use crate::tokenizer::{TokenKind, Tokenizer};
use crate::types::{DeclKind, FieldName, Record, RecordDecl, SumCase, Type};

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

/// Expect the source to fail during *parsing* and return the error. Used for the
/// checks the parser makes on its own, before any types exist.
fn parse_err(src: &str) -> FloErr {
    let tokens = Tokenizer::new(src).tokenize();
    match Parser::new(tokens).parse() {
        Ok(module) => panic!("expected parsing to fail, but it succeeded:\n{module:?}"),
        Err(err) => err,
    }
}

/// Parse `src` and run the whole-program entry-point check on the result.
fn entry_check(src: &str) -> Result<(), FloErr> {
    let tokens = Tokenizer::new(src).tokenize();
    let module = Parser::new(tokens)
        .parse()
        .expect("test source should parse without errors");
    check_entry_point(&module)
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

/// The operands of a `&&` / `||`. Those are the only operators that are not
/// calls, so `call_args` does not reach them (see `ExprKind::Logical`).
fn logical_operands(expr: &Expr) -> (&Expr, &Expr) {
    match &expr.kind {
        ExprKind::Logical(_, lhs, rhs) => (lhs, rhs),
        other => panic!("expected a `&&` / `||` expression, got {other:?}"),
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
    ($errs:expr, $pat:pat if $guard:expr) => {{
        let errs = &$errs;
        assert!(
            errs.iter().any(|e| matches!(e, $pat if $guard)),
            "expected an error matching `{}`, got: {errs:?}",
            stringify!($pat if $guard),
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
    // Not calls, so there is no overload to resolve: the node is bool and so are
    // both of its operands, always.
    for op in ["&&", "||"] {
        let src = format!("fn main() -> bool = true {op} false;");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::Bool);
        assert_eq!(main.body.ty, Type::Bool);
        let (lhs, rhs) = logical_operands(&main.body);
        assert_eq!(lhs.ty, Type::Bool);
        assert_eq!(rhs.ty, Type::Bool);
    }
}

#[test]
fn logical_operators_take_the_result_of_a_call() {
    // An operand does not have to be a literal; it just has to end up bool, and
    // that requirement resolves the overload the same way an `if` condition does.
    let module = check_ok(
        "
        fn main() -> bool = zero() && zero();
        fn zero() -> bool = true;
        fn zero() -> i32 = 0;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    let (lhs, rhs) = logical_operands(&main.body);
    let expected = m("zero", vec![], Type::Bool);
    assert_eq!(resolved_call_name(lhs), expected);
    assert_eq!(resolved_call_name(rhs), expected);
}

#[test]
fn a_diverging_left_operand_diverges_the_logical_operator() {
    // The left operand always runs, so nothing after this statement is reachable
    // and the tail-less scope satisfies i32 the way a bare `return` would. The
    // operator itself is still bool: NoReturn is absorbed, not propagated.
    let module = check_ok("fn main() -> i32 = { (return 1) && true; };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert!(tail.is_none());
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Bool);
}

#[test]
fn a_diverging_right_operand_does_not_diverge_the_logical_operator() {
    // The mirror of the test above: the right operand only runs when the left
    // does not already decide the answer, so it cannot make the scope diverge --
    // which leaves this one void where i32 was expected.
    let errs = check_err("fn main() -> i32 = { true && return 1; };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn logical_operators_are_not_overloadable() {
    // They short-circuit, so they cannot be functions -- a call evaluates every
    // argument before it runs. Rejected at the declaration, not the call site.
    for op in ["&&", "||"] {
        let src = format!("op {op}(a: bool, b: bool) -> bool = a; fn main() = {{}};");
        let err = parse_err(&src);
        assert!(
            matches!(err, FloErr::OpNotOverloadable { .. }),
            "expected an `OpNotOverloadable` error for `{op}`, got: {err:?}"
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

#[test]
fn shift_operators_resolve_on_integers() {
    for op in ["<<", ">>"] {
        let src = format!("fn main() -> i32 = 6 {op} 2;");
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
fn a_shift_amount_may_be_a_different_width() {
    // The shifts are the only builtin operators whose operands need not agree:
    // the amount says how far to shift, not what the result is.
    let module = check_ok(
        "
        fn shift(v: i32, by: u8) -> i32 = v << by;
        fn main() = {};
        ",
    );
    let shift = func_sig(&module, "shift", vec![Type::I32, Type::U8], Type::I32);
    assert_eq!(
        resolved_call_name(&shift.body),
        m("<<", vec![Type::I32, Type::U8], Type::I32)
    );
}

#[test]
fn a_shift_yields_the_shifted_operands_type() {
    let module = check_ok(
        "
        fn shift(v: u8, by: i64) -> u8 = v >> by;
        fn main() = {};
        ",
    );
    let shift = func_sig(&module, "shift", vec![Type::U8, Type::I64], Type::U8);
    assert_eq!(
        resolved_call_name(&shift.body),
        m(">>", vec![Type::U8, Type::I64], Type::U8)
    );
}

#[test]
fn an_unannotated_shift_defaults_both_operands() {
    let module = check_ok("fn main() = { let x = 1 << 2; };");
    let main = main_func(&module);
    let (stmts, _) = scope_parts(&main.body);
    let (_, ty, init) = let_parts(&stmts[0]);
    assert_eq!(*ty, Type::I32);
    assert_eq!(
        resolved_call_name(init.unwrap()),
        m("<<", vec![Type::I32, Type::I32], Type::I32)
    );
}

#[test]
fn an_annotation_narrows_only_the_shifted_operand() {
    // The annotation reaches the left operand through the return type; the
    // amount is free to stay whatever it defaults to.
    let module = check_ok("fn main() = { let x: u8 = 1 << 2; };");
    let main = main_func(&module);
    let (stmts, _) = scope_parts(&main.body);
    let (_, ty, init) = let_parts(&stmts[0]);
    assert_eq!(*ty, Type::U8);
    assert_eq!(
        resolved_call_name(init.unwrap()),
        m("<<", vec![Type::U8, Type::I32], Type::U8)
    );
}

#[test]
fn shift_operators_have_no_bool_overload() {
    // Unlike `&`, `|`, `^` and `~`, there is nothing to shift in a bool.
    for op in ["<<", ">>"] {
        let src = format!("fn main() -> bool = true {op} true;");
        let errs = check_err(&src);
        assert_err!(errs, FloErr::NoPossibleOverloads { .. });
    }
}

#[test]
fn nested_generic_arguments_close_with_two_angle_brackets() {
    // `>>` is not a token, which is exactly what lets this parse: the two `>`
    // close one argument list each. A shift is joined from two of them in the
    // parser instead (see `Parser::peek_shift`).
    let module = check_ok(
        "
        type View<T> = { data: T };
        fn inner(v: View<View<i32>>) -> i32 = v.data.data;
        fn main() = {};
        ",
    );
    let view_i32 = user("View", vec![Type::I32]);
    let inner = func_sig(
        &module,
        "inner",
        vec![user("View", vec![view_i32.clone()])],
        Type::I32,
    );
    assert_eq!(
        inner.ty,
        fn_ty(vec![user("View", vec![view_i32])], Type::I32)
    );
}

#[test]
fn a_gap_between_the_angle_brackets_is_not_a_shift() {
    // Adjacency in the source is what makes a shift, so this stays a comparison
    // against a stray `>`.
    let err = parse_err("fn main() -> i32 = 8 > > 2;");
    assert!(
        matches!(err, FloErr::UnexpectedToken { .. }),
        "expected an unexpected-token error, got: {err:?}"
    );
}

#[test]
fn bitwise_not_resolves_on_integers() {
    let module = check_ok("fn main() -> u8 = ~6;");
    let main = func_sig(&module, "main", vec![], Type::U8);
    assert_eq!(
        resolved_call_name(&main.body),
        m("~", vec![Type::U8], Type::U8)
    );
    assert_eq!(call_args(&main.body).len(), 1);
}

#[test]
fn bitwise_not_resolves_on_bool() {
    // `~` has a bool overload, like the other bitwise operators.
    let module = check_ok("fn main() -> bool = ~true;");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("~", vec![Type::Bool], Type::Bool)
    );
}

#[test]
fn logical_not_resolves_on_bool() {
    let module = check_ok("fn main() -> bool = !true;");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    assert_eq!(
        resolved_call_name(&main.body),
        m("!", vec![Type::Bool], Type::Bool)
    );
}

#[test]
fn logical_not_on_an_integer_is_an_error() {
    // There is no truthiness in the language: `!` negates a bool, and that is
    // the only overload it has.
    let errs = check_err("fn main() -> bool = !1;");
    assert_err!(errs, FloErr::NoPossibleOverloads { .. });
}

#[test]
fn double_not_nests() {
    // `!!true` == `!(!true)`, two unary calls.
    let module = check_ok("fn main() -> bool = !!true;");
    let main = func_sig(&module, "main", vec![], Type::Bool);
    let not_bool = m("!", vec![Type::Bool], Type::Bool);
    assert_eq!(resolved_call_name(&main.body), not_bool);
    assert_eq!(resolved_call_name(&call_args(&main.body)[0]), not_bool);
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
    // `&&` is not a call, so this is a plain mismatch against bool rather than
    // an exhausted overload set.
    let errs = check_err("fn main() -> bool = 1 && 2;");
    assert_err!(
        errs,
        FloErr::TypeMismatch {
            expected: Type::Bool,
            got: Type::Integer,
            ..
        }
    );
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

/// Names the operator at the root of `expr`, whichever shape it has: the
/// resolved (mangled) callee for the operators that are calls, and the bare
/// symbol for `&&` / `||`, which are not.
fn op_node_name(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Logical(Op::And, ..) => "&&".to_string(),
        ExprKind::Logical(Op::Or, ..) => "||".to_string(),
        _ => resolved_call_name(expr).to_string(),
    }
}

/// Operand `idx` of an operator node, call or not.
fn op_node_arg(expr: &Expr, idx: usize) -> &Expr {
    match &expr.kind {
        ExprKind::Logical(..) => {
            let (lhs, rhs) = logical_operands(expr);
            if idx == 0 { lhs } else { rhs }
        }
        _ => &call_args(expr)[idx],
    }
}

/// Root operator name and the operator name nested at argument `idx`.
fn op_and_nested(module: &Module, arg_idx: usize) -> (String, String) {
    let body = &main_func(module).body;
    let root = op_node_name(body);
    let nested = op_node_name(op_node_arg(body, arg_idx));
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
fn add_binds_tighter_than_a_shift() {
    // 1 << 2 + 3  ==  1 << (2 + 3)
    let module = check_ok("fn main() -> i32 = 1 << 2 + 3;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("<<", Type::I32, Type::I32));
    assert_eq!(nested, op("+", Type::I32, Type::I32));
}

#[test]
fn a_shift_binds_tighter_than_comparison() {
    // 1 << 2 < 3  ==  (1 << 2) < 3
    let module = check_ok("fn main() -> bool = 1 << 2 < 3;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("<", Type::I32, Type::Bool));
    assert_eq!(nested, op("<<", Type::I32, Type::I32));
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
    assert_eq!(root, "&&");
    assert_eq!(nested, op("|", Type::Bool, Type::Bool));
}

#[test]
fn logical_and_binds_tighter_than_logical_or() {
    // true || false && true  ==  true || (false && true)
    let module = check_ok("fn main() -> bool = true || false && true;");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, "||");
    assert_eq!(nested, "&&");
}

#[test]
fn full_precedence_ladder_nests_deepest_operator_last() {
    // A chain touching every precedence level, associating rightward:
    //   a || b && c | d ^ e & f == g < h << i + j * k
    // Each operator binds tighter than the one to its left, so the tree is a
    // right-leaning spine ending in the `*` (tightest) node. The leaves are
    // chosen so every level type-checks: bool down to the `==`, then the `<`
    // compares integers (yielding the bool that `==` consumes).
    let module = check_ok(
        "fn main() -> bool =
            true || true && true | true ^ true & true == 2 < 3 << 4 + 5 * 6;",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    let expected = [
        "||".to_string(),
        "&&".to_string(),
        op("|", Type::Bool, Type::Bool),
        op("^", Type::Bool, Type::Bool),
        op("&", Type::Bool, Type::Bool),
        op("==", Type::Bool, Type::Bool),
        op("<", Type::I32, Type::Bool),
        op("<<", Type::I32, Type::I32),
        op("+", Type::I32, Type::I32),
        op("*", Type::I32, Type::I32),
    ];
    let mut node = &main.body;
    for expected_name in expected {
        assert_eq!(op_node_name(node), expected_name);
        // Every level nests its tighter-binding neighbour in the right arg.
        node = op_node_arg(node, 1);
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
// Parenthesized grouping
//
// `( expr )` is a primary expression that exists only to override precedence.
// It produces no AST node of its own: the inner expression is returned as-is,
// with its source span widened to include the parentheses.
// --------------------------------------------------------------------------

#[test]
fn parens_override_precedence_on_the_left() {
    // `(1 + 2) * 3`: without the parens `*` would bind tighter and nest under
    // the `+` instead.
    let module = check_ok("fn main() -> i32 = (1 + 2) * 3;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("*", Type::I32, Type::I32));
    assert_eq!(nested, op("+", Type::I32, Type::I32));
}

#[test]
fn parens_override_precedence_on_the_right() {
    // `2 * (3 + 4)`: the `+` nests under the right operand of `*`, which is
    // where precedence alone would never put it.
    let module = check_ok("fn main() -> i32 = 2 * (3 + 4);");
    let (root, nested) = op_and_nested(&module, 1);
    assert_eq!(root, op("*", Type::I32, Type::I32));
    assert_eq!(nested, op("+", Type::I32, Type::I32));
}

#[test]
fn parens_group_against_left_associativity() {
    // `16 / (4 / 2)` == 8, the parse `/` would never produce on its own.
    let module = check_ok("fn main() -> i32 = 16 / (4 / 2);");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let args = call_args(&main.body);
    assert!(matches!(args[0].kind, ExprKind::Num(16)));
    assert_eq!(resolved_call_name(&args[1]), op("/", Type::I32, Type::I32));
}

#[test]
fn parens_wrap_no_node_of_their_own() {
    // Redundant parens are transparent: the body is the literal itself, not a
    // wrapper around it.
    let module = check_ok("fn main() -> i32 = ((5));");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert!(matches!(main.body.kind, ExprKind::Num(5)));
    assert_eq!(main.body.ty, Type::I32);
}

#[test]
fn parenthesized_scope_and_if_still_work() {
    // Composite expressions can be parenthesized too.
    let module = check_ok("fn main() -> i32 = (if true { 1 } else { 2 }) + ({ 3 });");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let args = call_args(&main.body);
    assert_eq!(args[0].ty, Type::I32);
    assert!(matches!(args[0].kind, ExprKind::If(..)));
    assert!(matches!(args[1].kind, ExprKind::Scope(..)));
}

#[test]
fn empty_parens_are_an_error() {
    let err = parse_err("fn main() -> i32 = ();");
    assert!(
        matches!(err, FloErr::UnexpectedToken { .. }),
        "expected an unexpected-token error, got: {err:?}"
    );
}

#[test]
fn unclosed_parens_are_an_error() {
    let err = parse_err("fn main() -> i32 = (1 + 2;");
    assert!(
        matches!(err, FloErr::ExpectedTokenNotFound { .. }),
        "expected a missing-token error, got: {err:?}"
    );
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
    assert_eq!(
        resolved_call_name(inner),
        m("-", vec![Type::I32], Type::I32)
    );
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
fn not_binds_tighter_than_equality() {
    // `!true == false` == `(!true) == false`, like every other unary operator.
    let module = check_ok("fn main() -> bool = !true == false;");
    let (root, nested) = op_and_nested(&module, 0);
    assert_eq!(root, op("==", Type::Bool, Type::Bool));
    assert_eq!(nested, m("!", vec![Type::Bool], Type::Bool));
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
    assert_eq!(
        resolved_call_name(outer),
        m("inc", vec![Type::I32], Type::I32)
    );
    let inner = &call_args(outer)[0];
    assert_eq!(
        resolved_call_name(inner),
        m("inc", vec![Type::I32], Type::I32)
    );
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

/// The scope's statements and its optional tail expression.
fn scope_parts(expr: &Expr) -> (&[Statement], Option<&Expr>) {
    match &expr.kind {
        ExprKind::Scope(stmts, tail) => (stmts, tail.as_deref()),
        other => panic!("expected a scope expression, got {other:?}"),
    }
}

/// The expression of a statement that is one, for the many tests that only care
/// about expressions in statement position.
fn stmt_expr(stmt: &Statement) -> &Expr {
    match &stmt.kind {
        StmtKind::Expr(e) => e,
        other => panic!("expected an expression statement, got {other:?}"),
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
fn block_statements_need_their_semicolon_too() {
    // There is no implicit `;`: a scope, an `if` and a `while` in statement
    // position each end in one, like everything else.
    let module = check_ok(
        "
        fn main() = {
            { nop() };
            if true { nop() };
            while false { nop() };
            nop()
        };
        fn nop() = {};
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmts.len(), 3);
    assert!(matches!(stmt_expr(&stmts[0]).kind, ExprKind::Scope(..)));
    assert!(matches!(stmt_expr(&stmts[1]).kind, ExprKind::If(..)));
    assert!(matches!(stmt_expr(&stmts[2]).kind, ExprKind::While(..)));
    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("nop", vec![], Type::Void)
    );
}

#[test]
fn a_block_without_its_semicolon_is_an_error() {
    // The separator is what keeps `}` followed by `.` from reading as field
    // access on the block, so it is not optional even where it looks redundant.
    let err = parse_err(
        "
        fn main() = {
            if true { nop() }
            nop()
        };
        fn nop() = {};
        ",
    );
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::Semicolon,
                ..
            }
        ),
        "expected a missing-`;` error, got: {err:?}"
    );
}

#[test]
fn a_trailing_block_is_the_scope_tail() {
    // Written last it has no `;`, and that is exactly what makes it the tail.
    let module = check_ok("fn main() -> i32 = { if true { 1 } else { 2 } };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert!(stmts.is_empty());
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn a_statement_without_its_semicolon_is_an_error() {
    let err = parse_err("fn main() = { nop() nop() };");
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::Semicolon,
                ..
            }
        ),
        "expected a missing-`;` error, got: {err:?}"
    );
}

#[test]
fn a_block_is_not_the_receiver_of_a_field_access() {
    // An expression may begin with `.`, so a block never takes one: the `.` here
    // starts the next statement, and the missing `;` is what gets reported.
    let err = parse_err(
        "
        fn main() = {
            if true { }
            .{ x: 1 };
        };
        ",
    );
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::Semicolon,
                ..
            }
        ),
        "expected a missing-`;` error, got: {err:?}"
    );
}

#[test]
fn a_parenthesized_block_is_a_receiver() {
    // Parenthesizing is how a block becomes one.
    let module = check_ok("fn main() -> i32 = ({ .{ x: 1 } }).x;");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(field_parts(&main.body).1, "x");
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
            true & false
        };
        fn nop() = {};
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Bool);
    let (stmts, tail) = scope_parts(&main.body);

    assert_eq!(stmts.len(), 1);
    assert_eq!(resolved_call_name(stmt_expr(&stmts[0])), m("nop", vec![], Type::Void));
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Void);

    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("&", vec![Type::Bool, Type::Bool], Type::Bool)
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
    for call in stmts
        .iter()
        .map(stmt_expr)
        .chain(std::iter::once(tail.unwrap()))
    {
        assert_eq!(
            resolved_call_name(call),
            m("id", vec![Type::I32], Type::I32)
        );
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
    assert_eq!(
        resolved_call_name(then_call),
        m("id", vec![Type::I32], Type::I32)
    );
    assert_eq!(
        resolved_call_name(else_call),
        m("id", vec![Type::I32], Type::I32)
    );
}

#[test]
fn if_branch_must_be_a_scope() {
    let err = parse_err("fn main() -> i32 = if true 1 else 2;");
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::LCurly,
                ..
            }
        ),
        "expected a missing-`{{` error, got: {err:?}"
    );
}

#[test]
fn else_branch_must_be_a_scope_or_an_if() {
    let err = parse_err("fn main() -> i32 = if true { 1 } else 2;");
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::LCurly,
                ..
            }
        ),
        "expected a missing-`{{` error, got: {err:?}"
    );
}

#[test]
fn else_if_chains_parse() {
    // `else if` is the one non-scope else: another `if`, nested as the else.
    let module = check_ok("fn main() -> i32 = if true { 1 } else if false { 2 } else { 3 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (_, _, otherwise) = if_parts(&main.body);
    let inner = otherwise.expect("expected an else branch");
    assert!(matches!(inner.kind, ExprKind::If(..)));
    assert_eq!(inner.ty, Type::I32);
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
    assert_eq!(
        resolved_call_name(then_call),
        m("id", vec![Type::I8], Type::I8)
    );
    assert_eq!(
        resolved_call_name(else_call),
        m("id", vec![Type::I8], Type::I8)
    );
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
    // built-in `&` (which returns bool) because the call site wants `void`.
    let module = check_ok(
        "
        op &(a: bool, b: bool) = nop();
        fn nop() = {};
        fn main() = true & false;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(
        resolved_call_name(&main.body),
        m("&", vec![Type::Bool, Type::Bool], Type::Void)
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
fn user_operator_overload_duplicating_a_builtin_is_not_an_error_on_its_own() {
    // This mangles identically to the built-in `+__i32_i32__i32`, giving two
    // functions with the same signature. Declaring them is fine; it is only
    // *calling* one that has no answer.
    check_ok(
        "
        op +(a: i32, b: i32) -> i32 = a;
        fn main() -> i32 = 0;
        ",
    );
}

#[test]
fn calling_a_duplicated_overload_is_ambiguous() {
    let errs = check_err(
        "
        op +(a: i32, b: i32) -> i32 = a;
        fn main() -> i32 = 1 + 2;
        ",
    );
    assert_err!(errs, FloErr::MultiplePossibleOverloads { .. });
}

// --------------------------------------------------------------------------
// Name mangling unit test
//
// This is the one place that deliberately pins the exact mangled-name format,
// so it is the single test that must be updated if the scheme changes.
// --------------------------------------------------------------------------

#[test]
fn mangle_name_format() {
    assert_eq!(
        mangle_name(&fn_ty(vec![], Type::I32), "main"),
        "main____i32"
    );
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
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Never);
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
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Never);
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
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Void);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn calls_in_if_branches_resolve() {
    // Regression guard: the call-resolution passes must recurse into both `if`
    // branches (relevant now that a branch may hold a diverging expression).
    let module = check_ok(
        "
        fn main() -> i32 = cond(true);
        fn cond(b: bool) -> i32 = if b { id(0) } else { id(1) };
        fn id(a: i32) -> i32 = a;
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::I32);
    let (_, then, otherwise) = if_parts(&cond.body);
    let then_tail = scope_parts(then).1.expect("then branch should have a tail");
    let else_tail = scope_parts(otherwise.unwrap())
        .1
        .expect("else branch should have a tail");
    assert_eq!(
        resolved_call_name(then_tail),
        m("id", vec![Type::I32], Type::I32)
    );
    assert_eq!(
        resolved_call_name(else_tail),
        m("id", vec![Type::I32], Type::I32)
    );
}

/// The variable id, variable type and optional initializer of a declaration.
fn let_parts(stmt: &Statement) -> (usize, &Type, Option<&Expr>) {
    match &stmt.kind {
        StmtKind::Let(id, ty, init) => (*id, ty, init.as_ref()),
        other => panic!("expected a let statement, got {other:?}"),
    }
}

/// The target and value of an assignment.
fn assign_parts(expr: &Expr) -> (&Expr, &Expr) {
    match &expr.kind {
        ExprKind::Assign(target, value) => (target, value),
        other => panic!("expected an assign expression, got {other:?}"),
    }
}

/// The variable id a `Var` expression reads.
fn var_id(expr: &Expr) -> usize {
    match &expr.kind {
        ExprKind::Var(id) => *id,
        other => panic!("expected a variable, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Variable declarations
//
// `let x [: T] [= init]` is an expression of type `void` that introduces `x`
// into the enclosing scope. The variable's own type is the annotation if it has
// one, otherwise a fresh type var shared with every `Var` that reads it — which
// is what lets inference flow both from an initializer and (for `let x;`) from a
// later assignment. Re-declaring a name shadows it: a new id, with the old one
// still readable from the initializer.
// --------------------------------------------------------------------------

#[test]
fn let_infers_its_type_from_the_initializer() {
    let module = check_ok(
        "
        fn main() -> i32 = f(2);
        fn f(n: i32) -> i32 = { let a = n; a };
        ",
    );
    let f = func_sig(&module, "f", vec![Type::I32], Type::I32);
    let (stmts, tail) = scope_parts(&f.body);
    let (id, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(var_ty, &Type::I32);
    assert_eq!(init.unwrap().ty, Type::I32);
    // ... and the tail reads that same variable.
    assert_eq!(var_id(tail.unwrap()), id);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn let_initializer_literal_defaults_to_i32() {
    let module = check_ok("fn main() -> i32 = { let a = 0; a };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(var_ty, &Type::I32);
    assert_eq!(init.unwrap().ty, Type::I32);
}

#[test]
fn let_annotation_narrows_the_initializer_literal() {
    // The annotation flows *into* the literal rather than the other way round,
    // so `1` never defaults to i32.
    let module = check_ok("fn main() -> i8 = { let a: i8 = 1; a };");
    let main = func_sig(&module, "main", vec![], Type::I8);
    let (stmts, tail) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(var_ty, &Type::I8);
    assert_eq!(init.unwrap().ty, Type::I8);
    assert_eq!(tail.unwrap().ty, Type::I8);
}

#[test]
fn let_annotation_selects_an_overload() {
    let module = check_ok(
        "
        fn main() -> u8 = { let a: u8 = id(1); a };
        fn id(a: i32) -> i32 = a;
        fn id(a: u8) -> u8 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::U8);
    let (stmts, _) = scope_parts(&main.body);
    let (_, _, init) = let_parts(&stmts[0]);
    assert_eq!(
        resolved_call_name(init.unwrap()),
        m("id", vec![Type::U8], Type::U8)
    );
}

#[test]
fn let_annotation_conflicting_with_initializer_is_an_error() {
    let errs = check_err("fn main() -> i8 = { let a: i8 = true; a };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn redeclaring_a_name_shadows_it() {
    // Each `let` gets its own id, and the second initializer still reads the
    // first binding — the new name is only in scope *after* the declaration.
    let module = check_ok("fn main() -> i32 = { let a = 1; let a = a + 1; a };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    let (first, _, _) = let_parts(&stmts[0]);
    let (second, _, second_init) = let_parts(&stmts[1]);
    assert_ne!(first, second);
    assert_eq!(var_id(&call_args(second_init.unwrap())[0]), first);
    // The tail reads the newer binding.
    assert_eq!(var_id(tail.unwrap()), second);
}

#[test]
fn shadowing_can_change_the_type() {
    let module = check_ok(
        "
        fn main() -> i32 = f(1);
        fn f(n: i32) -> i32 = { let a = true; let a = n; a };
        ",
    );
    let f = func_sig(&module, "f", vec![Type::I32], Type::I32);
    let (stmts, tail) = scope_parts(&f.body);
    assert_eq!(let_parts(&stmts[0]).1, &Type::Bool);
    assert_eq!(let_parts(&stmts[1]).1, &Type::I32);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn variable_does_not_escape_its_scope() {
    // Out of scope, so `a` is no longer a variable — and with no `use` bringing
    // in a case by that name, there is nothing else it could be.
    let err = parse_err("fn main() -> i32 = { { let a = 1; }; a };");
    assert!(
        matches!(err, FloErr::UnknownIdentifier { ref name, .. } if name == "a"),
        "expected an unknown-identifier error, got: {err:?}"
    );
}

#[test]
fn a_let_cannot_be_a_scope_tail() {
    // A declaration is a statement, so it always ends in a `;` — it can never be
    // the thing a scope yields.
    let err = parse_err("fn main() = { let a = 1 };");
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::Semicolon,
                ..
            }
        ),
        "expected a missing-`;` error, got: {err:?}"
    );
}

#[test]
fn let_as_a_function_body_is_an_error() {
    // A declaration is a statement, not an expression, so it needs a scope to
    // live in.
    let err = parse_err("fn main() = let a = 1;");
    assert!(
        matches!(err, FloErr::LetOutsideStatementPosition { .. }),
        "expected a let-outside-statement-position error, got: {err:?}"
    );
}

#[test]
fn let_in_operand_position_is_an_error() {
    let err = parse_err("fn main() -> i32 = { 1 + (let a = 2) };");
    assert!(
        matches!(err, FloErr::LetOutsideStatementPosition { .. }),
        "expected a let-outside-statement-position error, got: {err:?}"
    );
}

#[test]
fn let_initialized_by_a_diverging_expression_is_accepted() {
    // `let a = return 0;` never binds `a` to anything, so its type is pinned to
    // NoReturn instead of being left unresolvable. The `return` also makes the
    // whole scope diverge, which is what satisfies the i32 return type.
    let module = check_ok("fn main() -> i32 = { let a = return 0; };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, _) = scope_parts(&main.body);
    assert_eq!(let_parts(&stmts[0]).1, &Type::Never);
}

// --------------------------------------------------------------------------
// Declarations without an initializer
// --------------------------------------------------------------------------

#[test]
fn let_without_initializer_infers_from_a_later_assignment() {
    let module = check_ok("fn main() -> i32 = { let a; a = 1; a };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    let (id, var_ty, init) = let_parts(&stmts[0]);
    assert!(init.is_none());
    assert_eq!(var_ty, &Type::I32);
    assert_eq!(var_id(tail.unwrap()), id);
}

#[test]
fn let_without_initializer_takes_its_annotation() {
    let module = check_ok("fn main() -> i8 = { let a: i8; a = 1; a };");
    let main = func_sig(&module, "main", vec![], Type::I8);
    let (stmts, _) = scope_parts(&main.body);
    assert_eq!(let_parts(&stmts[0]).1, &Type::I8);
    // The assigned literal is narrowed to the annotated type.
    assert_eq!(assign_parts(stmt_expr(&stmts[1])).1.ty, Type::I8);
}

#[test]
fn never_assigned_let_cannot_be_inferred() {
    // KNOWN LIMITATION: this reports "unresolved type" rather than something like
    // "unused variable". It surfaces at the declaration, which is the right place.
    let errs = check_err("fn main() = { let a; };");
    assert_err!(errs, FloErr::UnresolvedType { .. });
}

// --------------------------------------------------------------------------
// Assignment
//
// `target = value` stores into an l-value and, like C, yields the value it
// stored. It is the lowest-precedence operator and the only right-associative
// one. Only the parser decides what may be assigned to (`Expr::is_lvalue`), so
// those errors come out before type checking.
// --------------------------------------------------------------------------

#[test]
fn assign_yields_the_value_it_stored() {
    // The assignment is the scope's tail, so the scope — and the function — take
    // its type. If it were void this would not type check.
    let module = check_ok("fn main() -> i32 = { let a = 0; a = 1 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    let (_, tail) = scope_parts(&main.body);
    let assign = tail.unwrap();
    assert_eq!(assign.ty, Type::I32);
    let (target, value) = assign_parts(assign);
    assert_eq!(target.ty, Type::I32);
    assert_eq!(value.ty, Type::I32);
}

#[test]
fn assigned_value_takes_the_variables_type() {
    // The i8 variable narrows the assigned literal, which would otherwise
    // default to i32.
    let module = check_ok("fn main() -> i8 = { let a: i8 = 0; a = 1 };");
    let main = func_sig(&module, "main", vec![], Type::I8);
    let (_, tail) = scope_parts(&main.body);
    let (target, value) = assign_parts(tail.unwrap());
    assert_eq!(target.ty, Type::I8);
    assert_eq!(value.ty, Type::I8);
    assert_eq!(tail.unwrap().ty, Type::I8);
}

#[test]
fn assign_binds_looser_than_every_other_operator() {
    // `a = 1 + 2` is `a = (1 + 2)`, not `(a = 1) + 2`.
    let module = check_ok("fn main() -> i32 = { let a = 0; a = 1 + 2 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (_, tail) = scope_parts(&main.body);
    let (_, value) = assign_parts(tail.unwrap());
    assert_eq!(
        resolved_call_name(value),
        m("+", vec![Type::I32, Type::I32], Type::I32)
    );
}

#[test]
fn assign_is_right_associative() {
    // `a = b = 1` is `a = (b = 1)`: the inner assignment is the outer one's
    // value, which only works because assignment yields what it stored.
    let module = check_ok("fn main() -> i32 = { let a = 0; let b = 0; a = b = 1 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    let a = let_parts(&stmts[0]).0;
    let b = let_parts(&stmts[1]).0;

    let (outer_target, outer_value) = assign_parts(tail.unwrap());
    assert_eq!(var_id(outer_target), a);
    let (inner_target, inner_value) = assign_parts(outer_value);
    assert_eq!(var_id(inner_target), b);
    assert!(matches!(inner_value.kind, ExprKind::Num(1)));
}

#[test]
fn assign_in_operand_position_resolves() {
    // Assignment is an ordinary expression, so it can be passed as an argument —
    // the parameter type is then what the assigned value has to satisfy.
    let module = check_ok(
        "
        fn main() -> i8 = { let a: i8 = 0; id(a = 1) };
        fn id(x: i8) -> i8 = x;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I8);
    let (_, tail) = scope_parts(&main.body);
    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("id", vec![Type::I8], Type::I8)
    );
    let arg = &call_args(tail.unwrap())[0];
    assert_eq!(arg.ty, Type::I8);
    assert_eq!(assign_parts(arg).1.ty, Type::I8);
}

#[test]
fn parenthesized_assign_is_an_operand() {
    // `(a = 1) + 2` only type checks because the assignment yields an i32.
    let module = check_ok("fn main() -> i32 = { let a = 0; (a = 1) + 2 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (_, tail) = scope_parts(&main.body);
    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("+", vec![Type::I32, Type::I32], Type::I32)
    );
    let args = call_args(tail.unwrap());
    assert_eq!(args[0].ty, Type::I32);
    assert_eq!(assign_parts(&args[0]).1.ty, Type::I32);
}

#[test]
fn parenthesized_variable_is_still_assignable() {
    // Parens produce no node, so the assignment target is still a plain `Var`.
    let module = check_ok("fn main() -> i32 = { let a = 0; (a) = 1 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    let (target, _) = assign_parts(tail.unwrap());
    assert_eq!(var_id(target), let_parts(&stmts[0]).0);
}

#[test]
fn assign_resolves_calls_on_both_sides() {
    let module = check_ok(
        "
        fn main() -> i32 = { let a = 0; a = id(1) };
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (_, tail) = scope_parts(&main.body);
    let (_, value) = assign_parts(tail.unwrap());
    assert_eq!(
        resolved_call_name(value),
        m("id", vec![Type::I32], Type::I32)
    );
}

#[test]
fn assign_type_mismatch_is_an_error() {
    let errs = check_err("fn main() = { let a: i32 = 0; a = true; };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn assigning_a_diverging_value_is_accepted() {
    // `a = return 0` never stores anything, so the assignment is NoReturn rather
    // than forcing `a` to it.
    let module = check_ok("fn main() -> i32 = { let a = 0; a = return 0; a };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmt_expr(&stmts[1]).ty, Type::Never);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn assigning_to_a_literal_is_an_error() {
    let err = parse_err("fn main() -> i32 = { 1 = 2 };");
    assert!(
        matches!(err, FloErr::NotAssignable { .. }),
        "expected a not-assignable error, got: {err:?}"
    );
}

#[test]
fn assigning_to_an_operator_result_is_an_error() {
    // A consequence of `=` binding loosest: `a + 1 = 2` parses as `(a + 1) = 2`,
    // whose target is not an l-value.
    let err = parse_err("fn main() -> i32 = { let a = 0; a + 1 = 2 };");
    assert!(
        matches!(err, FloErr::NotAssignable { .. }),
        "expected a not-assignable error, got: {err:?}"
    );
}

#[test]
fn assigning_to_a_call_result_is_an_error() {
    let err = parse_err(
        "
        fn main() -> i32 = { id(1) = 2 };
        fn id(a: i32) -> i32 = a;
        ",
    );
    assert!(
        matches!(err, FloErr::NotAssignable { .. }),
        "expected a not-assignable error, got: {err:?}"
    );
}

/// The condition and body of a while expression.
fn while_parts(expr: &Expr) -> (&Expr, &Expr) {
    match &expr.kind {
        ExprKind::While(cond, body) => (cond, body),
        other => panic!("expected a while expression, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// While loops
//
// `while cond body` is an expression of type `void` — always, whatever the body
// contains. The condition must be `bool` and the body must be `void`, since its
// value is discarded (the same rule an else-less `if` follows). A loop never
// diverges: the condition may be false on the first check, so control always
// reaches whatever follows it.
// --------------------------------------------------------------------------

#[test]
fn while_is_void() {
    let module = check_ok("fn main() = while true {};");
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(main.ty, fn_ty(vec![], Type::Void));
    assert_eq!(main.body.ty, Type::Void);
    let (cond, body) = while_parts(&main.body);
    assert!(matches!(cond.kind, ExprKind::Bool(true)));
    assert_eq!(cond.ty, Type::Bool);
    assert_eq!(body.ty, Type::Void);
}

#[test]
fn while_condition_must_be_bool() {
    let errs = check_err("fn main() = while 1 {};");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn while_condition_resolves_calls() {
    let module = check_ok(
        "
        fn main() = while cmp(1) {};
        fn cmp(a: i32) -> bool = true;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (cond, _) = while_parts(&main.body);
    assert_eq!(
        resolved_call_name(cond),
        m("cmp", vec![Type::I32], Type::Bool)
    );
}

#[test]
fn while_condition_selects_an_overload_by_bool_result() {
    // The condition must be bool, which is enough to pick the bool overload.
    let module = check_ok(
        "
        fn main() = while pick(1) {};
        fn pick(a: i32) -> bool = true;
        fn pick(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (cond, _) = while_parts(&main.body);
    assert_eq!(
        resolved_call_name(cond),
        m("pick", vec![Type::I32], Type::Bool)
    );
}

#[test]
fn while_body_must_be_void() {
    // The body's value is discarded, so a body that yields an i32 is rejected.
    let errs = check_err("fn main() = while true { 1 };");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn while_body_with_trailing_semicolon_is_void() {
    // The same body is fine once the `;` drops the tail.
    let module = check_ok("fn main() = while true { 1; };");
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (_, body) = while_parts(&main.body);
    assert_eq!(body.ty, Type::Void);
    let (stmts, tail) = scope_parts(body);
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::I32);
    assert!(tail.is_none());
}

#[test]
fn while_body_must_be_a_scope() {
    let err = parse_err(
        "
        fn main() = while true nop();
        fn nop() = {};
        ",
    );
    assert!(
        matches!(
            err,
            FloErr::ExpectedTokenNotFound {
                expected: TokenKind::LCurly,
                ..
            }
        ),
        "expected a missing-`{{` error, got: {err:?}"
    );
}

#[test]
fn while_body_resolves_calls() {
    let module = check_ok(
        "
        fn main() = while true { id(1); };
        fn id(a: i32) -> i32 = a;
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (_, body) = while_parts(&main.body);
    let (stmts, _) = scope_parts(body);
    assert_eq!(
        resolved_call_name(stmt_expr(&stmts[0])),
        m("id", vec![Type::I32], Type::I32)
    );
}

#[test]
fn while_for_non_void_return_is_an_error() {
    // A loop yields nothing, so it cannot be the body of an i32 function.
    let errs = check_err("fn main() -> i32 = while true {};");
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn while_sees_enclosing_variables() {
    let module = check_ok(
        "
        fn main() = count(3);
        fn count(n: i32) = {
            let i = 0;
            while i < n {
                i = i + 1;
            };
        };
        ",
    );
    let count = func_sig(&module, "count", vec![Type::I32], Type::Void);
    let (stmts, _) = scope_parts(&count.body);
    let i = let_parts(&stmts[0]).0;
    let (cond, body) = while_parts(stmt_expr(&stmts[1]));
    // The condition reads both the local and the parameter.
    assert_eq!(
        resolved_call_name(cond),
        m("<", vec![Type::I32, Type::I32], Type::Bool)
    );
    assert_eq!(var_id(&call_args(cond)[0]), i);
    // ... and the body assigns to that same local.
    let (assign_target, _) = assign_parts(stmt_expr(&scope_parts(body).0[0]));
    assert_eq!(var_id(assign_target), i);
}

#[test]
fn while_does_not_diverge_the_enclosing_scope() {
    // Even a body that always breaks leaves the loop itself `void`, so the scope
    // falls through to `5` and is i32 rather than NoReturn.
    let module = check_ok("fn main() -> i32 = { while true { break; }; 5 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(main.body.ty, Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Void);
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn nested_while_loops_resolve() {
    let module = check_ok("fn main() = while true { while false { break; }; };");
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (_, outer_body) = while_parts(&main.body);
    let inner = stmt_expr(&scope_parts(outer_body).0[0]);
    assert_eq!(inner.ty, Type::Void);
    let (_, inner_body) = while_parts(inner);
    assert_eq!(inner_body.ty, Type::Never);
}

#[test]
fn return_inside_a_loop_body_is_allowed() {
    // `return` still leaves the whole function, and it constrains its operand to
    // the enclosing function's return type from inside the loop.
    let module = check_ok(
        "
        fn main() -> i8 = { while true { return 1; }; 5 };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I8);
    let (stmts, _) = scope_parts(&main.body);
    let (_, body) = while_parts(stmt_expr(&stmts[0]));
    let ret = stmt_expr(&scope_parts(body).0[0]);
    assert_eq!(return_value(ret).unwrap().ty, Type::I8);
}

// --------------------------------------------------------------------------
// Break & continue
//
// Both are `NoReturn`, exactly like `return`, and neither carries a value yet.
// A body that always jumps therefore diverges — which is accepted by the
// "body must be void" rule, since NoReturn satisfies any type. Whether they sit
// inside a loop is checked by the *parser*, so those errors come out before any
// types exist.
// --------------------------------------------------------------------------

#[test]
fn break_and_continue_are_noreturn() {
    let module = check_ok("fn main() = while true { break; continue; };");
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (_, body) = while_parts(&main.body);
    let (stmts, tail) = scope_parts(body);
    assert!(matches!(stmt_expr(&stmts[0]).kind, ExprKind::Break));
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Never);
    assert!(matches!(stmt_expr(&stmts[1]).kind, ExprKind::Continue));
    assert_eq!(stmt_expr(&stmts[1]).ty, Type::Never);
    assert!(tail.is_none());
    // The jumps make the body itself diverge, which still satisfies the loop.
    assert_eq!(body.ty, Type::Never);
    assert_eq!(main.body.ty, Type::Void);
}

#[test]
fn break_may_be_the_body_tail() {
    // Without a `;` the `break` is the body's tail expression; it is NoReturn,
    // which satisfies the void-body rule.
    let module = check_ok("fn main() = while true { break };");
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (_, body) = while_parts(&main.body);
    let (_, tail) = scope_parts(body);
    assert!(matches!(tail.unwrap().kind, ExprKind::Break));
    assert_eq!(tail.unwrap().ty, Type::Never);
}

#[test]
fn break_may_be_the_only_statement() {
    let module = check_ok("fn main() = while true { break; };");
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (_, body) = while_parts(&main.body);
    let (stmts, tail) = scope_parts(body);
    assert!(matches!(stmt_expr(&stmts[0]).kind, ExprKind::Break));
    assert_eq!(stmt_expr(&stmts[0]).ty, Type::Never);
    assert!(tail.is_none());
}

#[test]
fn break_inside_an_if_inside_a_loop_is_ok() {
    // The else-less `if` is void (control may skip the jump), so the body stays
    // void rather than diverging.
    let module = check_ok(
        "
        fn main() = cond(true);
        fn cond(b: bool) = while b { if b { break; }; };
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::Void);
    let (_, body) = while_parts(&cond.body);
    let if_stmt = stmt_expr(&scope_parts(body).0[0]);
    assert_eq!(if_stmt.ty, Type::Void);
    let (_, then, _) = if_parts(if_stmt);
    assert_eq!(then.ty, Type::Never);
    assert_eq!(body.ty, Type::Void);
}

#[test]
fn continue_in_a_diverging_if_makes_the_body_diverge() {
    // With both branches jumping, the `if` — and so the body — is NoReturn.
    let module = check_ok(
        "
        fn main() = cond(true);
        fn cond(b: bool) = while b { if b { continue } else { break } };
        ",
    );
    let cond = func_sig(&module, "cond", vec![Type::Bool], Type::Void);
    let (_, body) = while_parts(&cond.body);
    assert_eq!(body.ty, Type::Never);
    assert_eq!(cond.body.ty, Type::Void);
}

#[test]
fn break_outside_a_loop_is_an_error() {
    let err = parse_err("fn main() = break;");
    assert!(
        matches!(err, FloErr::BreakOutsideLoop { .. }),
        "expected a break-outside-loop error, got: {err:?}"
    );
}

#[test]
fn continue_outside_a_loop_is_an_error() {
    let err = parse_err("fn main() = { continue; };");
    assert!(
        matches!(err, FloErr::ContinueOutsideLoop { .. }),
        "expected a continue-outside-loop error, got: {err:?}"
    );
}

#[test]
fn break_after_a_loop_is_an_error() {
    // The loop only covers its own body: past the closing brace `break` is out
    // of a loop again.
    let err = parse_err("fn main() = { while true { break; }; break; };");
    assert!(
        matches!(err, FloErr::BreakOutsideLoop { .. }),
        "expected a break-outside-loop error, got: {err:?}"
    );
}

#[test]
fn break_in_the_loop_condition_is_an_error() {
    // The condition is evaluated before the body runs, so it is not "inside" the
    // loop for the purposes of `break`/`continue`.
    let err = parse_err("fn main() = while break {};");
    assert!(
        matches!(err, FloErr::BreakOutsideLoop { .. }),
        "expected a break-outside-loop error, got: {err:?}"
    );
}

#[test]
fn break_with_a_value_is_a_parse_error() {
    // `break` takes no operand yet, so the `1` is left dangling.
    let err = parse_err("fn main() = while true { break 1; };");
    assert!(
        matches!(err, FloErr::ExpectedTokenNotFound { .. }),
        "expected a missing-token error, got: {err:?}"
    );
}

// --------------------------------------------------------------------------
// Entry point
//
// Having a `main`, and it looking like an entry point, is a property of a whole
// program rather than of one parse — so it is checked separately, by
// `check_entry_point`, and only the driver runs it. The rest of the tests in
// this file use `main` freely as a vehicle and never go through it.
// --------------------------------------------------------------------------

#[test]
fn void_main_is_a_valid_entry_point() {
    assert!(entry_check("fn main() = {};").is_ok());
}

#[test]
fn i32_main_is_a_valid_entry_point() {
    assert!(entry_check("fn main() -> i32 = 0;").is_ok());
}

#[test]
fn a_program_without_main_is_an_error() {
    let err = entry_check("fn other() = {};").unwrap_err();
    assert!(
        matches!(err, FloErr::MainFunctionNotFound),
        "expected a missing-main error, got: {err:?}"
    );
}

#[test]
fn main_may_not_be_overloaded() {
    let err = entry_check(
        "
        fn main() = {};
        fn main(a: i32) = {};
        ",
    )
    .unwrap_err();
    assert!(
        matches!(err, FloErr::MultipleMainFunction),
        "expected a multiple-main error, got: {err:?}"
    );
}

#[test]
fn main_returning_something_other_than_void_or_i32_is_an_error() {
    let err = entry_check("fn main() -> bool = true;").unwrap_err();
    assert!(
        matches!(err, FloErr::InvalidMainSignature { .. }),
        "expected an invalid-main-signature error, got: {err:?}"
    );
}

#[test]
fn main_taking_arbitrary_arguments_is_an_error() {
    // The only argument form the spec allows is `[]string`, which needs slices
    // to exist first; until then `main` takes nothing at all.
    let err = entry_check("fn main(a: i32) = {};").unwrap_err();
    assert!(
        matches!(err, FloErr::InvalidMainSignature { .. }),
        "expected an invalid-main-signature error, got: {err:?}"
    );
}

#[test]
fn a_parenthesized_block_is_an_operand_not_a_statement() {
    // The implicit `;` is for statements that *begin* with a block. Wrapping one
    // in parens makes it an ordinary operand again, so this is one subtraction.
    let module = check_ok("fn main() -> i32 = { ({ 3 }) - 1 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert!(stmts.is_empty());
    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("-", vec![Type::I32, Type::I32], Type::I32)
    );
}

#[test]
fn a_block_folded_into_a_binary_operator_is_not_a_statement() {
    // The `- 1` binds to the `if`, so the statement is a call rather than an
    // `if`, and it needs its `;` like any other.
    let module = check_ok("fn main() -> i32 = { if true { 3 } else { 4 } - 1 };");
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, tail) = scope_parts(&main.body);
    assert!(stmts.is_empty());
    assert_eq!(
        resolved_call_name(tail.unwrap()),
        m("-", vec![Type::I32, Type::I32], Type::I32)
    );
}

// --------------------------------------------------------------------------
// Generic functions
//
// A generic function has no single type, so it is never checked as written. It
// is checked once per instantiation, after its type parameters have been
// substituted away -- which is why a mistake inside one is only reported when
// something asks for an instantiation that exposes it.
// --------------------------------------------------------------------------

#[test]
fn a_generic_is_instantiated_at_its_call_site() {
    let module = check_ok(
        "
        fn main() -> i32 = id(1);
        fn id<T>(a: T) -> T = a;
        ",
    );
    // The instantiation is named as though it had been written out by hand.
    let id = func_sig(&module, "id", vec![Type::I32], Type::I32);
    assert_eq!(id.body.ty, Type::I32);
    assert!(id.type_params.is_empty());

    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("id", vec![Type::I32], Type::I32)
    );
}

#[test]
fn an_uninstantiated_generic_is_not_emitted() {
    // Nothing calls `unused`, so there is no instantiation of it to check and
    // nothing to emit.
    let module = check_ok(
        "
        fn main() = {};
        fn unused<T>(a: T) -> T = a;
        ",
    );
    assert!(
        !module.funcs.keys().any(|k| k.starts_with("unused")),
        "expected no `unused` instantiation, got: {:?}",
        module.funcs.keys().collect::<Vec<_>>()
    );
}

#[test]
fn one_generic_instantiates_at_several_types() {
    let module = check_ok(
        "
        fn main() -> i32 = {
            take_u8(id(1));
            take_bool(id(true));
            id(2)
        };
        fn id<T>(a: T) -> T = a;
        fn take_u8(a: u8) = {};
        fn take_bool(a: bool) = {};
        ",
    );
    func_sig(&module, "id", vec![Type::U8], Type::U8);
    func_sig(&module, "id", vec![Type::Bool], Type::Bool);
    func_sig(&module, "id", vec![Type::I32], Type::I32);
}

#[test]
fn the_same_instantiation_is_only_checked_once() {
    let module = check_ok(
        "
        fn main() -> i32 = id(1) + id(2);
        fn id<T>(a: T) -> T = a;
        ",
    );
    let insts = module.funcs[&m("id", vec![Type::I32], Type::I32)].len();
    assert_eq!(insts, 1, "expected one `id<i32>`, got {insts}");
}

#[test]
fn a_generic_may_have_several_type_params() {
    let module = check_ok(
        "
        fn main() -> i32 = first(1, true);
        fn first<A, B>(a: A, b: B) -> A = a;
        ",
    );
    func_sig(&module, "first", vec![Type::I32, Type::Bool], Type::I32);
}

#[test]
fn a_generic_type_param_is_inferred_from_the_return_type() {
    let module = check_ok(
        "
        fn main() -> u8 = zero();
        fn zero<T>() -> T = 0;
        ",
    );
    func_sig(&module, "zero", vec![], Type::U8);
}

#[test]
fn a_generic_may_call_another_generic() {
    // `outer<bool>` is only discovered by checking `main`, and `inner<bool>`
    // only by checking `outer<bool>` -- so the worklist has to keep going.
    let module = check_ok(
        "
        fn main() -> bool = outer(true);
        fn outer<T>(a: T) -> T = inner(a);
        fn inner<T>(a: T) -> T = a;
        ",
    );
    func_sig(&module, "outer", vec![Type::Bool], Type::Bool);
    func_sig(&module, "inner", vec![Type::Bool], Type::Bool);
}

#[test]
fn a_generic_can_be_recursive() {
    let module = check_ok(
        "
        fn main() -> i32 = countdown(3);
        fn countdown<T>(a: T) -> T = if a == 0 { a } else { countdown(a - 1) };
        ",
    );
    func_sig(&module, "countdown", vec![Type::I32], Type::I32);
}

#[test]
fn a_generic_coexists_with_a_concrete_overload() {
    // The concrete i8 overload and the generic both fit an i8 call, so that one
    // is ambiguous, but a u8 call has only the generic to pick.
    let module = check_ok(
        "
        fn main() -> u8 = id(1);
        fn id(a: i8) -> i8 = a;
        fn id<T>(a: T) -> T = a;
        ",
    );
    func_sig(&module, "id", vec![Type::U8], Type::U8);
}

#[test]
fn a_generic_pipes_like_any_other_function() {
    let module = check_ok(
        "
        fn main() -> i32 = 1 |> id;
        fn id<T>(a: T) -> T = a;
        ",
    );
    func_sig(&module, "id", vec![Type::I32], Type::I32);
}

#[test]
fn a_generic_body_is_checked_per_instantiation() {
    // `a + b` is fine for i32 and meaningless for bool, and only the bool
    // instantiation says so.
    check_ok(
        "
        fn main() -> i32 = add(1, 2);
        fn add<T>(a: T, b: T) -> T = a + b;
        ",
    );

    let errs = check_err(
        "
        fn main() -> bool = add(true, false);
        fn add<T>(a: T, b: T) -> T = a + b;
        ",
    );
    assert_err!(errs, FloErr::InGenericInstantiation { .. });
}

#[test]
fn an_instantiation_error_names_the_generic_and_its_arguments() {
    let errs = check_err(
        "
        fn main() -> bool = add(true, false);
        fn add<T>(a: T, b: T) -> T = a + b;
        ",
    );
    let Some(FloErr::InGenericInstantiation {
        name,
        type_args,
        cause,
        ..
    }) = errs.first()
    else {
        panic!("expected an instantiation error, got: {errs:?}");
    };
    assert_eq!(name, "add");
    assert_eq!(type_args, &vec![Type::Bool]);
    // The cause is the real mistake, reported inside `add`.
    assert!(
        matches!(**cause, FloErr::NoPossibleOverloads { .. }),
        "expected the cause to be an overload failure, got: {cause:?}"
    );
}

#[test]
fn an_uninferable_type_param_is_an_error() {
    let errs = check_err(
        "
        fn main() = ignore();
        fn ignore<T>() = {};
        ",
    );
    assert_err!(errs, FloErr::CannotInferTypeParam { .. });
}

// --------------------------------------------------------------------------
// No types inside expressions
// --------------------------------------------------------------------------

#[test]
fn a_turbofish_is_not_syntax() {
    // Nothing inside an expression may name a type, so there is no way to write an
    // instantiation: `::<` is simply not a thing the grammar has.
    let err = parse_err(
        "
        fn main() = { id::<u8>(1); };
        fn id<T>(a: T) -> T = a;
        ",
    );
    assert!(
        matches!(err, FloErr::NotImplemented { .. }),
        "expected a not-implemented error, got: {err:?}"
    );
}

#[test]
fn an_annotation_picks_the_instantiation() {
    // What the turbofish used to do, done by saying what the value flows into.
    let module = check_ok(
        "
        fn main() = { let a: u8 = id(1); };
        fn id<T>(a: T) -> T = a;
        ",
    );
    func_sig(&module, "id", vec![Type::U8], Type::U8);
}

#[test]
fn a_parameter_type_picks_the_instantiation_through_a_pipe() {
    let module = check_ok(
        "
        fn main() = { let a: u8 = 1 |> id(); };
        fn id<T>(a: T) -> T = a;
        ",
    );
    func_sig(&module, "id", vec![Type::U8], Type::U8);
}

#[test]
fn a_generic_and_a_concrete_overload_that_both_fit_are_ambiguous() {
    // There is no longer anything that could rule one of them out at the call
    // site: an annotation names the type, not which overload provides it.
    let errs = check_err(
        "
        fn id(a: i8) -> i8 = a;
        fn id<T>(a: T) -> T = a;
        fn call(v: i8) -> i8 = id(v);
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::MultiplePossibleOverloads { name, .. } if name == "id");
}

#[test]
fn an_annotation_conflicting_with_the_arguments_is_an_error() {
    // Only one overload, so it is chosen and then `T` is pinned by the argument —
    // which leaves the return type to disagree with the annotation.
    let errs = check_err(
        "
        fn main() = { let a: u8 = id(true); };
        fn id<T>(a: T) -> T = a;
        ",
    );
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn an_empty_type_param_list_is_a_parse_error() {
    let err = parse_err("fn id<>(a: i32) -> i32 = a;");
    assert!(
        matches!(err, FloErr::EmptyTypeParamList { .. }),
        "expected an empty-type-param-list error, got: {err:?}"
    );
}

#[test]
fn a_repeated_type_param_is_a_parse_error() {
    let err = parse_err("fn id<T, T>(a: T) -> T = a;");
    assert!(
        matches!(err, FloErr::RedifinitionOfTypeParam { .. }),
        "expected a repeated-type-param error, got: {err:?}"
    );
}

#[test]
fn type_params_do_not_leak_between_functions() {
    // `T` is only a type parameter in `id`. In `other` it reads as the name of
    // a declared type, and nothing declares one.
    let errs = check_err(
        "
        fn id<T>(a: T) -> T = a;
        fn other(a: T) -> T = a;
        ",
    );
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "T");
}

// --------------------------------------------------------------------------
// User defined types
// --------------------------------------------------------------------------

/// The case name and payload of a case literal.
fn case_lit(expr: &Expr) -> (&str, Option<&Expr>) {
    match &expr.kind {
        ExprKind::CaseLit(None, case, payload) => (case.as_str(), payload.as_deref()),
        ExprKind::CaseLit(Some(q), ..) => panic!("literal kept its qualifier {q:?}"),
        other => panic!("expected a case literal, got {other:?}"),
    }
}

/// The field initialisers of a record literal, after `resolve` has filled in the
/// ones the source left out.
fn record_lit(expr: &Expr) -> &[FieldInit] {
    match &expr.kind {
        ExprKind::RecordLit(None, fields) => fields,
        ExprKind::RecordLit(Some(q), ..) => panic!("literal kept its qualifier {q:?}"),
        other => panic!("expected a record literal, got {other:?}"),
    }
}

/// The value a record literal gives a named field, or `None` if it has no such
/// field at all.
fn field_init<'a>(expr: &'a Expr, name: &str) -> Option<&'a Expr> {
    record_lit(expr)
        .iter()
        .find(|f| f.name == named_field(name))
        .map(|f| &f.value)
}

/// The receiver and name of a field access.
fn field_parts(expr: &Expr) -> (&Expr, String) {
    match &expr.kind {
        ExprKind::Field(recv, name) => (recv, format!("{name:?}")),
        other => panic!("expected a field access, got {other:?}"),
    }
}

fn named_field(name: &str) -> FieldName {
    FieldName::Named(name.to_string())
}

fn user(name: &str, args: Vec<Type>) -> Type {
    Type::User(name.to_string(), args)
}

/// An anonymous record, for comparing against an inferred one.
fn anon_record(fields: Vec<(&str, Type)>) -> Type {
    Type::AnonRecord(Record::named(
        fields.into_iter().map(|(n, t)| (n.to_string(), t)).collect(),
    ))
}

/// An anonymous record of positional fields.
fn anon_pos(fields: Vec<Type>) -> Type {
    Type::AnonRecord(Record::Pos(fields))
}

/// An anonymous sum of one case, for comparing against an inferred one.
fn anon_sum(case: &str, payload: Option<Type>) -> Type {
    Type::AnonSum(vec![SumCase::new(case.to_string(), payload)])
}

// --- Declarations ---------------------------------------------------------

/// The record a declaration is, or a panic if it is a sum.
fn record_decl<'a>(module: &'a Module, name: &str) -> &'a RecordDecl {
    match &module.types[name].kind {
        DeclKind::Record(record) => record,
        DeclKind::Sum(_) => panic!("`{name}` is a sum, not a record"),
    }
}

#[test]
fn a_sum_is_declared_with_its_cases_in_order() {
    let module = check_ok(
        "
        type Option = Some { val: i32 } | None;
        fn main() = {};
        ",
    );

    let decl = &module.types["Option"];
    assert!(decl.type_params.is_empty());
    assert_eq!(
        decl.cases().iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["Some", "None"]
    );

    // A payload is a record, and a case that carries nothing has none at all —
    // which is not the same as carrying an empty one.
    let payload = decl.cases()[0].payload.as_ref().expect("Some has a payload");
    assert_eq!(payload.fields[0].name, named_field("val"));
    assert_eq!(payload.fields[0].ty, Type::I32);
    assert!(decl.cases()[1].payload.is_none());
}

#[test]
fn a_record_is_declared_with_its_fields_in_order() {
    let module = check_ok(
        "
        type Vec = { len: u64, cap: u64 };
        fn main() = {};
        ",
    );

    let record = record_decl(&module, "Vec");
    assert_eq!(
        record
            .fields
            .iter()
            .map(|f| format!("{:?}", f.name))
            .collect::<Vec<_>>(),
        vec!["len", "cap"]
    );
    // A record has no cases: `{ .. }` after the `=` is not a case whose name was
    // left off, it is the type itself.
    assert!(module.types["Vec"].cases().is_empty());
}

#[test]
fn a_record_and_a_one_case_sum_are_different_types() {
    // What follows the `=` is what decides which kind this is, and there is no
    // shorthand between them: only the record supports field access.
    let module = check_ok(
        "
        type Rec = { v: i32 };
        type Sum = Sum { v: i32 };
        fn main() -> i32 = { let r: Rec = .{ v: 1 }; r.v };
        ",
    );
    assert!(module.types["Rec"].is_record());
    assert!(!module.types["Sum"].is_record());

    let errs = check_err(
        "
        type Sum = Sum { v: i32 };
        fn main() -> i32 = { let s: Sum = .Sum .{ v: 1 }; s.v };
        ",
    );
    assert_err!(errs, FloErr::FieldAccessOnSumType { .. });
}

#[test]
fn a_positional_record_names_its_fields_by_index() {
    let module = check_ok(
        "
        type Pair = { i32, bool };
        fn main() -> i32 = { let p: Pair = .{ 1, true }; p._0 };
        ",
    );

    let record = record_decl(&module, "Pair");
    assert_eq!(record.fields[0].name, FieldName::Pos(0));
    assert_eq!(record.fields[1].name, FieldName::Pos(1));
    assert_eq!(record.fields[1].ty, Type::Bool);
}

#[test]
fn a_record_may_not_mix_named_and_positional_fields() {
    let err = parse_err("type Bad = { x: i32, bool };\nfn main() = {};");
    assert!(
        matches!(err, FloErr::MixedFieldKinds { .. }),
        "expected a mixed-field-kinds error, got: {err:?}"
    );
}

#[test]
fn a_positional_field_name_may_not_be_written() {
    let err = parse_err("type Bad = { _0: i32 };\nfn main() = {};");
    assert!(
        matches!(err, FloErr::ReservedFieldName { ref field, .. } if field == "_0"),
        "expected a reserved-field-name error, got: {err:?}"
    );
}

#[test]
fn type_and_field_lists_allow_trailing_commas() {
    let module = check_ok(
        "
        type Foo<T,> = { a: T, b: bool, };
        fn main() = {};
        ",
    );
    assert_eq!(module.types["Foo"].type_params.len(), 1);
    assert_eq!(record_decl(&module, "Foo").fields.len(), 2);
}

#[test]
fn a_type_and_a_function_may_share_a_name() {
    // Types, functions and variables are three separate namespaces.
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn Foo() -> Foo = Foo.{ bar: 0 };
        fn main() = { Foo(); };
        ",
    );
    func_sig(&module, "Foo", vec![], user("Foo", vec![]));
}

#[test]
fn a_type_declared_twice_is_an_error() {
    let err = parse_err(
        "
        type Foo = { a: i32 };
        type Foo = { b: i32 };
        fn main() = {};
        ",
    );
    assert!(
        matches!(err, FloErr::DuplicateType { ref name, .. } if name == "Foo"),
        "expected a duplicate type error, got: {err:?}"
    );
}

#[test]
fn a_case_declared_twice_is_an_error() {
    let err = parse_err("type Foo = A { x: i32 } | A { y: i32 };\nfn main() = {};");
    assert!(
        matches!(err, FloErr::DuplicateCase { ref case, .. } if case == "A"),
        "expected a duplicate case error, got: {err:?}"
    );
}

#[test]
fn a_field_declared_twice_is_an_error() {
    let err = parse_err("type Foo = { x: i32, x: bool };\nfn main() = {};");
    assert!(
        matches!(err, FloErr::DuplicateField { ref field, .. } if field == "x"),
        "expected a duplicate field error, got: {err:?}"
    );
}

#[test]
fn an_unknown_type_in_an_annotation_is_an_error() {
    let errs = check_err("fn main() -> Nope = 0;");
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "Nope");
}

#[test]
fn an_unknown_type_in_a_let_annotation_is_an_error() {
    let errs = check_err("fn main() = { let a: Nope = 0; };");
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "Nope");
}

#[test]
fn a_type_may_be_used_before_it_is_declared() {
    let module = check_ok(
        "
        fn main() -> Foo = Foo.{ bar: 0 };
        type Foo = { bar: i32 };
        ",
    );
    func_sig(&module, "main", vec![], user("Foo", vec![]));
}

#[test]
fn the_wrong_number_of_type_arguments_is_an_error() {
    let errs = check_err(
        "
        type Pair<T> = { a: T, b: T };
        fn main() -> Pair = 0;
        ",
    );
    assert_err!(
        errs,
        FloErr::TypeArityMismatch { name, expected: 1, got: 0, .. } if name == "Pair"
    );
}

// --- Recursion ------------------------------------------------------------

#[test]
fn a_directly_recursive_type_is_an_error() {
    let errs = check_err(
        "
        type List = Cons { next: List } | Nil;
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::RecursiveType { name, .. } if name == "List");
}

#[test]
fn a_mutually_recursive_type_is_an_error() {
    let errs = check_err(
        "
        type A = { b: B };
        type B = { a: A };
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::RecursiveType { .. });
    // One cycle, reported once, not once per type on it.
    assert_eq!(
        errs.iter()
            .filter(|e| matches!(e, FloErr::RecursiveType { .. }))
            .count(),
        1
    );
}

#[test]
fn recursion_through_a_generic_type_argument_is_an_error() {
    // `A` only contains itself because `Holder<T>` holds its `T` by value,
    // which is not visible until `A` is substituted in for it.
    let errs = check_err(
        "
        type Holder<T> = { it: T };
        type A = { held: Holder<A> };
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::RecursiveType { .. });
}

#[test]
fn a_generic_that_nests_itself_is_an_error() {
    let errs = check_err(
        "
        type Deep<T> = { next: Deep<Deep<T>> };
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::RecursiveType { name, .. } if name == "Deep");
}

#[test]
fn two_fields_of_the_same_type_are_not_recursion() {
    check_ok(
        "
        type Pair<T> = { a: T, b: T };
        type Both = { ints: Pair<i32>, bools: Pair<bool> };
        fn main() = {};
        ",
    );
}

// --- Literals -------------------------------------------------------------

#[test]
fn a_literal_takes_the_type_the_context_wants() {
    // A record literal names no type at all, so the return type is the only thing
    // that says which record this is.
    let module = check_ok(
        "
        type Foo = { bar: i32, baz: bool };
        fn main() -> Foo = .{ bar: 0, baz: true };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Foo", vec![]));
    assert_eq!(main.body.ty, user("Foo", vec![]));

    // The declared field type is what the literal's `0` became.
    assert_eq!(field_init(&main.body, "bar").unwrap().ty, Type::I32);
}

#[test]
fn literal_fields_may_come_in_any_order() {
    let module = check_ok(
        "
        type Foo = { bar: i32, baz: bool };
        fn main() -> Foo = .{ baz: true, bar: 0 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Foo", vec![]));

    // Order is not part of a named record's type, and `resolve` puts the fields
    // in the record's canonical order rather than the written one.
    let names = record_lit(&main.body)
        .iter()
        .map(|f| format!("{:?}", f.name))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["bar", "baz"]);
}

#[test]
fn a_positional_literal_matches_its_fields_by_index() {
    let module = check_ok(
        "
        type Pair = { i32, bool };
        fn main() -> Pair = .{ 1, true };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Pair", vec![]));
    let fields = record_lit(&main.body);
    assert_eq!(fields[0].name, FieldName::Pos(0));
    assert_eq!(fields[0].value.ty, Type::I32);
    assert_eq!(fields[1].value.ty, Type::Bool);
}

#[test]
fn a_literal_may_not_mix_named_and_positional_fields() {
    let err = parse_err(
        "
        type Pair = { i32, bool };
        fn main() -> Pair = .{ 1, _1: true };
        ",
    );
    assert!(
        matches!(err, FloErr::ReservedFieldName { .. } | FloErr::MixedFieldKinds { .. }),
        "expected a mixed/reserved field error, got: {err:?}"
    );
}

#[test]
fn a_literal_is_pinned_by_a_function_boundary() {
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn take(f: Foo) -> i32 = 0;
        fn main() -> i32 = take(Foo.{ bar: 1 });
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("take", vec![user("Foo", vec![])], Type::I32)
    );
    assert_eq!(call_args(&main.body)[0].ty, user("Foo", vec![]));
}

#[test]
fn a_literal_that_meets_no_declared_type_becomes_anonymous() {
    let module = check_ok(
        "
        type Nowhere = { n: i32 };
        fn main() = { let a = .{ n: 1 }; };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, _) = let_parts(&stmts[0]);
    // `Nowhere` has exactly this shape, and is deliberately not what this is:
    // nothing pinned the literal to it, so it closed into the anonymous record.
    assert_eq!(*var_ty, anon_record(vec![("n", Type::I32)]));
}

#[test]
fn identical_anonymous_records_are_the_same_type() {
    // Structural, so two literals written apart from each other unify.
    let module = check_ok(
        "
        fn main() = {
            let a = .{ x: 1, y: 2 };
            let b = .{ y: 4, x: 3 };
            a = b;
        };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, a_ty, _) = let_parts(&stmts[0]);
    let (_, b_ty, _) = let_parts(&stmts[1]);
    assert_eq!(a_ty, b_ty);
    assert_eq!(
        *a_ty,
        anon_record(vec![("x", Type::I32), ("y", Type::I32)])
    );
}

#[test]
fn an_anonymous_record_is_not_a_declared_record_of_the_same_shape() {
    // Nominal means nominal: a declared type is only itself, however an
    // anonymous one is shaped.
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn take(f: Foo) -> i32 = 0;
        fn main() -> i32 = {
            let a: { bar: i32 } = .{ bar: 1 };
            take(a)
        };
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { name, .. } if name == "take");
}

#[test]
fn an_anonymous_record_written_down_is_concrete() {
    let module = check_ok(
        "
        fn take(p: { x: i32, y: i32 }) -> i32 = p.x + p.y;
        fn main() -> i32 = take(.{ x: 1, y: 2 });
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let arg = anon_record(vec![("x", Type::I32), ("y", Type::I32)]);
    assert_eq!(
        resolved_call_name(&main.body),
        m("take", vec![arg.clone()], Type::I32)
    );
    assert_eq!(call_args(&main.body)[0].ty, arg);
}

#[test]
fn a_case_the_type_does_not_have_is_an_error() {
    let errs = check_err(
        "
        type Foo = A { bar: i32 } | B;
        fn main() -> Foo = .Nope .{ bar: 0 };
        ",
    );
    assert_err!(errs, FloErr::NoSuchCase { case, .. } if case == "Nope");
}

// --- Anonymous sums -------------------------------------------------------

#[test]
fn an_anonymous_sum_written_down_is_concrete() {
    let module = check_ok(
        "
        fn take(s: Alive | Dead) -> Alive | Dead = s;
        fn main() = { take(.Alive); };
        ",
    );
    let status = Type::AnonSum(vec![
        SumCase::new("Alive".to_string(), None),
        SumCase::new("Dead".to_string(), None),
    ]);
    let main = func_sig(&module, "main", vec![], Type::Void);
    assert_eq!(
        resolved_call_name(stmt_expr(&scope_parts(&main.body).0[0])),
        m("take", vec![status.clone()], status)
    );
}

#[test]
fn identical_anonymous_sums_are_the_same_type() {
    // Structural, and the case order is not part of the type.
    let module = check_ok(
        "
        fn a(s: Alive | Dead) -> i32 = 0;
        fn b(s: Dead | Alive) -> i32 = 1;
        fn main() = { a(.Alive); b(.Alive); };
        ",
    );
    let (stmts, _) = scope_parts(&func_sig(&module, "main", vec![], Type::Void).body);
    let arg_of = |i: usize| call_args(stmt_expr(&stmts[i]))[0].ty.clone();
    assert_eq!(arg_of(0), arg_of(1));
}

#[test]
fn an_anonymous_sum_is_not_a_declared_sum_of_the_same_shape() {
    let errs = check_err(
        "
        type Status = Alive | Dead;
        fn take(s: Status) -> i32 = 0;
        fn main() -> i32 = {
            let s: Alive | Dead = .Alive;
            take(s)
        };
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { name, .. } if name == "take");
}

#[test]
fn an_anonymous_sum_needs_at_least_two_cases() {
    // With one there is no `|`, and nothing to tell a case from a type of that
    // name — so the one-case form is a record written the long way round.
    let err = parse_err("fn take(x: Only { n: i32 }) = {};\nfn main() = {};");
    assert!(
        matches!(err, FloErr::SingleCaseAnonSum { ref case, .. } if case == "Only"),
        "expected a single-case-anon-sum error, got: {err:?}"
    );
}

#[test]
fn a_primitive_cannot_be_a_case_of_a_sum() {
    let err = parse_err("fn take(x: i32 | Dead) = {};\nfn main() = {};");
    assert!(
        matches!(err, FloErr::NotACase { .. }),
        "expected a not-a-case error, got: {err:?}"
    );
}

#[test]
fn a_case_named_twice_in_an_anonymous_sum_is_an_error() {
    let err = parse_err("fn take(x: Dead | Dead) = {};\nfn main() = {};");
    assert!(
        matches!(err, FloErr::DuplicateCaseInAnonSum { ref case, .. } if case == "Dead"),
        "expected a duplicate-case error, got: {err:?}"
    );
}

// --- Composition ----------------------------------------------------------

#[test]
fn a_record_composes_anonymous_records_and_sums() {
    // Anywhere a type may be written, any type may be written — so a field's type
    // can be a record or a sum of its own, nested as deep as it likes.
    let module = check_ok(
        "
        type Player = {
            pos: { x: i32, y: i32 },
            status: Alive | Dead,
        };
        fn main() -> i32 = {
            let p: Player = .{ pos: .{ x: 1, y: 2 }, status: .Alive };
            p.pos.x
        };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (_, tail) = scope_parts(&main.body);
    let (recv, field) = field_parts(tail.unwrap());
    assert_eq!(field, "x");
    assert_eq!(
        recv.ty,
        anon_record(vec![("x", Type::I32), ("y", Type::I32)])
    );
}

#[test]
fn a_field_left_out_of_a_composed_record_is_zero_initialized() {
    let module = check_ok(
        "
        type Player = {
            pos: { x: i32, y: i32 },
            status: Alive | Dead,
        };
        fn main() -> Player = .{ status: .Dead };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Player", vec![]));
    let pos = field_init(&main.body, "pos").unwrap();
    assert!(matches!(pos.kind, ExprKind::Zeroed));
    assert_eq!(
        pos.ty,
        anon_record(vec![("x", Type::I32), ("y", Type::I32)])
    );
}

#[test]
fn a_type_named_inside_an_anonymous_type_must_exist() {
    let errs = check_err("fn take(p: { inner: Nope }) = {};\nfn main() = {};");
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "Nope");
}

#[test]
fn recursion_through_an_anonymous_type_is_an_error() {
    // A cycle is a cycle whether or not it has a name at every step.
    let errs = check_err(
        "
        type A = { b: { c: A } };
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::RecursiveType { name, .. } if name == "A");
}

#[test]
fn recursion_through_a_case_payload_is_an_error() {
    let errs = check_err(
        "
        type A = Wrap { A } | Stop;
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::RecursiveType { name, .. } if name == "A");
}

#[test]
fn a_field_left_out_of_a_literal_is_zero_initialized() {
    let module = check_ok(
        "
        type Foo = { bar: i32, baz: bool };
        fn main() -> Foo = .{ bar: 0 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Foo", vec![]));

    // The literal reaching the back end gives every field of the record exactly
    // once, whatever the source wrote.
    assert_eq!(record_lit(&main.body).len(), 2);
    let baz = field_init(&main.body, "baz").unwrap();
    assert!(matches!(baz.kind, ExprKind::Zeroed));
    assert_eq!(baz.ty, Type::Bool);
}

#[test]
fn a_field_left_out_of_a_positional_literal_is_zero_initialized() {
    let module = check_ok(
        "
        type Pair = { i32, bool };
        fn main() -> Pair = .{ 1 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Pair", vec![]));
    let fields = record_lit(&main.body);
    assert_eq!(fields.len(), 2);
    assert!(matches!(fields[1].value.kind, ExprKind::Zeroed));
    assert_eq!(fields[1].value.ty, Type::Bool);
}

#[test]
fn merging_two_open_records_zero_initializes_both() {
    // An open record is the one type that grows: the union of the two field sets
    // is what they both are, so each literal is missing what the other gave.
    let module = check_ok(
        "
        fn main() = {
            let a = .{ x: 1 };
            let b = .{ y: 2 };
            a = b;
        };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);

    let (_, a_ty, a_init) = let_parts(&stmts[0]);
    assert_eq!(
        *a_ty,
        anon_record(vec![("x", Type::I32), ("y", Type::I32)])
    );
    let a_init = a_init.unwrap();
    assert!(matches!(field_init(a_init, "y").unwrap().kind, ExprKind::Zeroed));

    let (_, _, b_init) = let_parts(&stmts[1]);
    let b_init = b_init.unwrap();
    assert!(matches!(field_init(b_init, "x").unwrap().kind, ExprKind::Zeroed));
}

#[test]
fn a_literal_with_an_extra_field_is_an_error() {
    // A field left *out* is zero initialized; one the record does not have has
    // nowhere to go.
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn main() -> Foo = .{ bar: 0, nope: 1 };
        ",
    );
    assert_err!(errs, FloErr::UnexpectedField { field, .. } if field == "nope");
}

#[test]
fn two_concrete_records_of_different_shapes_do_not_unify() {
    let errs = check_err(
        "
        fn main() = {
            let a: { x: i32 } = .{ x: 1 };
            let b: { y: i32 } = .{ y: 1 };
            a = b;
        };
        ",
    );
    assert_err!(errs, FloErr::WrongFields { .. });
}

#[test]
fn a_field_given_twice_is_an_error() {
    let err = parse_err(
        "
        type Foo = { bar: i32 };
        fn main() -> Foo = .{ bar: 0, bar: 1 };
        ",
    );
    assert!(
        matches!(err, FloErr::DuplicateFieldInit { ref field, .. } if field == "bar"),
        "expected a duplicate field error, got: {err:?}"
    );
}

#[test]
fn a_field_of_the_wrong_type_is_an_error() {
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn main() -> Foo = .{ bar: true };
        ",
    );
    assert_err!(errs, FloErr::TypeMismatch { .. });
}

#[test]
fn a_case_with_no_payload_is_written_bare() {
    let module = check_ok(
        "
        type Option = Some { val: i32 } | None;
        fn main() -> Option = .None;
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Option", vec![]));
    let (case, payload) = case_lit(&main.body);
    assert_eq!(case, "None");
    assert!(payload.is_none());
    assert_eq!(main.body.ty, user("Option", vec![]));
}

#[test]
fn a_case_payload_is_a_record_literal() {
    let module = check_ok(
        "
        type Option = Some { val: i32 } | None;
        fn main() -> Option = .Some .{ val: 1 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Option", vec![]));
    let (case, payload) = case_lit(&main.body);
    assert_eq!(case, "Some");

    let payload = payload.expect("Some carries a payload");
    assert_eq!(payload.ty, anon_record(vec![("val", Type::I32)]));
    assert_eq!(field_init(payload, "val").unwrap().ty, Type::I32);
}

#[test]
fn a_positional_case_payload_is_matched_by_index() {
    let module = check_ok(
        "
        type Option<T> = Some { T } | None;
        fn main() -> Option<bool> = .Some .{ true };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Option", vec![Type::Bool]));
    let (_, payload) = case_lit(&main.body);
    assert_eq!(payload.unwrap().ty, anon_pos(vec![Type::Bool]));
}

#[test]
fn a_case_given_no_payload_when_it_takes_one_is_an_error() {
    let errs = check_err(
        "
        type Option = Some { val: i32 } | None;
        fn main() -> Option = .Some;
        ",
    );
    assert_err!(errs, FloErr::PayloadMismatch { case, expected: true, .. } if case == "Some");
}

#[test]
fn a_case_given_a_payload_it_does_not_take_is_an_error() {
    let errs = check_err(
        "
        type Option = Some { val: i32 } | None;
        fn main() -> Option = .None .{ val: 1 };
        ",
    );
    assert_err!(errs, FloErr::PayloadMismatch { case, expected: false, .. } if case == "None");
}

#[test]
fn a_record_literal_is_not_a_sum() {
    let errs = check_err(
        "
        type Option = Some { val: i32 } | None;
        fn main() -> Option = .{ val: 1 };
        ",
    );
    assert_err!(errs, FloErr::RecordSumMismatch { .. });
}

#[test]
fn a_case_literal_is_not_a_record() {
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn main() -> Foo = .Foo .{ bar: 1 };
        ",
    );
    assert_err!(errs, FloErr::RecordSumMismatch { .. });
}

#[test]
fn an_anonymous_record_and_an_anonymous_sum_do_not_unify() {
    let errs = check_err(
        "
        fn main() = {
            let a = .{ x: 1 };
            a = .Alive;
        };
        ",
    );
    assert_err!(errs, FloErr::RecordSumMismatch { .. });
}

#[test]
fn a_variable_wins_over_a_type_of_the_same_name() {
    // `Foo.` is field access when a variable `Foo` is in scope, and a literal's
    // qualifier only when one is not.
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn main() -> i32 = {
            let Foo = .{ bar: 7 };
            Foo.bar
        };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (_, tail) = scope_parts(&main.body);
    let (recv, field) = field_parts(tail.unwrap());
    assert!(matches!(recv.kind, ExprKind::Var(_)));
    assert_eq!(field, "bar");
}

#[test]
fn a_dot_must_be_followed_by_a_field_name_or_a_brace() {
    // A leading `.` starts a literal and only a literal, so there is nothing else
    // `.5` could be trying to be.
    let err = parse_err("fn main() -> i32 = .5;");
    assert!(
        matches!(err, FloErr::ExpectedLiteral { .. }),
        "expected an expected-literal error, got: {err:?}"
    );
}

#[test]
fn a_variable_in_a_condition_is_not_read_as_a_literal() {
    let module = check_ok(
        "
        fn main() -> i32 = {
            let flag = true;
            if flag { 1 } else { 2 }
        };
        ",
    );
    func_sig(&module, "main", vec![], Type::I32);
}

// --- Qualified literals ---------------------------------------------------

#[test]
fn a_qualifier_pins_a_record_literal_outright() {
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn main() = { let a = Foo.{ bar: 0 }; };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(*var_ty, user("Foo", vec![]));
    // The qualifier is dropped once it has done its job: `ty` says which type
    // this is, exactly as it does for a bare literal.
    assert_eq!(record_lit(init.unwrap()).len(), 1);
}

#[test]
fn a_qualifier_pins_a_case_literal_outright() {
    let module = check_ok(
        "
        type Option = Some { val: i32 } | None;
        fn main() = { let a = Option.None; };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(*var_ty, user("Option", vec![]));
    let (case, _) = case_lit(init.unwrap());
    assert_eq!(case, "None");
}

#[test]
fn a_qualifier_naming_a_case_the_type_lacks_is_an_error() {
    let errs = check_err(
        "
        type Foo = A { bar: i32 } | B;
        fn main() = { let a = Foo.Nope.{ bar: 0 }; };
        ",
    );
    assert_err!(errs, FloErr::NoSuchCase { case, .. } if case == "Nope");
}

#[test]
fn a_qualifier_that_names_no_type_is_an_error() {
    // Whether a name is a type cannot be known while parsing — the declaration may
    // be further down the file — so this is reported with the declarations.
    let errs = check_err("fn main() = { let a = Ghost.{ n: 0 }; };");
    assert_err!(errs, FloErr::NotATypeOrVariable { name, .. } if name == "Ghost");
}

#[test]
fn a_qualifier_infers_the_type_arguments() {
    // Naming the type is not the same as giving its arguments — which cannot be
    // written at all, since no type may appear inside an expression. `Option.Some`
    // says which declaration this is and leaves `T` to inference, which takes it
    // from the annotation.
    let module = check_ok(
        "
        type Option<T> = Some { val: T } | None;
        fn main() = { let a: Option<u8> = Option.Some.{ val: 1 }; };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(*var_ty, user("Option", vec![Type::U8]));
    assert_eq!(init.unwrap().ty, user("Option", vec![Type::U8]));
}

#[test]
fn a_qualifier_whose_arguments_cannot_be_inferred_is_an_error() {
    // Nothing here says what `T` is, and there is no way to write it down, so the
    // literal's type never becomes known.
    let errs = check_err(
        "
        type Option<T> = Some { val: T } | None;
        fn main() = { let a = Option.None; };
        ",
    );
    assert_err!(errs, FloErr::UnresolvedType { .. });
}

// --- Bare names -----------------------------------------------------------
//
// A bare name is a variable, or a call when parentheses follow, and nothing else.
// A case is only ever written after a `.`, and a type name only as a qualifier —
// which is a `.` too — so nothing has to be resolved to know what was written.
// --------------------------------------------------------------------------

#[test]
fn a_bare_case_name_is_an_unknown_identifier() {
    let err = parse_err(
        "
        type Option = Some { val: i32 } | None;
        fn main() -> Option = None;
        ",
    );
    assert!(
        matches!(err, FloErr::UnknownIdentifier { ref name, .. } if name == "None"),
        "expected an unknown-identifier error, got: {err:?}"
    );
}

#[test]
fn a_bare_type_name_is_an_unknown_identifier() {
    let err = parse_err(
        "
        type Foo = { bar: i32 };
        fn main() -> i32 = { let a = Foo; 0 };
        ",
    );
    assert!(
        matches!(err, FloErr::UnknownIdentifier { ref name, .. } if name == "Foo"),
        "expected an unknown-identifier error, got: {err:?}"
    );
}

#[test]
fn a_misspelt_variable_is_an_unknown_identifier() {
    let err = parse_err("fn main() -> i32 = { let count = 1; conut };");
    assert!(
        matches!(err, FloErr::UnknownIdentifier { ref name, .. } if name == "conut"),
        "expected an unknown-identifier error, got: {err:?}"
    );
}

#[test]
fn a_type_may_be_qualified_before_it_is_declared() {
    let module = check_ok(
        "
        fn main() -> Foo = Foo.{ bar: 0 };
        type Foo = { bar: i32 };
        ",
    );
    func_sig(&module, "main", vec![], user("Foo", vec![]));
}

#[test]
fn a_case_literal_needs_no_declaration_in_scope() {
    // There is nothing to bring into scope: the `.` is what makes the name a case,
    // and which type it belongs to is inferred.
    let module = check_ok(
        "
        type A = Same { n: i32 };
        type B = Same { n: i32 };
        fn main() -> B = .Same .{ n: 0 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("B", vec![]));
    assert_eq!(main.body.ty, user("B", vec![]));
}

// --- Shared case names ----------------------------------------------------

#[test]
fn a_shared_case_name_is_resolved_by_the_expected_type() {
    let module = check_ok(
        "
        type A = Same { n: i32 };
        type B = Same { n: i32 };
        fn main() -> B = .Same .{ n: 0 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("B", vec![]));
    assert_eq!(main.body.ty, user("B", vec![]));
}

#[test]
fn a_shared_case_name_prunes_overloads_by_its_fields() {
    // Both overloads take a type with a case called `Same`, so only the fields
    // can tell them apart.
    let module = check_ok(
        "
        type A = Same { n: i32 };
        type B = Same { flag: bool };
        fn take(a: A) -> i32 = 1;
        fn take(b: B) -> i32 = 2;
        fn main() -> i32 = take(.Same .{ flag: true });
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("take", vec![user("B", vec![])], Type::I32)
    );
}

#[test]
fn a_case_name_no_overload_can_take_is_an_error() {
    let errs = check_err(
        "
        type A = OnlyA { n: i32 };
        type B = Other { n: i32 };
        fn take(a: A) -> i32 = 1;
        fn main() -> i32 = take(.Other .{ n: 0 });
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { name, .. } if name == "take");
}

// --- Field access ---------------------------------------------------------

#[test]
fn a_field_can_be_read() {
    let module = check_ok(
        "
        type Foo = { bar: i32, baz: bool };
        fn get(f: Foo) -> i32 = f.bar;
        fn main() = {};
        ",
    );
    let get = func_sig(&module, "get", vec![user("Foo", vec![])], Type::I32);
    let (recv, name) = field_parts(&get.body);
    assert_eq!(name, "bar");
    assert_eq!(recv.ty, user("Foo", vec![]));
    assert_eq!(get.body.ty, Type::I32);
}

#[test]
fn a_field_can_be_assigned() {
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn set(f: Foo) = { f.bar = 100; };
        fn main() = {};
        ",
    );
    let set = func_sig(&module, "set", vec![user("Foo", vec![])], Type::Void);
    let (stmts, _) = scope_parts(&set.body);
    let (target, value) = assign_parts(stmt_expr(&stmts[0]));
    assert_eq!(field_parts(target).1, "bar");
    assert_eq!(value.ty, Type::I32);
}

#[test]
fn a_field_of_a_temporary_cannot_be_assigned() {
    let err = parse_err(
        "
        type Foo = { bar: i32 };
        fn make() -> Foo = Foo.{ bar: 0 };
        fn main() = make().bar = 1;
        ",
    );
    assert!(
        matches!(err, FloErr::NotAssignable { .. }),
        "expected a not-assignable error, got: {err:?}"
    );
}

#[test]
fn a_field_of_a_literal_type_needs_no_annotation() {
    let module = check_ok(
        "
        type Holder = { n: i32 };
        fn main() -> i32 = {
            let v = .{ n: 1 };
            v.n + 1
        };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, _) = let_parts(&stmts[0]);
    // Nothing pinned it to `Holder`, so it is the anonymous record of the same
    // shape — which is still something `.n` can be read of.
    assert_eq!(*var_ty, anon_record(vec![("n", Type::I32)]));
}

#[test]
fn field_access_chains() {
    let module = check_ok(
        "
        type Inner = { n: i32 };
        type Outer = { inner: Inner };
        fn get(o: Outer) -> i32 = o.inner.n;
        fn main() = {};
        ",
    );
    let get = func_sig(&module, "get", vec![user("Outer", vec![])], Type::I32);
    let (recv, name) = field_parts(&get.body);
    assert_eq!(name, "n");
    assert_eq!(recv.ty, user("Inner", vec![]));
}

#[test]
fn field_access_binds_tighter_than_an_operator() {
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn neg(f: Foo) -> i32 = -f.bar;
        fn main() = {};
        ",
    );
    let neg = func_sig(&module, "neg", vec![user("Foo", vec![])], Type::I32);
    // `-(f.bar)`, so the operand of the negation is the access.
    let arg = &call_args(&neg.body)[0];
    assert_eq!(field_parts(arg).1, "bar");
}

#[test]
fn a_field_in_a_condition_reads_as_a_condition() {
    let module = check_ok(
        "
        type Foo = { bar: i32, baz: bool };
        fn pick(f: Foo) -> i32 = if f.baz { 1 } else { 2 };
        fn main() = {};
        ",
    );
    func_sig(&module, "pick", vec![user("Foo", vec![])], Type::I32);
}

#[test]
fn a_field_of_a_sum_type_is_an_error() {
    let errs = check_err(
        "
        type Option = Some { val: i32 } | None;
        fn get(o: Option) -> i32 = o.val;
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::FieldAccessOnSumType { field, .. } if field == "val");
}

#[test]
fn a_field_the_type_does_not_have_is_an_error() {
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn get(f: Foo) -> i32 = f.nope;
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::UnknownField { field, .. } if field == "nope");
}

#[test]
fn a_field_of_a_primitive_is_an_error() {
    let errs = check_err("fn get(n: i32) -> i32 = n.bar;\nfn main() = {};");
    assert_err!(errs, FloErr::NotAStruct { field, .. } if field == "bar");
}

#[test]
fn a_positional_field_of_a_named_record_is_an_error() {
    // A record is addressed by name or by position, and which one is part of its
    // type.
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn get(f: Foo) -> i32 = f._0;
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::UnknownField { field, .. } if field == "_0");
}

#[test]
fn match_is_reserved_but_not_implemented() {
    let err = parse_err("fn main() -> i32 = match 1 { 0 -> 1, _ -> 2 };");
    assert!(
        matches!(err, FloErr::NotImplemented { what: "match", .. }),
        "expected a not-implemented error, got: {err:?}"
    );
}

#[test]
fn a_field_of_an_open_sum_is_an_error() {
    // The receiver is a sum from the moment the literal named a case, so there is
    // nothing to wait for: `.` never reaches into one.
    let errs = check_err(
        "
        type Option = Some { n: i32 } | None;
        fn main() -> i32 = {
            let v = .Some .{ n: 1 };
            v.n
        };
        ",
    );
    assert_err!(errs, FloErr::FieldAccessOnSumType { field, .. } if field == "n");
}

#[test]
fn an_access_is_rechecked_after_its_receiver_grows() {
    // `v.n` resolves while `v` is still an open record; the assignment below adds a
    // field, and it is the final check that sees the receiver as it ended up.
    let errs = check_err(
        "
        fn main() -> i32 = {
            let v = .{ n: 1 };
            let n = v.nope;
            v = .{ n: 2, other: 3 };
            n
        };
        ",
    );
    assert_err!(errs, FloErr::UnknownField { field, .. } if field == "nope");
}

// --- Generic types --------------------------------------------------------

#[test]
fn a_generic_type_is_specialized_by_its_use() {
    let module = check_ok(
        "
        type Holder<T> = { it: T };
        fn main() -> Holder<u8> = Holder.{ it: 1 };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Holder", vec![Type::U8]));
    // The field type came from the declaration with `T` substituted away.
    assert_eq!(field_init(&main.body, "it").unwrap().ty, Type::U8);
}

#[test]
fn one_generic_type_at_two_specializations() {
    let module = check_ok(
        "
        type Holder<T> = { it: T };
        fn take(h: Holder<i32>) -> i32 = 1;
        fn take(h: Holder<bool>) -> i32 = 2;
        fn main() -> i32 = take(Holder.{ it: true });
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    assert_eq!(
        resolved_call_name(&main.body),
        m("take", vec![user("Holder", vec![Type::Bool])], Type::I32)
    );
}

#[test]
fn a_generic_function_over_a_generic_type() {
    let module = check_ok(
        "
        type Holder<T> = { it: T };
        fn get<T>(h: Holder<T>) -> T = h.it;
        fn main() -> u8 = get(Holder.{ it: 1 });
        ",
    );
    func_sig(
        &module,
        "get",
        vec![user("Holder", vec![Type::U8])],
        Type::U8,
    );
}

#[test]
fn a_qualifier_does_not_fix_the_type_arguments() {
    // The qualifier says which declaration this is and nothing more, so `T` is
    // settled the way any other unknown is — here by the literal defaulting.
    let module = check_ok(
        "
        type Holder<T> = { it: T };
        fn main() = { let h = Holder.{ it: 1 }; };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(*var_ty, user("Holder", vec![Type::I32]));
    assert_eq!(field_init(init.unwrap(), "it").unwrap().ty, Type::I32);
}

#[test]
fn an_annotation_is_how_a_type_argument_is_forced() {
    // With no turbofish, annotating what the value flows into is the way to say
    // what a type argument is.
    let module = check_ok(
        "
        type Holder<T> = { it: T };
        fn main() = { let h: Holder<u8> = .{ it: 1 }; };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(*var_ty, user("Holder", vec![Type::U8]));
    assert_eq!(field_init(init.unwrap(), "it").unwrap().ty, Type::U8);
}

#[test]
fn a_user_type_appears_in_the_mangled_name() {
    let module = check_ok(
        "
        type Holder<T> = { it: T };
        fn take(h: Holder<i32>) -> i32 = 0;
        fn main() -> i32 = take(Holder.{ it: 1 });
        ",
    );
    assert!(
        module
            .funcs
            .keys()
            .any(|k| k.contains("Holder<i32>") && k.starts_with("take")),
        "expected a mangled name mentioning the specialization, got: {:?}",
        module.funcs.keys().collect::<Vec<_>>()
    );
}

// --- Interaction with the rest of the checker -----------------------------

#[test]
fn a_diverging_field_value_makes_the_literal_diverge() {
    let module = check_ok(
        "
        type Foo = { bar: i32 };
        fn main() -> i32 = { let f = Foo.{ bar: return 1 }; 0 };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::I32);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, init) = let_parts(&stmts[0]);
    assert_eq!(*var_ty, Type::Never);
    assert_eq!(init.unwrap().ty, Type::Never);
}

#[test]
fn a_literal_in_both_branches_of_an_if_is_one_type() {
    let module = check_ok(
        "
        type Option = Some { val: i32 } | None;
        fn pick(c: bool) -> Option = if c { .Some .{ val: 1 } } else { .None };
        fn main() = {};
        ",
    );
    let pick = func_sig(&module, "pick", vec![Type::Bool], user("Option", vec![]));
    assert_eq!(pick.body.ty, user("Option", vec![]));
}

#[test]
fn two_literals_that_join_become_one_anonymous_sum() {
    let module = check_ok(
        "
        type Option = Some { val: i32 } | None;
        fn main() = {
            let v = .Some .{ val: 1 };
            v = .None;
        };
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::Void);
    let (stmts, _) = scope_parts(&main.body);
    let (_, var_ty, _) = let_parts(&stmts[0]);
    assert_eq!(
        *var_ty,
        Type::AnonSum(vec![
            SumCase::new("None".to_string(), None),
            SumCase::new(
                "Some".to_string(),
                Some(anon_record(vec![("val", Type::I32)]))
            ),
        ])
    );
}

#[test]
fn a_case_that_disagrees_with_itself_is_an_error() {
    // Both payloads are open records, so they merge into `{ a, b }` — and then
    // there is no declared `Same` that has both.
    let errs = check_err(
        "
        type S = Same { a: i32 };
        fn take(s: S) = {};
        fn main() = {
            let v = .Same .{ a: 1 };
            v = .Same .{ b: 2 };
            take(v);
        };
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { name, .. } if name == "take");
}

#[test]
fn a_field_holds_a_literal_of_another_type() {
    let module = check_ok(
        "
        type Inner = { n: i32 };
        type Outer = { inner: Inner };
        fn main() -> Outer = Outer.{ inner: Inner.{ n: 1 } };
        ",
    );
    let main = func_sig(&module, "main", vec![], user("Outer", vec![]));
    assert_eq!(
        field_init(&main.body, "inner").unwrap().ty,
        user("Inner", vec![])
    );
}

#[test]
fn a_user_type_has_no_builtin_operators() {
    let errs = check_err(
        "
        type Foo = { bar: i32 };
        fn add(a: Foo, b: Foo) -> Foo = a + b;
        fn main() = {};
        ",
    );
    assert_err!(errs, FloErr::NoPossibleOverloads { name, .. } if name == "+");
}

#[test]
fn an_operator_can_be_overloaded_on_a_user_type() {
    let module = check_ok(
        "
        type Vec2 = { x: i32, y: i32 };
        op +(a: Vec2, b: Vec2) -> Vec2 = Vec2.{ x: a.x + b.x, y: a.y + b.y };
        fn plus(a: Vec2, b: Vec2) -> Vec2 = a + b;
        fn main() = {};
        ",
    );
    let plus = func_sig(
        &module,
        "plus",
        vec![user("Vec2", vec![]), user("Vec2", vec![])],
        user("Vec2", vec![]),
    );
    assert_eq!(
        resolved_call_name(&plus.body),
        m(
            "+",
            vec![user("Vec2", vec![]), user("Vec2", vec![])],
            user("Vec2", vec![])
        )
    );
}

#[test]
fn the_new_operators_can_be_overloaded_too() {
    // `!`, `~`, `<<` and `>>` are ordinary overloadable operators -- only the
    // short-circuiting pair is not. `op <<` also exercises declaring an operator
    // the tokenizer never produces as a single token.
    let module = check_ok(
        "
        type Bits = { n: i32 };
        op !(a: Bits) -> bool = a.n == 0;
        op ~(a: Bits) -> Bits = Bits.{ n: ~a.n };
        op <<(a: Bits, by: i32) -> Bits = Bits.{ n: a.n << by };
        op >>(a: Bits, by: i32) -> Bits = Bits.{ n: a.n >> by };
        fn shifted(a: Bits) -> Bits = a << 1;
        fn main() = {};
        ",
    );
    let bits = user("Bits", vec![]);

    func_sig(&module, "!", vec![bits.clone()], Type::Bool);
    func_sig(&module, "~", vec![bits.clone()], bits.clone());
    func_sig(&module, ">>", vec![bits.clone(), Type::I32], bits.clone());

    // And a use of one resolves to it rather than to any builtin.
    let shifted = func_sig(&module, "shifted", vec![bits.clone()], bits.clone());
    assert_eq!(
        resolved_call_name(&shifted.body),
        m("<<", vec![bits.clone(), Type::I32], bits)
    );
}

// --------------------------------------------------------------------------
// `@cast`
//
// A bit-cast between any two types that have bits. Its type is the one written
// into it, so there is nothing to infer, and it says nothing whatsoever about
// its operand's type. Neither side may be `void`.
// --------------------------------------------------------------------------

/// The target type and operand of a `@cast`.
fn cast_parts(expr: &Expr) -> (&Type, &Expr) {
    match &expr.kind {
        ExprKind::Cast(target, value) => (target, value),
        other => panic!("expected a cast expression, got {other:?}"),
    }
}

/// The query and the type asked about of a `@sizeof` / `@alignof`.
fn type_info_parts(expr: &Expr) -> (TypeQuery, &Type) {
    match &expr.kind {
        ExprKind::TypeInfo(query, ty) => (*query, ty),
        other => panic!("expected a `@sizeof` / `@alignof` expression, got {other:?}"),
    }
}

#[test]
fn a_cast_yields_the_type_it_names() {
    let module = check_ok("fn main() -> u8 = @cast(u8) 300;");
    let main = func_sig(&module, "main", vec![], Type::U8);
    let (target, value) = cast_parts(&main.body);
    assert_eq!(*target, Type::U8);
    assert_eq!(main.body.ty, Type::U8);
    // The cast constrains its operand not at all, so the literal defaults as it
    // would anywhere else.
    assert_eq!(value.ty, Type::I32);
}

#[test]
fn a_cast_leaves_its_operands_type_alone() {
    let module = check_ok(
        "
        fn narrow(a: i64) -> u8 = @cast(u8) a;
        fn main() = {};
        ",
    );
    let narrow = func_sig(&module, "narrow", vec![Type::I64], Type::U8);
    let (_, value) = cast_parts(&narrow.body);
    assert_eq!(value.ty, Type::I64);
}

#[test]
fn a_cast_binds_tighter_than_a_binary_operator() {
    // `@cast(u8) 1 + 2` == `(@cast(u8) 1) + 2`, as in C.
    let module = check_ok("fn main() -> u8 = @cast(u8) 1 + 2;");
    let main = func_sig(&module, "main", vec![], Type::U8);
    assert_eq!(resolved_call_name(&main.body), op("+", Type::U8, Type::U8));
    let (target, _) = cast_parts(&call_args(&main.body)[0]);
    assert_eq!(*target, Type::U8);
}

#[test]
fn a_cast_can_reinterpret_one_user_type_as_another() {
    // Any two types that have bits, which is the spec's "simple bit-cast".
    // Nothing checks that the two line up -- the size-mismatch diagnostic is a
    // warning, and there is no warning sink yet.
    let module = check_ok(
        "
        type Pair = { a: i32, b: i32 };
        type Wide = { n: i64 };
        fn widen(p: Pair) -> Wide = @cast(Wide) p;
        fn main() = {};
        ",
    );
    let (pair, wide) = (user("Pair", vec![]), user("Wide", vec![]));
    let widen = func_sig(&module, "widen", vec![pair.clone()], wide.clone());
    let (target, value) = cast_parts(&widen.body);
    assert_eq!(*target, wide);
    assert_eq!(value.ty, pair);
}

#[test]
fn a_cast_inside_a_generic_casts_to_the_instantiated_type() {
    let module = check_ok(
        "
        fn bits<T>(a: i32) -> T = @cast(T) a;
        fn main() -> u8 = bits(1);
        ",
    );
    let bits = func_sig(&module, "bits", vec![Type::I32], Type::U8);
    let (target, _) = cast_parts(&bits.body);
    assert_eq!(*target, Type::U8);
    assert_eq!(bits.body.ty, Type::U8);
}

#[test]
fn casting_to_void_is_an_error() {
    // The target is written down, so this one is the parser's to catch.
    let err = parse_err("fn main() = @cast(void) 1;");
    assert!(
        matches!(err, FloErr::TypeHasNoSize { ty: Type::Void, .. }),
        "expected a no-size error, got: {err:?}"
    );
}

#[test]
fn casting_a_void_value_is_an_error() {
    // The operand's type is only known once solving has settled, which is why
    // this side of the same rule is the checker's.
    let errs = check_err(
        "
        fn nothing() = {};
        fn main() -> u8 = @cast(u8) nothing();
        ",
    );
    assert_err!(errs, FloErr::TypeHasNoSize { ty: Type::Void, .. });
}

#[test]
fn casting_to_an_unknown_type_is_an_error() {
    // A cast's target is checked with every other written type.
    let errs = check_err("fn main() -> i32 = @cast(Nope) 1;");
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "Nope");
}

#[test]
fn a_cast_is_not_assignable() {
    let err = parse_err("fn main() = { let a = 1; @cast(u8) a = 2; };");
    assert!(
        matches!(err, FloErr::NotAssignable { .. }),
        "expected a not-assignable error, got: {err:?}"
    );
}

#[test]
fn an_unknown_builtin_is_an_error() {
    let err = parse_err("fn main() -> i32 = @frobnicate(i32);");
    assert!(
        matches!(&err, FloErr::UnknownBuiltin { name, .. } if name == "frobnicate"),
        "expected an unknown-builtin error, got: {err:?}"
    );
}

// --------------------------------------------------------------------------
// `@sizeof` / `@alignof`
//
// Both take a *type*, never a value, and both are u64. What a type's size
// actually is belongs to the target, so nothing here computes one: the front end
// checks the type and records which question was asked.
// --------------------------------------------------------------------------

#[test]
fn sizeof_and_alignof_are_u64() {
    for (builtin, query) in [("sizeof", TypeQuery::Size), ("alignof", TypeQuery::Align)] {
        let src = format!("fn main() -> u64 = @{builtin}(i32);");
        let module = check_ok(&src);
        let main = func_sig(&module, "main", vec![], Type::U64);
        let (found, ty) = type_info_parts(&main.body);
        assert_eq!(found, query);
        assert_eq!(*ty, Type::I32);
        assert_eq!(main.body.ty, Type::U64);
    }
}

#[test]
fn sizeof_is_a_u64_not_an_integer_literal() {
    // It does not coerce the way `0` does: the spec says u64, so u64 it is.
    let errs = check_err("fn main() -> u32 = @sizeof(i32);");
    assert_err!(
        errs,
        FloErr::TypeMismatch {
            expected: Type::U32,
            got: Type::U64,
            ..
        }
    );
}

#[test]
fn sizeof_takes_a_user_type() {
    let module = check_ok(
        "
        type Vec2 = { x: i32, y: i32 };
        fn main() -> u64 = @sizeof(Vec2);
        ",
    );
    let main = func_sig(&module, "main", vec![], Type::U64);
    let (_, ty) = type_info_parts(&main.body);
    assert_eq!(*ty, user("Vec2", vec![]));
}

#[test]
fn sizeof_is_an_operand_like_any_other() {
    let module = check_ok("fn main() -> u64 = @sizeof(i32) + @alignof(i64);");
    let main = func_sig(&module, "main", vec![], Type::U64);
    assert_eq!(
        resolved_call_name(&main.body),
        op("+", Type::U64, Type::U64)
    );
}

#[test]
fn sizeof_inside_a_generic_asks_about_the_instantiated_type() {
    // `T` is taken from the argument: with no turbofish, that is the only place a
    // type argument can come from.
    let module = check_ok(
        "
        fn size<T>(v: T) -> u64 = @sizeof(T);
        fn call(v: u8) -> u64 = size(v);
        fn main() = {};
        ",
    );
    let size = func_sig(&module, "size", vec![Type::U8], Type::U64);
    let (_, ty) = type_info_parts(&size.body);
    assert_eq!(*ty, Type::U8);
}

#[test]
fn sizeof_of_an_unknown_type_is_an_error() {
    let errs = check_err("fn main() -> u64 = @sizeof(Nope);");
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "Nope");
}

#[test]
fn sizeof_of_a_generic_type_needs_its_arguments() {
    // A written type gives its arguments in full; there is no value here for
    // them to be inferred from.
    let errs = check_err(
        "
        type Box<T> = { v: T };
        fn main() -> u64 = @sizeof(Box);
        ",
    );
    assert_err!(errs, FloErr::TypeArityMismatch { name, .. } if name == "Box");
}

#[test]
fn sizeof_of_void_is_an_error() {
    let err = parse_err("fn main() -> u64 = @sizeof(void);");
    assert!(
        matches!(err, FloErr::TypeHasNoSize { ty: Type::Void, .. }),
        "expected a no-size error, got: {err:?}"
    );
}

#[test]
fn sizeof_takes_a_type_and_not_a_value() {
    // So a variable's name in there is read as a type name, and there is no
    // type by that name.
    let errs = check_err("fn main() -> u64 = { let x = 1; @sizeof(x) };");
    assert_err!(errs, FloErr::UnknownType { name, .. } if name == "x");
}
