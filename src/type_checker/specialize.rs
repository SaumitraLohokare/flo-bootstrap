use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeKind},
};

use super::Residual;
use super::infer::{Obligation, candidates, gen_expr, no_match_err, solve};
use super::subst::{freshen, freshen_expr, freshen_residuals, resolve_concrete};
use super::unify::UnionFind;

/// A monomorphic instance key: `(function name, concrete arg types, concrete
/// return type)`. The return type is part of the key because a function can be
/// *return-polymorphic* — a nullary numeric-returning function like `fn zero() =
/// 0` has schema `() -> t0` whose result is fixed by the *caller's* context, not
/// by its (empty) arguments. Such a function has genuinely distinct `zero() -> i32`
/// and `zero() -> u8` instances, which the arg types alone can't tell apart.
///
/// The instance itself is `Option<Func>`: `None` is the in-progress marker
/// registered before the body is built (so recursion terminates on the memo),
/// `Some` is the finished monomorphic function. The concrete return type isn't
/// stored separately — it's already the third element of the key.
type InstanceKey = (String, Vec<Type>, Type);

/// The specialize pass (§8.3). Starting from `main`, it turns each polymorphic
/// schema into concrete instances keyed by `InstanceKey`, recursing into every
/// reachable call and defaulting leftover numeric variables as a last resort. It
/// reads schemas out of the (already bottom-up-solved) `module` + `residuals` and
/// never mutates them.
pub(super) struct Specializer<'m> {
    module: &'m Module,
    residuals: &'m HashMap<String, Vec<Vec<Residual>>>,
    /// Continues the bottom-up pass's monotonic id counter, so instantiation here
    /// never reuses an id the earlier pass minted.
    fresh: usize,
    instances: HashMap<InstanceKey, Option<Func>>,
}

impl<'m> Specializer<'m> {
    pub(super) fn new(
        module: &'m Module,
        residuals: &'m HashMap<String, Vec<Vec<Residual>>>,
        fresh: usize,
    ) -> Self {
        Self {
            module,
            residuals,
            fresh,
            instances: HashMap::new(),
        }
    }

    /// Specialize `name` at the concrete argument types `args`, returning its
    /// concrete return type. `expected_ret` is the return type the *caller*
    /// demands: `Some` at every ordinary call site (whose result type is already
    /// known), `None` only at the root (`main`, whose return is fixed by its own
    /// body). Threading it in is what lets a return-polymorphic callee adopt the
    /// caller's type instead of falling back to a default. Memoized: a repeat (or
    /// recursive) request returns the registered instance instead of re-solving,
    /// which is what makes recursion terminate.
    pub(super) fn specialize(
        &mut self,
        name: &str,
        args: &[Type],
        expected_ret: Option<Type>,
        loc: Loc,
    ) -> FloResult<Type> {
        // Fast path: when the caller pinned the return type the full key is known
        // up front, so a hit — including an in-progress recursive instance —
        // returns immediately without re-solving.
        if let Some(ret) = &expected_ret {
            let key = (name.to_string(), args.to_vec(), ret.clone());
            if self.instances.contains_key(&key) {
                return Ok(ret.clone());
            }
        }

        // Pick the overload. By now the argument types (and, at a real call site,
        // the demanded return type) are concrete, so exactly one candidate must
        // remain — the same one the solve pass committed to. Zero or several is an
        // error surfaced here.
        let index = self.select_overload(name, args, &expected_ret, loc)?;
        let func = &self.module.funcs[name][index];
        let Type::Fn(params, ret) = &func.ty else {
            unreachable!()
        };

        // Instantiate this function's schema (§5.1): freshen the signature, the
        // body, and the residual kind bounds with one shared map so variables
        // shared between them stay linked.
        let mut map: HashMap<usize, usize> = HashMap::new();
        let fparams: Vec<Type> = params
            .iter()
            .map(|p| freshen(p, &mut map, &mut self.fresh))
            .collect();
        let fret = freshen(ret, &mut map, &mut self.fresh);
        let mut body = func.body.clone();
        freshen_expr(&mut body, &mut map, &mut self.fresh);

        // Constraints: pin each parameter to the concrete argument type, pin the
        // return to the caller's demanded type (if any), tie the body to the
        // return, then regenerate the body's own constraints (§4).
        let mut eqs: Vec<(Type, Type, Loc)> = Vec::new();
        let mut kinds: Vec<(Type, TypeKind, Loc)> = Vec::new();
        let mut calls: Vec<Obligation> = Vec::new();

        for ((fp, a), aloc) in fparams.iter().zip(args).zip(&func.loc.arg_types) {
            eqs.push((fp.clone(), a.clone(), *aloc));
        }
        if let Some(er) = &expected_ret {
            eqs.push((fret.clone(), er.clone(), func.loc.ret_type));
        }
        eqs.push((body.ty.clone(), fret.clone(), func.loc.ret_type));
        gen_expr(&body, &mut eqs, &mut kinds, &mut calls);

        freshen_residuals(
            self.residuals.get(name).and_then(|v| v.get(index)),
            &mut map,
            &mut self.fresh,
            &mut kinds,
        );

        // Solve. Concrete argument types (and the demanded return type) have flowed
        // in through the equalities, so overload resolution here is strict: every
        // call in the body must narrow to exactly one overload.
        let mut uf = solve(
            &eqs,
            &kinds,
            &calls,
            self.module,
            self.residuals,
            &mut self.fresh,
            true,
        )?;

        // Defaulting is the last resort (§7): only after real information — the
        // caller's demanded return type included — has been propagated do the
        // still-free numeric variables become `i32`.
        uf.default_free();

        // Read off the (now concrete) return type and register the instance
        // *before* descending into the body, so a recursive self-call finds it.
        // The full key is only known now, in the `None` (root) case.
        let ret_ty = resolve_concrete(&fret, &mut uf, func.loc.ret_type)?;
        let key = (name.to_string(), args.to_vec(), ret_ty.clone());
        if self.instances.contains_key(&key) {
            return Ok(ret_ty);
        }
        self.instances.insert(key.clone(), None);

        // Build the concrete body: resolve every type and, at each call, specialize
        // the callee at its concrete argument types and rewrite the call target to
        // that instance's mangled name.
        let concrete_body = self.resolve_and_specialize(body, &mut uf)?;

        let concrete_params: Vec<Type> = fparams
            .iter()
            .map(|p| resolve_concrete(p, &mut uf, func.loc.definition))
            .collect::<FloResult<_>>()?;

        let concrete_func = Func {
            body: concrete_body,
            ty: Type::Fn(concrete_params, Box::new(ret_ty.clone())),
            loc: func.loc.clone(),
        };
        *self.instances.get_mut(&key).unwrap() = Some(concrete_func);

        Ok(ret_ty)
    }

