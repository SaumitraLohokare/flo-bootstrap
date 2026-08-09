use std::{collections::HashMap, fmt::Debug};

use crate::{
    tokenizer::Loc,
    types::{Type, TypeTable},
};

#[derive(Clone)]
pub struct Module {
    pub funcs: HashMap<String, Vec<Func>>,

    /// Every `type` declaration in the program, by name. Types live in their own
    /// namespace, so a type and a function may share a name.
    pub types: TypeTable,

    /// Every `use` written in the program, wherever it was written: at file
    /// scope, or as a statement inside a body. Name resolution already happened
    /// in the parser, so this flat list exists only to be validated — which has
    /// to wait until every `type` is in, since a `use` may name one declared
    /// further down the file.
    pub uses: Vec<UseDecl>,

    /// How many variable ids the parser handed out. A later pass that needs a
    /// temporary of its own mints it from here on, so it cannot collide with a
    /// source variable.
    pub var_count: usize,

    /// How many type variable ids the parser handed out. The checker mints its
    /// own from here on when instantiating a generic, so an instantiation's
    /// variables cannot collide with one written in the source.
    pub type_var_count: usize,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub body: Expr,
    pub ty: Type,
    pub loc: Loc,

    /// The function's type parameters: the name each was written with, and the
    /// type variable id standing for it inside `ty` and `body`. Empty for an
    /// ordinary function.
    ///
    /// A function with type parameters is never checked as written: it has no
    /// single type. It is checked once per instantiation, after `type_params`
    /// have been substituted away — so a `Func` reaching the back end always
    /// has this empty.
    pub type_params: Vec<(String, usize)>,
}

/// A `use Type::Case;`, wherever it was written.
///
/// The parser has already done everything this affects — it is what let a bare
/// `Case` parse as a literal rather than an unknown name — so all that is left
/// is to check that the type exists and has the case, which cannot happen until
/// every declaration is in.
#[derive(Debug, Clone)]
pub struct UseDecl {
    pub type_name: String,
    pub case: String,
    pub loc: Loc,
}

/// One step of a scope, before its tail.
///
/// A statement is not an expression: it has no type and yields no value. An
/// expression written in statement position is wrapped in [`StmtKind::Expr`]
/// rather than duplicated as a statement of its own, so `if`, `while` and the
/// rest exist in exactly one place.
#[derive(Debug, Clone)]
pub struct Statement {
    pub kind: StmtKind,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    // Let(var_id, var_ty, init) - `var_ty` is the variable's own type: the
    // annotation if it had one, else a fresh type var shared with every `Var`
    // that reads it. `init` is absent for `let x;`, whose type is then pinned by
    // whatever assigns to it first.
    Let(usize, Type, Option<Expr>),

    // Use(type_name, case) - brings a case name into scope for the rest of the
    // enclosing scope, so it can be written bare. Purely a name binding: there
    // is nothing to evaluate, and nothing for a later pass to lower.
    Use(String, String),

    // An expression evaluated for its effect; its value is discarded.
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    BuiltinOp(Op), // To specify it is a builtin function for a specific op

    Num(u64),
    Flt(f64),
    Bool(bool),
    Var(usize),

    // Call(name, type_args, args, resolved_name)
    //
    // `type_args` are the ones written explicitly with a turbofish
    // (`add::<i32>(a, b)`), and are empty otherwise — an inferred instantiation
    // leaves no trace here, it is recorded in the resolved name.
    Call(String, Vec<Type>, Vec<Expr>, Option<String>),

    // Scope(stmts, tail)
    Scope(Vec<Statement>, Option<Box<Expr>>),

    // If(cond, then, else)
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),

    // While(cond, body) - always void typed. The body's value is discarded, so
    // it must be void too; a `while` never diverges, because the condition may
    // be false on the first check.
    While(Box<Expr>, Box<Expr>),

    // Return(value) - always NoReturn typed; `value` is absent for a bare `return`
    Return(Option<Box<Expr>>),

    // Break / Continue - always NoReturn typed, like `Return`. Neither carries a
    // value yet. The parser rejects them outside a loop body.
    Break,
    Continue,

    // Assign(target, value) - yields the value of `value`, like C. `target` must
    // be an l-value (see `Expr::is_lvalue`).
    Assign(Box<Expr>, Box<Expr>),

    // CaseLit(qualifier, case, fields) - a type literal, `Some { val: 0 }`.
    //
    // A literal names a *case*, never a type: several types may have a case by
    // that name, so which one this is comes from context (see `Type::SomeType`).
    // `qualifier` is the `Vec::<i32>::` of `Vec::<i32>::Vec { .. }`, the way to
    // say it outright; it is `None` for the bare form a `use` allows, and
    // dropped once the checker has settled the type.
    //
    // Its type arguments are empty when no turbofish was written, which is not
    // the same as "this type has none": `Option::Some { val: 0 }` names the type
    // and leaves the argument to inference. The checker fills in a fresh
    // variable per parameter of the declaration.
    CaseLit(Option<(String, Vec<Type>)>, String, Vec<FieldInit>),

    // Field(receiver, name) - `foo.bar`. Only legal on a type with a single
    // case; reaching into a sum type needs `is`. An l-value when the receiver is.
    Field(Box<Expr>, String),
}

