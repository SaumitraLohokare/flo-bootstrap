//! Lowering: rewrites `defer` into ordinary statements.
//!
//! `defer <body>` registers `body` to run when control leaves the nearest
//! enclosing scope, whichever way it leaves — falling off the end, `return`,
//! `break` or `continue`. This pass makes that explicit, so that everything
//! downstream sees a plain AST with no [`ExprKind::Defer`] in it.
//!
//! The rules it implements:
//!
//! - **Nearest scope wins.** A `defer` in a loop body belongs to the loop body,
//!   so it runs on every iteration. A braceless body or branch (`while c f();`)
//!   is a scope too, so nothing is ever hoisted out of a conditional.
//! - **LIFO.** Within a scope, deferred bodies run last-registered first.
//! - **Registration is positional.** A `defer` is live only for the exits that
//!   come after it, so an earlier `return` does not run it. That falls out of
//!   lowering in source order.
//! - **The value comes first.** A `return e` (or a scope's tail expression)
//!   evaluates `e` into a temporary, *then* runs the deferred bodies, then
//!   yields the temporary — so a defer that touches what `e` reads cannot
//!   change the result.
//!
//! It runs after type checking, which buys two things: every expression already
//! carries a concrete type, so the temporaries this pass introduces need no
//! inference; and the deferred bodies it copies onto each exit path are already
//! resolved, so duplicating them cannot upset the checker's one-type-var-per-
//! call-site invariant.

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    tokenizer::Loc,
    type_checker::diverges,
    types::Type,
};

#[cfg(test)]
mod tests;

/// Rewrite every `defer` in the module into ordinary statements.
pub fn lower(module: Module) -> Module {
    let Module { funcs, var_count } = module;

    let mut lowerer = Lowerer {
        next_var: var_count,
        frames: Vec::new(),
    };

    let funcs = funcs
        .into_iter()
        .map(|(name, funcs)| {
            let funcs = funcs.into_iter().map(|f| lowerer.lower_func(f)).collect();
            (name, funcs)
        })
        .collect();

    Module {
        funcs,
        var_count: lowerer.next_var,
    }
}

/// What kind of scope a frame stands for. Only the distinction `break` and
/// `continue` care about is recorded — they stop unwinding at the loop body,
/// while `return` keeps going to the function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Plain,
    LoopBody,
}

/// One enclosing scope, while its contents are being lowered.
struct Frame {
    kind: FrameKind,
    /// The deferred bodies registered in this scope so far, in the order they
    /// were written. They run in reverse.
    defers: Vec<Expr>,
}

struct Lowerer {
    /// The next unused variable id, seeded past every id the parser handed out.
    next_var: usize,
    /// The scopes enclosing the expression being lowered, outermost first.
    frames: Vec<Frame>,
}

impl Lowerer {
    fn lower_func(&mut self, func: Func) -> Func {
        let Func { body, ty, loc } = func;
        // The body is a scope even when it isn't written as a block: a `defer`
        // in `fn f() = defer g();` has nowhere else to attach.
        let body = self.lower_scope_expr(body, FrameKind::Plain);
        Func { body, ty, loc }
    }

    // ----------------------------------------------------------------------
    // Scopes
    // ----------------------------------------------------------------------

    /// Lower `expr` as a scope of its own: any `defer` inside it that is not
    /// nested in a further scope attaches here.
    fn lower_scope_expr(&mut self, expr: Expr, kind: FrameKind) -> Expr {
        if matches!(expr.kind, ExprKind::Scope(..)) {
            return self.lower_block(expr, kind);
        }

        // Not written as a block, but still a scope. Lower it under a frame of
        // its own, and only build a block around it if something actually
        // deferred — otherwise the AST keeps the shape it was parsed with.
        self.frames.push(Frame {
            kind,
            defers: Vec::new(),
        });
        let inner = self.lower_expr(expr);
        let frame = self.frames.pop().unwrap();

        if frame.defers.is_empty() {
            return inner;
        }

        let ty = inner.ty.clone();
        let loc = inner.loc;
        let tail = (!is_noop(&inner)).then(|| Box::new(inner));
        self.close(frame, Vec::new(), tail, ty, loc)
    }

