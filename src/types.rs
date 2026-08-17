use std::{collections::HashMap, fmt::Debug};

use crate::tokenizer::Loc;

#[allow(unused)]
#[rustfmt::skip]
#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Type {
    // Type Var
    T(usize),

    // Fn(args, ret)
    Fn(Vec<Type>, Box<Type>),

    // User(name, type_args) - a declared type, applied to its type arguments.
    // Nominal: two of these are the same type only if they name the same
    // declaration. `type_args` is empty for a non-generic type.
    //
    // Whether the declaration is a record or a sum is not recorded here: a name
    // resolves to exactly one declaration, so that question is answered by the
    // `TypeTable` (which unification and pruning both have to hand) rather than
    // by two nearly identical variants.
    User(String, Vec<Type>),

    // The open type of a record literal, and the only type that can still GROW.
    // A record literal names no type at all -- `.{ x: 0 }` says only "something
    // with an `x`" -- so unification is what decides which record it is: meeting
    // a declared or anonymous record checks these fields against it, and meeting
    // another open record takes the union of the two field sets. Never
    // `is_known`; an open record that never meets a concrete one is closed into
    // an `AnonRecord` first.
    SomeRecord(Record),

    // A `SomeRecord` that never met a concrete record type, closed over exactly
    // the fields it was known to have. Structural: two of these with the same
    // fields are the same type, and one is never equal to a `User` of the same
    // shape. Also what a record written down in a signature or an annotation is
    // -- written means concrete.
    AnonRecord(Record),

    // The open type of a case literal. A literal names only a *case*, and many
    // sums may have a case by that name, so it does not pick one: it carries the
    // set of cases it is known to have, and unification narrows it. As with
    // records, never `is_known`.
    SomeSum(Vec<SumCase>),

    // A `SomeSum` that never met a concrete sum, closed over exactly the cases
    // it had -- and what an anonymous sum written down means. Structural.
    AnonSum(Vec<SumCase>),

    Void,

    // NoReturn / bottom type: the type of a `return` expression. Satisfies any
    // other type and is absorbed by joins so a diverging branch/statement never
    // forces its neighbours to NoReturn.
    Never,

    Bool,

    // {integer}
    Integer,

    U8, U16, U32, U64,
    I8, I16, I32, I64,

    // {decimal}
    Decimal,

    F32, F64,
}

/// The fields of a record type.
///
/// The two kinds never unify with each other: a record is all named or all
/// positional, which is checked where one is built rather than here.
#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Record {
    /// Named fields, kept sorted by name — which is what makes `==` and `Hash`
    /// structural, so two records whose fields were written in a different order
    /// are one type. An empty record is always this variant.
    Named(Vec<(String, Type)>),

    /// Positional fields, where the index *is* the name (`_0`, `_1`, ...). Order
    /// is part of the type, so this is never sorted. Never empty — an empty
    /// record has no positions and is `Named(vec![])`.
    Pos(Vec<Type>),
}

/// A field's name: written, or the index of a positional one.
///
/// `_0`, `_1`, ... name positional fields, so those spellings are not available
/// as field names in a named record. The spelling has to be exact, which is why
/// `_00` is an ordinary name.
#[derive(Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum FieldName {
    Named(String),
    Pos(usize),
}

/// One case of a sum type.
///
/// A payload is always a record — `Some { T }` and `Some { val: T }` differ only
/// in whether that record's fields are positional or named. `None` means the
/// case carries nothing, which is not the same as carrying an empty record: a
/// case declared with a payload has to be given one.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct SumCase {
    pub name: String,
    pub payload: Option<Type>,
}

impl FieldName {
    /// The field name an identifier denotes.
    pub fn parse(text: &str) -> FieldName {
        if let Some(digits) = text.strip_prefix('_')
            && let Ok(idx) = digits.parse::<usize>()
            // Exact spelling only, so `_00` stays a name of its own.
            && format!("_{idx}") == text
        {
            return FieldName::Pos(idx);
        }
        FieldName::Named(text.to_string())
    }

    pub fn is_positional(&self) -> bool {
        matches!(self, FieldName::Pos(_))
    }
}

impl Record {
    /// A named record with its fields put in canonical order.
    pub fn named(mut fields: Vec<(String, Type)>) -> Record {
        fields.sort_by(|(a, _), (b, _)| a.cmp(b));
        Record::Named(fields)
    }

