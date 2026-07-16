use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeKind},
};

use super::subst::{freshen, resolve_canon, resolve_expr_canon};
use super::unify::UnionFind;
use super::Residual;

/// A deferred call: which name is called, the argument types (with their locs for
/// error reporting), the call's own result type, and where the call is. Overload
/// resolution consumes these — a call is only turned into equality/kind
/// constraints once it's been narrowed to a single overload.
pub(super) struct Obligation {
    pub name: String,
    pub args: Vec<(Type, Loc)>,
    pub result: Type,
    pub loc: Loc,
}

/// Solve a single overload's body against the current schemas of its callees,
/// returning its resolved signature, resolved body, and residual kind bounds.
/// Does *not* default or monomorphize — free variables stay free, and calls whose
/// overload set can't yet be narrowed to one candidate stay unresolved (that's
/// deferred to the specialize pass, where argument types are concrete).
pub(super) fn solve_func(
    name: &str,
    index: usize,
    module: &Module,
    residuals: &HashMap<String, Vec<Vec<Residual>>>,
    fresh: &mut usize,
) -> FloResult<(Type, Expr, Vec<Residual>)> {
    let func = &module.funcs[name][index];

    let Type::Fn(_, ret_ty) = &func.ty else {
        unreachable!()
    };

    // 1. Generate constraints.
    let mut eqs: Vec<(Type, Type, Loc)> = Vec::new();
    let mut kinds: Vec<(Type, TypeKind, Loc)> = Vec::new();
    let mut calls: Vec<Obligation> = Vec::new();

    // The body's type must equal the (possibly annotated) return type.
    eqs.push((func.body.ty.clone(), (**ret_ty).clone(), func.loc.ret_type));
    gen_expr(&func.body, &mut eqs, &mut kinds, &mut calls);

    // 2. Solve. Non-strict: ambiguous calls (2+ possible overloads) are left
    // unresolved rather than erroring, because concrete argument types at a later
    // specialization may narrow them.
    let mut uf = solve(&eqs, &kinds, &calls, module, residuals, fresh, false)?;

    // 3. Read off the schema. Canonicalize the signature first so residual bounds
    // are keyed to interface variables; internal literal vars get canonical ids
    // afterwards while resolving the body (shared `canon`/`next`).
    let mut canon: HashMap<usize, usize> = HashMap::new();
    let mut next = 0usize;

    let resolved_ty = resolve_canon(&func.ty, &mut uf, &mut canon, &mut next);

    let mut residual: Vec<Residual> = Vec::new();
    for (&rep, &cid) in &canon {
        if let Some(&(kind, loc)) = uf.kind_bound(rep) {
            residual.push((cid, kind, loc));
        }
    }
    residual.sort_by_key(|r| r.0);

    let mut resolved_body = func.body.clone();
    resolve_expr_canon(&mut resolved_body, &mut uf, &mut canon, &mut next);

    Ok((resolved_ty, resolved_body, residual))
}

/// Walk an expression, emitting equality/kind constraints for literals and
/// recording every call as an `Obligation` for the overload-resolving solver.
pub(super) fn gen_expr(
    expr: &Expr,
    eqs: &mut Vec<(Type, Type, Loc)>,
    kinds: &mut Vec<(Type, TypeKind, Loc)>,
    calls: &mut Vec<Obligation>,
) {
    match &expr.kind {
        ExprKind::Num(_) => kinds.push((expr.ty.clone(), TypeKind::Integral, expr.loc)),
        // A variable reference already shares its parameter's type variable, so
        // there's nothing to relate here. A built-in's body is a sentinel with no
        // constraints of its own.
        ExprKind::Var(_) | ExprKind::Intrinsic => {}
        ExprKind::Call(name, args) => {
            for arg in args {
                gen_expr(arg, eqs, kinds, calls);
            }
            calls.push(Obligation {
                name: name.clone(),
                args: args.iter().map(|a| (a.ty.clone(), a.loc)).collect(),
                result: expr.ty.clone(),
                loc: expr.loc,
            });
        }
    }
}

