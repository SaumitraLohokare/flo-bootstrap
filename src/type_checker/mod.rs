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
    func_types: HashMap<String, Type>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            func_types: HashMap::new(),
        }
    }

    // Was thinking this should return a new Module
    // instead of modifying the old one?
    pub fn check(mut self, module: &Module) -> Result<Module, Vec<FloErr>> {
        for (name, func) in &module.funcs {
            self.func_types.insert(name.clone(), func.ty.clone());
        }

        let mut errs = Vec::new();
        let mut funcs = HashMap::new();
        for (name, func) in &module.funcs {
            match self.check_func(func) {
                Ok(func) => {
                    funcs.insert(name.clone(), func);
                }
                Err(err) => errs.push(err),
            }
        }

        if errs.is_empty() {
            Ok(Module { funcs })
        } else {
            Err(errs)
        }
    }

    fn check_func(&self, func: &Func) -> FloResult<Func> {
        // 1. Collect Constraints

        let mut constraints = Vec::new();
        self.collect_func_constraints(func, &mut constraints);
        self.collect_expr_constraints(&func.body, &mut constraints)?;

        // 2. Solve Constraints

        let mut set = ReplaceSet::new();
        self.solve_constraints(&mut set, constraints)?;

        // 3. Default Types

        set.default_types();

        // 5. Make Resolved Func & return it

        self.resolve_func(func, &set)
    }

    fn collect_func_constraints(&self, func: &Func, constraints: &mut Vec<IsEqual>) {
        // Constraints for argument types are not added, because they're already
        // concrete types
        let Type::Fn(_arg_tys, ret_ty) = &func.ty else {
            unreachable!()
        };

        constraints.push(IsEqual(
            *ret_ty.clone(),
            func.body.ty.clone(),
            func.body.loc,
        ));
    }

    fn collect_expr_constraints(
        &self,
        expr: &Expr,
        constraints: &mut Vec<IsEqual>,
    ) -> FloResult<()> {
        use ExprKind::*;
        use Type::*;

        match &expr.kind {
            Num(_) => {
                constraints.push(IsEqual(Integer, expr.ty.clone(), expr.loc));
            }
            Var(_) => {}
            Call(name, args) => {
                for arg in args {
                    self.collect_expr_constraints(arg, constraints)?;
                }

                let Type::Fn(arg_tys, ret_ty) =
                    self.func_types.get(name).ok_or(FloErr::UndefinedFunction {
                        name: name.clone(),
                        loc: expr.loc,
                    })?
                else {
                    unreachable!()
                };

                if arg_tys.len() != args.len() {
                    return Err(FloErr::CallArityMismatch {
                        expected: arg_tys.len(),
                        got: args.len(),
                        loc: expr.loc,
                    });
                }

                for (arg, arg_ty) in args.iter().zip(arg_tys) {
                    constraints.push(IsEqual(arg_ty.clone(), arg.ty.clone(), arg.loc));
                }

                constraints.push(IsEqual(*ret_ty.clone(), expr.ty.clone(), expr.loc));
            }
        }

        Ok(())
    }

    fn solve_constraints(&self, set: &mut ReplaceSet, constraints: Vec<IsEqual>) -> FloResult<()> {
        use Type::*;

        for IsEqual(t1, t2, loc) in constraints {
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
        }

        Ok(())
    }

    fn resolve_func(&self, func: &Func, set: &ReplaceSet) -> FloResult<Func> {
        func.resolve(set)
    }
}

impl Func {
    fn resolve(&self, set: &ReplaceSet) -> FloResult<Self> {
        let ty = set.resolve(&self.ty)?;
        Ok(Func {
            body: self.body.resolve(set)?,
            ty,
            loc: self.loc.clone(),
        })
    }
}

impl Expr {
    fn resolve(&self, set: &ReplaceSet) -> FloResult<Self> {
        use ExprKind::*;

        let ty = set.resolve(&self.ty)?;
        let loc = self.loc;
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
            Call(name, args) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(arg.resolve(set)?);
                }
                Expr {
                    kind: Call(name.clone(), new_args),
                    ty,
                    loc,
                }
            }
        })
    }
}