    /// Build a record from fields that already know their names. Returns `None`
    /// if the two kinds were mixed, or if the positions are not exactly
    /// `0..len` — either way there is no record this could be.
    pub fn build(fields: Vec<(FieldName, Type)>) -> Option<Record> {
        if fields.is_empty() {
            return Some(Record::Named(Vec::new()));
        }

        if fields.iter().all(|(n, _)| n.is_positional()) {
            let mut slots: Vec<Option<Type>> = vec![None; fields.len()];
            for (name, ty) in fields {
                let FieldName::Pos(idx) = name else {
                    unreachable!()
                };
                // Out of range, or given twice.
                if idx >= slots.len() || slots[idx].is_some() {
                    return None;
                }
                slots[idx] = Some(ty);
            }
            return Some(Record::Pos(slots.into_iter().map(|t| t.unwrap()).collect()));
        }

        if fields.iter().any(|(n, _)| n.is_positional()) {
            return None; // mixed
        }

        Some(Record::named(
            fields
                .into_iter()
                .map(|(n, t)| match n {
                    FieldName::Named(n) => (n, t),
                    FieldName::Pos(_) => unreachable!(),
                })
                .collect(),
        ))
    }

    pub fn len(&self) -> usize {
        match self {
            Record::Named(fields) => fields.len(),
            Record::Pos(tys) => tys.len(),
        }
    }

    pub fn is_positional(&self) -> bool {
        matches!(self, Record::Pos(_))
    }

    /// Whether both records name their fields the same way. The empty record is
    /// `Named`, and counts as agreeing with either.
    pub fn same_kind(&self, other: &Record) -> bool {
        self.len() == 0 || other.len() == 0 || self.is_positional() == other.is_positional()
    }

    pub fn get(&self, name: &FieldName) -> Option<&Type> {
        match (self, name) {
            (Record::Named(fields), FieldName::Named(n)) => {
                fields.iter().find(|(f, _)| f == n).map(|(_, t)| t)
            }
            (Record::Pos(tys), FieldName::Pos(i)) => tys.get(*i),
            _ => None,
        }
    }

    pub fn names(&self) -> Vec<FieldName> {
        match self {
            Record::Named(fields) => fields
                .iter()
                .map(|(n, _)| FieldName::Named(n.clone()))
                .collect(),
            Record::Pos(tys) => (0..tys.len()).map(FieldName::Pos).collect(),
        }
    }

    /// The fields in canonical order, paired with their names.
    pub fn iter(&self) -> Box<dyn Iterator<Item = (FieldName, &Type)> + '_> {
        match self {
            Record::Named(fields) => Box::new(
                fields
                    .iter()
                    .map(|(n, t)| (FieldName::Named(n.clone()), t)),
            ),
            Record::Pos(tys) => Box::new(tys.iter().enumerate().map(|(i, t)| (FieldName::Pos(i), t))),
        }
    }

    pub fn types(&self) -> impl Iterator<Item = &Type> {
        match self {
            Record::Named(fields) => Box::new(fields.iter().map(|(_, t)| t)) as Box<dyn Iterator<Item = &Type>>,
            Record::Pos(tys) => Box::new(tys.iter()),
        }
    }

    pub fn map_types(&self, f: &mut impl FnMut(&Type) -> Type) -> Record {
        match self {
            Record::Named(fields) => {
                Record::Named(fields.iter().map(|(n, t)| (n.clone(), f(t))).collect())
            }
            Record::Pos(tys) => Record::Pos(tys.iter().map(|t| f(t)).collect()),
        }
    }
}

impl SumCase {
    pub fn new(name: String, payload: Option<Type>) -> Self {
        Self { name, payload }
    }

    pub fn map_types(&self, f: &mut impl FnMut(&Type) -> Type) -> SumCase {
        SumCase {
            name: self.name.clone(),
            payload: self.payload.as_ref().map(|t| f(t)),
        }
    }
}

