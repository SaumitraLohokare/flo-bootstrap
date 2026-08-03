//! Lowering tests.
//!
//! These run the whole front end (tokenize -> parse -> type check -> lower) on
//! small Flo programs and assert on the shape of the lowered AST: which calls
//! ended up where, and in what order.
//!
//! Deferred bodies are always calls to single-overload marker functions (`a`,
//! `b`, ...), so a scope's statement list can be summarised as a list of names
//! and compared directly.

use super::lower;
use crate::ast::{Expr, ExprKind, Func, Module};
use crate::parser::Parser;
use crate::tokenizer::Tokenizer;
use crate::type_checker::TypeChecker;
use crate::types::Type;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Marker functions the tests defer calls to. Each is void and takes no
/// arguments, so a call to one is unambiguous and cheap to recognise.
const MARKERS: &str = "
    fn a() = {};
    fn b() = {};
    fn c() = {};
";

/// Run the front end on `src` (with the marker functions appended) and return
/// the lowered module.
fn lowered(src: &str) -> Module {
    let src = format!("{src}{MARKERS}");
    let tokens = Tokenizer::new(&src).tokenize();
    let module = Parser::new(tokens)
        .parse()
        .expect("test source should parse without errors");
    let module = match TypeChecker::new().check(module) {
        Ok(module) => module,
        Err(errs) => panic!("expected type check to succeed, got errors: {errs:?}"),
    };
    lower(module)
}

/// The lowered body of `main`.
fn main_body(src: &str) -> Expr {
    let module = lowered(src);
    let mains = module
        .funcs
        .iter()
        .filter(|(name, _)| name.starts_with("main__"))
        .flat_map(|(_, funcs)| funcs)
        .collect::<Vec<&Func>>();
    match mains[..] {
        [main] => main.body.clone(),
        _ => panic!("expected exactly one `main`, found {}", mains.len()),
    }
}

/// The statements and tail of a scope.
fn scope_parts(expr: &Expr) -> (&[Expr], Option<&Expr>) {
    match &expr.kind {
        ExprKind::Scope(stmts, tail) => (stmts, tail.as_deref()),
        other => panic!("expected a scope expression, got {other:?}"),
    }
}

/// A one-word summary of an expression, enough to tell the interesting shapes
/// apart when comparing a statement list.
///
/// - a call to a marker function is its name (`"a"`)
/// - a `let` of a temporary is `"let"`, a read of one is `"var"`
/// - the exits are `"return"` / `"break"` / `"continue"`
/// - a nested scope is `"{...}"`, and anything else is its variant name
fn tag(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Call(name, ..) => name.clone(),
        ExprKind::Let(..) => "let".to_string(),
        ExprKind::Var(..) => "var".to_string(),
        ExprKind::Return(..) => "return".to_string(),
        ExprKind::Break => "break".to_string(),
        ExprKind::Continue => "continue".to_string(),
        ExprKind::Scope(..) => "{...}".to_string(),
        ExprKind::If(..) => "if".to_string(),
        ExprKind::While(..) => "while".to_string(),
        ExprKind::Assign(..) => "=".to_string(),
        ExprKind::Num(n) => n.to_string(),
        other => format!("{other:?}"),
    }
}

/// The statements of a scope, tagged, with its tail (if any) tagged and marked
/// with a leading `=` so the two are never confused.
fn tags(expr: &Expr) -> Vec<String> {
    let (stmts, tail) = scope_parts(expr);
    let mut out = stmts.iter().map(tag).collect::<Vec<_>>();
    if let Some(tail) = tail {
        out.push(format!("={}", tag(tail)));
    }
    out
}

/// Assert a scope's tagged contents, so tests read as the statement list they
/// expect the lowered scope to have.
macro_rules! assert_scope {
    ($expr:expr, [$($tag:literal),* $(,)?]) => {{
        let expr = &$expr;
        let expected: Vec<String> = vec![$($tag.to_string()),*];
        assert_eq!(
            tags(expr),
            expected,
            "unexpected scope contents; full expression: {expr:?}"
        );
    }};
}

/// Every `Defer` node must be gone once lowering has run.
fn assert_no_defers(expr: &Expr) {
    use ExprKind::*;

    assert!(
        !matches!(expr.kind, Defer(_)),
        "a `defer` survived lowering: {expr:?}"
    );

    match &expr.kind {
        Call(_, args, _) => args.iter().for_each(assert_no_defers),
        Scope(stmts, tail) => {
            stmts.iter().for_each(assert_no_defers);
            tail.iter().for_each(|t| assert_no_defers(t));
        }
        If(cond, then, otherwise) => {
            assert_no_defers(cond);
            assert_no_defers(then);
            otherwise.iter().for_each(|e| assert_no_defers(e));
        }
        While(cond, body) => {
            assert_no_defers(cond);
            assert_no_defers(body);
        }
        Return(value) => value.iter().for_each(|e| assert_no_defers(e)),
        Let(_, _, init) => init.iter().for_each(|e| assert_no_defers(e)),
        Assign(target, value) => {
            assert_no_defers(target);
            assert_no_defers(value);
        }
        Defer(body) => assert_no_defers(body),
        BuiltinOp(_) | Num(_) | Flt(_) | Bool(_) | Var(_) | Break | Continue => {}
    }
}

