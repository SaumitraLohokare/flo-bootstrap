use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    type_checker::replace_set::ReplaceSet,
    types::Type,
};

mod replace_set;

#[cfg(test)]
mod tests;

/// A fact collected from the AST. Collection is a single pass that never
/// consults the solver, so everything the checker knows about a function is in
/// one flat list before any of it is solved.
#[derive(Debug)]
enum Constraint {
    /// The two types must unify.
    IsEqual(Type, Type, Loc),

    /// The type variable belongs to an expression that diverges. Kept separate
    /// from `IsEqual` because `NoReturn` is deliberately *not* propagated
    /// through unification (see [`TypeChecker::solve_constraint`]) — divergence
    /// is pinned onto a composite's own variable and nowhere else.
    Diverges(Type, Loc),

    /// A call whose overload has not been pinned down yet.
    Call(CallConstraint),
}

#[derive(Debug)]
struct CallConstraint {
    /// Raw type-var id of the call expression. Every `Call` gets a fresh type
    /// var from the parser and nothing clones an `Expr` before type checking,
    /// so this uniquely identifies the call site: it is the key the chosen
    /// overload is recorded under, and `T(key)` is the call's return type.
    key: usize,
    name: String,
    args: Vec<(Type, Loc)>,
    loc: Loc,
    /// Indices into `func_types[name]` that are still compatible. `None` until
    /// the first solver round so that an undefined function is reported while
    /// solving rather than while collecting — that keeps a type mismatch
    /// anywhere in the function winning over an undefined call, as before.
    cands: Option<Vec<usize>>,
}

