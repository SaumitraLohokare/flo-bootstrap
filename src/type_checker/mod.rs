use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    type_checker::replace_set::ReplaceSet,
    types::Type,
};

mod replace_set;

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
                        let name = mangle_name(&func.ty, name);
                        assert!(!new_funcs.contains_key(&name));
                        new_funcs.entry(name).or_default().push(func);
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

        let mut set = ReplaceSet::new();
        self.solve_func_constraints(func, &mut set)?;
        self.solve_expr_constraints(&func.body, &mut set)?;

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

    fn solve_expr_constraints(&self, expr: &Expr, set: &mut ReplaceSet) -> FloResult<()> {
        use ExprKind::*;
        use Type::*;

        match &expr.kind {
            Num(_) => {
                let constraint = IsEqual(Integer, expr.ty.clone(), expr.loc);
                self.solve_constraint(set, constraint)?;
            }
            Var(_) => {}
            Call(_, arg_exprs, _) => {
                for arg_expr in arg_exprs {
                    self.solve_expr_constraints(arg_expr, set)?;
                }
            }
        }

        Ok(())
    }

    fn solve_constraint(
        &self,
        set: &mut ReplaceSet,
        IsEqual(t1, t2, loc): IsEqual,
    ) -> FloResult<()> {
        use Type::*;

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

        if let Call(name, args, resolved) = &expr.kind {
            for arg in args {
                self.check_calls_resolved(arg, set)?;
            }

            if resolved.is_none() {
                let possible = self.possible_overloads(name, args, &expr.ty, expr.loc, set)?;

                // TODO: Might wanna add locations of the possible overloads to
                // the ambiguous error
                if possible.is_empty() {
                    Err(FloErr::NoPossibleOverloads {
                        name: name.clone(),
                        loc: expr.loc,
                    })?
                } else {
                    Err(FloErr::AmbiguousOverloads {
                        name: name.clone(),
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

    format!("{name}__{arg_list}_{ret:?}")
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

        Ok(match &self.kind {
            Num(n) => Expr {
                kind: Num(*n),
                ty,
                loc,
            },
            Var(v) => Expr {
                kind: Var(*v),
                ty,
                loc,
            },
            Call(name, args, resolved) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(arg.resolve(set)?);
                }
                assert!(
                    resolved.is_some(),
                    "All calls should be resolved at this point"
                );
                Expr {
                    kind: Call(name.clone(), new_args, resolved.clone()),
                    ty,
                    loc,
                }
            }
        })
    }
}
