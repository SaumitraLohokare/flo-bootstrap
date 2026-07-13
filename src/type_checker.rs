///! DISCLAIMER: Claude generated
use std::collections::{HashMap, HashSet};

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeKind},
};

/// A residual kind bound: "the variable with this (canonical) id in a function's
/// signature must satisfy this kind". These are the leftover constraints a
/// function's `Type::Fn` signature can't itself express — e.g. `fn bar() -> 'a =
/// 0` has signature `() -> t0` but `t0` must be `Numeric`. The `Loc` records
/// where the bound was introduced, for error reporting.
type Residual = (usize, TypeKind, Loc);

pub struct TypeChecker<'a> {
    module: &'a mut Module,
    /// Per-function leftover kind bounds, keyed by function name. Together with
    /// the function's (in-place mutated) `func.ty`, this *is* the function's
    /// schema — no separate signature copy is kept.
    residuals: HashMap<String, Vec<Residual>>,
    /// Monotonic source of fresh type-variable ids for schema instantiation.
    fresh: usize,
}

impl<'a> TypeChecker<'a> {
    pub fn new(module: &'a mut Module) -> Self {
        Self {
            module,
            residuals: HashMap::new(),
            fresh: 0,
        }
    }

    pub fn check(mut self) -> Vec<FloErr> {
        // SCCs in reverse-topological order (callees before callers). Collect
        // owned names so we no longer borrow the module and can mutate it below.
        let sccs: Vec<Vec<String>> = CallGraph::build(self.module)
            .into_iter()
            .map(|scc| scc.into_iter().map(str::to_string).collect())
            .collect();

        // Fresh ids minted while freshening callee schemas must never collide
        // with the ids the parser already baked into the module, so start above
        // every existing id. Canonical ids produced per function are always
        // smaller than this, so they never collide with freshened ids either.
        self.fresh = max_var_id(self.module) + 1;

        let mut errs = Vec::new();

        for scc in &sccs {
            // Bottom-up pass: iterate the SCC to a fixpoint, mutating each
            // member's `func.ty`/`func.body` in place. A non-recursive singleton
            // settles almost immediately; a recursive SCC keeps recomputing every
            // member off the previous iteration's schemas until nothing changes.
            let mut errored: HashSet<&str> = HashSet::new();
            loop {
                let mut changed = false;
                for name in scc {
                    if !self.module.funcs.contains_key(name) || errored.contains(name.as_str()) {
                        continue; // external/undefined callee, or already errored
                    }

                    match solve_func(name, self.module, &self.residuals, &mut self.fresh) {
                        Ok((ty, body, residual)) => {
                            // The signature/residual drives fixpoint convergence
                            // (that's what callers depend on); the body always
                            // gets its resolved types written back.
                            let sig_changed = self.module.funcs[name].ty != ty
                                || !residual_same(self.residuals.get(name), &residual);

                            let func = self.module.funcs.get_mut(name).unwrap();
                            func.ty = ty;
                            func.body = body;
                            self.residuals.insert(name.clone(), residual);

                            if sig_changed {
                                changed = true;
                            }
                        }
                        Err(err) => {
                            errs.push(err);
                            errored.insert(name);
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
        }

        // The specialize pass instantiates the polymorphic schemas we just learned.
        // It pushes concrete types downward from `main`, so if the bottom-up pass
        // already found errors the schemas are untrustworthy — bail out first.
        if !errs.is_empty() {
            return errs;
        }

        // Specialize pass: monomorphize every function reachable from `main`, one
        // instance per distinct set of concrete argument types. `main` takes no
        // arguments (the parser guarantees it exists), so it's the single root.
        let mono = {
            let mut spec = Specializer {
                module: &*self.module,
                residuals: &self.residuals,
                fresh: self.fresh,
                instances: HashMap::new(),
            };
            if let Err(err) = spec.specialize("main", &[], None) {
                return vec![err];
            }
            spec.into_module()
        };

        // Replace the module with the monomorphized instances. Functions never
        // reached from `main` are simply dropped — they stay polymorphic and no
        // code is emitted for them.
        self.module.funcs = mono;

        errs
    }
}

/// Solve a single function's body against the current schemas of its callees,
/// returning its resolved signature, resolved body, and residual kind bounds.
/// Does *not* default or monomorphize — free variables stay free.
fn solve_func(
    name: &str,
    module: &Module,
    residuals: &HashMap<String, Vec<Residual>>,
    fresh: &mut usize,
) -> FloResult<(Type, Expr, Vec<Residual>)> {
    let func = &module.funcs[name];

    let Type::Fn(_, ret_ty) = &func.ty else {
        unreachable!()
    };

    // 1. Generate constraints.
    let mut eqs: Vec<(Type, Type, Loc)> = Vec::new();
    let mut kinds: Vec<(Type, TypeKind, Loc)> = Vec::new();

    // The body's type must equal the (possibly annotated) return type.
    eqs.push((func.body.ty.clone(), (**ret_ty).clone(), func.loc.ret_type));
    gen_expr(&func.body, module, residuals, fresh, &mut eqs, &mut kinds)?;

    // 2. Solve. Unification is order-independent and there are no overload sets
    // to narrow, so a single pass suffices: unify all equalities, then apply the
    // kind bounds (which check against any concrete type a variable was pinned to).
    let mut uf = solve(&eqs, &kinds)?;

    // 3. Read off the schema. Canonicalize the signature first so residual bounds
    // are keyed to interface variables; internal literal vars get canonical ids
    // afterwards while resolving the body (shared `canon`/`next`).
    let mut canon: HashMap<usize, usize> = HashMap::new();
    let mut next = 0usize;

    let resolved_ty = resolve_canon(&func.ty, &mut uf, &mut canon, &mut next);

    let mut residual: Vec<Residual> = Vec::new();
    for (&rep, &cid) in &canon {
        if let Some(&(kind, loc)) = uf.kind.get(&rep) {
            residual.push((cid, kind, loc));
        }
    }
    residual.sort_by_key(|r| r.0);

    let mut resolved_body = func.body.clone();
    resolve_expr_canon(&mut resolved_body, &mut uf, &mut canon, &mut next);

    Ok((resolved_ty, resolved_body, residual))
}

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
struct Specializer<'m> {
    module: &'m Module,
    residuals: &'m HashMap<String, Vec<Residual>>,
    /// Continues the bottom-up pass's monotonic id counter, so instantiation here
    /// never reuses an id the earlier pass minted.
    fresh: usize,
    instances: HashMap<InstanceKey, Option<Func>>,
}

impl<'m> Specializer<'m> {
    /// Specialize `name` at the concrete argument types `args`, returning its
    /// concrete return type. `expected_ret` is the return type the *caller*
    /// demands: `Some` at every ordinary call site (whose result type is already
    /// known), `None` only at the root (`main`, whose return is fixed by its own
    /// body). Threading it in is what lets a return-polymorphic callee adopt the
    /// caller's type instead of falling back to a default. Memoized: a repeat (or
    /// recursive) request returns the registered instance instead of re-solving,
    /// which is what makes recursion terminate.
    fn specialize(
        &mut self,
        name: &str,
        args: &[Type],
        expected_ret: Option<Type>,
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

        let func = &self.module.funcs[name];
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

        for ((fp, a), aloc) in fparams.iter().zip(args).zip(&func.loc.arg_types) {
            eqs.push((fp.clone(), a.clone(), *aloc));
        }
        if let Some(er) = &expected_ret {
            eqs.push((fret.clone(), er.clone(), func.loc.ret_type));
        }
        eqs.push((body.ty.clone(), fret.clone(), func.loc.ret_type));
        gen_expr(
            &body,
            self.module,
            self.residuals,
            &mut self.fresh,
            &mut eqs,
            &mut kinds,
        )?;

        freshen_residuals(self.residuals.get(name), &mut map, &mut self.fresh, &mut kinds);

        // Solve. Concrete argument types (and the demanded return type) have flowed
        // in through the equalities.
        let mut uf = solve(&eqs, &kinds)?;

        // Defaulting is the last resort (§7): only after real information — the
        // caller's demanded return type included — has been propagated do the
        // still-free numeric variables become `i32`.
        default_free(&mut uf);

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
            ExprKind::Num(_) | ExprKind::Var(_) => {}
            ExprKind::Call(name, args) => {
                let old_args = std::mem::take(args);
                let mut new_args = Vec::with_capacity(old_args.len());
                for arg in old_args {
                    new_args.push(self.resolve_and_specialize(arg, uf)?);
                }
                let arg_tys: Vec<Type> = new_args.iter().map(|a| a.ty.clone()).collect();

                let callee = name.clone();
                self.specialize(&callee, &arg_tys, Some(this_ret.clone()))?;
                *name = self.mangle(&callee, &arg_tys, &this_ret);
                *args = new_args;
            }
        }

        Ok(expr)
    }

    /// Collapse the memo table into a module: one `Func` per finished instance,
    /// keyed by its mangled name.
    fn into_module(self) -> HashMap<String, Func> {
        let mut funcs = HashMap::new();
        for ((name, args, ret), inst) in &self.instances {
            if let Some(func) = inst {
                funcs.insert(self.mangle(name, args, ret), func.clone());
            }
        }
        funcs
    }

    /// The name a monomorphic instance is stored under. The source name for the
    /// entry point (`main` stays `main`) and for a zero-argument, return-monomorphic
    /// function; otherwise the arg types are appended (`foo$u8`), plus the return
    /// type when the function is *return-polymorphic* (`zero$u8`) so its distinct
    /// instances don't collide.
    fn mangle(&self, name: &str, args: &[Type], ret: &Type) -> String {
        if name == "main" {
            return "main".to_string();
        }

        let mut s = name.to_string();
        for a in args {
            s.push('$');
            s.push_str(&type_tag(a));
        }
        if schema_return_polymorphic(&self.module.funcs[name].ty) {
            s.push('$');
            s.push_str(&type_tag(ret));
        }
        s
    }
}

/// Freshen every type variable inside an expression tree (in place), consistent
/// with an in-progress instantiation `map`.
fn freshen_expr(expr: &mut Expr, map: &mut HashMap<usize, usize>, fresh: &mut usize) {
    expr.ty = freshen(&expr.ty, map, fresh);
    match &mut expr.kind {
        ExprKind::Num(_) | ExprKind::Var(_) => {}
        ExprKind::Call(_, args) => {
            for arg in args {
                freshen_expr(arg, map, fresh);
            }
        }
    }
}

/// Bind every still-free variable that carries a kind bound to that kind's default
/// concrete type (`Numeric` → `i32`). Real information has already been unified in,
/// so this only touches variables nothing else pinned down.
fn default_free(uf: &mut UnionFind) {
    let bounds: Vec<usize> = uf.kind.keys().copied().collect();
    for var in bounds {
        let root = uf.find(var);
        if !uf.binding.contains_key(&root) {
            if let Some(&(kind, _)) = uf.kind.get(&root) {
                uf.binding.insert(root, kind.default_type());
            }
        }
    }
}

/// Resolve a type to a fully concrete type against the union-find. A variable that
/// is still free after solving and defaulting is a genuine unresolved type.
fn resolve_concrete(ty: &Type, uf: &mut UnionFind, loc: Loc) -> FloResult<Type> {
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

/// Whether a function's return type is polymorphic in a way its arguments can't
/// pin down: it contains a free variable that none of the parameters mention. Such
/// a function's instances must be distinguished (and mangled) by return type.
fn schema_return_polymorphic(ty: &Type) -> bool {
    let Type::Fn(params, ret) = ty else {
        return false;
    };

    let mut param_vars = HashSet::new();
    for p in params {
        collect_vars(p, &mut param_vars);
    }
    let mut ret_vars = HashSet::new();
    collect_vars(ret, &mut ret_vars);

    ret_vars.iter().any(|v| !param_vars.contains(v))
}

fn collect_vars(ty: &Type, out: &mut HashSet<usize>) {
    match ty {
        Type::T(id) => {
            out.insert(*id);
        }
        Type::Fn(args, ret) => {
            for a in args {
                collect_vars(a, out);
            }
            collect_vars(ret, out);
        }
        _ => {}
    }
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
    }
}

/// Walk an expression, emitting equality and kind constraints. Calls instantiate
/// the callee's current schema with freshened variables (§5.1 of the guide).
fn gen_expr(
    expr: &Expr,
    module: &Module,
    residuals: &HashMap<String, Vec<Residual>>,
    fresh: &mut usize,
    eqs: &mut Vec<(Type, Type, Loc)>,
    kinds: &mut Vec<(Type, TypeKind, Loc)>,
) -> FloResult<()> {
    match &expr.kind {
        ExprKind::Num(_) => kinds.push((expr.ty.clone(), TypeKind::Integral, expr.loc)),
        // A variable reference already shares its parameter's type variable, so
        // there's nothing to relate here.
        ExprKind::Var(_) => {}
        ExprKind::Call(name, args) => {
            for arg in args {
                gen_expr(arg, module, residuals, fresh, eqs, kinds)?;
            }

            let callee = module.funcs.get(name).ok_or(FloErr::UndefinedFunction {
                name: name.clone(),
                loc: expr.loc,
            })?;

            let Type::Fn(params, ret) = &callee.ty else {
                unreachable!()
            };

            if params.len() != args.len() {
                return Err(FloErr::CallArityMismatch {
                    expected: params.len(),
                    got: args.len(),
                    loc: expr.loc,
                });
            }

            // Freshen the callee's schema (signature + residuals) so its type
            // variables are independent of every other call site.
            let mut map: HashMap<usize, usize> = HashMap::new();
            for (arg, param) in args.iter().zip(params) {
                let fp = freshen(param, &mut map, fresh);
                eqs.push((arg.ty.clone(), fp, arg.loc));
            }
            let fret = freshen(ret, &mut map, fresh);
            eqs.push((expr.ty.clone(), fret, expr.loc));

            freshen_residuals(residuals.get(name), &mut map, fresh, kinds);
        }
    }

    Ok(())
}

/// Solve a constraint set: unify every equality, then apply every kind bound
/// (which checks against whatever concrete type a variable was pinned to). Order
/// doesn't matter — there are no overload sets to narrow — so one pass suffices.
fn solve(
    eqs: &[(Type, Type, Loc)],
    kinds: &[(Type, TypeKind, Loc)],
) -> FloResult<UnionFind> {
    let mut uf = UnionFind::new();
    for (a, b, loc) in eqs {
        uf.unify(a, b, *loc)?;
    }
    for (t, kind, loc) in kinds {
        uf.add_kind(t, *kind, *loc)?;
    }
    Ok(uf)
}

/// Freshen a callee's residual kind bounds into `kinds`, reusing the same
/// instantiation `map` as the rest of that call's freshened signature so shared
/// variables stay linked.
fn freshen_residuals(
    residuals: Option<&Vec<Residual>>,
    map: &mut HashMap<usize, usize>,
    fresh: &mut usize,
    kinds: &mut Vec<(Type, TypeKind, Loc)>,
) {
    if let Some(res) = residuals {
        for &(vid, kind, loc) in res {
            let ft = freshen(&Type::T(vid), map, fresh);
            kinds.push((ft, kind, loc));
        }
    }
}

/// Produce a fresh copy of a type: each schema variable maps to a brand-new id,
/// consistently within one instantiation (`map`).
fn freshen(ty: &Type, map: &mut HashMap<usize, usize>, fresh: &mut usize) -> Type {
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

/// Resolve a type against the union-find, renumbering its free variables into a
/// canonical namespace (shared `map`/`next` keeps a whole function consistent).
fn resolve_canon(
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

fn resolve_expr_canon(
    expr: &mut Expr,
    uf: &mut UnionFind,
    map: &mut HashMap<usize, usize>,
    next: &mut usize,
) {
    expr.ty = resolve_canon(&expr.ty, uf, map, next);
    match &mut expr.kind {
        ExprKind::Num(_) | ExprKind::Var(_) => {}
        ExprKind::Call(_, args) => {
            for arg in args {
                resolve_expr_canon(arg, uf, map, next);
            }
        }
    }
}

/// Compare a stored residual list against a freshly computed one, ignoring locs
/// (they're informational and don't affect the schema's semantic shape).
fn residual_same(stored: Option<&Vec<Residual>>, new: &[Residual]) -> bool {
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

/// The largest type-variable id used anywhere in the module.
fn max_var_id(module: &Module) -> usize {
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
        .map(|f| ty_max(&f.ty).max(expr_max(&f.body)))
        .max()
        .unwrap_or(0)
}

/// Union-find over type variables with concrete-type bindings and `Numeric`-style
/// kind bounds. Structural unification for compound (function) types keeps it
/// extensible for pointers/arrays later.
struct UnionFind {
    parent: HashMap<usize, usize>,
    /// Representative -> the concrete type its set resolved to.
    binding: HashMap<usize, Type>,
    /// Representative -> its kind bound (and where the bound was introduced).
    kind: HashMap<usize, (TypeKind, Loc)>,
}

impl UnionFind {
    fn new() -> Self {
        Self {
            parent: HashMap::new(),
            binding: HashMap::new(),
            kind: HashMap::new(),
        }
    }

    fn find(&mut self, id: usize) -> usize {
        let mut root = id;
        while let Some(&p) = self.parent.get(&root) {
            if p == root {
                break;
            }
            root = p;
        }
        // Path compression.
        let mut cur = id;
        while cur != root {
            let next = *self.parent.get(&cur).unwrap_or(&root);
            self.parent.insert(cur, root);
            cur = next;
        }
        root
    }

    /// Resolve a type to its head: a concrete type, or its free representative.
    fn resolve_head(&mut self, ty: &Type) -> Type {
        match ty {
            Type::T(id) => {
                let r = self.find(*id);
                match self.binding.get(&r) {
                    Some(t) => t.clone(),
                    None => Type::T(r),
                }
            }
            other => other.clone(),
        }
    }

    fn unify(&mut self, a: &Type, b: &Type, loc: Loc) -> FloResult<()> {
        let ra = self.resolve_head(a);
        let rb = self.resolve_head(b);

        match (ra, rb) {
            (Type::T(ia), Type::T(ib)) => {
                if ia == ib {
                    return Ok(());
                }
                // Neither is bound (else resolve_head returned concrete), so just
                // union and carry any kind bound over to the new root.
                self.parent.insert(ia, ib);
                if let Some(k) = self.kind.remove(&ia) {
                    self.kind.entry(ib).or_insert(k);
                }
                Ok(())
            }
            (Type::T(i), concrete) | (concrete, Type::T(i)) => self.bind(i, concrete, loc),
            (c1, c2) => self.unify_concrete(&c1, &c2, loc),
        }
    }

    fn bind(&mut self, var: usize, concrete: Type, loc: Loc) -> FloResult<()> {
        let root = self.find(var);
        if let Some(existing) = self.binding.get(&root).cloned() {
            return self.unify_concrete(&existing, &concrete, loc);
        }
        if let Some(&(kind, kloc)) = self.kind.get(&root) {
            if !kind.satisfies_type(&concrete) {
                return Err(FloErr::UnsatisfiedTypeKind {
                    ty: concrete,
                    ty_loc: loc,
                    kind,
                    loc: kloc,
                });
            }
        }
        self.binding.insert(root, concrete);
        Ok(())
    }

    fn unify_concrete(&mut self, a: &Type, b: &Type, loc: Loc) -> FloResult<()> {
        match (a, b) {
            (Type::Fn(pa, ra), Type::Fn(pb, rb)) => {
                if pa.len() != pb.len() {
                    return Err(FloErr::TypeMismatch {
                        t1: a.clone(),
                        loc1: loc,
                        t2: b.clone(),
                        loc2: loc,
                    });
                }
                for (x, y) in pa.iter().zip(pb) {
                    self.unify(x, y, loc)?;
                }
                self.unify(ra, rb, loc)
            }
            _ if a == b => Ok(()),
            _ => Err(FloErr::TypeMismatch {
                t1: a.clone(),
                loc1: loc,
                t2: b.clone(),
                loc2: loc,
            }),
        }
    }

    fn add_kind(&mut self, ty: &Type, kind: TypeKind, loc: Loc) -> FloResult<()> {
        match self.resolve_head(ty) {
            Type::T(root) => {
                self.kind.entry(root).or_insert((kind, loc));
                Ok(())
            }
            concrete => {
                if kind.satisfies_type(&concrete) {
                    Ok(())
                } else {
                    Err(FloErr::UnsatisfiedTypeKind {
                        ty: concrete,
                        ty_loc: loc,
                        kind,
                        loc,
                    })
                }
            }
        }
    }
}

struct CallGraph;

impl CallGraph {
    fn build<'a>(module: &'a Module) -> Vec<Vec<&'a str>> {
        let mut adj = HashMap::new();

        for (name, func) in &module.funcs {
            adj.insert(name.as_str(), Self::collect_calls(&func.body));
        }

        Self::tarjans(adj)
    }

    fn collect_calls<'a>(expr: &'a Expr) -> Vec<&'a str> {
        use ExprKind::*;
        let mut calls = Vec::new();

        match &expr.kind {
            Num(_) | Var(_) => {}
            Call(name, args) => {
                calls.push(name.as_str());
                for arg in args {
                    calls.extend(Self::collect_calls(&arg));
                }
            }
        }

        calls
    }

    /// Returns SCCs in reverse topological order w.r.t. the call edges.
    /// That means: if A calls B (edge A -> B), then B's SCC appears
    /// *before* A's SCC in the returned Vec. This is the natural order
    /// for e.g. bottom-up type inference or bottom-up analysis, since
    /// callees are processed before callers.
    fn tarjans<'a>(graph: HashMap<&'a str, Vec<&'a str>>) -> Vec<Vec<&'a str>> {
        struct TarjanState<'a> {
            index: HashMap<&'a str, usize>,
            lowlink: HashMap<&'a str, usize>,
            on_stack: HashMap<&'a str, bool>,
            stack: Vec<&'a str>,
            next_index: usize,
            sccs: Vec<Vec<&'a str>>,
        }

        fn strongconnect<'a>(
            v: &'a str,
            graph: &HashMap<&'a str, Vec<&'a str>>,
            st: &mut TarjanState<'a>,
        ) {
            st.index.insert(v, st.next_index);
            st.lowlink.insert(v, st.next_index);
            st.next_index += 1;
            st.stack.push(v);
            st.on_stack.insert(v, true);

            if let Some(succs) = graph.get(v) {
                for &w in succs {
                    if !st.index.contains_key(w) {
                        // w not yet visited: recurse
                        strongconnect(w, graph, st);
                        let w_low = st.lowlink[w];
                        let v_low = st.lowlink[v];
                        st.lowlink.insert(v, v_low.min(w_low));
                    } else if *st.on_stack.get(w).unwrap_or(&false) {
                        // w is on stack: back edge, use its index
                        let w_idx = st.index[w];
                        let v_low = st.lowlink[v];
                        st.lowlink.insert(v, v_low.min(w_idx));
                    }
                    // else: w visited, not on stack -> already in a
                    // completed SCC, ignore (cross edge).
                }
            }

            // If v is a root node, pop the stack and produce an SCC.
            if st.lowlink[v] == st.index[v] {
                let mut component = Vec::new();
                loop {
                    let w = st.stack.pop().expect("stack not empty");
                    st.on_stack.insert(w, false);
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                st.sccs.push(component);
            }
        }

        let mut st = TarjanState {
            index: HashMap::new(),
            lowlink: HashMap::new(),
            on_stack: HashMap::new(),
            stack: Vec::new(),
            next_index: 0,
            sccs: Vec::new(),
        };

        // Iterate over all known nodes (both callers and any callees
        // that might not be keys in `adj`, e.g. external/undefined funcs).
        let mut all_nodes: Vec<&'a str> = graph.keys().copied().collect();
        for callees in graph.values() {
            for &c in callees {
                if !graph.contains_key(c) {
                    all_nodes.push(c);
                }
            }
        }
        all_nodes.sort();
        all_nodes.dedup();

        for node in all_nodes {
            if !st.index.contains_key(node) {
                strongconnect(node, &graph, &mut st);
            }
        }

        st.sccs
    }
}

