use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeKind},
};

type TypeLoc = (Type, Loc);

#[derive(Debug)]
struct ReplaceSet {
    ty_to_id: HashMap<TypeLoc, usize>,
    id_to_ty: Vec<TypeLoc>,

    parents: Vec<usize>,
}

impl ReplaceSet {
    fn new() -> Self {
        Self {
            ty_to_id: HashMap::new(),
            id_to_ty: Vec::new(),

            parents: Vec::new(),
        }
    }

    // We should add all known types first
    fn add(&mut self, ty: Type, loc: Loc) -> usize {
        let type_loc = (ty, loc);
        if let Some(&id) = self.ty_to_id.get(&type_loc) {
            return id;
        }
        let id = self.id_to_ty.len();
        self.id_to_ty.push(type_loc.clone());
        self.ty_to_id.insert(type_loc, id);
        self.parents.push(id);
        id
    }

    fn find(&mut self, type_id: usize) -> usize {
        // All types should already be added
        debug_assert!(self.id_to_ty.len() > type_id);

        let mut root = type_id;
        while self.parents[root] != root {
            root = self.parents[root];
        }

        // Path Compression
        let mut cur = type_id;
        while cur != root {
            let next = self.parents[cur];
            self.parents[cur] = root;
            cur = next;
        }

        root
    }

    fn union(&mut self, a: usize, b: usize) -> FloResult<()> {
        let ra = self.find(a);
        let rb = self.find(b);

        let (ra_type, ra_loc) = &self.id_to_ty[ra];
        let (rb_type, rb_loc) = &self.id_to_ty[rb];

        match (ra_type.is_known(), rb_type.is_known()) {
            (true, true) => {
                if ra_type != rb_type {
                    Err(FloErr::TypeMismatch {
                        t1: ra_type.clone(),
                        loc1: *ra_loc,
                        t2: rb_type.clone(),
                        loc2: *rb_loc,
                    })
                } else {
                    Ok(())
                }
            }
            (false, _) => {
                self.parents[ra] = rb;
                Ok(())
            }
            (_, false) => {
                self.parents[rb] = ra;
                Ok(())
            }
        }
    }

    fn resolve(&mut self, ty: Type, loc: Loc) -> FloResult<Type> {
        let type_loc = (ty, loc);

        let Some(&id) = self.ty_to_id.get(&type_loc) else {
            unreachable!()
        };
        let root = self.find(id);
        let (root_type, _) = self.id_to_ty[root].clone();

        if root_type.is_known() {
            Ok(root_type.clone())
        } else {
            Err(FloErr::UnresolvedType { loc })
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Constraint {
    IsEqual(usize, usize),
    IsKind(usize, TypeKind, Loc),
}

pub struct TypeChecker;

impl TypeChecker {
    pub fn new() -> Self {
        Self
    }

    pub fn check(self, module: &mut Module) -> FloResult<()> {
        for (_, func) in &mut module.funcs {
            self.check_func(func)?;
        }

        Ok(())
    }

    fn check_func(&self, func: &mut Func) -> FloResult<()> {
        // 1. Generate Constraints

        let mut set = ReplaceSet::new();
        let mut constraints = Vec::new();
        self.generate_func_constraints(func, &mut set, &mut constraints);
        self.generate_expr_constraints(&func.body, &mut set, &mut constraints);

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
    ) {
        use Constraint::*;
        use ExprKind::*;
        use TypeKind::*;

        match expr.kind {
            Num(_) => {
                let id = set.add(expr.ty.clone(), expr.loc);
                constraints.push(IsKind(id, Integral, expr.loc));
            }
            Var(_) => {}
        }
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

        match expr.kind {
            ExprKind::Num(_) => {}
            ExprKind::Var(_) => {}
        }

        Ok(())
    }
}
