use std::{collections::HashMap, fmt::Debug};

use crate::{errors::{FloErr, FloResult}, tokenizer::Loc, types::Type};

#[derive(Clone)]
pub struct Module {
    pub funcs: HashMap<String, Func>,
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

impl Expr {
    fn replace_types(&mut self, replace_map: &HashMap<Type, Type>) {
        self.ty.replace_types(replace_map);

        match self.kind {
            ExprKind::Num(_) => {}
        }
    }

    fn ensure_resolved(&self) -> FloResult<()> {
        if !self.ty.is_known() {
            return Err(FloErr::UnresolvedType { loc: self.loc });
        }
        
        match self.kind {
            ExprKind::Num(_) => {}
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Num(u64),
}

impl Func {
    pub fn replace_types(&mut self, replace_map: &HashMap<Type, Type>) {
        self.ty.replace_types(replace_map);
        self.body.replace_types(replace_map);
    }

    pub fn ensure_resolved(&self) -> FloResult<()> {
        self.body.ensure_resolved()
    }
}

// -------------------------------------------

impl Debug for Module {
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
    fn pretty_print(&self, indent: usize) -> String {
        let indent = " ".repeat(indent);
        match self.kind {
            ExprKind::Num(num) => format!("{indent}{num}:{:?}", self.ty),
        }
    }
}