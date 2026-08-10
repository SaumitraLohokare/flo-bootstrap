use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use crate::{
    ast::{Expr, ExprKind, FieldInit, Func, Module, Statement, StmtKind},
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    type_checker::replace_set::ReplaceSet,
    types::{Type, TypeCase, TypeTable},
    util::Iota,
};

mod decls;
mod replace_set;

pub use decls::check_type_decls;

#[cfg(test)]
mod tests;

/// A fact collected from the AST. Collection is a single pass that never
/// consults the solver, so everything the checker knows about a function is in
/// one flat list before any of it is solved.
#[derive(Debug)]
enum Constraint {
    /// The two types must unify.
    IsEqual(Type, Type, Loc),

    /// The type variable belongs to an expression that diverges. Kept separate
    /// from `IsEqual` because `NoReturn` is deliberately *not* propagated
    /// through unification (see [`TypeChecker::solve_constraint`]) — divergence
    /// is pinned onto a composite's own variable and nowhere else.
    Diverges(Type, Loc),

    /// A call whose overload has not been pinned down yet.
    Call(CallConstraint),

    /// A field access whose receiver type is not known yet.
    Field(FieldConstraint),
}

/// `recv.field`, waiting for `recv` to become something with fields.
#[derive(Debug)]
struct FieldConstraint {
    /// Raw type-var id of the access expression, the same way a call is
    /// identified. `T(key)` is the field's type.
    key: usize,
    recv: Type,
    field: String,
    loc: Loc,
    /// Whether the field's type has been bound yet. The constraint is kept
    /// either way: an open receiver can gain cases after this resolved, and the
    /// final check has to see the receiver as it ended up.
    bound: bool,
}

#[derive(Debug)]
struct CallConstraint {
    /// Raw type-var id of the call expression. Every `Call` gets a fresh type
    /// var from the parser, and a generic body is only ever copied *whole* into
    /// a separately-solved instantiation, so within one solve this uniquely
    /// identifies the call site: it is the key the chosen overload is recorded
    /// under, and `T(key)` is the call's return type.
    key: usize,
    name: String,
    args: Vec<(Type, Loc)>,
    /// Type arguments written explicitly with a turbofish. Empty otherwise.
    type_args: Vec<Type>,
    loc: Loc,
    /// The overloads that are still compatible. `None` until the first solver
    /// round so that an undefined function is reported while solving rather
    /// than while collecting — that keeps a type mismatch anywhere in the
    /// function winning over an undefined call, as before.
    cands: Option<Vec<Candidate>>,
}

/// One overload still in the running at a call site.
#[derive(Debug, Clone)]
struct Candidate {
    /// Which overload of the name this is: an index into `schemes[name]`.
    idx: usize,
    /// The scheme's signature with its type parameters replaced — by the
    /// turbofish arguments if there were any, otherwise by fresh variables for
    /// the solver to pin down. Identical to the scheme's signature when the
    /// overload isn't generic.
    sig: Type,
    /// What each type parameter was replaced with, in declaration order. Empty
    /// when the overload isn't generic.
    type_args: Vec<Type>,
}

/// A function signature, plus the type parameters quantified over it. A
/// non-generic function is just a scheme with no parameters.
#[derive(Debug, Clone)]
struct Scheme {
    ty: Type,
    /// Name and type variable id of each parameter, in declaration order. The
    /// name is only ever used to say which one couldn't be inferred.
    type_params: Vec<(String, usize)>,
}

impl Scheme {
    fn is_generic(&self) -> bool {
        !self.type_params.is_empty()
    }
}

/// One function still to be type checked: an overload of `name`, at
/// `type_args` if it is generic.
#[derive(Debug, Clone)]
struct WorkItem {
    name: String,
    /// Index into `schemes[name]` / `module.funcs[name]`.
    idx: usize,
    type_args: Vec<Type>,
    /// The call site that asked for this instantiation, for error context.
    /// Absent for the non-generic functions the queue is seeded with.
    requested_at: Option<Loc>,
}

/// The overload a call site committed to.
///
/// The mangled name is *not* stored: at commit time a generic's type arguments
/// may still be unbound, so the name is built in [`Expr::resolve`], once the
/// solver has settled and `sig` resolves to something concrete.
#[derive(Debug, Clone)]
struct Resolution {
    name: String,
    /// Index into `schemes[name]`, so an instantiation can name the exact
    /// overload it came from even when two of them share a signature.
    idx: usize,
    /// The instantiated signature, still in terms of solver variables.
    sig: Type,
    /// The chosen type arguments, in declaration order. Empty for a
    /// non-generic overload.
    type_args: Vec<Type>,
    loc: Loc,
}

/// How many distinct instantiations of one generic function are allowed before
/// we assume it is instantiating itself without a base case. Nothing legitimate
/// comes close; this only exists so the queue cannot spin forever.
const MONOMORPHIZATION_LIMIT: usize = 256;

