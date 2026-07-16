use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::Type,
};

use super::unify::UnionFind;
use super::Residual;

/// Produce a fresh copy of a type: each schema variable maps to a brand-new id,
/// consistently within one instantiation (`map`).
pub(super) fn freshen(ty: &Type, map: &mut HashMap<usize, usize>, fresh: &mut usize) -> Type {
    match ty {
        Type::T(id) => {
            let nid = *map.entry(*id).or_insert_with(|| {
                let n = *fresh;
                *fresh += 1;
                n
            });
            Type::T(nid)
        }
        Type::Fn(args, ret) => Type::Fn(
            args.iter().map(|a| freshen(a, map, fresh)).collect(),
            Box::new(freshen(ret, map, fresh)),
        ),
        other => other.clone(),
    }
}

/// Freshen every type variable inside an expression tree (in place), consistent
/// with an in-progress instantiation `map`.
pub(super) fn freshen_expr(expr: &mut Expr, map: &mut HashMap<usize, usize>, fresh: &mut usize) {
    expr.ty = freshen(&expr.ty, map, fresh);
    match &mut expr.kind {
        ExprKind::Num(_) | ExprKind::Var(_) | ExprKind::Intrinsic => {}
        ExprKind::Call(_, args) => {
            for arg in args {
                freshen_expr(arg, map, fresh);
            }
        }
    }
}

/// Freshen a callee's residual kind bounds into `kinds`, reusing the same
/// instantiation `map` as the rest of that call's freshened signature so shared
/// variables stay linked.
pub(super) fn freshen_residuals(
    residuals: Option<&Vec<Residual>>,
    map: &mut HashMap<usize, usize>,
    fresh: &mut usize,
    kinds: &mut Vec<(Type, crate::types::TypeKind, Loc)>,
) {
    if let Some(res) = residuals {
        for &(vid, kind, loc) in res {
            let ft = freshen(&Type::T(vid), map, fresh);
            kinds.push((ft, kind, loc));
        }
    }
}

/// Resolve a type against the union-find, renumbering its free variables into a
/// canonical namespace (shared `map`/`next` keeps a whole function consistent).
pub(super) fn resolve_canon(
    ty: &Type,
    uf: &mut UnionFind,
    map: &mut HashMap<usize, usize>,
    next: &mut usize,
) -> Type {
    match uf.resolve_head(ty) {
        Type::T(rep) => {
            let cid = *map.entry(rep).or_insert_with(|| {
                let n = *next;
                *next += 1;
                n
            });
            Type::T(cid)
        }
        Type::Fn(args, ret) => Type::Fn(
            args.iter()
                .map(|a| resolve_canon(a, uf, map, next))
                .collect(),
            Box::new(resolve_canon(&ret, uf, map, next)),
        ),
        other => other,
    }
}

pub(super) fn resolve_expr_canon(
    expr: &mut Expr,
    uf: &mut UnionFind,
    map: &mut HashMap<usize, usize>,
    next: &mut usize,
) {
    expr.ty = resolve_canon(&expr.ty, uf, map, next);
    match &mut expr.kind {
        ExprKind::Num(_) | ExprKind::Var(_) | ExprKind::Intrinsic => {}
        ExprKind::Call(_, args) => {
            for arg in args {
                resolve_expr_canon(arg, uf, map, next);
            }
        }
    }
}

/// Resolve a type to a fully concrete type against the union-find. A variable that
/// is still free after solving and defaulting is a genuine unresolved type.
pub(super) fn resolve_concrete(ty: &Type, uf: &mut UnionFind, loc: Loc) -> FloResult<Type> {
    match uf.resolve_head(ty) {
        Type::T(_) => Err(FloErr::UnresolvedType { loc }),
        Type::Fn(args, ret) => {
            let args = args
                .iter()
                .map(|a| resolve_concrete(a, uf, loc))
                .collect::<FloResult<_>>()?;
            let ret = resolve_concrete(&ret, uf, loc)?;
            Ok(Type::Fn(args, Box::new(ret)))
        }
        other => Ok(other),
    }
}

/// The largest type-variable id used anywhere in the module.
pub(super) fn max_var_id(module: &crate::ast::Module) -> usize {
    fn ty_max(ty: &Type) -> usize {
        match ty {
            Type::T(id) => *id,
            Type::Fn(args, ret) => args.iter().map(ty_max).max().unwrap_or(0).max(ty_max(ret)),
            _ => 0,
        }
    }

    fn expr_max(expr: &Expr) -> usize {
        let mut m = ty_max(&expr.ty);
        if let ExprKind::Call(_, args) = &expr.kind {
            for arg in args {
                m = m.max(expr_max(arg));
            }
        }
        m
    }

    module
        .funcs
        .values()
        .flatten()
        .map(|f| ty_max(&f.ty).max(expr_max(&f.body)))
        .max()
        .unwrap_or(0)
}