/// The condition and body of a while expression.
fn while_parts(expr: &Expr) -> (&Expr, &Expr) {
    match &expr.kind {
        ExprKind::While(cond, body) => (cond, body),
        other => panic!("expected a while expression, got {other:?}"),
    }
}

/// The branches of an if expression.
fn if_parts(expr: &Expr) -> (&Expr, Option<&Expr>) {
    match &expr.kind {
        ExprKind::If(_, then, otherwise) => (then, otherwise.as_deref()),
        other => panic!("expected an if expression, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Falling off the end of a scope
// --------------------------------------------------------------------------

#[test]
fn defer_moves_to_the_end_of_its_scope() {
    let body = main_body("fn main() = { defer a(); b(); };");
    assert_no_defers(&body);
    assert_scope!(body, ["b", "a"]);
}

#[test]
fn defers_run_in_reverse_order() {
    // LIFO: the last one registered is the first one to run.
    let body = main_body("fn main() = { defer a(); defer b(); defer c(); };");
    assert_scope!(body, ["c", "b", "a"]);
}

#[test]
fn a_defer_alone_in_a_scope_is_all_that_is_left() {
    let body = main_body("fn main() = { defer a(); };");
    assert_scope!(body, ["a"]);
}

#[test]
fn defer_in_tail_position_still_runs() {
    // `{ defer a() }` — no semicolon, so the `defer` is the tail. It is `void`,
    // so the scope stays `void` and the deferred call is all that remains.
    let body = main_body("fn main() = { defer a() };");
    assert_eq!(body.ty, Type::Void);
    assert_scope!(body, ["a"]);
}

#[test]
fn defer_on_a_braceless_function_body_wraps_it_in_a_scope() {
    let body = main_body("fn main() = defer a();");
    assert_eq!(body.ty, Type::Void);
    assert_scope!(body, ["a"]);
}

#[test]
fn a_function_without_defers_keeps_its_shape() {
    // Lowering must be the identity on a body with nothing to move, including
    // not wrapping a braceless body in a block.
    let body = main_body("fn main() = a();");
    assert_eq!(tag(&body), "a");
}

#[test]
fn nested_scopes_keep_their_own_defers() {
    let body = main_body("fn main() = { defer a(); { defer b(); c(); }; };");
    assert_scope!(body, ["{...}", "a"]);

    let (stmts, _) = scope_parts(&body);
    assert_scope!(stmts[0], ["c", "b"]);
}

// --------------------------------------------------------------------------
// Tail values
// --------------------------------------------------------------------------

#[test]
fn a_scope_tail_is_evaluated_before_the_defers_run() {
    // The tail is stashed in a temporary so the scope still yields it, but the
    // deferred call happens after it is computed.
    let body = main_body("fn main() -> i32 = { defer a(); 1 };");
    assert_eq!(body.ty, Type::I32);
    assert_scope!(body, ["let", "a", "=var"]);

    let (stmts, tail) = scope_parts(&body);
    let ExprKind::Let(let_id, let_ty, Some(init)) = &stmts[0].kind else {
        unreachable!("expected the temporary's declaration")
    };
    assert_eq!(*let_ty, Type::I32);
    assert!(matches!(init.kind, ExprKind::Num(1)));

    let ExprKind::Var(var_id) = tail.unwrap().kind else {
        unreachable!("expected the temporary to be read back")
    };
    assert_eq!(var_id, *let_id, "the tail must read the stashed value");
    assert_eq!(tail.unwrap().ty, Type::I32);
}

#[test]
fn a_defer_cannot_change_the_value_a_scope_yields() {
    // `x` is read into the temporary before the deferred assignment runs, so
    // the scope still yields the old value.
    let body = main_body("fn main() -> i32 = { let x = 1; defer x = x + 1; x };");
    assert_scope!(body, ["let", "let", "=", "=var"]);
}

#[test]
fn a_void_tail_needs_no_temporary() {
    // Nothing to hand back, so the tail just becomes another statement.
    let body = main_body("fn main() = { defer a(); b() };");
    assert_eq!(body.ty, Type::Void);
    assert_scope!(body, ["b", "a"]);
}

#[test]
fn temporaries_do_not_collide_with_source_variables() {
    let module = lowered("fn main() -> i32 = { let x = 1; defer a(); x };");
    let main = module
        .funcs
        .iter()
        .find(|(name, _)| name.starts_with("main__"))
        .map(|(_, funcs)| &funcs[0])
        .unwrap();

    let (stmts, tail) = scope_parts(&main.body);
    let ExprKind::Let(x_id, ..) = stmts[0].kind else {
        unreachable!("expected `let x`")
    };
    let ExprKind::Let(tmp_id, ..) = stmts[1].kind else {
        unreachable!("expected the temporary's declaration")
    };
    assert_ne!(x_id, tmp_id);
    assert!(matches!(tail.unwrap().kind, ExprKind::Var(id) if id == tmp_id));
    assert!(
        module.var_count > tmp_id,
        "the module's variable count must cover the temporaries lowering minted"
    );
}

// --------------------------------------------------------------------------
// return
// --------------------------------------------------------------------------

#[test]
fn return_runs_the_defers_registered_above_it() {
    let body = main_body("fn main() = { defer a(); return; };");
    // The `return` is replaced by a block that runs the defers first. No copy is
    // left at the end of the scope: nothing reaches it.
    assert_scope!(body, ["{...}"]);

    let (stmts, _) = scope_parts(&body);
    assert_scope!(stmts[0], ["a", "=return"]);
}

#[test]
fn return_stashes_its_value_before_running_defers() {
    let body = main_body("fn main() -> i32 = { defer a(); return 1; };");
    let (stmts, _) = scope_parts(&body);
    assert_scope!(stmts[0], ["let", "a", "=return"]);

    // The `return` hands back the temporary, not the original expression.
    let (inner_stmts, inner_tail) = scope_parts(&stmts[0]);
    let ExprKind::Let(let_id, ..) = inner_stmts[0].kind else {
        unreachable!("expected the temporary's declaration")
    };
    let ExprKind::Return(Some(value)) = &inner_tail.unwrap().kind else {
        unreachable!("expected a return with a value")
    };
    assert!(matches!(value.kind, ExprKind::Var(id) if id == let_id));
}

#[test]
fn a_defer_below_a_return_does_not_run_at_it() {
    // Registration is positional: the `return` is above the `defer`, so it has
    // nothing to run and is left exactly as written. The `defer` itself is
    // unreachable — control has already left — so it disappears entirely.
    let body = main_body("fn main() = { return; defer a(); };");
    assert_scope!(body, ["return"]);
}

#[test]
fn return_runs_the_defers_of_every_enclosing_scope() {
    // Innermost scope first, and within a scope last-registered first.
    let body = main_body("fn main() = { defer a(); { defer b(); defer c(); return; }; };");
    let (stmts, _) = scope_parts(&body);
    assert_scope!(stmts[0], ["{...}"]);

    let (inner, _) = scope_parts(&stmts[0]);
    assert_scope!(inner[0], ["c", "b", "a", "=return"]);
}

#[test]
fn a_return_inside_a_branch_still_runs_the_outer_defers() {
    let body = main_body("fn main(f: bool) = { defer a(); if f { return; }; b(); };");
    assert_scope!(body, ["if", "b", "a"]);

    let (stmts, _) = scope_parts(&body);
    let (then, _) = if_parts(&stmts[0]);
    assert_scope!(then, ["{...}"]);

    let (then_stmts, _) = scope_parts(then);
    assert_scope!(then_stmts[0], ["a", "=return"]);
}

// --------------------------------------------------------------------------
// Loops
// --------------------------------------------------------------------------

#[test]
fn defer_in_a_loop_body_runs_on_every_iteration() {
    // The loop body is a scope of its own, so the deferred call stays inside it.
    let body = main_body("fn main(f: bool) = { while f { defer a(); b(); }; c(); };");
    assert_scope!(body, ["while", "c"]);

    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    assert_scope!(loop_body, ["b", "a"]);
}

#[test]
fn defer_on_a_braceless_loop_body_still_runs_per_iteration() {
    let body = main_body("fn main(f: bool) = { while f defer a(); };");
    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    assert_scope!(loop_body, ["a"]);
}

#[test]
fn break_runs_the_loop_body_defers() {
    let body = main_body("fn main(f: bool) = { while f { defer a(); break; }; };");
    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    assert_scope!(loop_body, ["{...}"]);

    let (body_stmts, _) = scope_parts(loop_body);
    assert_scope!(body_stmts[0], ["a", "=break"]);
}

#[test]
fn continue_runs_the_loop_body_defers() {
    let body = main_body("fn main(f: bool) = { while f { defer a(); continue; }; };");
    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    let (body_stmts, _) = scope_parts(loop_body);
    assert_scope!(body_stmts[0], ["a", "=continue"]);
}

#[test]
fn break_does_not_run_defers_from_outside_the_loop() {
    // `a` belongs to the function body, which `break` does not leave; `b`
    // belongs to the loop body, which it does.
    let body = main_body("fn main(f: bool) = { defer a(); while f { defer b(); break; }; };");
    assert_scope!(body, ["while", "a"]);

    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    let (body_stmts, _) = scope_parts(loop_body);
    assert_scope!(body_stmts[0], ["b", "=break"]);
}

#[test]
fn break_runs_the_defers_of_scopes_nested_in_the_loop_body() {
    let body = main_body("fn main(f: bool) = { while f { defer a(); { defer b(); break; }; }; };");
    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    let (body_stmts, _) = scope_parts(loop_body);

    let (inner, _) = scope_parts(&body_stmts[0]);
    assert_scope!(inner[0], ["b", "a", "=break"]);
}

#[test]
fn return_from_a_loop_runs_the_loop_and_function_defers() {
    let body = main_body("fn main(f: bool) = { defer a(); while f { defer b(); return; }; };");
    let (stmts, _) = scope_parts(&body);
    let (_, loop_body) = while_parts(&stmts[0]);
    let (body_stmts, _) = scope_parts(loop_body);
    assert_scope!(body_stmts[0], ["b", "a", "=return"]);
}

#[test]
fn a_loop_condition_belongs_to_the_enclosing_scope() {
    // The condition is evaluated before each check, outside the body, so a
    // `defer` written in it attaches to the scope holding the loop.
    let body = main_body("fn main() = { while { defer a(); false } {}; b(); };");
    assert_scope!(body, ["while", "b"]);

    let (stmts, _) = scope_parts(&body);
    let (cond, _) = while_parts(&stmts[0]);
    assert_scope!(cond, ["let", "a", "=var"]);
}

// --------------------------------------------------------------------------
// Branches
// --------------------------------------------------------------------------

#[test]
fn defer_in_a_branch_stays_in_that_branch() {
    let body = main_body("fn main(f: bool) = { if f { defer a(); b(); } else { defer c(); }; };");
    let (stmts, _) = scope_parts(&body);
    let (then, otherwise) = if_parts(&stmts[0]);
    assert_scope!(then, ["b", "a"]);
    assert_scope!(otherwise.unwrap(), ["c"]);
}

#[test]
fn defer_on_a_braceless_branch_is_not_hoisted_out_of_it() {
    // A branch is a scope whether or not it is written with braces, so `a` runs
    // only when the branch is taken.
    let body = main_body("fn main(f: bool) = { if f defer a(); b(); };");
    assert_scope!(body, ["if", "b"]);

    let (stmts, _) = scope_parts(&body);
    let (then, _) = if_parts(&stmts[0]);
    assert_scope!(then, ["a"]);
}

#[test]
fn a_branch_that_yields_a_value_keeps_yielding_it() {
    let body = main_body("fn main(f: bool) -> i32 = { if f { defer a(); 1 } else { 2 } };");
    assert_eq!(body.ty, Type::I32);

    let (_, tail) = scope_parts(&body);
    let (then, otherwise) = if_parts(tail.unwrap());
    assert_eq!(then.ty, Type::I32);
    assert_scope!(then, ["let", "a", "=var"]);
    assert_scope!(otherwise.unwrap(), ["=2"]);
}

// --------------------------------------------------------------------------
// Odds and ends
// --------------------------------------------------------------------------

#[test]
fn a_deferred_body_may_be_any_expression() {
    // Not just a call: the body's value is discarded wherever it ends up.
    let body = main_body("fn main() -> i32 = { let x = 0; defer x = x + 1; x };");
    assert_no_defers(&body);
    assert_scope!(body, ["let", "let", "=", "=var"]);
}

#[test]
fn a_deferred_value_is_discarded() {
    // `id` yields an i32; deferring it is fine, the value just goes nowhere.
    let module = lowered(
        "
        fn main() = { defer id(1); };
        fn id(n: i32) -> i32 = n;
        ",
    );
    let main = module
        .funcs
        .iter()
        .find(|(name, _)| name.starts_with("main__"))
        .map(|(_, funcs)| &funcs[0])
        .unwrap();
    assert_eq!(main.body.ty, Type::Void);
    assert_scope!(main.body, ["id"]);
}

#[test]
fn a_deferred_body_can_itself_hold_a_scope_with_defers() {
    let body = main_body("fn main() = { defer { defer a(); b(); }; c(); };");
    assert_scope!(body, ["c", "{...}"]);

    let (stmts, _) = scope_parts(&body);
    assert_scope!(stmts[1], ["b", "a"]);
}
