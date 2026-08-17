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
    ast::{Expr, ExprKind, Module, Statement, StmtKind},
    errors::FloErr,
    tokenizer::Loc,
    types::{DeclKind, RecordDecl, Type, TypeTable},
};

/// Check every type the program declares or mentions. Returns all the problems
/// found rather than stopping at the first, so one run reports them together.
pub fn check_type_decls(module: &Module) -> Vec<FloErr> {
    let mut errs = Vec::new();

    check_mentions(module, &mut errs);

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
        match &decl.kind {
            DeclKind::Record(record) => check_record_decl(record, &module.types, errs),
            DeclKind::Sum(cases) => {
                for case in cases {
                    if let Some(payload) = &case.payload {
                        check_record_decl(payload, &module.types, errs);
                    }
                }
            }
        }
    }

    for funcs in module.funcs.values() {
        for func in funcs {
            check_type(&func.ty, func.loc, &module.types, errs);
            walk_written_types(&func.body, &mut |ty, loc| {
                check_type(ty, loc, &module.types, errs)
            });
            check_qualifiers(&func.body, &module.types, errs);
        }
    }
}

fn check_record_decl(record: &RecordDecl, types: &TypeTable, errs: &mut Vec<FloErr>) {
    for field in &record.fields {
        check_type(&field.ty, field.loc, types, errs);
    }
}

/// Every declared type a written type mentions must exist and be given the
/// arguments it takes — including the ones nested inside an anonymous record or
/// sum, which are written types like any other.
fn check_type(ty: &Type, loc: Loc, types: &TypeTable, errs: &mut Vec<FloErr>) {
    use Type::*;

    match ty {
        User(name, args) => {
            match types.get(name) {
                None => errs.push(FloErr::UnknownType {
                    name: name.clone(),
                    loc,
                }),
                Some(decl) if decl.type_params.len() != args.len() => {
                    errs.push(FloErr::TypeArityMismatch {
                        name: name.clone(),
                        expected: decl.type_params.len(),
                        got: args.len(),
                        loc,
                    })
                }
                Some(_) => {}
            }

            for arg in args {
                check_type(arg, loc, types, errs);
            }
        }
        Fn(args, ret) => {
            for arg in args {
                check_type(arg, loc, types, errs);
            }
            check_type(ret, loc, types, errs);
        }
        // Written down, so whatever is inside was written down too. `SomeRecord`
        // and `SomeSum` cannot appear here: nothing that was written is open.
        AnonRecord(record) => {
            for field_ty in record.types() {
                check_type(field_ty, loc, types, errs);
            }
        }
        AnonSum(cases) => {
            for case in cases {
                if let Some(payload) = &case.payload {
                    check_type(payload, loc, types, errs);
                }
            }
        }
        _ => {}
    }
}

// --------------------------------------------------------------------------
// Every literal's qualifier names a real type
// --------------------------------------------------------------------------

/// A qualifier is the one place a name is read as a type without the parser
/// having been able to check it: whether `foo` in `foo.bar` is a type cannot be
/// answered while parsing, since the declaration may be further down the file. So
/// `foo.bar` is parsed as a qualified literal and the name is checked here.
///
/// Only the name is checked. A qualifier cannot carry type arguments, so there is
/// no arity to check, and whether the type is the right *kind* — a record for a
/// record literal, a sum with that case for a case literal — is settled by
/// unification, which has better information to say it with.
fn check_qualifiers(expr: &Expr, types: &TypeTable, errs: &mut Vec<FloErr>) {
    walk_qualifiers(expr, &mut |name, loc| {
        if !types.contains_key(name) {
            errs.push(FloErr::NotATypeOrVariable {
                name: name.to_string(),
                loc,
            });
        }
    });
}

/// Visit every type *written down* in a body: the annotation on a `let` and the
/// argument of a builtin. Every other type in the tree is a variable the parser
/// minted, and says nothing about what the source named — a literal's qualifier
/// is a bare name and is checked by [`walk_qualifiers`] instead.
fn walk_written_types(expr: &Expr, f: &mut impl FnMut(&Type, Loc)) {
    use ExprKind::*;

    match &expr.kind {
        Call(_, args, _) => {
            for arg in args {
                walk_written_types(arg, f);
            }
        }
        RecordLit(_, fields) => {
            for field in fields {
                walk_written_types(&field.value, f);
            }
        }
        CaseLit(_, _, payload) => {
            if let Some(payload) = payload {
                walk_written_types(payload, f);
            }
        }
        Field(recv, _) => walk_written_types(recv, f),
        // A builtin's type argument is written out in full — there is no context
        // for it to be inferred from.
        Cast(target, value) => {
            f(target, expr.loc);
            walk_written_types(value, f);
        }
        TypeInfo(_, ty) => f(ty, expr.loc),

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

        BuiltinOp(_) | Num(_) | Flt(_) | Bool(_) | Var(_) | Zeroed | Break | Continue => {}
    }
}

