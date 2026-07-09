use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::Type,
};

#[derive(Debug, Clone, Copy)]
pub enum TypeKind {
    Numeric,
}

impl TypeKind {
    fn satisfies_type(&self, ty: &Type) -> bool {
        use Type::*;
        use TypeKind::*;

        match self {
            Numeric => matches!(ty, I32),
        }
    }

    fn default_type(&self) -> Type {
        match self {
            TypeKind::Numeric => Type::I32,
        }
    }
}

#[derive(Debug, Clone)]
enum Constraint {
    IsEqual(Type, Loc, Type, Loc),
    IsKind(Type, Loc, TypeKind),
}

pub struct TypeChecker {
    func_types: HashMap<String, Type>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            func_types: HashMap::new(),
        }
    }

    pub fn check(&mut self, module: &mut Module) -> FloResult<()> {
        for (name, func) in module.funcs.iter() {
            // NOTE: Redefinition is already checked in parsing
            self.func_types.insert(name.clone(), func.ty.clone());
        }

        for (_name, func) in module.funcs.iter_mut() {
            self.check_func(func)?;
        }

        Ok(())
    }

    fn check_func(&self, func: &mut Func) -> FloResult<()> {
        // Using Union-Find solve the constraints
        // If unsolved constraints: Default types
        // Solve again
        // Resolve types for all expressions
        // Unsolved => Error

        // 1. Generate Constraints & Initialize UnionFind

        let Type::Fn(_, ret_ty) = &func.ty else {
            unreachable!()
        };

        let mut constraints = Vec::new();

        // NOTE: This will have to change once we add `return`
        constraints.push(Constraint::IsEqual(
            *ret_ty.clone(),
            func.loc,
            func.body.ty.clone(),
            func.body.loc,
        ));

        self.generate_expr_constraints(&func.body, &mut constraints);

        // 2. Solve constraints

        let mut replace_map = HashMap::new();
        constraints = self.solve_constraints(constraints, &mut replace_map)?;

        // 3. Default Types

        if !constraints.is_empty() {
            for constraint in &constraints {
                match constraint {
                    Constraint::IsKind(ty, _, kind) => {
                        assert!(!ty.is_known());

                        replace_map.insert(ty.clone(), kind.default_type());
                    }
                    _ => {}
                }
            }
        }

        // 4. Solve again

        _ = self.solve_constraints(constraints, &mut replace_map)?;

        // 5. Replace types in func

        func.replace_types(&replace_map);
        func.ensure_resolved()
    }

    fn generate_expr_constraints(&self, expr: &Expr, constraints: &mut Vec<Constraint>) {
        use Constraint::*;
        use ExprKind::*;
        use TypeKind::*;

        let ty = expr.ty.clone();

        match expr.kind {
            Num(_) => constraints.push(IsKind(ty, expr.loc, Numeric)),
        }
    }

    fn solve_constraints(
        &self,
        mut constraints: Vec<Constraint>,
        replace_map: &mut HashMap<Type, Type>,
    ) -> FloResult<Vec<Constraint>> {
        loop {
            let mut keep_solving = false;
            let mut new_constraints = Vec::new();
            for constraint in constraints {
                match constraint {
                    Constraint::IsEqual(ref t1, loc1, ref t2, loc2) => {
                        match (t1.is_known(), t2.is_known()) {
                            (true, true) => {
                                if t1 != t2 {
                                    return Err(FloErr::TypeMismatch {
                                        t1: t1.clone(),
                                        loc1,
                                        t2: t2.clone(),
                                        loc2,
                                    });
                                }
                            }
                            (false, true) => {
                                let ty = replace_map.entry(t1.clone()).or_insert(t2.clone());
                                if ty != t2 {
                                    return Err(FloErr::TypeMismatch {
                                        t1: t1.clone(),
                                        loc1,
                                        t2: t2.clone(),
                                        loc2,
                                    });
                                }
                            }
                            (true, false) => {
                                let ty = replace_map.entry(t2.clone()).or_insert(t1.clone());
                                if ty != t1 {
                                    return Err(FloErr::TypeMismatch {
                                        t1: t1.clone(),
                                        loc1,
                                        t2: t2.clone(),
                                        loc2,
                                    });
                                }
                            }
                            (false, false) => new_constraints.push(constraint),
                        }
                    }
                    Constraint::IsKind(ref ty, loc, kind) => {
                        if ty.is_known() {
                            if !kind.satisfies_type(ty) {
                                return Err(FloErr::UnsatisfiedTypeKind {
                                    ty: ty.clone(),
                                    kind,
                                    loc,
                                });
                            }
                        } else {
                            new_constraints.push(constraint);
                        }
                    }
                }
            }

            // Replace all types
            for constraint in new_constraints.iter_mut() {
                match constraint {
                    Constraint::IsEqual(t1, _, t2, _) => {
                        if let Some(replace_type) = replace_map.get(t1) {
                            *t1 = replace_type.clone();
                            keep_solving = true;
                        }
                        if let Some(replace_type) = replace_map.get(t2) {
                            *t2 = replace_type.clone();
                            keep_solving = true;
                        }
                    }
                    Constraint::IsKind(ty, _, _) => {
                        if let Some(replace_type) = replace_map.get(ty) {
                            *ty = replace_type.clone();
                            keep_solving = true;
                        }
                    }
                }
            }

            constraints = new_constraints;
            if !keep_solving {
                break;
            }
        }

        Ok(constraints)
    }
}
