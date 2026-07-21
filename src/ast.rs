use std::{collections::HashMap, fmt::Debug};

use crate::{tokenizer::Loc, types::Type};

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
    Num(u64),
    Var(usize),
    // Call(name, args, resolved_name)
    Call(String, Vec<Expr>, Option<String>),
}

// -------------------------------------------

impl Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Module:")?;

        for (name, funcs) in &self.funcs {
            for func in funcs {
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
        }
    }
}
