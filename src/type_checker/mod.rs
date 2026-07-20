mod replace_set;

use replace_set::ReplaceSet;
use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, FuncLocs, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeKind},
};

type TypeLoc = (Type, Loc);


#[derive(Debug, Clone, Copy)]
enum Constraint {
    IsEqual(usize, usize),
    IsKind(usize, TypeKind, Loc),
}

pub struct TypeChecker {
    func_types: HashMap<String, Type>,
    func_locs: HashMap<String, FuncLocs>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            func_types: HashMap::new(),
            func_locs: HashMap::new(),
        }
    }

    pub fn check(mut self, module: &mut Module) -> Vec<FloErr> {
        for (name, func) in &module.funcs {
            self.func_types.insert(name.clone(), func.ty.clone());
            self.func_locs.insert(name.clone(), func.loc.clone());
        }

        let mut errs = Vec::new();
        for (_, func) in &mut module.funcs {
            if let Err(err) = self.check_func(func) {
                errs.push(err);
            }
        }

        errs
    }

    fn check_func(&self, func: &mut Func) -> FloResult<()> {
        // 1. Generate Constraints

        let mut set = ReplaceSet::new();
        let mut constraints = Vec::new();
        self.generate_func_constraints(func, &mut set, &mut constraints);
        self.generate_expr_constraints(&func.body, &mut set, &mut constraints)?;

        // 2. Solve Constraints

        constraints = self.solve_constraints(&mut set, &mut constraints)?;

        // 3. Default Types

        for constraint in &constraints {
            use Constraint::*;
            if let IsKind(id, kind, loc) = constraint {
                let root = set.find(*id);
                if !set.id_to_ty[root].0.is_known() {
                    let default_id = set.add(kind.default_type(), *loc);
                    set.union(root, default_id)?;
                }
            }
        }

        // 4. Solve Constraints

        constraints = self.solve_constraints(&mut set, &mut constraints)?;
        debug_assert!(constraints.is_empty());

        // 5. Replace Types in Func

        self.resolve_func(func, &mut set)
    }

    fn generate_func_constraints(
        &self,
        func: &Func,
        set: &mut ReplaceSet,
        constraints: &mut Vec<Constraint>,
    ) {
        use Constraint::*;

        let Type::Fn(arg_tys, ret_ty) = &func.ty else {
            unreachable!()
        };

        for (arg_ty, &arg_loc) in arg_tys.iter().zip(&func.loc.arg_types) {
            set.add(arg_ty.clone(), arg_loc);
        }

        let a = set.add(*ret_ty.clone(), func.loc.ret_type);
        let b = set.add(func.body.ty.clone(), func.body.loc);
        constraints.push(IsEqual(b, a));
    }

    fn generate_expr_constraints(
        &self,
        expr: &Expr,
        set: &mut ReplaceSet,
        constraints: &mut Vec<Constraint>,
    ) -> FloResult<()> {
        use Constraint::*;
        use ExprKind::*;
        use TypeKind::*;

        match &expr.kind {
            Num(_) => {
                let id = set.add(expr.ty.clone(), expr.loc);
                constraints.push(IsKind(id, Integral, expr.loc));
            }
            Var(_) => {}
            Call(name, args) => {
                for arg in args {
                    self.generate_expr_constraints(arg, set, constraints)?;
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

                let callee_locs = self.func_locs.get(name).unwrap();

                for (arg, (arg_ty, arg_ty_loc)) in
                    args.iter().zip(arg_tys.iter().zip(&callee_locs.arg_types))
                {
                    let a = set.add(arg.ty.clone(), arg.loc);
                    let b = set.add(arg_ty.clone(), *arg_ty_loc);
                    constraints.push(IsEqual(a, b));
                }

                let a = set.add(expr.ty.clone(), expr.loc);
                let b = set.add(*ret_ty.clone(), expr.loc);
                constraints.push(IsEqual(b, a));
            }
        }

        Ok(())
    }

    fn solve_constraints(
        &self,
        set: &mut ReplaceSet,
        constraints: &mut Vec<Constraint>,
    ) -> FloResult<Vec<Constraint>> {
        let mut pending = Vec::new();

        for constraint in constraints {
            match constraint {
                Constraint::IsEqual(a, b) => set.union(*a, *b)?,
                Constraint::IsKind(id, kind, loc) => {
                    let root = set.find(*id);
                    let (ty, ty_loc) = set.id_to_ty[root].clone();
                    if ty.is_known() {
                        if !kind.satisfies_type(&ty) {
                            return Err(FloErr::UnsatisfiedTypeKind {
                                ty,
                                ty_loc,
                                kind: *kind,
                                loc: *loc,
                            });
                        }
                    } else {
                        pending.push(*constraint);
                    }
                }
            }
        }

        Ok(pending)
    }

    fn resolve_func(&self, func: &mut Func, set: &mut ReplaceSet) -> FloResult<()> {
        let Type::Fn(arg_types, ret_type) = &mut func.ty else {
            unreachable!()
        };

        for (arg_ty, arg_loc) in arg_types.iter_mut().zip(&func.loc.arg_types) {
            *arg_ty = set.resolve(arg_ty.clone(), *arg_loc)?;
        }
        *ret_type = Box::new(set.resolve(*ret_type.clone(), func.loc.ret_type)?);

        self.resolve_expr(&mut func.body, set)
    }

    fn resolve_expr(&self, expr: &mut Expr, set: &mut ReplaceSet) -> FloResult<()> {
        expr.ty = set.resolve(expr.ty.clone(), expr.loc)?;

        match &mut expr.kind {
            ExprKind::Num(_) => {}
            ExprKind::Var(_) => {}
            ExprKind::Call(_, args) => {
                for arg in args {
                    self.resolve_expr(arg, set)?;
                }
            }
        }

        Ok(())
    }
}