pub struct TypeChecker {
    schemes: HashMap<String, Vec<Scheme>>,
    /// Every declared type. Shared with the solver, which needs it to unify an
    /// open literal type with the declaration it belongs to.
    types: Rc<TypeTable>,
    /// Hands out type variables for instantiating generics, seeded past every
    /// id the parser used so the two cannot collide.
    fresh: Iota,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            schemes: HashMap::new(),
            types: Rc::new(TypeTable::new()),
            fresh: Iota::new(),
        }
    }

    /// Type check every function that the program can actually reach.
    ///
    /// Non-generic functions are all checked. A generic one never is, as
    /// written — it has no single type, so there is nothing to check. It is
    /// checked once per instantiation instead, after its type parameters have
    /// been substituted away, which is why checking is a worklist rather than a
    /// loop: checking one function can discover instantiations that need
    /// checking themselves.
    pub fn check(mut self, module: Module) -> Result<Module, Vec<FloErr>> {
        self.fresh = Iota::seeded(module.type_var_count);
        self.types = Rc::new(module.types.clone());

        // Nothing below can say anything sensible about a type that does not
        // exist or has no size, so these come first and on their own.
        let errs = check_type_decls(&module);
        if !errs.is_empty() {
            return Err(errs);
        }

        for (name, funcs) in &module.funcs {
            let schemes = funcs
                .iter()
                .map(|f| Scheme {
                    ty: f.ty.clone(),
                    type_params: f.type_params.clone(),
                })
                .collect();
            self.schemes.insert(name.clone(), schemes);
        }

        // Seed with every non-generic function. Generic ones enter the queue
        // only when something calls them.
        let mut queue: Vec<WorkItem> = Vec::new();
        for (name, funcs) in &module.funcs {
            for (idx, func) in funcs.iter().enumerate() {
                if func.type_params.is_empty() {
                    queue.push(WorkItem {
                        name: name.clone(),
                        idx,
                        type_args: Vec::new(),
                        requested_at: None,
                    });
                }
            }
        }

        let mut errs = Vec::new();
        let mut new_funcs: HashMap<String, Vec<Func>> = HashMap::new();
        // Keyed by the exact source function and instantiation, not by the
        // mangled name: two overloads may legitimately share a signature (see
        // `overload_error`), and both still have to be checked.
        let mut done: HashSet<(String, usize, Vec<Type>)> = HashSet::new();
        let mut instantiations: HashMap<String, usize> = HashMap::new();

        while let Some(item) = queue.pop() {
            let key = (item.name.clone(), item.idx, item.type_args.clone());
            if !done.insert(key) {
                continue;
            }

            let generic = &module.funcs[&item.name][item.idx];

            if !item.type_args.is_empty() {
                let count = instantiations.entry(item.name.clone()).or_insert(0);
                *count += 1;
                if *count > MONOMORPHIZATION_LIMIT {
                    errs.push(FloErr::MonomorphizationLimit {
                        name: item.name.clone(),
                        limit: MONOMORPHIZATION_LIMIT,
                        loc: item.requested_at.unwrap_or(generic.loc),
                    });
                    break;
                }
            }

            let subst = generic
                .type_params
                .iter()
                .map(|(_, id)| *id)
                .zip(item.type_args.iter().cloned())
                .collect::<HashMap<_, _>>();
            let func = generic.instantiate(&subst);

            let mut requested = Vec::new();
            match self.check_func(&func, &mut requested) {
                // Two functions may share a mangled name: declaring the same
                // signature twice is not an error in itself, it just makes
                // every call to it undecidable. That is reported at the call
                // site, by `overload_error`.
                Ok(func) => new_funcs
                    .entry(mangle_name(&func.ty, &item.name))
                    .or_default()
                    .push(func),
                Err(err) => errs.push(match item.requested_at {
                    Some(call_loc) => FloErr::InGenericInstantiation {
                        name: item.name.clone(),
                        type_args: item.type_args.clone(),
                        call_loc,
                        cause: Box::new(err),
                    },
                    None => err,
                }),
            }

            queue.extend(requested);
        }

        if errs.is_empty() {
            Ok(Module {
                funcs: new_funcs,
                types: module.types,
                uses: module.uses,
                var_count: module.var_count,
                type_var_count: self.fresh.count(),
            })
        } else {
            Err(errs)
        }
    }

    /// Check one concrete function, appending any generic instantiations its
    /// calls asked for to `requested`.
    fn check_func(&mut self, func: &Func, requested: &mut Vec<WorkItem>) -> FloResult<Func> {
        debug_assert!(
            func.type_params.is_empty(),
            "check_func on an uninstantiated generic"
        );

        // 1. Collect every constraint in a single AST pass.

        let mut constraints = Vec::new();
        self.collect_func_constraints(func, &mut constraints);

        // 2. Solve them, resolving calls to a fixpoint.

        let (mut set, resolutions) = self.solve(constraints)?;

        // 3. Rebuild the func with concrete types and resolved call names,
        //    noting which generics that pinned down along the way.

        func.resolve(&mut set, &resolutions, requested, &self.schemes)
    }

    // ----------------------------------------------------------------------
    // Collection
    // ----------------------------------------------------------------------

    fn collect_func_constraints(&mut self, func: &Func, out: &mut Vec<Constraint>) {
        // Constraints for argument types are not added, because they're already
        // concrete types
        let Type::Fn(_arg_tys, ret_ty) = &func.ty else {
            unreachable!()
        };

        out.push(Constraint::IsEqual(
            *ret_ty.clone(),
            func.body.ty.clone(),
            func.body.loc,
        ));

        self.collect_expr_constraints(&func.body, ret_ty.as_ref(), out);
    }

    /// `ret_ty` is the enclosing function's return type, needed to constrain the
    /// operand of any `return` expression that appears in the body.
    ///
    /// Children are visited before the constraints that mention them, so the
    /// list can be solved in order in one pass: by the time a parent's
    /// constraint is reached, a diverging child has already been pinned to
    /// `NoReturn` and the parent's constraint correctly becomes a no-op.
    fn collect_expr_constraints(&mut self, expr: &Expr, ret_ty: &Type, out: &mut Vec<Constraint>) {
        use ExprKind::*;
        use Type::*;

        match &expr.kind {
            Num(_) => out.push(Constraint::IsEqual(Integer, expr.ty.clone(), expr.loc)),
            Flt(_) => out.push(Constraint::IsEqual(Decimal, expr.ty.clone(), expr.loc)),
            ExprKind::Bool(_) => {
                out.push(Constraint::IsEqual(Type::Bool, expr.ty.clone(), expr.loc))
            }
            BuiltinOp(_) | Var(_) => {}
            Call(name, type_args, arg_exprs, _) => {
                for arg_expr in arg_exprs {
                    self.collect_expr_constraints(arg_expr, ret_ty, out);
                }

                let Type::T(key) = expr.ty else {
                    unreachable!("call expression without a fresh type var")
                };

                // Pushed after the arguments' constraints, so the pending-call
                // list ends up in post-order and the innermost unresolvable
                // call (the root cause) is the first one reported.
                out.push(Constraint::Call(CallConstraint {
                    key,
                    name: name.clone(),
                    args: arg_exprs
                        .iter()
                        .map(|arg| (arg.ty.clone(), arg.loc))
                        .collect(),
                    type_args: type_args.clone(),
                    loc: expr.loc,
                    cands: None,
                }));
            }
            Scope(stmts, tail) => {
                for stmt in stmts {
                    self.collect_stmt_constraints(stmt, ret_ty, out);
                }
                if let Some(tail) = tail {
                    self.collect_expr_constraints(tail, ret_ty, out);
                }

                // A scope diverges if any statement diverges or its tail does.
                // Statement divergence (a `return` before the tail) makes the
                // whole scope NoReturn even though the tail is dead code.
                if diverges(expr) {
                    out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                } else if let Some(tail) = tail {
                    out.push(Constraint::IsEqual(
                        tail.ty.clone(),
                        expr.ty.clone(),
                        expr.loc,
                    ));
                } else {
                    // No tail and no divergence => an empty/`;`-terminated scope
                    // is void.
                    out.push(Constraint::IsEqual(Void, expr.ty.clone(), expr.loc));
                }
            }
            If(cond, then, otherwise) => {
                self.collect_expr_constraints(cond, ret_ty, out);
                out.push(Constraint::IsEqual(Type::Bool, cond.ty.clone(), cond.loc));

                self.collect_expr_constraints(then, ret_ty, out);
                // The `if`'s own type is the join of its branches. A NoReturn
                // branch is absorbed: the constraint below is a no-op for it (see
                // `solve_constraint`), so the `if` takes the other branch's type.
                out.push(Constraint::IsEqual(
                    expr.ty.clone(),
                    then.ty.clone(),
                    then.loc,
                ));

                if let Some(otherwise) = otherwise {
                    self.collect_expr_constraints(otherwise, ret_ty, out);
                    out.push(Constraint::IsEqual(
                        expr.ty.clone(),
                        otherwise.ty.clone(),
                        otherwise.loc,
                    ));

                    // If BOTH branches diverge the whole `if` diverges. The two
                    // constraints above bound nothing (both were no-ops), so pin
                    // the `if`'s own type to NoReturn explicitly.
                    if diverges(expr) {
                        out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                    }
                } else {
                    // With no `else`, the `if` is void — control may skip `then`
                    // entirely, so a diverging `then` does not make it NoReturn.
                    out.push(Constraint::IsEqual(Void, expr.ty.clone(), expr.loc));
                }
            }
            Logical(_, lhs, rhs) => {
                self.collect_expr_constraints(lhs, ret_ty, out);
                out.push(Constraint::IsEqual(Type::Bool, lhs.ty.clone(), lhs.loc));

                self.collect_expr_constraints(rhs, ret_ty, out);
                out.push(Constraint::IsEqual(Type::Bool, rhs.ty.clone(), rhs.loc));

                // The result is bool whatever the operands turn out to be (set by
                // the parser), so there is nothing to say about `expr.ty`. Nor is
                // there an overload to resolve: unlike every other operator, this
                // is not a call.
            }
            While(cond, body) => {
                self.collect_expr_constraints(cond, ret_ty, out);
                out.push(Constraint::IsEqual(Type::Bool, cond.ty.clone(), cond.loc));

                self.collect_expr_constraints(body, ret_ty, out);
                // The body's value is discarded, so it has to be void — the same
                // rule an else-less `if` follows. A body that diverges (`break`,
                // `continue`, `return`) is NoReturn, which this constraint accepts.
                out.push(Constraint::IsEqual(Void, body.ty.clone(), body.loc));

                // The loop's own type is already void (set by the parser), and it
                // never diverges: the condition may be false on the first check,
                // so control can always reach the expression after it.
            }
            Break | Continue => {
                // Both are NoReturn (set by the parser) and carry no operand, so
                // there is nothing to constrain. Which loop they belong to was
                // already checked while parsing.
            }
            Return(value) => {
                match value {
                    Some(e) => {
                        self.collect_expr_constraints(e, ret_ty, out);
                        // The returned value must match the function's return type.
                        out.push(Constraint::IsEqual(ret_ty.clone(), e.ty.clone(), e.loc));
                    }
                    None => {
                        // Bare `return` yields void; only valid in a void function.
                        out.push(Constraint::IsEqual(ret_ty.clone(), Void, expr.loc));
                    }
                }
                // The `return` expression's own type is already NoReturn (set by
                // the parser), so it needs no constraint here.
            }
            CaseLit(qualifier, case, fields) => {
                for field in fields {
                    self.collect_expr_constraints(&field.value, ret_ty, out);
                }

                if diverges(expr) {
                    // A field's value diverging means the literal is never
                    // built, so there is no type to pin it to.
                    out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                    return;
                }

                // The literal names a case, not a type. All it says is that
                // whatever this is, it has *this* case with *these* fields —
                // which type that makes it is left to unification.
                let known = TypeCase::new(
                    case.clone(),
                    fields
                        .iter()
                        .map(|f| (f.name.clone(), f.value.ty.clone()))
                        .collect(),
                );
                out.push(Constraint::IsEqual(
                    expr.ty.clone(),
                    Type::SomeType(vec![known]),
                    expr.loc,
                ));

                // A qualifier says outright which type it is, so it is just one
                // more equality — and the check above is what validates it.
                if let Some((name, args)) = qualifier {
                    // No turbofish does not mean "no type arguments": in
                    // `Option::Some { val: 0 }` the argument is simply left to
                    // inference, so it gets a fresh variable per parameter of
                    // the declaration. An undeclared name has no parameters to
                    // count and is reported by the declaration check.
                    let args = if args.is_empty() {
                        let arity = self.types.get(name).map_or(0, |d| d.type_params.len());
                        (0..arity).map(|_| Type::T(self.fresh.next())).collect()
                    } else {
                        args.clone()
                    };

                    out.push(Constraint::IsEqual(
                        expr.ty.clone(),
                        Type::User(name.clone(), args),
                        expr.loc,
                    ));
                }
            }
            Field(recv, name) => {
                self.collect_expr_constraints(recv, ret_ty, out);

                if diverges(recv) {
                    out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                    return;
                }

                let Type::T(key) = expr.ty else {
                    unreachable!("field access without a fresh type var")
                };

                out.push(Constraint::Field(FieldConstraint {
                    key,
                    recv: recv.ty.clone(),
                    field: name.clone(),
                    loc: expr.loc,
                    bound: false,
                }));
            }
            Cast(_, value) => {
                // Only the operand is walked, and nothing is said about either
                // type. A cast reinterprets whatever bits it is handed, so it
                // constrains its operand not at all — and its own type is the
                // one that was written, which the parser already put on
                // `expr.ty`. Whether the operand's type is one that *has* bits
                // has to wait for `resolve`, when it is known.
                self.collect_expr_constraints(value, ret_ty, out);
            }
            TypeInfo(..) => {
                // Both halves are fixed by the syntax: the type asked about is
                // written down, and the answer is a u64.
            }
            Assign(target, value) => {
                self.collect_expr_constraints(target, ret_ty, out);
                self.collect_expr_constraints(value, ret_ty, out);

                if diverges(value) {
                    out.push(Constraint::Diverges(expr.ty.clone(), expr.loc));
                } else {
                    out.push(Constraint::IsEqual(
                        target.ty.clone(),
                        value.ty.clone(),
                        expr.loc,
                    ));
                    // An assignment yields the value it stored, like C.
                    out.push(Constraint::IsEqual(
                        expr.ty.clone(),
                        target.ty.clone(),
                        expr.loc,
                    ));
                }
            }
        }
    }

    fn collect_stmt_constraints(
        &mut self,
        stmt: &Statement,
        ret_ty: &Type,
        out: &mut Vec<Constraint>,
    ) {
        match &stmt.kind {
            // A statement's value is discarded, so unlike a tail it constrains
            // nothing: whatever it evaluates to is fine.
            StmtKind::Expr(e) => self.collect_expr_constraints(e, ret_ty, out),

            // Purely a name binding, resolved while parsing. Nothing to check
            // here; that the type has the case is checked with the declarations.
            StmtKind::Use(..) => {}

            StmtKind::Let(_, var_ty, init) => {
                if let Some(init) = init {
                    self.collect_expr_constraints(init, ret_ty, out);

                    if diverges(init) {
                        // `let x = return 1;` — nothing ever flows into `x`, so
                        // pin it rather than leaving it unresolvable (the
                        // equality below would be a no-op against NoReturn).
                        out.push(Constraint::Diverges(var_ty.clone(), init.loc));
                    } else {
                        out.push(Constraint::IsEqual(
                            var_ty.clone(),
                            init.ty.clone(),
                            init.loc,
                        ));
                    }
                }

                // `let x;` pushes nothing at all: the variable keeps its fresh
                // type var for a later assignment to bind. If nothing ever does,
                // `resolve` reports it at the declaration.
            }
        }
    }

    // ----------------------------------------------------------------------
    // Solving
    // ----------------------------------------------------------------------

    /// Solve the collected constraints, returning the bindings and the overload
    /// chosen for each call site.
    ///
    /// Equality and divergence constraints are solved exactly once each, in
    /// collection order: a union-find solve is confluent, so re-running them
    /// would be wasted work — and the `NoReturn` short-circuit in
    /// `solve_constraint` reads solver state, so re-running would not even be a
    /// no-op. Only calls need iterating, because committing one binds type
    /// variables that can narrow another call's overload set.
    fn solve(
        &mut self,
        constraints: Vec<Constraint>,
    ) -> FloResult<(ReplaceSet, HashMap<usize, Resolution>)> {
        let mut set = ReplaceSet::new(Rc::clone(&self.types));
        let mut resolutions = HashMap::new();
        let mut pending: Vec<CallConstraint> = Vec::new();
        let mut fields: Vec<FieldConstraint> = Vec::new();

        for constraint in constraints {
            match constraint {
                Constraint::IsEqual(t1, t2, loc) => self.solve_constraint(&mut set, t1, t2, loc)?,
                Constraint::Diverges(ty, loc) => mark_never(&ty, &mut set, loc)?,
                Constraint::Call(call) => {
                    debug_assert!(
                        !pending.iter().any(|c| c.key == call.key),
                        "two call sites share type var 't{}",
                        call.key
                    );
                    pending.push(call);
                }
                Constraint::Field(field) => fields.push(field),
            }
        }

        // Resolve calls and field accesses to a fixpoint (before defaulting)
        self.reduce(&mut pending, &mut fields, &mut set, &mut resolutions)?;

        // Default types ({integer} => i32)
        set.default_types();

        // Again: defaulting may have unblocked calls that were ambiguous before
        self.reduce(&mut pending, &mut fields, &mut set, &mut resolutions)?;

        // Every literal type that never met a declared type is now as narrow as
        // it will ever be, so close each one into the anonymous type of exactly
        // the cases it has.
        set.close_some_types();

        // And again: an access whose receiver was open is now on a real type.
        self.reduce(&mut pending, &mut fields, &mut set, &mut resolutions)?;

        // Any call still pending is now a genuine error. The list is in
        // post-order, so the first one is the innermost.
        if let Some(call) = pending.first() {
            Err(self.overload_error(call, &mut set))?
        }

        // Field accesses are checked against the receiver as it finally is, not
        // as it was when the field's type got bound: an open receiver can gain a
        // case after an access resolved against it, which would make that access
        // a read of a sum type.
        for field in &fields {
            self.check_field(field, &mut set)?;
        }

        Ok((set, resolutions))
    }

    /// Repeatedly attempt to commit every pending call and field access until a
    /// full round commits nothing new. Committing one binds type variables,
    /// which can unblock a call's parent (via the argument types) or its
    /// children (via the return type), and can give a field access the receiver
    /// it was waiting on — so no single order works, and we iterate until the
    /// worklist stops shrinking.
    fn reduce(
        &mut self,
        pending: &mut Vec<CallConstraint>,
        fields: &mut Vec<FieldConstraint>,
        set: &mut ReplaceSet,
        resolutions: &mut HashMap<usize, Resolution>,
    ) -> FloResult<()> {
        loop {
            let mut progress = false;
            let mut unresolved = Vec::with_capacity(pending.len());

            for mut call in std::mem::take(pending) {
                match self.try_resolve_call(&mut call, set)? {
                    Some(resolution) => {
                        resolutions.insert(call.key, resolution);
                        progress = true;
                    }
                    None => unresolved.push(call),
                }
            }
            *pending = unresolved;

            for field in fields.iter_mut() {
                if field.bound {
                    continue;
                }
                // An error here would be premature: the receiver may still be
                // an open type that has not met its declaration yet. The final
                // pass in `solve` is what reports.
                if let Ok(Some(ty)) = lookup_field(field, set, &self.types) {
                    self.solve_constraint(set, ty, Type::T(field.key), field.loc)?;
                    field.bound = true;
                    progress = true;
                }
            }

            if !progress {
                return Ok(());
            }
        }
    }

    /// Report whatever is wrong with a field access, now that everything is as
    /// resolved as it is going to get.
    fn check_field(&self, field: &FieldConstraint, set: &mut ReplaceSet) -> FloResult<()> {
        match lookup_field(field, set, &self.types) {
            Err(err) => Err(err),
            // The receiver never became anything with fields. Its own
            // `UnresolvedType` would be reported at the receiver, which says
            // less than naming the access that needed it.
            Ok(None) => Err(FloErr::UnresolvedType {
                ty: set.resolve(&field.recv),
                loc: field.loc,
            }),
            Ok(Some(_)) => Ok(()),
        }
    }

    /// Narrow the call's overload set against the current bindings and commit if
    /// exactly one candidate survives; 0 or 2+ leaves it pending for a later
    /// round (a neighbouring call may resolve and narrow it down). This never
    /// reports an overload error — that happens once the fixpoint has settled.
    fn try_resolve_call(
        &mut self,
        call: &mut CallConstraint,
        set: &mut ReplaceSet,
    ) -> FloResult<Option<Resolution>> {
        if !self.schemes.contains_key(&call.name) {
            return Err(FloErr::UndefinedFunction {
                name: call.name.clone(),
                loc: call.loc,
            });
        }

        // Candidates are instantiated once, on the first round, and reused: a
        // generic's fresh variables have to be the *same* ones each round, or
        // everything the solver learned about them last round is thrown away and
        // the fixpoint never converges.
        if call.cands.is_none() {
            call.cands = Some(self.instantiate_candidates(call));
        }
        let cands = call.cands.as_mut().unwrap();

        // Pruning is monotone: `satisfies_type` only ever goes true -> false as
        // bindings accumulate, so a candidate dropped here can never become
        // viable again and the set only has to be filtered against what changed.
        let types = Rc::clone(&self.types);
        prune(cands, &call.args, &Type::T(call.key), set, &types);

        // Where every survivor agrees on a parameter's type, that IS the
        // argument's type — whichever candidate is eventually chosen has to be
        // one of these, since the set only ever shrinks. Binding it now rather
        // than waiting for the commit is what keeps an overload set that is
        // *wide* from being decided by defaulting: `let x: u8 = 1 << 2` prunes
        // to the eight `u8 << {any width}` shifts, and if nothing pinned the
        // left operand first, `default_types` would answer i32 for it and every
        // one of the eight would then be ruled out.
        self.bind_agreed_params(cands, &call.args, set)?;

        // Only commit when EXACTLY one overload survives.
        let [cand] = &cands[..] else {
            return Ok(None);
        };
        let cand = cand.clone();

        let Type::Fn(params, ret) = &cand.sig else {
            unreachable!()
        };

        // Feeding the chosen signature back in as ordinary equality constraints
        // is all the pinning that's needed: binding a variable already bound to
        // {integer} joins the two and yields the concrete param type. For a
        // generic, this is also what binds its type parameters.
        for ((arg_ty, arg_loc), param) in call.args.iter().zip(params) {
            self.solve_constraint(set, param.clone(), arg_ty.clone(), *arg_loc)?;
        }

        self.solve_constraint(set, *ret.clone(), Type::T(call.key), call.loc)?;

        // Mangling is deliberately *not* done here. A generic's type arguments
        // may still be unbound at this point and only get pinned by a later
        // round, so the name is built in `Expr::resolve`, once everything has
        // settled.
        Ok(Some(Resolution {
            name: call.name.clone(),
            idx: cand.idx,
            sig: cand.sig,
            type_args: cand.type_args,
            loc: call.loc,
        }))
    }

    /// Bind each argument whose type every surviving candidate agrees on.
    ///
    /// This is sound for the same reason pruning is: the set only shrinks, so a
    /// type all of today's candidates share is a type tomorrow's chosen one has.
    /// And it terminates, because a binding only happens where the argument does
    /// not already resolve to that type — so each one leaves strictly fewer
    /// variables unbound and a later round finds nothing left to do.
    ///
    /// Nothing is agreed unless there is at least one candidate; an empty set is
    /// a call that has failed, which `overload_error` reports.
    fn bind_agreed_params(
        &self,
        cands: &[Candidate],
        args: &[(Type, Loc)],
        set: &mut ReplaceSet,
    ) -> FloResult<()> {
        let [first, rest @ ..] = cands else {
            return Ok(());
        };

        let first_params = params_of(first);
        for (i, (arg_ty, arg_loc)) in args.iter().enumerate() {
            let Some(param) = first_params.get(i) else {
                continue;
            };

            // Only a type that is fully known can be agreed on. A generic
            // candidate's parameter is a variable minted per candidate, and two
            // of those standing for different things must never be read as
            // agreeing just because neither is bound yet.
            if !param.is_known() {
                continue;
            }
            if !rest.iter().all(|c| params_of(c).get(i) == Some(param)) {
                continue;
            }

            // Already settled: binding again would be a no-op that still looked
            // like progress, and `reduce` would never reach its fixpoint.
            if set.resolve(arg_ty) == *param {
                continue;
            }

            self.solve_constraint(set, param.clone(), arg_ty.clone(), *arg_loc)?;
        }

        Ok(())
    }

    /// Build the initial candidate set for a call: every overload of the name,
    /// with its type parameters replaced.
    ///
    /// An overload whose parameter count doesn't match an explicit turbofish is
    /// dropped outright — including every non-generic one, since a turbofish
    /// can only ever have been meant for a generic.
    fn instantiate_candidates(&mut self, call: &CallConstraint) -> Vec<Candidate> {
        let schemes = &self.schemes[&call.name];

        let mut out = Vec::with_capacity(schemes.len());
        for (idx, scheme) in schemes.iter().enumerate() {
            if !call.type_args.is_empty() && call.type_args.len() != scheme.type_params.len() {
                continue;
            }

            if !scheme.is_generic() {
                out.push(Candidate {
                    idx,
                    sig: scheme.ty.clone(),
                    type_args: Vec::new(),
                });
                continue;
            }

            let type_args = if call.type_args.is_empty() {
                scheme
                    .type_params
                    .iter()
                    .map(|_| Type::T(self.fresh.next()))
                    .collect::<Vec<_>>()
            } else {
                call.type_args.clone()
            };

            let subst = scheme
                .type_params
                .iter()
                .map(|(_, id)| *id)
                .zip(type_args.iter().cloned())
                .collect::<HashMap<_, _>>();

            out.push(Candidate {
                idx,
                sig: scheme.ty.substitute(&subst),
                type_args,
            });
        }

        out
    }

    /// Turn a call that survived the fixpoint unresolved into an error.
    fn overload_error(&self, call: &CallConstraint, set: &mut ReplaceSet) -> FloErr {
        let cands = call.cands.as_deref().unwrap_or(&[]);

        if cands.is_empty() {
            let arg_tys = call.args.iter().map(|(ty, _)| set.resolve(ty)).collect();
            let known_ty = Type::Fn(arg_tys, Box::new(set.resolve(&Type::T(call.key))));
            FloErr::NoPossibleOverloads {
                name: call.name.clone(),
                known_ty,
                loc: call.loc,
            }
        } else {
            let possible_tys = cands
                .iter()
                .map(|c| set.resolve(&c.sig))
                .collect::<Vec<Type>>();
            FloErr::MultiplePossibleOverloads {
                name: call.name.clone(),
                possible_tys,
                loc: call.loc,
            }
        }
    }

    fn solve_constraint(
        &self,
        set: &mut ReplaceSet,
        t1: Type,
        t2: Type,
        loc: Loc,
    ) -> FloResult<()> {
        use Type::*;

        // NoReturn satisfies any binding: a constraint touching it is vacuously
        // solved. Crucially it never binds a variable to NoReturn — divergence is
        // propagated structurally (via `Constraint::Diverges`), not through
        // unification, so a diverging expression cannot poison a neighbour's
        // type variable.
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
}