/// `cases` in canonical order, so a set built in any order compares equal.
pub fn sorted_cases(mut cases: Vec<SumCase>) -> Vec<SumCase> {
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

// --------------------------------------------------------------------------
// Declared types
// --------------------------------------------------------------------------

/// Every type the program declares, by name.
pub type TypeTable = HashMap<String, TypeDecl>;

/// A `type` declaration: either a record (`type Vec2 = { x: i32 };`) or a sum
/// (`type Option<T> = Some { T } | None;`).
#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: String,

    /// The name each type parameter was written with, and the type variable id
    /// standing for it inside the field types. Empty for an ordinary type. Like
    /// a generic function, a generic type is never used as written:
    /// [`TypeDecl::record_at`] / [`TypeDecl::case_at`] substitute the arguments
    /// in first.
    pub type_params: Vec<(String, usize)>,

    pub kind: DeclKind,

    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub enum DeclKind {
    Record(RecordDecl),
    /// The cases in declaration order — which is also the order their tags will
    /// be assigned in, so it is not sorted.
    Sum(Vec<CaseDecl>),
}

/// A record as it was written: fields in declaration order, which is the order
/// they will be laid out in, each with the span to report it at.
#[derive(Debug, Clone)]
pub struct RecordDecl {
    pub fields: Vec<FieldDecl>,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct CaseDecl {
    pub name: String,
    /// Absent for a case that carries nothing (`None`), which is not the same as
    /// a case carrying an empty record.
    pub payload: Option<RecordDecl>,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: FieldName,
    pub ty: Type,
    pub loc: Loc,
}

impl RecordDecl {
    /// This record with `subst` applied to every field type — the shape one
    /// value of it actually has.
    pub fn at(&self, subst: &HashMap<usize, Type>) -> Record {
        Record::build(
            self.fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.substitute(subst)))
                .collect(),
        )
        // Mixed or gappy fields are rejected where the declaration is parsed, so
        // a declaration that got this far always describes a record.
        .expect("a declaration's own fields")
    }
}

impl TypeDecl {
    pub fn is_record(&self) -> bool {
        matches!(self.kind, DeclKind::Record(_))
    }

    /// The declaration's cases, or an empty slice for a record.
    pub fn cases(&self) -> &[CaseDecl] {
        match &self.kind {
            DeclKind::Sum(cases) => cases,
            DeclKind::Record(_) => &[],
        }
    }

    pub fn case(&self, name: &str) -> Option<&CaseDecl> {
        self.cases().iter().find(|c| c.name == name)
    }

    /// This declaration's fields as a [`Record`], with its type parameters
    /// replaced by `args`. `None` if it is a sum, which has no fields of its own.
    pub fn record_at(&self, args: &[Type]) -> Option<Record> {
        match &self.kind {
            DeclKind::Record(record) => Some(record.at(&self.subst(args))),
            DeclKind::Sum(_) => None,
        }
    }

    /// The named case with this declaration's type parameters replaced by `args`
    /// — the shape one value of `Name<args>` actually has. `None` if there is no
    /// such case, or if this is a record.
    pub fn case_at(&self, name: &str, args: &[Type]) -> Option<SumCase> {
        let case = self.case(name)?;
        let subst = self.subst(args);
        Some(SumCase {
            name: case.name.clone(),
            payload: case
                .payload
                .as_ref()
                .map(|record| Type::AnonRecord(record.at(&subst))),
        })
    }

    pub fn subst(&self, args: &[Type]) -> HashMap<usize, Type> {
        self.type_params
            .iter()
            .map(|(_, id)| *id)
            .zip(args.iter().cloned())
            .collect()
    }
}

// --------------------------------------------------------------------------

impl Type {
    pub fn is_known(&self) -> bool {
        use Type::*;
        match self {
            T(_) => false,
            Fn(args, ret) => args.iter().all(|a| a.is_known()) && ret.is_known(),
            User(_, args) => args.iter().all(|a| a.is_known()),
            // Still open: it may yet gain fields or cases, or turn out to be a
            // declared type. Closing it is what makes it concrete.
            SomeRecord(_) | SomeSum(_) => false,
            AnonRecord(record) => record.types().all(|t| t.is_known()),
            AnonSum(cases) => cases
                .iter()
                .all(|c| c.payload.as_ref().is_none_or(|t| t.is_known())),
            Integer => false,
            Decimal => false,
            Void | Never | Bool | U8 | U16 | U32 | U64 | I8 | I16 | I32 | I64 | F32 | F64 => true,
        }
    }

