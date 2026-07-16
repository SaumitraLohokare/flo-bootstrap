use std::{collections::HashMap, fmt::Debug};

use crate::{tokenizer::Loc, types::Type};

#[derive(Debug, Clone)]
pub struct FuncLocs {
    pub definition: Loc,
    pub arg_types: Vec<Loc>,
    pub ret_type: Loc,
}

#[derive(Clone)]
pub struct Module {
    /// Every function definition, grouped by source name. A name maps to a list
    /// of overloads (keyed conceptually by their `(params, return)` signature);
    /// the parser never rejects duplicates — an unresolvable overload set instead
    /// surfaces as an ambiguity error during type checking.
    pub funcs: HashMap<String, Vec<Func>>,
}

/// The output of the type checker: every overload has been resolved and every
/// reachable function monomorphized, so names are now mangled and unique and each
/// maps to exactly one concrete `Func`.
#[derive(Clone)]
pub struct ResolvedModule {
    pub funcs: HashMap<String, Func>,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub body: Expr,
    pub ty: Type,
    pub loc: FuncLocs,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Num(u64),
    Bool(bool),
    Var(usize),
    Call(String, Vec<Expr>),
    Scope(Vec<Expr>, Option<Box<Expr>>),
    /// A body-less built-in. Operators desugar to `Call`s against built-in
    /// overloads (`+`, `-`, …) whose bodies are this sentinel: they carry a
    /// concrete signature but no source to walk, so every AST walk treats it as a
    /// leaf. A real implementation is filled in later (e.g. by the interpreter).
    Intrinsic,
}

// -------------------------------------------

impl Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Module:")?;

        for (name, overloads) in &self.funcs {
            for func in overloads {
                let expr_string = func.body.pretty_print(0);
                writeln!(f, "fn {name}{:?} = {expr_string};", func.ty)?;
            }
        }

        Ok(())
    }
}

impl Debug for ResolvedModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Module:")?;

        for (name, func) in &self.funcs {
            let expr_string = func.body.pretty_print(0);
            writeln!(f, "fn {name}{:?} = {expr_string};", func.ty)?;
        }

        Ok(())
    }
}

impl Expr {
    fn pretty_print(&self, indent_amt: usize) -> String {
        let indent = " ".repeat(indent_amt);
        match &self.kind {
            ExprKind::Num(num) => format!("{indent}{num}:{:?}", self.ty),
            ExprKind::Bool(value) => format!("{indent}{value}:{:?}", self.ty),
            ExprKind::Var(id) => format!("{indent}var_{id}:{:?}", self.ty),
            ExprKind::Call(name, exprs) => {
                let arg_list = exprs
                    .iter()
                    .map(|arg| arg.pretty_print(0))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{indent}{name}({arg_list}):{:?}", self.ty)
            }
            ExprKind::Intrinsic => format!("{indent}<intrinsic>:{:?}", self.ty),
            ExprKind::Scope(exprs, tail) => {
                let exprs_str = exprs
                    .iter()
                    .map(|e| format!("{};", e.pretty_print(indent_amt + 2)))
                    .collect::<Vec<_>>()
                    .join("\n");
                let tail_str = if let Some(e) = tail {
                    format!("\n{}", e.pretty_print(indent_amt + 2))
                } else {
                    "".to_string()
                };

                format!("{indent}{{\n{exprs_str}{tail_str}\n}}")
            }
        }
    }
}