/// A candidate's parameter types. Every signature is a `Fn`, by construction.
fn params_of(cand: &Candidate) -> &[Type] {
    match &cand.sig {
        Type::Fn(params, _) => params,
        _ => unreachable!(),
    }
}

/// Drop every overload that the current bindings rule out.
fn prune(
    cands: &mut Vec<Candidate>,
    args: &[(Type, Loc)],
    ret_ty: &Type,
    set: &mut ReplaceSet,
    types: &TypeTable,
) {
    cands.retain(|cand| {
        let Type::Fn(params, ret) = &cand.sig else {
            unreachable!()
        };

        // Arity mismatch => prune overload
        if params.len() != args.len() {
            return false;
        }

        let mut should_keep = true;
        // Ensure all arg types satisfy
        for ((arg_ty, _), param) in args.iter().zip(params) {
            should_keep &= set.resolve(arg_ty).satisfies_type(param, types);
        }
        // Ensure return type satisfies
        should_keep & set.resolve(ret_ty).satisfies_type(ret, types)
    });
}

/// The type of `field.field` on its receiver, or `None` while the receiver is
/// still unknown. An error means the access itself cannot work — but during the
/// fixpoint the receiver may only be *temporarily* wrong, so callers there
/// treat an error as "not yet" and let [`TypeChecker::check_field`] report.
fn lookup_field(
    field: &FieldConstraint,
    set: &mut ReplaceSet,
    types: &TypeTable,
) -> FloResult<Option<Type>> {
    use Type::*;

    let recv = set.resolve(&field.recv);

    let sum_type_err = || FloErr::FieldAccessOnSumType {
        ty: recv.clone(),
        field: field.field.clone(),
        loc: field.loc,
    };

    let case = match &recv {
        // Nothing to look the field up in yet.
        T(_) => return Ok(None),
        // The receiver never yields a value, so neither does the access.
        Never => return Ok(Some(Never)),

        User(name, args) => {
            let Some(decl) = types.get(name) else {
                // Undeclared; reported by the declaration check.
                return Ok(None);
            };
            let [only] = &decl.cases[..] else {
                return Err(sum_type_err());
            };
            decl.case_at(&only.name, args)
                .expect("a declaration's own case")
        }

        // A literal type is read the same way, so `let v = Foo { n: 0 }; v.n`
        // needs no annotation. It is checked again once the type is closed, in
        // case it gained a case in the meantime.
        SomeType(cases) | Anon(cases) => {
            let [only] = &cases[..] else {
                return Err(sum_type_err());
            };
            only.clone()
        }

        _ => {
            return Err(FloErr::NotAStruct {
                ty: recv.clone(),
                field: field.field.clone(),
                loc: field.loc,
            });
        }
    };

    match case.field(&field.field) {
        Some(ty) => Ok(Some(ty.clone())),
        None => Err(FloErr::UnknownField {
            ty: recv,
            field: field.field.clone(),
            loc: field.loc,
        }),
    }
}

