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

#[derive(Debug)]
struct IsEqual(Type, Type, Loc);

pub struct TypeChecker {
    func_types: HashMap<String, Vec<Type>>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            func_types: HashMap::new(),
        }
    }

    // Was thinking this should return a new Module
    // instead of modifying the old one?
    pub fn check(mut self, mut module: Module) -> Result<Module, Vec<FloErr>> {
        for (name, funcs) in &module.funcs {
            self.func_types.insert(
                name.clone(),
                funcs.iter().map(|f| f.ty.clone()).collect::<Vec<_>>(),
            );
        }

        let mut errs = Vec::new();
        let mut new_funcs: HashMap<String, Vec<Func>> = HashMap::new();
        for (name, funcs) in &mut module.funcs {
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
            Ok(Module { funcs: new_funcs })
        } else {
            Err(errs)
        }
    }

    fn check_func(&self, func: &mut Func) -> FloResult<Func> {
        // 1. Collect & Solve Constraints

        let Type::Fn(_arg_tys, ret_ty) = func.ty.clone() else {
            unreachable!()
        };

        let mut set = ReplaceSet::new();
        self.solve_func_constraints(func, &mut set)?;
        self.solve_expr_constraints(&func.body, ret_ty.as_ref(), &mut set)?;

        // 2. Resolve calls to a fixpoint (before defaulting)

        self.solve_calls_to_fixpoint(&mut func.body, &mut set)?;

        // 3. Default Types ({integer} => i32)

        set.default_types();

        // 4. Resolve calls to a fixpoint again
        //    (defaulting may have unblocked calls that were ambiguous before)

        self.solve_calls_to_fixpoint(&mut func.body, &mut set)?;

        // 5. Any call still unresolved is now a genuine error

        self.check_calls_resolved(&func.body, &mut set)?;

        // 6. Make Resolved Func & return it

        self.resolve_func(func, &mut set)
    }

    fn solve_func_constraints(&self, func: &Func, set: &mut ReplaceSet) -> FloResult<()> {
        // Constraints for argument types are not added, because they're already
        // concrete types
        let Type::Fn(_arg_tys, ret_ty) = &func.ty else {
            unreachable!()
        };

        self.solve_constraint(
            set,
            IsEqual(*ret_ty.clone(), func.body.ty.clone(), func.body.loc),
        )
    }

    /// `ret_ty` is the enclosing function's return type, needed to constrain the
    /// operand of any `return` expression that appears in the body.
    fn solve_expr_constraints(
        &self,
        expr: &Expr,
        ret_ty: &Type,
        set: &mut ReplaceSet,
    ) -> FloResult<()> {
        use ExprKind::*;
        use Type::*;

        match &expr.kind {
            Num(_) => {
                let constraint = IsEqual(Integer, expr.ty.clone(), expr.loc);
                self.solve_constraint(set, constraint)?;
            }
            Flt(_) => {
                let constraint = IsEqual(Decimal, expr.ty.clone(), expr.loc);
                self.solve_constraint(set, constraint)?;
            }
            ExprKind::Bool(_) => {
                let constraint = IsEqual(Type::Bool, expr.ty.clone(), expr.loc);
                self.solve_constraint(set, constraint)?;
            }
            BuiltinOp(_) | Var(_) => {}
            Call(_, arg_exprs, _) => {
                for arg_expr in arg_exprs {
                    self.solve_expr_constraints(arg_expr, ret_ty, set)?;
                }
            }
            Scope(stmts, tail) => {
                for stmt in stmts {
                    self.solve_expr_constraints(stmt, ret_ty, set)?;
                }
                if let Some(tail) = tail {
                    self.solve_expr_constraints(tail, ret_ty, set)?;
                }

                // A scope diverges if any statement diverges or its tail does.
                // Statement divergence (a `return` before the tail) makes the
                // whole scope NoReturn even though the tail is dead code.
                let diverges = stmts.iter().any(|s| matches!(set.resolve(&s.ty), Never))
                    || tail
                        .as_ref()
                        .is_some_and(|t| matches!(set.resolve(&t.ty), Never));

                if diverges {
                    self.mark_never(&expr.ty, set, expr.loc)?;
                } else if let Some(tail) = tail {
                    let constraint = IsEqual(tail.ty.clone(), expr.ty.clone(), expr.loc);
                    self.solve_constraint(set, constraint)?;
                } else {
                    // No tail and no divergence => an empty/`;`-terminated scope
                    // is void.
                    let constraint = IsEqual(Void, expr.ty.clone(), expr.loc);
                    self.solve_constraint(set, constraint)?;
                }
            }
            If(cond, then, otherwise) => {
                self.solve_expr_constraints(cond, ret_ty, set)?;
                let cond_constr = IsEqual(Type::Bool, cond.ty.clone(), cond.loc);
                self.solve_constraint(set, cond_constr)?;

                self.solve_expr_constraints(then, ret_ty, set)?;
                // The `if`'s own type is the join of its branches. A NoReturn
                // branch is absorbed: the constraint below is a no-op for it (see
                // `solve_constraint`), so the `if` takes the other branch's type.
                let then_constr = IsEqual(expr.ty.clone(), then.ty.clone(), then.loc);
                self.solve_constraint(set, then_constr)?;

                if let Some(otherwise) = otherwise {
                    self.solve_expr_constraints(otherwise, ret_ty, set)?;
                    let branch_constr =
                        IsEqual(expr.ty.clone(), otherwise.ty.clone(), otherwise.loc);
                    self.solve_constraint(set, branch_constr)?;

                    // If BOTH branches diverge the whole `if` diverges. The two
                    // constraints above bound nothing (both were no-ops), so pin
                    // the `if`'s own type to NoReturn explicitly.
                    if matches!(set.resolve(&then.ty), Never)
                        && matches!(set.resolve(&otherwise.ty), Never)
                    {
                        self.mark_never(&expr.ty, set, expr.loc)?;
                    }
                } else {
                    // With no `else`, the `if` is void — control may skip `then`
                    // entirely, so a diverging `then` does not make it NoReturn.
                    let void_constr = IsEqual(Void, expr.ty.clone(), expr.loc);
                    self.solve_constraint(set, void_constr)?;
                }
            }
            Return(value) => {
                match value {
                    Some(e) => {
                        self.solve_expr_constraints(e, ret_ty, set)?;
                        // The returned value must match the function's return type.
                        let constraint = IsEqual(ret_ty.clone(), e.ty.clone(), e.loc);
                        self.solve_constraint(set, constraint)?;
                    }
                    None => {
                        // Bare `return` yields void; only valid in a void function.
                        let constraint = IsEqual(ret_ty.clone(), Void, expr.loc);
                        self.solve_constraint(set, constraint)?;
                    }
                }
                // The `return` expression's own type is already NoReturn (set by
                // the parser), so it needs no constraint here.
            }
        }

        Ok(())
    }

    /// Pin a composite expression's own (always fresh) type variable to NoReturn
    /// when it is determined to diverge. Only the composite's own variable is
    /// touched, so a diverging branch or statement never poisons its siblings.
    fn mark_never(&self, ty: &Type, set: &mut ReplaceSet, loc: Loc) -> FloResult<()> {
        match ty {
            Type::T(id) => set.bind(*id, Type::Never, loc),
            _ => Ok(()),
        }
    }

    fn solve_constraint(
        &self,
        set: &mut ReplaceSet,
        IsEqual(t1, t2, loc): IsEqual,
    ) -> FloResult<()> {
        use Type::*;

        // NoReturn satisfies any binding: a constraint touching it is vacuously
        // solved. Crucially it never binds a variable to NoReturn — divergence is
        // propagated structurally (via `mark_never`), not through unification, so
        // a diverging expression cannot poison a neighbour's type variable.
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

    /// Returns every overload of `name` that is still compatible with the
    /// current (partially resolved) argument and return types. As inference
    /// binds more type variables this set only ever shrinks, so once it has
    /// exactly one entry that choice can never be wrong.
    fn possible_overloads(
        &self,
        name: &str,
        args: &[Expr],
        ret_ty: &Type,
        loc: Loc,
        set: &mut ReplaceSet,
    ) -> FloResult<Vec<&Type>> {
        use Type::*;

        let overloads = self.func_types.get(name).ok_or(FloErr::UndefinedFunction {
            name: name.to_string(),
            loc,
        })?;

        let possible = overloads
            .iter()
            .filter(|f| {
                let Fn(params, ret) = f else { unreachable!() };

                // Arity mismatch => prune overload
                if params.len() != args.len() {
                    return false;
                }

                let mut should_keep = true;
                // Ensure all arg types satisfy
                for (arg_expr, param) in args.iter().zip(params) {
                    should_keep &= set.resolve(&arg_expr.ty).satisfies_type(param);
                }
                // Ensure return type satisfies
                should_keep & set.resolve(ret_ty).satisfies_type(ret)
            })
            .collect::<Vec<_>>();

        Ok(possible)
    }

    /// Repeatedly attempt to resolve every call until a full pass resolves
    /// nothing new. Resolving one call binds type variables, which can unblock
    /// its parent (via the argument types) or its children (via the return
    /// type), so no single traversal order works — we iterate until the whole
    /// tree stops changing.
    fn solve_calls_to_fixpoint(&self, expr: &mut Expr, set: &mut ReplaceSet) -> FloResult<()> {
        loop {
            let mut progress = false;
            self.try_solve_calls(expr, set, &mut progress)?;
            if !progress {
                break;
            }
        }
        Ok(())
    }

    /// One pass over the tree: commit every call that currently has exactly
    /// one possible overload, leaving ambiguous (0 or 2+) calls untouched for a
    /// later pass. Sets `progress` whenever a call is newly resolved. This
    /// never reports overload errors — that happens in `check_calls_resolved`
    /// once the fixpoint has settled.
    fn try_solve_calls(
        &self,
        expr: &mut Expr,
        set: &mut ReplaceSet,
        progress: &mut bool,
    ) -> FloResult<()> {
        use ExprKind::*;
        use Type::*;

        if let Scope(exprs, tail) = &mut expr.kind {
            for expr in exprs.iter_mut() {
                self.try_solve_calls(expr, set, progress)?;
            }
            if let Some(tail) = tail {
                self.try_solve_calls(tail, set, progress)?;
            }
            return Ok(());
        }

        if let If(cond, then, otherwise) = &mut expr.kind {
            self.try_solve_calls(cond, set, progress)?;
            self.try_solve_calls(then, set, progress)?;
            if let Some(otherwise) = otherwise {
                self.try_solve_calls(otherwise, set, progress)?;
            }
            return Ok(());
        }

        if let Return(value) = &mut expr.kind {
            if let Some(e) = value {
                self.try_solve_calls(e, set, progress)?;
            }
            return Ok(());
        }

        if let Call(name, args, resolved) = &mut expr.kind {
            if resolved.is_none() {
                let possible = self.possible_overloads(name, args, &expr.ty, expr.loc, set)?;

                // Only commit when EXACTLY one overload survives; 0 or 2+ are
                // left for a later pass (a neighbouring call may resolve and
                // narrow this one down).
                if possible.len() == 1 {
                    let Fn(params, ret) = possible[0] else {
                        unreachable!()
                    };

                    for (arg_expr, param) in args.iter().zip(params) {
                        assert!(param.is_known());
                        match set.resolve(&arg_expr.ty) {
                            T(id) => set.bind(id, param.clone(), arg_expr.loc)?,
                            // Pin an {integer} literal to the concrete param type
                            Integer if let T(id) = arg_expr.ty => {
                                set.bind(id, param.clone(), arg_expr.loc)?;
                            }
                            // Pin a {decimal} literal to the concrete param type
                            Decimal if let T(id) = arg_expr.ty => {
                                set.bind(id, param.clone(), arg_expr.loc)?;
                            }
                            _ => {}
                        }
                    }

                    assert!(ret.is_known());
                    match set.resolve(&expr.ty) {
                        T(id) => set.bind(id, *ret.clone(), expr.loc)?,
                        // Pin an {integer} result to the concrete return type
                        Integer if let T(id) = expr.ty => {
                            set.bind(id, *ret.clone(), expr.loc)?;
                        }
                        _ => {}
                    }

                    // Mark as resolved & replace with mangled name
                    *resolved = Some(mangle_name(possible[0], name));
                    *progress = true;
                }
            }

            // Always recurse into args, resolved or not: a nested call may
            // become resolvable once a sibling or parent binds more types.
            for arg in args.iter_mut() {
                self.try_solve_calls(arg, set, progress)?;
            }
        }

        Ok(())
    }

    /// After the fixpoint has settled, any call that is still unresolved is a
    /// genuine error. Children are checked first so the innermost (root cause)
    /// error surfaces instead of an ambiguity it caused higher up.
    fn check_calls_resolved(&self, expr: &Expr, set: &mut ReplaceSet) -> FloResult<()> {
        use ExprKind::*;

        if let Scope(exprs, tail) = &expr.kind {
            for expr in exprs {
                self.check_calls_resolved(expr, set)?;
            }
            if let Some(tail) = tail {
                self.check_calls_resolved(tail, set)?;
            }
            return Ok(());
        }

        if let If(cond, then, otherwise) = &expr.kind {
            self.check_calls_resolved(cond, set)?;
            self.check_calls_resolved(then, set)?;
            if let Some(otherwise) = otherwise {
                self.check_calls_resolved(otherwise, set)?;
            }
            return Ok(());
        }

        if let Return(value) = &expr.kind {
            if let Some(e) = value {
                self.check_calls_resolved(e, set)?;
            }
            return Ok(());
        }

        if let Call(name, args, resolved) = &expr.kind {
            for arg in args {
                self.check_calls_resolved(arg, set)?;
            }

            if resolved.is_none() {
                let possible = self.possible_overloads(name, args, &expr.ty, expr.loc, set)?;

                if possible.is_empty() {
                    let arg_tys = args.iter().map(|a| set.resolve(&a.ty)).collect();
                    let known_ty = Type::Fn(arg_tys, Box::new(set.resolve(&expr.ty)));
                    Err(FloErr::NoPossibleOverloads {
                        name: name.clone(),
                        known_ty,
                        loc: expr.loc,
                    })?
                } else {
                    let possible_tys = possible
                        .into_iter()
                        .map(|t| set.resolve(t))
                        .collect::<Vec<Type>>();
                    Err(FloErr::MultiplePossibleOverloads {
                        name: name.clone(),
                        possible_tys,
                        loc: expr.loc,
                    })?
                }
            }
        }

        Ok(())
    }

    fn resolve_func(&self, func: &Func, set: &mut ReplaceSet) -> FloResult<Func> {
        func.resolve(set)
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
    fn resolve(&self, set: &mut ReplaceSet) -> FloResult<Self> {
        let ty = set.resolve(&self.ty);
        if ty.is_known() {
            Ok(Func {
                body: self.body.resolve(set)?,
                ty,
                loc: self.loc.clone(),
            })
        } else {
            Err(FloErr::UnresolvedType { ty, loc: self.loc })
        }
    }
}

impl Expr {
    fn resolve(&self, set: &mut ReplaceSet) -> FloResult<Self> {
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
            Call(name, args, resolved) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(arg.resolve(set)?);
                }
                assert!(
                    resolved.is_some(),
                    "Unresolved call {name} (args: {args:?})"
                );
                Call(name.clone(), new_args, resolved.clone())
            }
            Scope(exprs, tail) => {
                let mut new_exprs = Vec::new();
                for expr in exprs {
                    new_exprs.push(expr.resolve(set)?);
                }

                let new_tail = match tail {
                    Some(e) => Some(Box::new(e.resolve(set)?)),
                    None => None,
                };
                Scope(new_exprs, new_tail)
            }
            If(cond, then, otherwise) => {
                let cond = Box::new(cond.resolve(set)?);
                let then = Box::new(then.resolve(set)?);
                let otherwise = match otherwise {
                    Some(e) => Some(Box::new(e.resolve(set)?)),
                    None => None,
                };
                If(cond, then, otherwise)
            }
            Return(value) => {
                let new_value = match value {
                    Some(e) => Some(Box::new(e.resolve(set)?)),
                    None => None,
                };
                Return(new_value)
            }

            Num(n) => Num(*n),
            Flt(n) => Flt(*n),
            Bool(n) => Bool(*n),
            Var(v) => Var(*v),
            BuiltinOp(op) => BuiltinOp(*op),
        };

        Ok(Expr { kind, ty, loc })
    }
}
