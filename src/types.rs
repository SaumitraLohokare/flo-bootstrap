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
    User(String, Vec<Type>),

    // The open type of a type literal. A literal names only a *case*, and many
    // types may have a case by that name, so the literal does not pick one: it
    // yields the set of cases it is known to have, and unification narrows it.
    // Meeting a `User` checks those cases against that declaration and becomes
    // it; meeting another `SomeType` merges the two sets. Never `is_known` — an
    // open type that never meets a declared one is closed into an `Anon` first.
    SomeType(Vec<TypeCase>),

    // A `SomeType` that never met a declared type, closed over exactly the cases
    // it was known to have. Structural: two `Anon`s with the same cases are the
    // same type, and an `Anon` is never equal to a `User` of the same shape.
    Anon(Vec<TypeCase>),

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

/// One case of a structural type, and the fields it is known to carry.
///
/// The fields are kept sorted by name, and a set of cases sorted by case name,
/// which is what makes `==` and `Hash` structural: two of these built from
/// literals that listed their fields in a different order are the same case.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct TypeCase {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

impl TypeCase {
    /// A case with its fields put in canonical order.
    pub fn new(name: String, mut fields: Vec<(String, Type)>) -> Self {
        fields.sort_by(|(a, _), (b, _)| a.cmp(b));
        Self { name, fields }
    }

    pub fn field(&self, name: &str) -> Option<&Type> {
        self.fields.iter().find(|(f, _)| f == name).map(|(_, t)| t)
    }

    pub fn field_names(&self) -> Vec<&str> {
        self.fields.iter().map(|(f, _)| f.as_str()).collect()
    }

    pub fn map_types(&self, f: &mut impl FnMut(&Type) -> Type) -> TypeCase {
        TypeCase {
            name: self.name.clone(),
            fields: self.fields.iter().map(|(n, t)| (n.clone(), f(t))).collect(),
        }
    }
}

/// `cases` in canonical order, so a set built in any order compares equal.
pub fn sorted_cases(mut cases: Vec<TypeCase>) -> Vec<TypeCase> {
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

// --------------------------------------------------------------------------
// Declared types
// --------------------------------------------------------------------------

/// Every type the program declares, by name.
pub type TypeTable = HashMap<String, TypeDecl>;

/// A `type` declaration: `type Name<T> = Case { field: T } | Other;`
#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: String,

    /// The name each type parameter was written with, and the type variable id
    /// standing for it inside the cases' field types. Empty for an ordinary
    /// type. Like a generic function, a generic type is never used as written:
    /// [`TypeDecl::case_at`] substitutes the arguments in first.
    pub type_params: Vec<(String, usize)>,

    /// The cases in declaration order — which is also the order their tags will
    /// be assigned in, so it is not sorted.
    pub cases: Vec<CaseDecl>,

    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct CaseDecl {
    pub name: String,
    /// Fields in declaration order, which is the order they will be laid out in.
    pub fields: Vec<FieldDecl>,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Type,
    pub loc: Loc,
}

impl TypeDecl {
    pub fn case(&self, name: &str) -> Option<&CaseDecl> {
        self.cases.iter().find(|c| c.name == name)
    }

    /// The named case as a [`TypeCase`], with this declaration's type parameters
    /// replaced by `args` — the shape one value of `Name<args>` actually has.
    pub fn case_at(&self, name: &str, args: &[Type]) -> Option<TypeCase> {
        let case = self.case(name)?;
        let subst = self.subst(args);
        Some(TypeCase::new(
            case.name.clone(),
            case.fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.substitute(&subst)))
                .collect(),
        ))
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
            // Still open: it may yet gain cases, or turn out to be a declared
            // type. Closing it into an `Anon` is what makes it concrete.
            SomeType(_) => false,
            Anon(cases) => cases
                .iter()
                .all(|c| c.fields.iter().all(|(_, t)| t.is_known())),
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
            SomeType(cases) => SomeType(
                cases
                    .iter()
                    .map(|c| c.map_types(&mut |t| t.substitute(subst)))
                    .collect(),
            ),
            Anon(cases) => Anon(
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

            // An open type could still become any declared type that has all of
            // its cases, with the same fields — which is exactly what
            // unification will go on to check.
            (SomeType(cases), User(name, args)) | (User(name, args), SomeType(cases)) => {
                match types.get(name) {
                    // An undeclared name is reported by the declaration check,
                    // not by silently pruning every overload here.
                    None => true,
                    Some(decl) => cases.iter().all(|c| match decl.case_at(&c.name, args) {
                        None => false,
                        Some(d) => cases_match(c, &d, types),
                    }),
                }
            }
            (SomeType(c1), SomeType(c2)) | (SomeType(c1), Anon(c2)) | (Anon(c1), SomeType(c2)) => {
                // Cases only one side knows about say nothing: an open set is
                // allowed to be missing cases the other side has.
                c1.iter().all(|a| match c2.iter().find(|b| b.name == a.name) {
                    None => true,
                    Some(b) => cases_match(a, b, types),
                })
            }
            // Closed on both sides, so the case sets have to line up exactly.
            (Anon(c1), Anon(c2)) => {
                c1.len() == c2.len()
                    && c1
                        .iter()
                        .zip(c2)
                        .all(|(a, b)| a.name == b.name && cases_match(a, b, types))
            }

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

/// Whether two cases of the same name could be the same case: a literal has to
/// give a case's fields exactly, so the field names must match, and each field's
/// type must still be able to unify.
fn cases_match(a: &TypeCase, b: &TypeCase, types: &TypeTable) -> bool {
    a.field_names() == b.field_names()
        && a.fields
            .iter()
            .zip(&b.fields)
            .all(|((_, x), (_, y))| x.satisfies_type(y, types))
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
            // The `?` marks the set as still open, so an error naming one is not
            // mistaken for the closed `Anon` it would have become.
            SomeType(cases) => write!(f, "?{}", fmt_cases(cases)),
            Anon(cases) => write!(f, "{}", fmt_cases(cases)),
        }
    }
}

fn fmt_cases(cases: &[TypeCase]) -> String {
    let list = cases
        .iter()
        .map(|c| {
            if c.fields.is_empty() {
                c.name.clone()
            } else {
                let fields = c
                    .fields
                    .iter()
                    .map(|(n, t)| format!("{n}: {t:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {fields} }}", c.name)
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    format!("{{{list}}}")
}