/// Whether an expression diverges (never yields a value), determined purely
/// from its shape.
///
/// `NoReturn` only ever enters the solver through a `Constraint::Diverges`,
/// which is emitted for exactly the expressions this returns true for, so this
/// gives the same answer the old solver-consulting check did — without needing
/// the solver to have run first.
fn diverges(expr: &Expr) -> bool {
    use ExprKind::*;

    match &expr.kind {
        // The parser types `return`, `break` and `continue` as NoReturn directly.
        Return(_) | Break | Continue => true,
        Scope(stmts, tail) => {
            stmts.iter().any(stmt_diverges) || tail.as_deref().is_some_and(diverges)
        }
        // Both branches must diverge; with no `else` control can skip `then`.
        If(_, then, Some(otherwise)) => diverges(then) && diverges(otherwise),
        // A loop never diverges, however its body is written: the condition may
        // be false on the first check, so control always reaches what follows.
        // Spotting that `while true` cannot exit would need real flow analysis.
        While(..) => false,
        // Only the left operand counts: the right one is skipped whenever the
        // left already decides the answer, so it may never run.
        Logical(_, lhs, _) => diverges(lhs),
        Assign(target, value) => diverges(target) || diverges(value),
        // A literal whose field value diverges is never built, and a field of a
        // receiver that diverges is never read.
        CaseLit(_, _, fields) => fields.iter().any(|f| diverges(&f.value)),
        Field(recv, _) => diverges(recv),
        // There are no bits to reinterpret if the operand never yields any.
        // Note that no `Constraint::Diverges` is emitted for a cast (see the
        // collection arm): its type is the one written, never a variable, so
        // there is nothing divergence could be pinned onto.
        Cast(_, value) => diverges(value),
        _ => false,
    }
}