    fn lower_block(&mut self, expr: Expr, kind: FrameKind) -> Expr {
        let Expr {
            kind: ExprKind::Scope(stmts, tail),
            ty,
            loc,
        } = expr
        else {
            unreachable!("lower_block on a non-scope expression")
        };

        self.frames.push(Frame {
            kind,
            defers: Vec::new(),
        });

        // Statements are lowered in source order, so by the time an exit is
        // reached the frame holds exactly the defers written above it.
        let mut new_stmts = Vec::with_capacity(stmts.len());
        for stmt in stmts {
            let stmt = self.lower_expr(stmt);
            // What a `defer` leaves behind does nothing, and neither does a
            // hand-written `{}` in statement position.
            if !is_noop(&stmt) {
                new_stmts.push(stmt);
            }
        }

        let tail = tail
            .map(|t| self.lower_expr(*t))
            .filter(|t| !is_noop(t))
            .map(Box::new);

        let frame = self.frames.pop().unwrap();
        self.close(frame, new_stmts, tail, ty, loc)
    }

    /// Assemble a scope from its lowered parts and the defers registered in it.
    /// The scope keeps the type it was given: a tail that yields a value is
    /// stashed in a temporary, so the deferred bodies run after the tail is
    /// evaluated and the scope still yields what the tail produced.
    fn close(
        &mut self,
        frame: Frame,
        mut stmts: Vec<Expr>,
        tail: Option<Box<Expr>>,
        ty: Type,
        loc: Loc,
    ) -> Expr {
        // Nothing reaches the end of a scope that always exits some other way,
        // and every such exit has already had these defers spliced into it. A
        // copy here would only be unreachable duplicate.
        let reaches_end =
            !stmts.iter().any(diverges) && !tail.as_deref().is_some_and(|t| diverges(t));

        if frame.defers.is_empty() || !reaches_end {
            return Expr {
                kind: ExprKind::Scope(stmts, tail),
                ty,
                loc,
            };
        }

        let tail = match tail {
            None => {
                stmts.extend(frame.defers.into_iter().rev());
                None
            }

            // No value to preserve: run the tail for its effects, then the
            // deferred bodies. The scope was `void` and stays `void`.
            Some(t) if t.ty == Type::Void => {
                stmts.push(*t);
                stmts.extend(frame.defers.into_iter().rev());
                None
            }

            Some(t) => {
                let t_ty = t.ty.clone();
                let t_loc = t.loc;
                let id = self.fresh_var();
                stmts.push(let_stmt(id, t_ty.clone(), *t));
                stmts.extend(frame.defers.into_iter().rev());
                Some(Box::new(var_expr(id, t_ty, t_loc)))
            }
        };

        Expr {
            kind: ExprKind::Scope(stmts, tail),
            ty,
            loc,
        }
    }

    // ----------------------------------------------------------------------
    // Expressions
    // ----------------------------------------------------------------------

    fn lower_expr(&mut self, expr: Expr) -> Expr {
        use ExprKind::*;

        let Expr { kind, ty, loc } = expr;

        let kind = match kind {
            Scope(..) => return self.lower_block(Expr { kind, ty, loc }, FrameKind::Plain),

            Defer(body) => {
                // The body is lowered against the defers registered *above* this
                // one, and only then registered itself, so it can never pick up
                // its own copy.
                let body = self.lower_expr(*body);
                self.frames
                    .last_mut()
                    .expect("`defer` outside of any scope")
                    .defers
                    .push(body);
                return noop(ty, loc);
            }

            Return(value) => return self.lower_return(value, ty, loc),
            Break | Continue => return self.lower_loop_jump(kind, ty, loc),

            // A branch is a scope, braces or not, so a `defer` in one is never
            // hoisted out of the conditional it was written under.
            If(cond, then, otherwise) => If(
                Box::new(self.lower_expr(*cond)),
                Box::new(self.lower_scope_expr(*then, FrameKind::Plain)),
                otherwise.map(|e| Box::new(self.lower_scope_expr(*e, FrameKind::Plain))),
            ),

            // The condition is evaluated outside the body, before each check, so
            // it belongs to the enclosing scope.
            While(cond, body) => While(
                Box::new(self.lower_expr(*cond)),
                Box::new(self.lower_scope_expr(*body, FrameKind::LoopBody)),
            ),

            Call(name, args, resolved) => Call(
                name,
                args.into_iter().map(|a| self.lower_expr(a)).collect(),
                resolved,
            ),
            Let(id, var_ty, init) => Let(id, var_ty, init.map(|e| Box::new(self.lower_expr(*e)))),
            Assign(target, value) => Assign(
                Box::new(self.lower_expr(*target)),
                Box::new(self.lower_expr(*value)),
            ),

            BuiltinOp(_) | Num(_) | Flt(_) | Bool(_) | Var(_) => kind,
        };

        Expr { kind, ty, loc }
    }

