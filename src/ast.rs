use std::{collections::HashMap, fmt::Debug};

use crate::{
    tokenizer::{Loc, TokenKind},
    types::Type,
};

#[derive(Clone)]
pub struct Module {
    pub funcs: HashMap<String, Vec<Func>>,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub body: Expr,
    pub ty: Type,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    BuiltinOp(TokenKind), // To specify it is a builtin function for a specific op

    Num(u64),
    Flt(f64),
    Bool(bool),
    Var(usize),
    // Call(name, args, resolved_name)
    Call(String, Vec<Expr>, Option<String>),
}

// -------------------------------------------

impl Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Module:")?;

        let mut names = self.funcs.keys().collect::<Vec<_>>();
        names.sort();

        for name in names {
            let funcs = self.funcs.get(name).unwrap();
            for func in funcs {
                if let ExprKind::BuiltinOp(_) = func.body.kind {
                    break; // Don't print if function was builtin
                }

                let expr_string = func.body.pretty_print(0);
                writeln!(f, "fn {name}{:?} = {expr_string};", func.ty)?;
            }
        }

        Ok(())
    }
}

impl Expr {
    fn pretty_print(&self, indent: usize) -> String {
        let indent = " ".repeat(indent);
        match &self.kind {
            ExprKind::Num(num) => format!("{indent}{num}:{:?}", self.ty),
            ExprKind::Flt(num) => format!("{indent}{num}:{:?}", self.ty),
            ExprKind::Bool(b) => format!("{indent}{b}:{:?}", self.ty),
            ExprKind::Var(id) => format!("{indent}var_{id}:{:?}", self.ty),
            ExprKind::Call(name, exprs, resolved) => {
                let arg_list = exprs
                    .iter()
                    .map(|arg| arg.pretty_print(0))
                    .collect::<Vec<_>>()
                    .join(", ");
                let name = if let Some(resolved) = resolved {
                    resolved.to_string()
                } else {
                    format!("[unresolved]{name}")
                };
                format!("{indent}{name}({arg_list}):{:?}", self.ty)
            }
            ExprKind::BuiltinOp(op) => format!("{indent}@builtin({})", op.pretty_name()),
        }
    }
}