    /// Replace every type variable listed in `subst` with the type it maps to.
    /// Used to turn a generic function's signature and body into one
    /// instantiation of it; variables not in `subst` are left alone.
    pub fn substitute(&self, subst: &HashMap<usize, Type>) -> Type {
        use Type::*;
        match self {
            T(id) => match subst.get(id) {
                Some(ty) => ty.clone(),
                None => self.clone(),
            },
            Fn(args, ret) => Fn(
                args.iter().map(|a| a.substitute(subst)).collect(),
                Box::new(ret.substitute(subst)),
            ),
            User(name, args) => User(
                name.clone(),
                args.iter().map(|a| a.substitute(subst)).collect(),
            ),
            SomeRecord(record) => SomeRecord(record.map_types(&mut |t| t.substitute(subst))),
            AnonRecord(record) => AnonRecord(record.map_types(&mut |t| t.substitute(subst))),
            SomeSum(cases) => SomeSum(
                cases
                    .iter()
                    .map(|c| c.map_types(&mut |t| t.substitute(subst)))
                    .collect(),
            ),
            AnonSum(cases) => AnonSum(
                cases
                    .iter()
                    .map(|c| c.map_types(&mut |t| t.substitute(subst)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Whether these two could still be made equal. Used to prune overloads, so
    /// it must be monotone: as bindings accumulate an answer may go true ->
    /// false, never the other way, or a pruned candidate could have been needed.
    ///
    /// An open record only ever gains fields and an open sum only ever gains
    /// cases, so every arm below that asks "are these a subset of those" is
    /// monotone for the same reason.
    pub fn satisfies_type(&self, other: &Type, types: &TypeTable) -> bool {
        use Type::*;
        match (self, other) {
            // Either side being an unbound variable means the two *could* still
            // be made equal, which is all this asks. The second arm matters for
            // generics: a candidate's parameter is a fresh variable until the
            // call site pins it, and without this every generic overload would
            // be pruned away on sight.
            (T(_), _) | (_, T(_)) => true,
            // A NoReturn value satisfies any expected type (bottom type).
            (Never, _) => true,
            (Fn(args_1, ret_1), Fn(args_2, ret_2)) => {
                let mut satisfies = true;
                for (arg_1, arg_2) in args_1.iter().zip(args_2) {
                    satisfies &= arg_1.satisfies_type(arg_2, types);
                }
                satisfies & ret_1.satisfies_type(ret_2, types)
            }

            // Nominal: the same declaration, and every type argument compatible.
            (User(n1, a1), User(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1.iter().zip(a2).all(|(x, y)| x.satisfies_type(y, types))
            }

            // An open record could still become any record that has all of its
            // fields — the ones it is missing get zero initialized, which is
            // what unification will go on to arrange.
            (SomeRecord(open), User(name, args)) | (User(name, args), SomeRecord(open)) => {
                match types.get(name) {
                    // An undeclared name is reported by the declaration check,
                    // not by silently pruning every overload here.
                    None => true,
                    // A sum is not a record, however its fields look.
                    Some(decl) => match decl.record_at(args) {
                        None => false,
                        Some(declared) => record_fits(open, &declared, types),
                    },
                }
            }
            (SomeRecord(open), AnonRecord(closed)) | (AnonRecord(closed), SomeRecord(open)) => {
                record_fits(open, closed, types)
            }
            // Neither is the whole set, so only the fields both know about have
            // to agree.
            (SomeRecord(a), SomeRecord(b)) => records_agree(a, b, types),
            // Closed on both sides: the field sets have to line up exactly.
            (AnonRecord(a), AnonRecord(b)) => records_same(a, b, types),

            // The same three shapes again, for sums.
            (SomeSum(open), User(name, args)) | (User(name, args), SomeSum(open)) => {
                match types.get(name) {
                    None => true,
                    Some(decl) if decl.is_record() => false,
                    Some(decl) => open.iter().all(|c| match decl.case_at(&c.name, args) {
                        None => false,
                        Some(d) => cases_match(c, &d, types),
                    }),
                }
            }
            (SomeSum(open), AnonSum(closed)) | (AnonSum(closed), SomeSum(open)) => {
                cases_fit(open, closed, types)
            }
            (SomeSum(a), SomeSum(b)) => cases_agree(a, b, types),
            (AnonSum(a), AnonSum(b)) => cases_same(a, b, types),

            (Integer, U8)
            | (Integer, U16)
            | (Integer, U32)
            | (Integer, U64)
            | (Integer, I8)
            | (Integer, I16)
            | (Integer, I32)
            | (Integer, I64)
            | (U8, Integer)
            | (U16, Integer)
            | (U32, Integer)
            | (U64, Integer)
            | (I8, Integer)
            | (I16, Integer)
            | (I32, Integer)
            | (I64, Integer) => true,

            (Decimal, F32) | (F32, Decimal) | (Decimal, F64) | (F64, Decimal) => true,

            (a, b) if a == b => true,
            _ => false,
        }
    }
}

/// Whether an open record could be this concrete one: same kind of field names,
/// no field it does not have, and every shared field still able to unify. Fields
/// the open side is *missing* say nothing — those get zero initialized.
fn record_fits(open: &Record, closed: &Record, types: &TypeTable) -> bool {
    if !open.same_kind(closed) || open.len() > closed.len() {
        return false;
    }
    open.iter().all(|(name, ty)| match closed.get(&name) {
        None => false,
        Some(other) => ty.satisfies_type(other, types),
    })
}

/// Whether two open records could be merged: neither is the whole set, so only
/// the fields both sides know about have to agree.
fn records_agree(a: &Record, b: &Record, types: &TypeTable) -> bool {
    if !a.same_kind(b) {
        return false;
    }
    a.iter().all(|(name, ty)| match b.get(&name) {
        None => true,
        Some(other) => ty.satisfies_type(other, types),
    })
}

/// Whether two closed records are the same record.
fn records_same(a: &Record, b: &Record, types: &TypeTable) -> bool {
    a.names() == b.names()
        && a.iter()
            .zip(b.iter())
            .all(|((_, x), (_, y))| x.satisfies_type(y, types))
}

fn cases_fit(open: &[SumCase], closed: &[SumCase], types: &TypeTable) -> bool {
    open.iter().all(|c| match closed.iter().find(|d| d.name == c.name) {
        None => false,
        Some(d) => cases_match(c, d, types),
    })
}

fn cases_agree(a: &[SumCase], b: &[SumCase], types: &TypeTable) -> bool {
    a.iter().all(|c| match b.iter().find(|d| d.name == c.name) {
        None => true,
        Some(d) => cases_match(c, d, types),
    })
}

fn cases_same(a: &[SumCase], b: &[SumCase], types: &TypeTable) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.name == y.name && cases_match(x, y, types))
}

/// Whether two cases of the same name could be the same case. A case either
/// carries a payload or does not, so those must agree before the payloads
/// themselves are compared.
fn cases_match(a: &SumCase, b: &SumCase, types: &TypeTable) -> bool {
    match (&a.payload, &b.payload) {
        (None, None) => true,
        (Some(x), Some(y)) => x.satisfies_type(y, types),
        _ => false,
    }
}

impl Debug for FieldName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldName::Named(name) => write!(f, "{name}"),
            FieldName::Pos(idx) => write!(f, "_{idx}"),
        }
    }
}

impl Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list = match self {
            Record::Named(fields) => fields
                .iter()
                .map(|(n, t)| format!("{n}: {t:?}"))
                .collect::<Vec<_>>(),
            Record::Pos(tys) => tys.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
        };
        write!(f, "{{{}}}", list.join(", "))
    }
}