    /// A `return` leaves every enclosing scope, so every live defer runs — the
    /// innermost scope's first, and within a scope the last registered first.
    fn lower_return(&mut self, value: Option<Box<Expr>>, ty: Type, loc: Loc) -> Expr {
        let value = value.map(|v| Box::new(self.lower_expr(*v)));
        let defers = self.unwind(self.frames.len());

        if defers.is_empty() {
            return Expr {
                kind: ExprKind::Return(value),
                ty,
                loc,
            };
        }

        let mut stmts = Vec::with_capacity(defers.len() + 1);

        let value = match value {
            None => None,

            // The operand never yields, so it has already run these defers on
            // whatever exit it takes; this `return` is never reached.
            Some(v) if v.ty == Type::Never => {
                return Expr {
                    kind: ExprKind::Return(Some(v)),
                    ty,
                    loc,
                };
            }

            // Nothing to hand back, so there is nothing to stash: evaluate the
            // operand for its effects and return void.
            Some(v) if v.ty == Type::Void => {
                stmts.push(*v);
                None
            }

            // The returned value is computed before the deferred bodies, so one
            // that touches what it reads cannot change what comes back.
            Some(v) => {
                let v_ty = v.ty.clone();
                let v_loc = v.loc;
                let id = self.fresh_var();
                stmts.push(let_stmt(id, v_ty.clone(), *v));
                Some(Box::new(var_expr(id, v_ty, v_loc)))
            }
        };

        stmts.extend(defers);

        let ret = Expr {
            kind: ExprKind::Return(value),
            ty: ty.clone(),
            loc,
        };
        Expr {
            kind: ExprKind::Scope(stmts, Some(Box::new(ret))),
            ty,
            loc,
        }
    }

    /// `break` and `continue` both leave the loop body, so both run the defers
    /// of every scope up to and including it — and no further.
    fn lower_loop_jump(&mut self, kind: ExprKind, ty: Type, loc: Loc) -> Expr {
        let defers = self.unwind(self.loop_depth());

        if defers.is_empty() {
            return Expr { kind, ty, loc };
        }

        let jump = Expr {
            kind,
            ty: ty.clone(),
            loc,
        };
        Expr {
            kind: ExprKind::Scope(defers, Some(Box::new(jump))),
            ty,
            loc,
        }
    }

    /// Copies of the deferred bodies that run when control leaves the innermost
    /// `depth` scopes, in the order they run.
    ///
    /// These are copies, so a body that declares a variable ends up declaring
    /// the same id on several exit paths. That is fine — only one of those paths
    /// is ever taken.
    fn unwind(&self, depth: usize) -> Vec<Expr> {
        self.frames
            .iter()
            .rev()
            .take(depth)
            .flat_map(|frame| frame.defers.iter().rev().cloned())
            .collect()
    }

    /// How many frames a `break`/`continue` unwinds: up to and including the
    /// innermost loop body. The parser has already rejected them outside a loop,
    /// so that frame is always there.
    fn loop_depth(&self) -> usize {
        self.frames
            .iter()
            .rposition(|f| f.kind == FrameKind::LoopBody)
            .map(|i| self.frames.len() - i)
            .unwrap_or(0)
    }

    fn fresh_var(&mut self) -> usize {
        let id = self.next_var;
        self.next_var += 1;
        id
    }
}

// --------------------------------------------------------------------------

/// What a `defer` leaves behind where it was written.
fn noop(ty: Type, loc: Loc) -> Expr {
    Expr {
        kind: ExprKind::Scope(Vec::new(), None),
        ty,
        loc,
    }
}

fn is_noop(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Scope(stmts, None) if stmts.is_empty())
}

fn let_stmt(id: usize, ty: Type, init: Expr) -> Expr {
    let loc = init.loc;
    Expr {
        kind: ExprKind::Let(id, ty, Some(Box::new(init))),
        ty: Type::Void,
        loc,
    }
}

fn var_expr(id: usize, ty: Type, loc: Loc) -> Expr {
    Expr {
        kind: ExprKind::Var(id),
        ty,
        loc,
    }
}