#[cfg(test)]
mod tests {
    use super::TypeChecker;
    use crate::{errors::FloErr, parser::Parser, tokenizer::Tokenizer};

    /// Run the whole pipeline (tokenize → parse → type-check + specialize) and,
    /// on success, return the monomorphized functions as sorted `fn … = …;` lines.
    /// Sorting makes the result independent of the `funcs` HashMap's iteration
    /// order so tests can compare against a fixed set.
    fn compile(src: &str) -> Result<Vec<String>, Vec<FloErr>> {
        let src = src.to_string();
        let tokens = Tokenizer::new(&src).tokenize();
        let mut module = Parser::new(tokens).parse().map_err(|e| vec![e])?;

        let errs = TypeChecker::new(&mut module).check();
        if !errs.is_empty() {
            return Err(errs);
        }

        let dump = format!("{module:?}");
        let mut lines: Vec<String> = dump
            .lines()
            .filter(|l| l.starts_with("fn "))
            .map(str::to_string)
            .collect();
        lines.sort();
        Ok(lines)
    }

    /// Assert that `src` type-checks and produces exactly `expected` (order
    /// insensitive).
    fn assert_funcs(src: &str, expected: &[&str]) {
        let got = compile(src).expect("expected a successful type check");
        let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn numeric_literal_defaults_to_i32() {
        // Nothing constrains the literal, so defaulting (the last resort) picks i32.
        assert_funcs("fn main() = 0;", &["fn main() -> i32 = 0:i32;"]);
    }

    #[test]
    fn polymorphic_passthrough_chain() {
        // `bar` is monomorphized at i32; the annotation on `foo` pins the literal.
        assert_funcs(
            "fn main() = foo();
             fn foo() -> i32 = bar(0);
             fn bar(x: 'a) -> 'a = x;",
            &[
                "fn main() -> i32 = foo():i32;",
                "fn foo() -> i32 = bar$i32(0:i32):i32;",
                "fn bar$i32(i32) -> i32 = var_0:i32;",
            ],
        );
    }

    #[test]
    fn self_recursion_terminates() {
        // The recursive call resolves to the in-progress `fact$i32` instance
        // instead of specializing forever.
        assert_funcs(
            "fn main() = fact(id(0));
             fn fact(n: i32) -> i32 = fact(n);
             fn id(x: 'a) -> 'a = x;",
            &[
                "fn main() -> i32 = fact$i32(id$i32(0:i32):i32):i32;",
                "fn fact$i32(i32) -> i32 = fact$i32(var_0:i32):i32;",
                "fn id$i32(i32) -> i32 = var_0:i32;",
            ],
        );
    }

    #[test]
    fn caller_context_beats_defaulting() {
        // Regression: `zero`'s free numeric return must adopt the `u8` demanded by
        // `foo`'s parameter, *not* default to i32. It's return-polymorphic, so its
        // instance is mangled with the return type (`zero$u8`).
        assert_funcs(
            "fn zero() = 0;
             fn foo(x: u8) = x;
             fn main() = foo(zero());",
            &[
                "fn main() -> u8 = foo$u8(zero$u8():u8):u8;",
                "fn foo$u8(u8) -> u8 = var_0:u8;",
                "fn zero$u8() -> u8 = 0:u8;",
            ],
        );
    }

    #[test]
    fn unconstrained_return_polymorphic_defaults() {
        // With no caller context, `zero`'s return falls back to the i32 default.
        assert_funcs(
            "fn main() = zero();
             fn zero() = 0;",
            &[
                "fn main() -> i32 = zero$i32():i32;",
                "fn zero$i32() -> i32 = 0:i32;",
            ],
        );
    }

    #[test]
    fn undefined_function_is_an_error() {
        let errs = compile("fn main() = nope();").expect_err("call to unknown fn");
        assert!(matches!(errs[0], FloErr::UndefinedFunction { .. }));
    }

    #[test]
    fn call_arity_mismatch_is_an_error() {
        let errs = compile(
            "fn main() = foo(0);
             fn foo() = 0;",
        )
        .expect_err("too many arguments");
        assert!(matches!(errs[0], FloErr::CallArityMismatch { .. }));
    }

    #[test]
    fn kind_violation_is_an_error() {
        // `0` is `Integral`, but the parameter it flows into is `void`.
        let errs = compile(
            "fn takes_void(x: void) = x;
             fn main() = takes_void(0);",
        )
        .expect_err("integral literal used where void required");
        assert!(matches!(errs[0], FloErr::UnsatisfiedTypeKind { .. }));
    }
}