pub struct TypeChecker {
    func_types: HashMap<String, Vec<Type>>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            func_types: HashMap::new(),
        }
    }

    pub fn check(mut self, module: Module) -> Result<Module, Vec<FloErr>> {
        for (name, funcs) in &module.funcs {
            self.func_types.insert(
                name.clone(),
                funcs.iter().map(|f| f.ty.clone()).collect::<Vec<_>>(),
            );
        }

        let mut errs = Vec::new();
        let mut new_funcs: HashMap<String, Vec<Func>> = HashMap::new();
        for (name, funcs) in &module.funcs {
            for func in funcs {
                match self.check_func(func) {
                    Ok(func) => {
                        let mangled_name = mangle_name(&func.ty, name);
                        if !new_funcs.contains_key(&mangled_name) {
                            new_funcs.entry(mangled_name).or_default().push(func);
                        } else {
                            let previous_loc = new_funcs.get(&mangled_name).unwrap()[0].loc;
                            errs.push(FloErr::AmbiguousOverload {
                                name: name.clone(),
                                found_loc: func.loc,
                                previous_loc,
                            });
                        }
                    }
                    Err(err) => errs.push(err),
                }
            }
        }

        if errs.is_empty() {
            Ok(Module {
                funcs: new_funcs,
                var_count: module.var_count,
            })
        } else {
            Err(errs)
        }
    }

    fn check_func(&self, func: &Func) -> FloResult<Func> {
        // 1. Collect every constraint in a single AST pass.

        let mut constraints = Vec::new();
        self.collect_func_constraints(func, &mut constraints);

        // 2. Solve them, resolving calls to a fixpoint.

        let (mut set, resolutions) = self.solve(constraints)?;

        // 3. Rebuild the func with concrete types and resolved call names.

        func.resolve(&mut set, &resolutions)
    }

    // ----------------------------------------------------------------------
    // Collection
    // ----------------------------------------------------------------------

    fn collect_func_constraints(&self, func: &Func, out: &mut Vec<Constraint>) {
        // Constraints for argument types are not added, because they're already
        // concrete types
        let Type::Fn(_arg_tys, ret_ty) = &func.ty else {
            unreachable!()
        };

        out.push(Constraint::IsEqual(
            *ret_ty.clone(),
            func.body.ty.clone(),
            func.body.loc,
        ));

        self.collect_expr_constraints(&func.body, ret_ty.as_ref(), out);
    }

    /// `ret_ty` is the enclosing function's return type, needed to constrain the
    /// operand of any `return` expression that appears in the body.
    ///
    /// Children are visited before the constraints that mention them, so the
    /// list can be solved in order in one pass: by the time a parent's
    /// constraint is reached, a diverging child has already been pinned to
    /// `NoReturn` and the parent's constraint correctly becomes a no-op.
    fn collect_expr_constraints(&self, expr: &Expr, ret_ty: &Type, out: &mut Vec<Constraint>) {
        use ExprKind::*;
        use Type::*;

        match &expr.kind {
            Num(_) => out.push(Constraint::IsEqual(Integer, expr.ty.clone(), expr.loc)),
            Flt(_) => out.push(Constraint::IsEqual(Decimal, expr.ty.clone(), expr.loc)),
            ExprKind::Bool(_) => {
                out.push(Constraint::IsEqual(Type::Bool, expr.ty.clone(), expr.loc))
            }
            BuiltinOp(_) | Var(_) => {}
            Call(name, arg_exprs, _) => {
                for arg_expr in arg_exprs {
                    self.collect_expr_constraints(arg_expr, ret_ty, out);
                }

                let Type::T(key) = expr.ty else {
                    unreachable!("call expression without a fresh type var")
                };

                // Pushed after the arguments' constraints, so the pending-call
                // list ends up in post-order and the innermost unresolvable
                // call (the root cause) is the first one reported.
                out.push(Constraint::Call(CallConstraint {
                    key,
                    name: name.clone(),
                    args: arg_exprs
                        .iter()
                        .map(|arg| (arg.ty.clone(), arg.loc))
                        .collect(),
                    loc: expr.loc,
                    cands: None,
                }));
            }
            Scope(stmts, tail) => {
                for stmt in stmts {
                    self.collect_expr_constraints(stmt, ret_ty, out);
                }
                if let Some(tail) = tail {
                    self.collect_expr_constraints(tail, ret_ty, out);
                }

                // A scope diverges if any statement diverges or its tail does.
                // Statement divergence (a `return` before the tail) makes the
                // whole scope NoReturn even though the tail is dead code.
                if diverges(expr) {
                    out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                } else if let Some(tail) = tail {
                    out.push(Constraint::IsEqual(
                        tail.ty.clone(),
                        expr.ty.clone(),
                        expr.loc,
                    ));
                } else {
                    // No tail and no divergence => an empty/`;`-terminated scope
                    // is void.
                    out.push(Constraint::IsEqual(Void, expr.ty.clone(), expr.loc));
                }
            }
            If(cond, then, otherwise) => {
                self.collect_expr_constraints(cond, ret_ty, out);
                out.push(Constraint::IsEqual(Type::Bool, cond.ty.clone(), cond.loc));

                self.collect_expr_constraints(then, ret_ty, out);
                // The `if`'s own type is the join of its branches. A NoReturn
                // branch is absorbed: the constraint below is a no-op for it (see
                // `solve_constraint`), so the `if` takes the other branch's type.
                out.push(Constraint::IsEqual(
                    expr.ty.clone(),
                    then.ty.clone(),
                    then.loc,
                ));

                if let Some(otherwise) = otherwise {
                    self.collect_expr_constraints(otherwise, ret_ty, out);
                    out.push(Constraint::IsEqual(
                        expr.ty.clone(),
                        otherwise.ty.clone(),
                        otherwise.loc,
                    ));

                    // If BOTH branches diverge the whole `if` diverges. The two
                    // constraints above bound nothing (both were no-ops), so pin
                    // the `if`'s own type to NoReturn explicitly.
                    if diverges(expr) {
                        out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                    }
                } else {
                    // With no `else`, the `if` is void — control may skip `then`
                    // entirely, so a diverging `then` does not make it NoReturn.
                    out.push(Constraint::IsEqual(Void, expr.ty.clone(), expr.loc));
                }
            }
            While(cond, body) => {
                self.collect_expr_constraints(cond, ret_ty, out);
                out.push(Constraint::IsEqual(Type::Bool, cond.ty.clone(), cond.loc));

                self.collect_expr_constraints(body, ret_ty, out);
                // The body's value is discarded, so it has to be void — the same
                // rule an else-less `if` follows. A body that diverges (`break`,
                // `continue`, `return`) is NoReturn, which this constraint accepts.
                out.push(Constraint::IsEqual(Void, body.ty.clone(), body.loc));

                // The loop's own type is already void (set by the parser), and it
                // never diverges: the condition may be false on the first check,
                // so control can always reach the expression after it.
            }
            Break | Continue => {
                // Both are NoReturn (set by the parser) and carry no operand, so
                // there is nothing to constrain. Which loop they belong to was
                // already checked while parsing.
            }
            Return(value) => {
                match value {
                    Some(e) => {
                        self.collect_expr_constraints(e, ret_ty, out);
                        // The returned value must match the function's return type.
                        out.push(Constraint::IsEqual(ret_ty.clone(), e.ty.clone(), e.loc));
                    }
                    None => {
                        // Bare `return` yields void; only valid in a void function.
                        out.push(Constraint::IsEqual(ret_ty.clone(), Void, expr.loc));
                    }
                }
                // The `return` expression's own type is already NoReturn (set by
                // the parser), so it needs no constraint here.
            }
            Let(_, var_ty, init) => {
                if let Some(init) = init {
                    self.collect_expr_constraints(init, ret_ty, out);

                    if diverges(init) {
                        // `let x = return 1;` — nothing ever flows into `x`, so
                        // pin it rather than leaving it unresolvable (the
                        // equality below would be a no-op against NoReturn).
                        out.push(Constraint::Diverges(var_ty.clone(), init.loc));
                    } else {
                        out.push(Constraint::IsEqual(
                            var_ty.clone(),
                            init.ty.clone(),
                            init.loc,
                        ));
                    }
                }

                // `let x;` pushes nothing at all: the variable keeps its fresh
                // type var for a later assignment to bind. If nothing ever does,
                // `resolve` reports it at the declaration.
                //
                // The declaration's own type is `void` (set by the parser), so it
                // needs no constraint either.
            }
            Assign(target, value) => {
                self.collect_expr_constraints(target, ret_ty, out);
                self.collect_expr_constraints(value, ret_ty, out);

                if diverges(value) {
                    out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                } else {
                    out.push(Constraint::IsEqual(
                        target.ty.clone(),
                        value.ty.clone(),
                        expr.loc,
                    ));
                    // An assignment yields the value it stored, like C.
                    out.push(Constraint::IsEqual(
                        expr.ty.clone(),
                        target.ty.clone(),
                        expr.loc,
                    ));
                }
            }
            Defer(body) => {
                // The body still has to check on its own, but nothing constrains
                // its type: wherever it ends up running its value is discarded,
                // exactly as a statement's is.
                self.collect_expr_constraints(body, ret_ty, out);

                // The `defer` itself is `void` (set by the parser), so it needs
                // no constraint either.
            }
        }
    }

    // ----------------------------------------------------------------------
    // Solving
    // ----------------------------------------------------------------------

    /// Solve the collected constraints, returning the bindings and the overload
    /// chosen for each call site.
    ///
    /// Equality and divergence constraints are solved exactly once each, in
    /// collection order: a union-find solve is confluent, so re-running them
    /// would be wasted work — and the `NoReturn` short-circuit in
    /// `solve_constraint` reads solver state, so re-running would not even be a
    /// no-op. Only calls need iterating, because committing one binds type
    /// variables that can narrow another call's overload set.
    fn solve(
        &self,
        constraints: Vec<Constraint>,
    ) -> FloResult<(ReplaceSet, HashMap<usize, String>)> {
        let mut set = ReplaceSet::new();
        let mut resolutions = HashMap::new();
        let mut pending: Vec<CallConstraint> = Vec::new();

        for constraint in constraints {
            match constraint {
                Constraint::IsEqual(t1, t2, loc) => self.solve_constraint(&mut set, t1, t2, loc)?,
                Constraint::Diverges(ty, loc) => mark_never(&ty, &mut set, loc)?,
                Constraint::Call(call) => {
                    debug_assert!(
                        !pending.iter().any(|c| c.key == call.key),
                        "two call sites share type var 't{}",
                        call.key
                    );
                    pending.push(call);
                }
            }
        }

        // Resolve calls to a fixpoint (before defaulting)
        self.resolve_calls(&mut pending, &mut set, &mut resolutions)?;

        // Default types ({integer} => i32)
        set.default_types();

        // Resolve calls to a fixpoint again (defaulting may have unblocked
        // calls that were ambiguous before)
        self.resolve_calls(&mut pending, &mut set, &mut resolutions)?;

        // Any call still pending is now a genuine error. The list is in
        // post-order, so the first one is the innermost.
        if let Some(call) = pending.first() {
            Err(self.overload_error(call, &mut set))?
        }

        Ok((set, resolutions))
    }

    /// Repeatedly attempt to commit every pending call until a full round
    /// commits nothing new. Committing one call binds type variables, which can
    /// unblock its parent (via the argument types) or its children (via the
    /// return type), so no single order works — we iterate until the worklist
    /// stops shrinking.
    fn resolve_calls(
        &self,
        pending: &mut Vec<CallConstraint>,
        set: &mut ReplaceSet,
        resolutions: &mut HashMap<usize, String>,
    ) -> FloResult<()> {
        loop {
            let mut progress = false;
            let mut unresolved = Vec::with_capacity(pending.len());

            for mut call in std::mem::take(pending) {
                match self.try_resolve_call(&mut call, set)? {
                    Some(mangled) => {
                        resolutions.insert(call.key, mangled);
                        progress = true;
                    }
                    None => unresolved.push(call),
                }
            }
            *pending = unresolved;

            if !progress {
                return Ok(());
            }
        }
    }

    /// Narrow the call's overload set against the current bindings and commit if
    /// exactly one candidate survives; 0 or 2+ leaves it pending for a later
    /// round (a neighbouring call may resolve and narrow it down). This never
    /// reports an overload error — that happens once the fixpoint has settled.
    fn try_resolve_call(
        &self,
        call: &mut CallConstraint,
        set: &mut ReplaceSet,
    ) -> FloResult<Option<String>> {
        let overloads =
            self.func_types
                .get(&call.name)
                .ok_or_else(|| FloErr::UndefinedFunction {
                    name: call.name.clone(),
                    loc: call.loc,
                })?;

        // Pruning is monotone: `satisfies_type` only ever goes true -> false as
        // bindings accumulate, so a candidate dropped here can never become
        // viable again and the set only has to be filtered against what changed.
        let cands = call
            .cands
            .get_or_insert_with(|| (0..overloads.len()).collect());
        prune(cands, overloads, &call.args, &Type::T(call.key), set);

        // Only commit when EXACTLY one overload survives.
        let [idx] = cands[..] else {
            return Ok(None);
        };

        let fn_ty = &overloads[idx];
        let Type::Fn(params, ret) = fn_ty else {
            unreachable!()
        };

        // Feeding the chosen signature back in as ordinary equality constraints
        // is all the pinning that's needed: binding a variable already bound to
        // {integer} joins the two and yields the concrete param type.
        for ((arg_ty, arg_loc), param) in call.args.iter().zip(params) {
            assert!(param.is_known());
            self.solve_constraint(set, param.clone(), arg_ty.clone(), *arg_loc)?;
        }

        assert!(ret.is_known());
        self.solve_constraint(set, *ret.clone(), Type::T(call.key), call.loc)?;

        Ok(Some(mangle_name(fn_ty, &call.name)))
    }

    /// Turn a call that survived the fixpoint unresolved into an error.
    fn overload_error(&self, call: &CallConstraint, set: &mut ReplaceSet) -> FloErr {
        let cands = call.cands.as_deref().unwrap_or(&[]);

        if cands.is_empty() {
            let arg_tys = call.args.iter().map(|(ty, _)| set.resolve(ty)).collect();
            let known_ty = Type::Fn(arg_tys, Box::new(set.resolve(&Type::T(call.key))));
            FloErr::NoPossibleOverloads {
                name: call.name.clone(),
                known_ty,
                loc: call.loc,
            }
        } else {
            let overloads = &self.func_types[&call.name];
            let possible_tys = cands
                .iter()
                .map(|&i| set.resolve(&overloads[i]))
                .collect::<Vec<Type>>();
            FloErr::MultiplePossibleOverloads {
                name: call.name.clone(),
                possible_tys,
                loc: call.loc,
            }
        }
    }

    fn solve_constraint(
        &self,
        set: &mut ReplaceSet,
        t1: Type,
        t2: Type,
        loc: Loc,
    ) -> FloResult<()> {
        use Type::*;

        // NoReturn satisfies any binding: a constraint touching it is vacuously
        // solved. Crucially it never binds a variable to NoReturn — divergence is
        // propagated structurally (via `Constraint::Diverges`), not through
        // unification, so a diverging expression cannot poison a neighbour's
        // type variable.
        if matches!(set.resolve(&t1), Never) || matches!(set.resolve(&t2), Never) {
            return Ok(());
        }

        match (t1, t2) {
            (T(a), T(b)) => set.unify(a, b, loc)?,
            (T(id), ty) | (ty, T(id)) => set.bind(id, ty, loc)?,
            (t1, t2) if t1 != t2 => Err(FloErr::TypeMismatch {
                expected: t1,
                got: t2,
                loc,
            })?,
            _ => {}
        }

        Ok(())
    }
}