/// Solve a constraint set with overload resolution. Unify every equality and
/// apply every kind bound, then repeatedly narrow each call's overload set
/// against the *current* solution: any call down to exactly one candidate is
/// committed (its signature + residuals are unified in), which may in turn narrow
/// others. A call with zero candidates is always an error; a call still stuck at
/// two or more is an ambiguity error when `strict`, or left unresolved otherwise.
pub(super) fn solve(
    eqs: &[(Type, Type, Loc)],
    kinds: &[(Type, TypeKind, Loc)],
    calls: &[Obligation],
    module: &Module,
    residuals: &HashMap<String, Vec<Vec<Residual>>>,
    fresh: &mut usize,
    strict: bool,
) -> FloResult<UnionFind> {
    let mut uf = UnionFind::new();
    for (a, b, loc) in eqs {
        uf.unify(a, b, *loc)?;
    }
    for (t, kind, loc) in kinds {
        uf.add_kind(t, *kind, *loc)?;
    }

    let mut pending: Vec<&Obligation> = calls.iter().collect();
    while !pending.is_empty() {
        let mut next: Vec<&Obligation> = Vec::new();
        let mut changed = false;

        for call in pending {
            let cands = call_candidates(&mut uf, module, residuals, call);
            match cands.as_slice() {
                [idx] => {
                    commit(&mut uf, module, residuals, call, *idx, fresh)?;
                    changed = true;
                }
                // Pruning is monotonic (unification only makes types more
                // concrete, which only ever removes candidates), so zero
                // candidates now means zero forever — report it.
                [] => return Err(no_match_err(module, &call.name, call.args.len(), call.loc)),
                _ => next.push(call),
            }
        }

        // No singleton resolved this round: every remaining call is stuck on the
        // information available so far.
        if !changed {
            if strict {
                // Resolve → default → resolve (§7/§8.3). Real information has
                // already been propagated, so as a last resort default the still-
                // free numeric variables to `i32` and retry: pinning a bare
                // `1 + 2`'s operands to `i32` prunes the operator's overload set to
                // one. `default_free` reports whether it bound anything new, so we
                // only loop while defaulting makes progress (it can't spin, since
                // each pass binds at least one previously-free variable) and error
                // only once nothing free remains yet calls are still ambiguous.
                if uf.default_free() {
                    pending = next;
                    continue;
                }

                let call = next[0];
                let candidates = call_candidates(&mut uf, module, residuals, call).len();
                return Err(FloErr::AmbiguousCall {
                    name: call.name.clone(),
                    loc: call.loc,
                    candidates,
                });
            }
            break;
        }

        pending = next;
    }

    Ok(uf)
}

/// Resolve a call's argument and result types to their heads against the current
/// union-find, then return the indices of the overloads that could still match.
fn call_candidates(
    uf: &mut UnionFind,
    module: &Module,
    residuals: &HashMap<String, Vec<Vec<Residual>>>,
    call: &Obligation,
) -> Vec<usize> {
    let arg_heads: Vec<Type> = call.args.iter().map(|(t, _)| uf.resolve_head(t)).collect();
    let result_head = uf.resolve_head(&call.result);
    candidates(module, residuals, &call.name, &arg_heads, &result_head)
}

/// The overloads of `name` that could match a call with these (head-resolved)
/// argument and result types. Used both here and by the specialize pass. `args`
/// and `result` entries that are still type variables are treated permissively;
/// only concrete types (and the callee's residual kind bounds) can rule a
/// candidate out. `result` lets an otherwise-identical pair of overloads that
/// differ only by return type be told apart when the caller's context is known.
pub(super) fn candidates(
    module: &Module,
    residuals: &HashMap<String, Vec<Vec<Residual>>>,
    name: &str,
    args: &[Type],
    result: &Type,
) -> Vec<usize> {
    let Some(overloads) = module.funcs.get(name) else {
        return Vec::new();
    };
    let empty: Vec<Residual> = Vec::new();
    let res_lists = residuals.get(name);

    let mut out = Vec::new();
    for (i, func) in overloads.iter().enumerate() {
        let res = res_lists.and_then(|v| v.get(i)).unwrap_or(&empty);
        if overload_matches(func, res, args, result) {
            out.push(i);
        }
    }
    out
}