/// One `name: value` in a type literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    /// Of the field's name, so a duplicate or unknown field underlines the name
    /// rather than the whole literal.
    pub loc: Loc,
}

#[derive(Debug, Clone, Copy)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,

    BitAnd,
    BitOr,
    BitXor,

    And,
    Or,

    Eq,
    NEq,
    Lt,
    Lte,
    Gt,
    Gte,
}

// -------------------------------------------

impl Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Module:")?;

        let mut type_names = self.types.keys().collect::<Vec<_>>();
        type_names.sort();

        for name in type_names {
            let decl = &self.types[name];

            let params = if decl.type_params.is_empty() {
                String::new()
            } else {
                let list = decl
                    .type_params
                    .iter()
                    .map(|(name, id)| format!("{name}='t{id}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("<{list}>")
            };

            let cases = decl
                .cases
                .iter()
                .map(|case| {
                    if case.fields.is_empty() {
                        case.name.clone()
                    } else {
                        let fields = case
                            .fields
                            .iter()
                            .map(|field| format!("{}: {:?}", field.name, field.ty))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{} {{ {fields} }}", case.name)
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ");

            writeln!(f, "type {name}{params} = {cases};")?;
        }

        // Only the file-scope ones would be interesting on their own, but the
        // list does not say which is which — a scoped `use` also prints inside
        // the body it belongs to, so it simply shows up twice.
        for use_decl in &self.uses {
            writeln!(f, "use {}::{};", use_decl.type_name, use_decl.case)?;
        }

        let mut names = self.funcs.keys().collect::<Vec<_>>();
        names.sort();

        for name in names {
            let funcs = self.funcs.get(name).unwrap();
            for func in funcs {
                if let ExprKind::BuiltinOp(_) = func.body.kind {
                    break; // Don't print if function was builtin
                }

                let params = if func.type_params.is_empty() {
                    String::new()
                } else {
                    let list = func
                        .type_params
                        .iter()
                        .map(|(name, id)| format!("{name}='t{id}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("<{list}>")
                };

                let expr_string = func.body.pretty_print(0);
                writeln!(f, "fn {name}{params}{:?} = {expr_string};", func.ty)?;
            }
        }

        Ok(())
    }
}

impl Func {
    /// One instantiation of a generic function: `subst` maps each of this
    /// function's [`Func::type_params`] to a concrete type, and the result is
    /// an ordinary function that can be checked like any other.
    ///
    /// The body is *copied*, so the same variable and type variable ids appear
    /// in every instantiation. That is fine: each one is checked on its own,
    /// with its own solver state, and its locals are its own.
    pub fn instantiate(&self, subst: &HashMap<usize, Type>) -> Func {
        Func {
            body: self.body.substitute(subst),
            ty: self.ty.substitute(subst),
            loc: self.loc,
            type_params: Vec::new(),
        }
    }
}

impl Statement {
    /// As [`Expr::substitute`], for one step of a scope.
    fn substitute(&self, subst: &HashMap<usize, Type>) -> Statement {
        let kind = match &self.kind {
            // The declared type is substituted too: it is where a `let x: T`
            // annotation inside a generic body lives.
            StmtKind::Let(id, var_ty, init) => StmtKind::Let(
                *id,
                var_ty.substitute(subst),
                init.as_ref().map(|e| e.substitute(subst)),
            ),
            // Nothing but names, and a name is not a type.
            StmtKind::Use(ty, case) => StmtKind::Use(ty.clone(), case.clone()),
            StmtKind::Expr(e) => StmtKind::Expr(e.substitute(subst)),
        };

        Statement {
            kind,
            loc: self.loc,
        }
    }

    fn pretty_print(&self, indent_amt: usize) -> String {
        let indent = " ".repeat(indent_amt);
        match &self.kind {
            StmtKind::Let(id, var_ty, init) => {
                let init = match init {
                    Some(e) => format!(" = {}", e.pretty_print(0)),
                    None => "".to_string(),
                };
                format!("{indent}let var_{id}:{var_ty:?}{init}")
            }
            StmtKind::Use(ty, case) => format!("{indent}use {ty}::{case}"),
            StmtKind::Expr(e) => e.pretty_print(indent_amt),
        }
    }
}

impl Expr {
    /// This expression with every type variable in `subst` replaced. Only types
    /// are touched — the shape of the tree, its spans and its variable ids are
    /// all preserved.
    fn substitute(&self, subst: &HashMap<usize, Type>) -> Expr {
        use ExprKind::*;

        let sub_box = |e: &Expr| Box::new(e.substitute(subst));

        let kind = match &self.kind {
            Call(name, type_args, args, resolved) => Call(
                name.clone(),
                type_args.iter().map(|t| t.substitute(subst)).collect(),
                args.iter().map(|a| a.substitute(subst)).collect(),
                resolved.clone(),
            ),
            Scope(stmts, tail) => Scope(
                stmts.iter().map(|s| s.substitute(subst)).collect(),
                tail.as_deref().map(sub_box),
            ),
            If(cond, then, otherwise) => If(
                sub_box(cond),
                sub_box(then),
                otherwise.as_deref().map(sub_box),
            ),
            While(cond, body) => While(sub_box(cond), sub_box(body)),
            Return(value) => Return(value.as_deref().map(sub_box)),
            Assign(target, value) => Assign(sub_box(target), sub_box(value)),
            // A qualifier's type arguments can mention a type parameter too:
            // `Vec::<T>::Vec { .. }` inside a generic function.
            CaseLit(qualifier, case, fields) => CaseLit(
                qualifier.as_ref().map(|(name, args)| {
                    (
                        name.clone(),
                        args.iter().map(|a| a.substitute(subst)).collect(),
                    )
                }),
                case.clone(),
                fields
                    .iter()
                    .map(|f| FieldInit {
                        name: f.name.clone(),
                        value: f.value.substitute(subst),
                        loc: f.loc,
                    })
                    .collect(),
            ),
            Field(recv, name) => Field(sub_box(recv), name.clone()),

            BuiltinOp(op) => BuiltinOp(*op),
            Num(n) => Num(*n),
            Flt(n) => Flt(*n),
            Bool(b) => Bool(*b),
            Var(id) => Var(*id),
            Break => Break,
            Continue => Continue,
        };

        Expr {
            kind,
            ty: self.ty.substitute(subst),
            loc: self.loc,
        }
    }

    fn pretty_print(&self, indent_amt: usize) -> String {
        let indent = " ".repeat(indent_amt);
        match &self.kind {
            ExprKind::Num(num) => format!("{indent}{num}:{:?}", self.ty),
            ExprKind::Flt(num) => format!("{indent}{num}:{:?}", self.ty),
            ExprKind::Bool(b) => format!("{indent}{b}:{:?}", self.ty),
            ExprKind::Var(id) => format!("{indent}var_{id}:{:?}", self.ty),
            ExprKind::Call(name, type_args, exprs, resolved) => {
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
                let turbofish = if type_args.is_empty() {
                    String::new()
                } else {
                    let list = type_args
                        .iter()
                        .map(|t| format!("{t:?}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("::<{list}>")
                };
                format!("{indent}{name}{turbofish}({arg_list}):{:?}", self.ty)
            }
            ExprKind::BuiltinOp(op) => format!("{indent}@builtin({op:?})"),
            ExprKind::Scope(stmts, tail) => {
                let exprs_list = if !stmts.is_empty() {
                    format!(
                        "{}\n",
                        stmts
                            .iter()
                            .map(|s| s.pretty_print(indent_amt + 2))
                            .collect::<Vec<_>>()
                            .join(";\n")
                    )
                } else {
                    "".to_string()
                };
                let tail = match tail {
                    Some(e) => format!("{}\n", e.pretty_print(indent_amt + 2)),
                    None => "".to_string(),
                };
                format!("{indent}{{\n{exprs_list}{tail}}}:{:?}", self.ty)
            }
            ExprKind::If(cond, then, otherwise) => {
                let otherwise = match otherwise {
                    Some(e) => format!(" else {}", e.pretty_print(indent_amt)),
                    None => "".to_string(),
                };
                format!(
                    "{indent}if:{:?} {} {}{}",
                    self.ty,
                    cond.pretty_print(indent_amt),
                    then.pretty_print(indent_amt),
                    otherwise
                )
            }
            ExprKind::While(cond, body) => format!(
                "{indent}while:{:?} {} {}",
                self.ty,
                cond.pretty_print(indent_amt),
                body.pretty_print(indent_amt),
            ),
            ExprKind::Break => format!("{indent}break:{:?}", self.ty),
            ExprKind::Continue => format!("{indent}continue:{:?}", self.ty),
            ExprKind::Return(value) => match value {
                Some(e) => format!("{indent}return {}:{:?}", e.pretty_print(0), self.ty),
                None => format!("{indent}return:{:?}", self.ty),
            },
            ExprKind::Assign(target, value) => format!(
                "{indent}{} = {}:{:?}",
                target.pretty_print(0),
                value.pretty_print(0),
                self.ty
            ),
            ExprKind::CaseLit(qualifier, case, fields) => {
                let qualifier = match qualifier {
                    Some((name, args)) if args.is_empty() => format!("{name}::"),
                    Some((name, args)) => {
                        let list = args
                            .iter()
                            .map(|a| format!("{a:?}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{name}::<{list}>::")
                    }
                    None => String::new(),
                };
                // A case with no fields is written bare, so it prints that way.
                let field_list = if fields.is_empty() {
                    String::new()
                } else {
                    let list = fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, f.value.pretty_print(0)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" {{ {list} }}")
                };
                format!("{indent}{qualifier}{case}{field_list}:{:?}", self.ty)
            }
            ExprKind::Field(recv, name) => {
                format!("{indent}{}.{name}:{:?}", recv.pretty_print(0), self.ty)
            }
        }
    }
}