fn walk_stmt_written_types(stmt: &Statement, f: &mut impl FnMut(&Type, Loc)) {
    match &stmt.kind {
        StmtKind::Let(_, ty, init) => {
            f(ty, stmt.loc);
            if let Some(init) = init {
                walk_written_types(init, f);
            }
        }
        StmtKind::Expr(e) => walk_written_types(e, f),
    }
}

/// Visit the type name of every qualified literal in a body.
fn walk_qualifiers(expr: &Expr, f: &mut impl FnMut(&str, Loc)) {
    use ExprKind::*;

    match &expr.kind {
        RecordLit(qualifier, fields) => {
            if let Some(name) = qualifier {
                f(name, expr.loc);
            }
            for field in fields {
                walk_qualifiers(&field.value, f);
            }
        }
        CaseLit(qualifier, _, payload) => {
            if let Some(name) = qualifier {
                f(name, expr.loc);
            }
            if let Some(payload) = payload {
                walk_qualifiers(payload, f);
            }
        }
        Call(_, args, _) => {
            for arg in args {
                walk_qualifiers(arg, f);
            }
        }
        Field(recv, _) => walk_qualifiers(recv, f),
        Cast(_, value) => walk_qualifiers(value, f),
        Scope(stmts, tail) => {
            for stmt in stmts {
                match &stmt.kind {
                    StmtKind::Let(_, _, Some(init)) => walk_qualifiers(init, f),
                    StmtKind::Let(..) => {}
                    StmtKind::Expr(e) => walk_qualifiers(e, f),
                }
            }
            if let Some(tail) = tail {
                walk_qualifiers(tail, f);
            }
        }
        If(cond, then, otherwise) => {
            walk_qualifiers(cond, f);
            walk_qualifiers(then, f);
            if let Some(otherwise) = otherwise {
                walk_qualifiers(otherwise, f);
            }
        }
        While(cond, body) => {
            walk_qualifiers(cond, f);
            walk_qualifiers(body, f);
        }
        Logical(_, lhs, rhs) => {
            walk_qualifiers(lhs, f);
            walk_qualifiers(rhs, f);
        }
        Return(value) => {
            if let Some(value) = value {
                walk_qualifiers(value, f);
            }
        }
        Assign(target, value) => {
            walk_qualifiers(target, f);
            walk_qualifiers(value, f);
        }

        BuiltinOp(_) | Num(_) | Flt(_) | Bool(_) | Var(_) | Zeroed | TypeInfo(..) | Break
        | Continue => {}
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
    match &decl.kind {
        DeclKind::Record(record) => visit_record(record, &subst, types, path)?,
        DeclKind::Sum(cases) => {
            for case in cases {
                if let Some(payload) = &case.payload {
                    visit_record(payload, &subst, types, path)?;
                }
            }
        }
    }

    path.pop();
    Ok(())
}

fn visit_record<'a>(
    record: &RecordDecl,
    subst: &std::collections::HashMap<usize, Type>,
    types: &'a TypeTable,
    path: &mut Vec<&'a str>,
) -> Result<(), Vec<&'a str>> {
    for field in &record.fields {
        visit_type(&field.ty.substitute(subst), types, path)?;
    }
    Ok(())
}

/// Walk into one field's type. A declared type is followed into its declaration;
/// an anonymous one is walked *through*, since a cycle may run through one —
/// `type A = { b: { c: A } };` has no size either.
fn visit_type<'a>(
    ty: &Type,
    types: &'a TypeTable,
    path: &mut Vec<&'a str>,
) -> Result<(), Vec<&'a str>> {
    match ty {
        Type::User(name, args) => {
            // Borrow the name from the table rather than from the substituted
            // type, which is a temporary.
            if let Some((name, _)) = types.get_key_value(name) {
                visit(name.as_str(), args, types, path)?;
            }
            Ok(())
        }
        Type::AnonRecord(record) => {
            for field_ty in record.types() {
                visit_type(field_ty, types, path)?;
            }
            Ok(())
        }
        Type::AnonSum(cases) => {
            for case in cases {
                if let Some(payload) = &case.payload {
                    visit_type(payload, types, path)?;
                }
            }
            Ok(())
        }
        // A primitive, or a type parameter that nothing substituted — which stays
        // a variable, since which type it stands for is not known here. Whether
        // *that* has a size is checked wherever it is written down.
        _ => Ok(()),
    }
}