/// Whether control leaves the program (or the function) part-way through this
/// statement, so that nothing after it in the scope can run.
fn stmt_diverges(stmt: &Statement) -> bool {
    match &stmt.kind {
        StmtKind::Expr(e) => diverges(e),
        StmtKind::Let(_, _, init) => init.as_ref().is_some_and(diverges),
        // Nothing to evaluate.
        StmtKind::Use(..) => false,
    }
}

/// Pin a composite expression's own (always fresh) type variable to NoReturn
/// when it is determined to diverge. Only the composite's own variable is
/// touched, so a diverging branch or statement never poisons its siblings.
fn mark_never(ty: &Type, set: &mut ReplaceSet, loc: Loc) -> FloResult<()> {
    match ty {
        Type::T(id) => set.bind(*id, Type::Never, loc),
        _ => Ok(()),
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

/// What the final rebuild pass needs beyond the solver state: the overload each
/// call committed to, the schemes those overloads came from, and somewhere to
/// note the generic instantiations it discovers.
struct ResolveCtx<'a> {
    schemes: &'a HashMap<String, Vec<Scheme>>,
    res: &'a HashMap<usize, Resolution>,
    requested: &'a mut Vec<WorkItem>,
}

impl Func {
    fn resolve(
        &self,
        set: &mut ReplaceSet,
        res: &HashMap<usize, Resolution>,
        requested: &mut Vec<WorkItem>,
        schemes: &HashMap<String, Vec<Scheme>>,
    ) -> FloResult<Self> {
        let ty = set.resolve(&self.ty);
        if !ty.is_known() {
            return Err(FloErr::UnresolvedType { ty, loc: self.loc });
        }

        let mut ctx = ResolveCtx {
            schemes,
            res,
            requested,
        };

        Ok(Func {
            body: self.body.resolve(set, &mut ctx)?,
            ty,
            loc: self.loc,
            // An instantiated function is not generic; that is the whole point.
            type_params: Vec::new(),
        })
    }
}

impl Statement {
    fn resolve(&self, set: &mut ReplaceSet, ctx: &mut ResolveCtx) -> FloResult<Self> {
        let kind = match &self.kind {
            StmtKind::Expr(e) => StmtKind::Expr(e.resolve(set, ctx)?),
            StmtKind::Use(ty, case) => StmtKind::Use(ty.clone(), case.clone()),
            StmtKind::Let(id, var_ty, init) => {
                // A statement has no type of its own, so nothing else would ever
                // look at the variable's. An unresolved one is reported here, at
                // the declaration.
                let var_ty = set.resolve(var_ty);
                if !var_ty.is_known() {
                    Err(FloErr::UnresolvedType {
                        ty: var_ty.clone(),
                        loc: self.loc,
                    })?
                }

                let new_init = match init {
                    Some(e) => Some(e.resolve(set, ctx)?),
                    None => None,
                };
                StmtKind::Let(*id, var_ty, new_init)
            }
        };

        Ok(Statement {
            kind,
            loc: self.loc,
        })
    }
}

impl Expr {
    fn resolve(&self, set: &mut ReplaceSet, ctx: &mut ResolveCtx) -> FloResult<Self> {
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
            Call(name, _, args, _) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(arg.resolve(set, ctx)?);
                }

                // NOTE: `self.ty`, not the resolved `ty` above — the call site's
                // identity is its *raw* type var, which `ty` has replaced with a
                // concrete type by now.
                let Type::T(key) = self.ty else {
                    unreachable!("call expression without a fresh type var")
                };
                let resolution = ctx
                    .res
                    .get(&key)
                    .unwrap_or_else(|| panic!("Unresolved call {name} (args: {args:?})"))
                    .clone();

                // Now that the solver has settled, the chosen signature is
                // concrete and can be named.
                let sig = set.resolve(&resolution.sig);
                let mangled = mangle_name(&sig, &resolution.name);

                // A generic overload needs its instantiation checked, which is
                // only possible once its type arguments are actually known.
                let mut type_args = Vec::with_capacity(resolution.type_args.len());
                for (i, arg) in resolution.type_args.iter().enumerate() {
                    let arg = set.resolve(arg);
                    if !arg.is_known() {
                        let param = &ctx.schemes[&resolution.name][resolution.idx].type_params[i];
                        return Err(FloErr::CannotInferTypeParam {
                            name: param.0.clone(),
                            loc: resolution.loc,
                        });
                    }
                    type_args.push(arg);
                }

                if !type_args.is_empty() {
                    ctx.requested.push(WorkItem {
                        name: resolution.name.clone(),
                        idx: resolution.idx,
                        type_args,
                        requested_at: Some(resolution.loc),
                    });
                }

                Call(name.clone(), Vec::new(), new_args, Some(mangled))
            }
            Scope(stmts, tail) => {
                let mut new_stmts = Vec::new();
                for stmt in stmts {
                    new_stmts.push(stmt.resolve(set, ctx)?);
                }

                let new_tail = match tail {
                    Some(e) => Some(Box::new(e.resolve(set, ctx)?)),
                    None => None,
                };
                Scope(new_stmts, new_tail)
            }
            If(cond, then, otherwise) => {
                let cond = Box::new(cond.resolve(set, ctx)?);
                let then = Box::new(then.resolve(set, ctx)?);
                let otherwise = match otherwise {
                    Some(e) => Some(Box::new(e.resolve(set, ctx)?)),
                    None => None,
                };
                If(cond, then, otherwise)
            }
            Logical(op, lhs, rhs) => Logical(
                *op,
                Box::new(lhs.resolve(set, ctx)?),
                Box::new(rhs.resolve(set, ctx)?),
            ),
            While(cond, body) => While(
                Box::new(cond.resolve(set, ctx)?),
                Box::new(body.resolve(set, ctx)?),
            ),
            Return(value) => {
                let new_value = match value {
                    Some(e) => Some(Box::new(e.resolve(set, ctx)?)),
                    None => None,
                };
                Return(new_value)
            }
            Assign(target, value) => Assign(
                Box::new(target.resolve(set, ctx)?),
                Box::new(value.resolve(set, ctx)?),
            ),
            CaseLit(_, case, fields) => {
                let mut new_fields = Vec::with_capacity(fields.len());
                for field in fields {
                    new_fields.push(FieldInit {
                        name: field.name.clone(),
                        value: field.value.resolve(set, ctx)?,
                        loc: field.loc,
                    });
                }

                // The qualifier has done its job: `ty` now says which type this
                // is, the same as it does for an unqualified literal.
                CaseLit(None, case.clone(), new_fields)
            }
            Field(recv, name) => Field(Box::new(recv.resolve(set, ctx)?), name.clone()),
            Cast(_, value) => {
                let value = value.resolve(set, ctx)?;

                // The written target was checked while parsing; the operand
                // could only be checked once solving had given it a type. A
                // NoReturn operand is fine — nothing is ever cast, because the
                // cast is never reached.
                if matches!(value.ty, Type::Void) {
                    return Err(FloErr::TypeHasNoSize {
                        ty: value.ty,
                        loc: value.loc,
                    });
                }

                // `ty` is the resolved target: it and the node's own type are
                // the same thing for a cast, so taking it from one place keeps
                // them from ever drifting apart.
                Cast(ty.clone(), Box::new(value))
            }
            TypeInfo(query, queried) => TypeInfo(*query, set.resolve(queried)),

            Num(n) => Num(*n),
            Flt(n) => Flt(*n),
            Bool(n) => Bool(*n),
            Var(v) => Var(*v),
            BuiltinOp(op) => BuiltinOp(*op),
            Break => Break,
            Continue => Continue,
        };

        Ok(Expr { kind, ty, loc })
    }
}