fn overload_matches(func: &Func, res: &[Residual], args: &[Type], result: &Type) -> bool {
    let Type::Fn(params, ret) = &func.ty else {
        return false;
    };
    if params.len() != args.len() {
        return false;
    }
    for (param, arg) in params.iter().zip(args) {
        if !compat(param, arg, res) {
            return false;
        }
    }
    compat(ret, result, res)
}

/// Could a call value of (head-resolved) type `actual` satisfy the schema slot
/// `sig`? A free `actual` rules nothing out; a schema variable accepts anything
/// its residual kind bound allows; concrete types must match structurally.
fn compat(sig: &Type, actual: &Type, res: &[Residual]) -> bool {
    if matches!(actual, Type::T(_)) {
        return true;
    }
    match sig {
        Type::T(vid) => res
            .iter()
            .find(|r| r.0 == *vid)
            .map_or(true, |r| r.1.satisfies_type(actual)),
        Type::Fn(sp, sr) => match actual {
            Type::Fn(ap, ar) => {
                sp.len() == ap.len()
                    && sp.iter().zip(ap).all(|(s, a)| compat(s, a, res))
                    && compat(sr, ar, res)
            }
            _ => false,
        },
        concrete => concrete == actual,
    }
}

/// Commit to a resolved overload: freshen its signature + residuals with a shared
/// instantiation map, then unify each argument against its parameter and the
/// call's result against the return type, adding the freshened residual kind
/// bounds. Mirrors the single-function constraint generation for a monomorphic
/// callee — the difference is only *which* overload we picked.
fn commit(
    uf: &mut UnionFind,
    module: &Module,
    residuals: &HashMap<String, Vec<Vec<Residual>>>,
    call: &Obligation,
    index: usize,
    fresh: &mut usize,
) -> FloResult<()> {
    let func = &module.funcs[&call.name][index];
    let Type::Fn(params, ret) = &func.ty else {
        unreachable!()
    };

    let mut map: HashMap<usize, usize> = HashMap::new();
    for ((arg_ty, arg_loc), param) in call.args.iter().zip(params) {
        let fp = freshen(param, &mut map, fresh);
        uf.unify(arg_ty, &fp, *arg_loc)?;
    }
    let fret = freshen(ret, &mut map, fresh);
    uf.unify(&call.result, &fret, call.loc)?;

    if let Some(res) = residuals.get(&call.name).and_then(|v| v.get(index)) {
        for &(vid, kind, loc) in res {
            let ft = freshen(&Type::T(vid), &mut map, fresh);
            uf.add_kind(&ft, kind, loc)?;
        }
    }

    Ok(())
}

/// The error for a call that matched no overload. An undefined name is reported
/// as such; a lone overload whose arity is wrong keeps the precise arity
/// diagnostic; anything else is a generic "no matching overload".
pub(super) fn no_match_err(module: &Module, name: &str, arg_count: usize, loc: Loc) -> FloErr {
    match module.funcs.get(name) {
        None => FloErr::UndefinedFunction {
            name: name.to_string(),
            loc,
        },
        Some(overloads) => {
            if overloads.len() == 1 {
                if let Type::Fn(params, _) = &overloads[0].ty {
                    if params.len() != arg_count {
                        return FloErr::CallArityMismatch {
                            expected: params.len(),
                            got: arg_count,
                            loc,
                        };
                    }
                }
            }
            FloErr::NoMatchingOverload {
                name: name.to_string(),
                loc,
            }
        }
    }
}

/// Compare a stored residual list against a freshly computed one, ignoring locs
/// (they're informational and don't affect the schema's semantic shape).
pub(super) fn residual_same(stored: Option<&Vec<Residual>>, new: &[Residual]) -> bool {
    match stored {
        None => new.is_empty(),
        Some(old) => {
            old.len() == new.len()
                && old
                    .iter()
                    .zip(new)
                    .all(|(a, b)| a.0 == b.0 && a.1 == b.1)
        }
    }
}
