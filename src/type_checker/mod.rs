//! DISCLAIMER: Claude generated
use std::collections::{HashMap, HashSet};

use crate::{
    ast::{Expr, ExprKind, Func, FuncLocs, Module, ResolvedModule},
    errors::FloErr,
    tokenizer::Loc,
    types::{Type, TypeKind},
};

mod callgraph;
mod infer;
mod specialize;
mod subst;
#[cfg(test)]
mod tests;
mod unify;

use callgraph::CallGraph;
use infer::{residual_same, solve_func};
use specialize::Specializer;
use subst::max_var_id;

/// A residual kind bound: "the variable with this (canonical) id in a function's
/// signature must satisfy this kind". These are the leftover constraints a
/// function's `Type::Fn` signature can't itself express — e.g. `fn bar() -> 'a =
/// 0` has signature `() -> t0` but `t0` must be `Numeric`. The `Loc` records
/// where the bound was introduced, for error reporting.
pub(super) type Residual = (usize, TypeKind, Loc);

pub struct TypeChecker<'a> {
    module: &'a mut Module,
    /// Per-overload leftover kind bounds. Keyed by function name; the inner `Vec`
    /// is parallel to that name's overload list in `module.funcs`. Together with
    /// each overload's (in-place mutated) `func.ty`, this *is* its schema — no
    /// separate signature copy is kept.
    residuals: HashMap<String, Vec<Vec<Residual>>>,
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

    pub fn check(mut self) -> Result<ResolvedModule, Vec<FloErr>> {
        // Inject the built-in operator overloads (`+`, `-`, …) so operator calls
        // desugared by the parser resolve against real overloads. They have
        // concrete signatures and body-less (`Intrinsic`) bodies, so both passes
        // handle them like any other function.
        register_builtins(self.module);

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

        // One (initially empty) residual slot per overload, parallel to each
        // name's overload list, so `residuals[name][index]` is always valid.
        for (name, overloads) in &self.module.funcs {
            self.residuals
                .insert(name.clone(), vec![Vec::new(); overloads.len()]);
        }

        let mut errs = Vec::new();

        for scc in &sccs {
            // Bottom-up pass: iterate the SCC to a fixpoint, mutating each
            // overload's `func.ty`/`func.body` in place. A non-recursive singleton
            // settles almost immediately; a recursive SCC keeps recomputing every
            // member off the previous iteration's schemas until nothing changes.
            let mut errored: HashSet<(String, usize)> = HashSet::new();
            loop {
                let mut changed = false;
                for name in scc {
                    let Some(overloads) = self.module.funcs.get(name) else {
                        continue; // external/undefined callee
                    };
                    for index in 0..overloads.len() {
                        if errored.contains(&(name.clone(), index)) {
                            continue; // already errored
                        }

                        match solve_func(name, index, self.module, &self.residuals, &mut self.fresh)
                        {
                            Ok((ty, body, residual)) => {
                                // The signature/residual drives fixpoint
                                // convergence (that's what callers depend on); the
                                // body always gets its resolved types written back.
                                let sig_changed = self.module.funcs[name][index].ty != ty
                                    || !residual_same(
                                        Some(&self.residuals[name][index]),
                                        &residual,
                                    );

                                let func = &mut self.module.funcs.get_mut(name).unwrap()[index];
                                func.ty = ty;
                                func.body = body;
                                self.residuals.get_mut(name).unwrap()[index] = residual;

                                if sig_changed {
                                    changed = true;
                                }
                            }
                            Err(err) => {
                                errs.push(err);
                                errored.insert((name.clone(), index));
                            }
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
            return Err(errs);
        }

        // Specialize pass: monomorphize every function reachable from `main`, one
        // instance per distinct set of concrete argument types + overload selected.
        // The parser guarantees exactly one (nullary) `main`, so it's the single
        // root.
        let main_loc = self.module.funcs["main"][0].loc.definition;
        let mono = {
            let mut spec = Specializer::new(&*self.module, &self.residuals, self.fresh);
            if let Err(err) = spec.specialize("main", &[], None, main_loc) {
                return Err(vec![err]);
            }
            spec.into_module()
        };

        // Every reachable overload is now resolved to a distinct, fully concrete
        // monomorphic instance under a unique mangled name. Functions never
        // reached from `main` are simply dropped.
        let resolved = ResolvedModule { funcs: mono };
        assert!(
            resolved.funcs.values().all(|f| fully_concrete(&f.ty)),
            "resolved module must be fully monomorphic (no unresolved type variables)"
        );

        Ok(resolved)
    }
}

/// Register the built-in operator overloads. Arithmetic operators desugar to
/// `Call`s named by their symbol; each gets one overload per numeric type. Unary
/// `-` shares the `-` name with binary `-` — overload resolution tells them apart
/// by arity. Symbol names can never collide with user identifiers (which are
/// alphanumeric/underscore), so injecting them here is always safe. Bodies are
/// left as `Intrinsic` sentinels until there's an interpreter to fill them in.
fn register_builtins(module: &mut Module) {
    // The numeric types operators are defined over (mirrors `TypeKind::Integral`).
    const NUMERIC: [Type; 2] = [Type::I32, Type::U8];
    const BINOPS: [&str; 5] = ["+", "-", "*", "/", "%"];

    for op in BINOPS {
        for ty in &NUMERIC {
            let func = builtin_func(vec![ty.clone(), ty.clone()], ty.clone());
            module.funcs.entry(op.to_string()).or_default().push(func);
        }
    }

    // Unary minus: `-(t) -> t`, added under the same `-` name as binary minus.
    for ty in &NUMERIC {
        let func = builtin_func(vec![ty.clone()], ty.clone());
        module.funcs.entry("-".to_string()).or_default().push(func);
    }

    // Boolean operators
    module
        .funcs
        .entry("&&".to_string())
        .or_default()
        .push(builtin_func(vec![Type::Bool, Type::Bool], Type::Bool));
    module
        .funcs
        .entry("||".to_string())
        .or_default()
        .push(builtin_func(vec![Type::Bool, Type::Bool], Type::Bool));
    module
        .funcs
        .entry("!".to_string())
        .or_default()
        .push(builtin_func(vec![Type::Bool], Type::Bool));
}

/// Build a body-less built-in `Func` with the given (already concrete) parameter
/// and return types. The `Intrinsic` body carries the return type so the
/// body-equals-return constraint the passes emit is trivially satisfied; its loc
/// is a dummy since built-ins have no source position.
fn builtin_func(params: Vec<Type>, ret: Type) -> Func {
    let dummy = Loc { start: 0, end: 0 };
    let arity = params.len();
    Func {
        body: Expr {
            kind: ExprKind::Intrinsic,
            ty: ret.clone(),
            loc: dummy,
        },
        ty: Type::Fn(params, Box::new(ret)),
        loc: FuncLocs {
            definition: dummy,
            arg_types: vec![dummy; arity],
            ret_type: dummy,
        },
    }
}

/// Whether a type is fully concrete — the invariant every function in a
/// `ResolvedModule` must satisfy.
fn fully_concrete(ty: &Type) -> bool {
    match ty {
        Type::T(_) => false,
        Type::Fn(args, ret) => args.iter().all(fully_concrete) && fully_concrete(ret),
        _ => true,
    }
}
