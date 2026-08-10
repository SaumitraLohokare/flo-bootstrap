//! Whole-program checks on `type` declarations.
//!
//! These run before any function is checked, and are separate from it for two
//! reasons: a declaration may mention one written further down the file, so
//! nothing can be checked until every declaration is in; and a type that does
//! not exist or has no size makes every error after it noise.
//!
//! Duplicate types, cases and fields are caught while parsing — the parser has
//! both locations to hand there, which makes for a better message.

use std::collections::HashSet;

use crate::{
    ast::{Expr, ExprKind, Module, Statement, StmtKind, UseDecl},
    errors::FloErr,
    tokenizer::Loc,
    types::{Type, TypeTable},
};

/// Check every type the program declares or mentions. Returns all the problems
/// found rather than stopping at the first, so one run reports them together.
pub fn check_type_decls(module: &Module) -> Vec<FloErr> {
    let mut errs = Vec::new();

    check_mentions(module, &mut errs);
    check_uses(module, &mut errs);

    // Only worth walking types that exist and take the right arguments — the
    // walk substitutes type arguments in, and would go wrong on either.
    if errs.is_empty() {
        check_sizes(&module.types, &mut errs);
    }

    errs
}

// --------------------------------------------------------------------------
// Named types exist, and take the arguments they were given
// --------------------------------------------------------------------------

fn check_mentions(module: &Module, errs: &mut Vec<FloErr>) {
    for decl in module.types.values() {
        for case in &decl.cases {
            for field in &case.fields {
                check_type(&field.ty, field.loc, &module.types, errs, true);
            }
        }
    }

    for funcs in module.funcs.values() {
        for func in funcs {
            check_type(&func.ty, func.loc, &module.types, errs, true);
            walk_written_types(&func.body, &mut |ty, loc, arity| {
                check_type(ty, loc, &module.types, errs, arity)
            });
        }
    }
}

/// `arity` is whether the type arguments have to match the declaration's
/// parameters. They always do where a type is *written*, and do not for a
/// literal's qualifier, where leaving them off means inferring them.
fn check_type(ty: &Type, loc: Loc, types: &TypeTable, errs: &mut Vec<FloErr>, arity: bool) {
    use Type::*;

    match ty {
        User(name, args) => {
            match types.get(name) {
                None => errs.push(FloErr::UnknownType {
                    name: name.clone(),
                    loc,
                }),
                Some(decl) if arity && decl.type_params.len() != args.len() => {
                    errs.push(FloErr::TypeArityMismatch {
                        name: name.clone(),
                        expected: decl.type_params.len(),
                        got: args.len(),
                        loc,
                    })
                }
                Some(_) => {}
            }

            // Only the outermost mention can have its arguments inferred; an
            // argument that *is* written has to be a whole type.
            for arg in args {
                check_type(arg, loc, types, errs, true);
            }
        }
        Fn(args, ret) => {
            for arg in args {
                check_type(arg, loc, types, errs, true);
            }
            check_type(ret, loc, types, errs, true);
        }
        _ => {}
    }
}

// --------------------------------------------------------------------------
// Every `use` names a real case of a real type
// --------------------------------------------------------------------------

fn check_uses(module: &Module, errs: &mut Vec<FloErr>) {
    for use_decl in &module.uses {
        check_use(use_decl, &module.types, errs);
    }
}

fn check_use(use_decl: &UseDecl, types: &TypeTable, errs: &mut Vec<FloErr>) {
    let UseDecl {
        type_name,
        case,
        loc,
    } = use_decl;

    match types.get(type_name) {
        None => errs.push(FloErr::UnknownType {
            name: type_name.clone(),
            loc: *loc,
        }),
        // A generic type's arguments are not named by a `use` and are not needed
        // to answer this, so the type is reported bare.
        Some(decl) if decl.case(case).is_none() => errs.push(FloErr::NoSuchCase {
            ty: Type::User(type_name.clone(), Vec::new()),
            case: case.clone(),
            loc: *loc,
        }),
        Some(_) => {}
    }
}