/// Drop every overload that the current bindings rule out.
fn prune(
    cands: &mut Vec<usize>,
    overloads: &[Type],
    args: &[(Type, Loc)],
    ret_ty: &Type,
    set: &mut ReplaceSet,
) {
    cands.retain(|&i| {
        let Type::Fn(params, ret) = &overloads[i] else {
            unreachable!()
        };

        // Arity mismatch => prune overload
        if params.len() != args.len() {
            return false;
        }

        let mut should_keep = true;
        // Ensure all arg types satisfy
        for ((arg_ty, _), param) in args.iter().zip(params) {
            should_keep &= set.resolve(arg_ty).satisfies_type(param);
        }
        // Ensure return type satisfies
        should_keep & set.resolve(ret_ty).satisfies_type(ret)
    });
}

/// Whether an expression diverges (never yields a value), determined purely
/// from its shape.
///
/// `NoReturn` only ever enters the solver through a `Constraint::Diverges`,
/// which is emitted for exactly the expressions this returns true for, so this
/// gives the same answer the old solver-consulting check did — without needing
/// the solver to have run first.
pub fn diverges(expr: &Expr) -> bool {
    use ExprKind::*;

    match &expr.kind {
        // The parser types `return`, `break` and `continue` as NoReturn directly.
        Return(_) | Break | Continue => true,
        Scope(stmts, tail) => stmts.iter().any(diverges) || tail.as_deref().is_some_and(diverges),
        // Both branches must diverge; with no `else` control can skip `then`.
        If(_, then, Some(otherwise)) => diverges(then) && diverges(otherwise),
        // A loop never diverges, however its body is written: the condition may
        // be false on the first check, so control always reaches what follows.
        // Spotting that `while true` cannot exit would need real flow analysis.
        While(..) => false,
        Let(_, _, init) => init.as_deref().is_some_and(diverges),
        Assign(target, value) => diverges(target) || diverges(value),
        // A deferred body runs on every path out of its scope, so a scope
        // holding a diverging `defer` cannot be left normally either. Saying so
        // here keeps the type the checker gives a scope equal to the type its
        // lowered form would get.
        Defer(body) => diverges(body),
        _ => false,
    }
}