impl Debug for SumCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.payload {
            None => write!(f, "{}", self.name),
            Some(payload) => write!(f, "{} {payload:?}", self.name),
        }
    }
}

impl Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Type::*;
        match self {
            T(n) => write!(f, "'t{n}"),
            Void => write!(f, "void"),
            Never => write!(f, "noreturn"),
            Bool => write!(f, "bool"),
            U8 => write!(f, "u8"),
            U16 => write!(f, "u16"),
            U32 => write!(f, "u32"),
            U64 => write!(f, "u64"),
            I8 => write!(f, "i8"),
            I16 => write!(f, "i16"),
            I32 => write!(f, "i32"),
            I64 => write!(f, "i64"),
            F32 => write!(f, "f32"),
            F64 => write!(f, "f64"),
            Integer => write!(f, "{{integer}}"),
            Decimal => write!(f, "{{decimal}}"),
            Fn(args, ret) => {
                let arg_list = args
                    .iter()
                    .map(|a| format!("{a:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "({arg_list}) -> {ret:?}")
            }
            User(name, args) if args.is_empty() => write!(f, "{name}"),
            User(name, args) => {
                let arg_list = args
                    .iter()
                    .map(|a| format!("{a:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{name}<{arg_list}>")
            }
            // The `?` marks a set as still open, so an error naming one is not
            // mistaken for the closed type it would have become.
            SomeRecord(record) => write!(f, "?{record:?}"),
            AnonRecord(record) => write!(f, "{record:?}"),
            SomeSum(cases) => write!(f, "?{}", fmt_cases(cases)),
            AnonSum(cases) => write!(f, "{}", fmt_cases(cases)),
        }
    }
}

fn fmt_cases(cases: &[SumCase]) -> String {
    let list = cases
        .iter()
        .map(|c| format!("{c:?}"))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("{{{list}}}")
}