/// Visit every type *written down* in a body: the annotation on a `let`, a
/// turbofish's arguments, and a literal's qualifier. Every other type in the
/// tree is a variable the parser minted, and says nothing about what the source
/// named.
///
/// The flag passed to `f` is whether the mention has to give the declaration's
/// type arguments in full.
fn walk_written_types(expr: &Expr, f: &mut impl FnMut(&Type, Loc, bool)) {
    use ExprKind::*;

    match &expr.kind {
        Call(_, type_args, args, _) => {
            for ty in type_args {
                f(ty, expr.loc, true);
            }
            for arg in args {
                walk_written_types(arg, f);
            }
        }
        CaseLit(qualifier, _, fields) => {
            if let Some((name, args)) = qualifier {
                // A qualifier with no turbofish leaves the arguments to
                // inference, so only the name has to check out.
                let arity = !args.is_empty();
                f(&Type::User(name.clone(), args.clone()), expr.loc, arity);
            }
            for field in fields {
                walk_written_types(&field.value, f);
            }
        }
        Field(recv, _) => walk_written_types(recv, f),

        Scope(stmts, tail) => {
            for stmt in stmts {
                walk_stmt_written_types(stmt, f);
            }
            if let Some(tail) = tail {
                walk_written_types(tail, f);
            }
        }
        If(cond, then, otherwise) => {
            walk_written_types(cond, f);
            walk_written_types(then, f);
            if let Some(otherwise) = otherwise {
                walk_written_types(otherwise, f);
            }
        }
        While(cond, body) => {
            walk_written_types(cond, f);
            walk_written_types(body, f);
        }
        Logical(_, lhs, rhs) => {
            walk_written_types(lhs, f);
            walk_written_types(rhs, f);
        }
        Return(value) => {
            if let Some(value) = value {
                walk_written_types(value, f);
            }
        }
        Assign(target, value) => {
            walk_written_types(target, f);
            walk_written_types(value, f);
        }

        BuiltinOp(_) | Num(_) | Flt(_) | Bool(_) | Var(_) | Break | Continue => {}
    }
}

fn walk_stmt_written_types(stmt: &Statement, f: &mut impl FnMut(&Type, Loc, bool)) {
    match &stmt.kind {
        StmtKind::Let(_, ty, init) => {
            f(ty, stmt.loc, true);
            if let Some(init) = init {
                walk_written_types(init, f);
            }
        }
        // Its type is checked from `Module::uses`, which has every `use` in the
        // program — including this one.
        StmtKind::Use(..) => {}
        StmtKind::Expr(e) => walk_written_types(e, f),
    }
}

// --------------------------------------------------------------------------
// Every type has a size
// --------------------------------------------------------------------------

/// Reject a type that contains itself. Every field holds its type by value —
/// there is no indirection in the language yet — so any cycle at all means the
/// type has no size. Once pointers land a `*T` field will break the cycle, and
/// only the direct kind will remain an error.
fn check_sizes(types: &TypeTable, errs: &mut Vec<FloErr>) {
    // One cycle is reachable from every type on it, and reporting it once per
    // member would be noise.
    let mut reported: HashSet<&str> = HashSet::new();

    let mut names = types.keys().collect::<Vec<_>>();
    names.sort();

    for name in names {
        if reported.contains(name.as_str()) {
            continue;
        }

        let mut path = Vec::new();
        if let Err(cycle) = visit(name, &[], types, &mut path) {
            for member in &cycle {
                reported.insert(member);
            }
            errs.push(FloErr::RecursiveType {
                name: cycle[0].to_string(),
                cycle: cycle.iter().map(|s| s.to_string()).collect(),
                loc: types[cycle[0]].loc,
            });
        }
    }
}

/// Walk into `name<args>`, failing with the cycle if the walk comes back to a
/// type it is already inside of.
///
/// `args` is what makes this exact rather than a guess about the declarations:
/// `type A = { v: B<A> };` only contains itself because `B<T>` holds its `T`,
/// which is only visible once `A` has been substituted in for it. The path is
/// keyed by name and not by name-and-arguments, so a type that nests itself
/// ever deeper (`type L<T> = C { next: L<L<T>> }`) is caught rather than walked
/// forever — it is genuinely infinite either way.
fn visit<'a>(
    name: &'a str,
    args: &[Type],
    types: &'a TypeTable,
    path: &mut Vec<&'a str>,
) -> Result<(), Vec<&'a str>> {
    if let Some(at) = path.iter().position(|n| *n == name) {
        let mut cycle = path[at..].to_vec();
        cycle.push(name);
        return Err(cycle);
    }

    let Some(decl) = types.get(name) else {
        // Undeclared, and already reported.
        return Ok(());
    };

    path.push(decl.name.as_str());

    let subst = decl.subst(args);
    for case in &decl.cases {
        for field in &case.fields {
            // A field's type is a primitive, a type parameter, or a named type.
            // A parameter that nothing substituted stays a variable: which type
            // it stands for is not known here, and whether *that* has a size is
            // checked wherever it is written down.
            let Type::User(inner, inner_args) = field.ty.substitute(&subst) else {
                continue;
            };
            // Borrow the name from the table rather than from the substituted
            // type, which is a temporary.
            if let Some((inner, _)) = types.get_key_value(&inner) {
                visit(inner.as_str(), &inner_args, types, path)?;
            }
        }
    }

    path.pop();
    Ok(())
}