/// Pin a composite expression's own (always fresh) type variable to NoReturn
/// when it is determined to diverge. Only the composite's own variable is
/// touched, so a diverging branch or statement never poisons its siblings.
fn mark_never(ty: &Type, set: &mut ReplaceSet, loc: Loc) -> FloResult<()> {
    match ty {
        Type::T(id) => set.bind(*id, Type::Never, loc),
        _ => Ok(()),
    }
}

fn mangle_name(fn_ty: &Type, name: &str) -> String {
    let Type::Fn(args, ret) = fn_ty else {
        unreachable!()
    };

    let arg_list = args
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join("_");

    format!("{name}__{arg_list}__{ret:?}")
}

impl Func {
    fn resolve(&self, set: &mut ReplaceSet, res: &HashMap<usize, String>) -> FloResult<Self> {
        let ty = set.resolve(&self.ty);
        if ty.is_known() {
            Ok(Func {
                body: self.body.resolve(set, res)?,
                ty,
                loc: self.loc.clone(),
            })
        } else {
            Err(FloErr::UnresolvedType { ty, loc: self.loc })
        }
    }
}

impl Expr {
    fn resolve(&self, set: &mut ReplaceSet, res: &HashMap<usize, String>) -> FloResult<Self> {
        use ExprKind::*;

        let ty = set.resolve(&self.ty);
        let loc = self.loc;

        if !ty.is_known() {
            Err(FloErr::UnresolvedType {
                ty: ty.clone(),
                loc,
            })?
        }

        let kind = match &self.kind {
            Call(name, args, _) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(arg.resolve(set, res)?);
                }

                // NOTE: `self.ty`, not the resolved `ty` above — the call site's
                // identity is its *raw* type var, which `ty` has replaced with a
                // concrete type by now.
                let Type::T(key) = self.ty else {
                    unreachable!("call expression without a fresh type var")
                };
                let resolved = res.get(&key).cloned();
                assert!(
                    resolved.is_some(),
                    "Unresolved call {name} (args: {args:?})"
                );

                Call(name.clone(), new_args, resolved)
            }
            Scope(exprs, tail) => {
                let mut new_exprs = Vec::new();
                for expr in exprs {
                    new_exprs.push(expr.resolve(set, res)?);
                }

                let new_tail = match tail {
                    Some(e) => Some(Box::new(e.resolve(set, res)?)),
                    None => None,
                };
                Scope(new_exprs, new_tail)
            }
            If(cond, then, otherwise) => {
                let cond = Box::new(cond.resolve(set, res)?);
                let then = Box::new(then.resolve(set, res)?);
                let otherwise = match otherwise {
                    Some(e) => Some(Box::new(e.resolve(set, res)?)),
                    None => None,
                };
                If(cond, then, otherwise)
            }
            While(cond, body) => While(
                Box::new(cond.resolve(set, res)?),
                Box::new(body.resolve(set, res)?),
            ),
            Return(value) => {
                let new_value = match value {
                    Some(e) => Some(Box::new(e.resolve(set, res)?)),
                    None => None,
                };
                Return(new_value)
            }
            Let(id, var_ty, init) => {
                // The declaration is `void`, so the check at the top of this
                // function says nothing about the variable's own type. An
                // unresolved one is reported here, at the declaration.
                let var_ty = set.resolve(var_ty);
                if !var_ty.is_known() {
                    Err(FloErr::UnresolvedType {
                        ty: var_ty.clone(),
                        loc,
                    })?
                }

                let new_init = match init {
                    Some(e) => Some(Box::new(e.resolve(set, res)?)),
                    None => None,
                };
                Let(*id, var_ty, new_init)
            }
            Assign(target, value) => Assign(
                Box::new(target.resolve(set, res)?),
                Box::new(value.resolve(set, res)?),
            ),
            Defer(body) => Defer(Box::new(body.resolve(set, res)?)),

            Num(n) => Num(*n),
            Flt(n) => Flt(*n),
            Bool(n) => Bool(*n),
            Var(v) => Var(*v),
            BuiltinOp(op) => BuiltinOp(*op),
            Break => Break,
            Continue => Continue,
        };

        Ok(Expr { kind, ty, loc })
    }
}