    /// Resolve an expression's types to concrete types and, for each call, recurse
    /// into `specialize` and rewrite the callee name to its monomorphic mangling.
    fn resolve_and_specialize(&mut self, mut expr: Expr, uf: &mut UnionFind) -> FloResult<Expr> {
        let loc = expr.loc;
        expr.ty = resolve_concrete(&expr.ty, uf, loc)?;
        // The call's own (now concrete) result type is exactly the return type the
        // callee must produce here.
        let this_ret = expr.ty.clone();

        match &mut expr.kind {
            ExprKind::Num(_) | ExprKind::Bool(_) | ExprKind::Var(_) | ExprKind::Intrinsic => {}
            ExprKind::Call(name, args) => {
                let old_args = std::mem::take(args);
                let mut new_args = Vec::with_capacity(old_args.len());
                for arg in old_args {
                    new_args.push(self.resolve_and_specialize(arg, uf)?);
                }
                let arg_tys: Vec<Type> = new_args.iter().map(|a| a.ty.clone()).collect();

                let callee = name.clone();
                self.specialize(&callee, &arg_tys, Some(this_ret.clone()), loc)?;
                *name = mangle(&callee, &arg_tys, &this_ret);
                *args = new_args;
            }
        }

        Ok(expr)
    }

    /// Choose the single overload of `name` that a call with these concrete
    /// argument types (and demanded return type, if any) resolves to. A missing
    /// demanded return type — only the root `main` — is permissive.
    fn select_overload(
        &self,
        name: &str,
        args: &[Type],
        expected_ret: &Option<Type>,
        loc: Loc,
    ) -> FloResult<usize> {
        let result = expected_ret.clone().unwrap_or(Type::T(usize::MAX));
        let cands = candidates(self.module, self.residuals, name, args, &result);
        match cands.as_slice() {
            [idx] => Ok(*idx),
            [] => Err(no_match_err(self.module, name, args.len(), loc)),
            _ => Err(FloErr::AmbiguousCall {
                name: name.to_string(),
                loc,
                candidates: cands.len(),
            }),
        }
    }

    /// Collapse the memo table into a module: one `Func` per finished instance,
    /// keyed by its mangled name.
    pub(super) fn into_module(self) -> HashMap<String, Func> {
        let mut funcs = HashMap::new();
        for ((name, args, ret), inst) in &self.instances {
            if let Some(func) = inst {
                funcs.insert(mangle(name, args, ret), func.clone());
            }
        }
        funcs
    }
}

/// The name a monomorphic instance is stored under. The entry point keeps its
/// source name (`main` stays `main`); every other instance is mangled with each
/// argument type followed by its return type (`foo$u8$u8`, `zero$i32`), so that
/// instances differing only in argument or return types never collide.
fn mangle(name: &str, args: &[Type], ret: &Type) -> String {
    if name == "main" {
        return "main".to_string();
    }

    let mut s = name.to_string();
    for a in args {
        s.push('$');
        s.push_str(&type_tag(a));
    }
    s.push('$');
    s.push_str(&type_tag(ret));
    s
}

fn type_tag(ty: &Type) -> String {
    match ty {
        Type::I32 => "i32".to_string(),
        Type::Void => "void".to_string(),
        Type::T(n) => format!("t{n}"),
        Type::Fn(args, ret) => {
            let args = args.iter().map(type_tag).collect::<Vec<_>>().join("_");
            format!("fn_{args}_{}", type_tag(ret))
        }
        Type::U8 => "u8".to_string(),
        Type::Bool => "bool".to_string(),
    }
}
