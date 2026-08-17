use std::{collections::HashMap, rc::Rc};

use crate::{
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{FieldName, Record, SumCase, Type, TypeTable, sorted_cases},
};

#[derive(Debug)]
pub(super) struct ReplaceSet {
    parents: HashMap<usize, usize>,
    bindings: HashMap<usize, Type>,
    /// Needed to unify an open record or sum with a declared type: the `Type`
    /// only carries the declaration's name, and its fields or cases have to be
    /// looked up to be checked against. It is also what says whether a declared
    /// name is a record or a sum, which nothing else can answer.
    types: Rc<TypeTable>,
}

impl ReplaceSet {
    pub(super) fn new(types: Rc<TypeTable>) -> Self {
        Self {
            parents: HashMap::new(),
            bindings: HashMap::new(),
            types,
        }
    }

    // If doesn't exist, add as root pointing to self
    pub(super) fn add(&mut self, ty_id: usize) {
        self.parents.entry(ty_id).or_insert(ty_id);
    }

    pub(super) fn find(&mut self, ty_id: usize) -> usize {
        self.add(ty_id);

        let mut root = ty_id;
        while self.parents[&root] != root {
            root = self.parents[&root];
        }

        // path compression
        let mut cur = ty_id;
        while self.parents[&cur] != root {
            let next = self.parents[&cur];
            self.parents.insert(cur, root);
            cur = next;
        }

        root
    }

    pub(super) fn unify(&mut self, t1: usize, t2: usize, loc: Loc) -> FloResult<()> {
        let r1 = self.find(t1);
        let r2 = self.find(t2);
        if r1 == r2 {
            return Ok(());
        }

        let t1 = self.bindings.remove(&r1);
        let t2 = self.bindings.remove(&r2);

        self.parents.insert(r2, r1);

        // Unify types if known
        let merged = match (t1, t2) {
            (Some(a), Some(b)) => Some(self.unify_types(a, b, loc)?),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };

        if let Some(t) = merged {
            self.bindings.insert(r1, t);
        }

        Ok(())
    }

    /// Unify two types that are not (or not only) variables, for callers outside
    /// the union-find itself. The merged type is returned, and any bindings the
    /// merge implied are recorded.
    pub(super) fn merge(&mut self, t1: Type, t2: Type, loc: Loc) -> FloResult<Type> {
        self.unify_types(t1, t2, loc)
    }

    fn unify_types(&mut self, t1: Type, t2: Type, loc: Loc) -> FloResult<Type> {
        use Type::*;
        let result = match (t1, t2) {
            // NoReturn is absorbed by a join: it never overrides a real type and
            // never binds a variable to NoReturn. Listed first so it wins even
            // against the type-var arms below.
            (Never, other) | (other, Never) => other,

            (T(a), T(b)) => {
                self.unify(a, b, loc)?;
                self.resolve(&T(a))
            }
            // Which side the variable is on is what says whether the incoming type
            // is the expected one — see `bind_as`.
            (T(a), t2) => {
                self.bind_as(a, t2, false, loc)?;
                self.resolve(&T(a))
            }
            (t1, T(b)) => {
                self.bind_as(b, t1, true, loc)?;
                self.resolve(&T(b))
            }

            (Integer, I8) | (I8, Integer) => I8,
            (Integer, I16) | (I16, Integer) => I16,
            (Integer, I32) | (I32, Integer) => I32,
            (Integer, I64) | (I64, Integer) => I64,
            (Integer, U8) | (U8, Integer) => U8,
            (Integer, U16) | (U16, Integer) => U16,
            (Integer, U32) | (U32, Integer) => U32,
            (Integer, U64) | (U64, Integer) => U64,

            (Decimal, F32) | (F32, Decimal) => F32,
            (Decimal, F64) | (F64, Decimal) => F64,

            (Fn(a1, r1), Fn(a2, r2)) => {
                if a1.len() != a2.len() {
                    return Err(FloErr::CallArityMismatch {
                        expected: a1.len(),
                        got: a2.len(),
                        loc,
                    });
                }
                let mut args = Vec::with_capacity(a1.len());
                for (x, y) in a1.into_iter().zip(a2) {
                    args.push(self.unify_types(x, y, loc)?);
                }
                let ret = Box::new(self.unify_types(*r1, *r2, loc)?);
                Fn(args, ret)
            }

            // Nominal, so the names have to match outright; only the type
            // arguments are unified.
            (User(n1, a1), User(n2, a2)) if n1 == n2 && a1.len() == a2.len() => {
                let mut args = Vec::with_capacity(a1.len());
                for (x, y) in a1.into_iter().zip(a2) {
                    args.push(self.unify_types(x, y, loc)?);
                }
                User(n1, args)
            }

            // ------------------------------------------------------------------
            // Records
            // ------------------------------------------------------------------

            // An open record meeting the declared record it belongs to. This is
            // where a literal is finally pinned down. The declaration's own type
            // is the result — it is nominal, so the merged fields are thrown
            // away; what they were for is the *bindings* unifying them made.
            (SomeRecord(open), User(name, args)) | (User(name, args), SomeRecord(open)) => {
                let whole = User(name.clone(), args.clone());
                let types = Rc::clone(&self.types);
                match types.get(&name) {
                    // Reported by the declaration check; nothing useful to add.
                    None => {}
                    Some(decl) => match decl.record_at(&args) {
                        // A sum is not a record, however its fields look.
                        None => {
                            return Err(FloErr::RecordSumMismatch {
                                record: SomeRecord(open),
                                sum: whole,
                                loc,
                            });
                        }
                        Some(declared) => {
                            self.fit_record(open, declared, &whole, loc)?;
                        }
                    },
                }
                whole
            }

            // An open record meeting a concrete anonymous one.
            (SomeRecord(open), AnonRecord(closed)) | (AnonRecord(closed), SomeRecord(open)) => {
                let whole = AnonRecord(closed.clone());
                AnonRecord(self.fit_record(open, closed, &whole, loc)?)
            }

            // Two open records: neither knows the whole set, so take the union.
            // This is the one place a record type *grows* — and why a field given
            // in one literal and left out of another ends up in both.
            (SomeRecord(a), SomeRecord(b)) => SomeRecord(self.merge_records(a, b, loc)?),

            // Structural, and closed on both sides, so the field sets have to
            // line up exactly.
            (AnonRecord(a), AnonRecord(b)) => AnonRecord(self.unify_same_record(a, b, loc)?),

            // ------------------------------------------------------------------
            // Sums
            // ------------------------------------------------------------------

            (SomeSum(open), User(name, args)) | (User(name, args), SomeSum(open)) => {
                let whole = User(name.clone(), args.clone());
                let types = Rc::clone(&self.types);
                match types.get(&name) {
                    None => {}
                    Some(decl) if decl.is_record() => {
                        return Err(FloErr::RecordSumMismatch {
                            record: whole,
                            sum: SomeSum(open),
                            loc,
                        });
                    }
                    Some(decl) => {
                        for case in open {
                            let Some(declared) = decl.case_at(&case.name, &args) else {
                                return Err(FloErr::NoSuchCase {
                                    ty: whole,
                                    case: case.name,
                                    loc,
                                });
                            };
                            self.unify_case(case, declared, loc)?;
                        }
                    }
                }
                whole
            }

            (SomeSum(open), AnonSum(closed)) | (AnonSum(closed), SomeSum(open)) => {
                AnonSum(self.fit_cases(open, closed, loc)?)
            }

            (SomeSum(c1), SomeSum(c2)) => SomeSum(self.merge_cases(c1, c2, loc)?),

            (AnonSum(c1), AnonSum(c2)) => AnonSum(self.unify_same_cases(c1, c2, loc)?),

            // A record meeting a sum, in whichever forms the two are in. Worth its
            // own error because it is the easiest mistake to make: `.{ .. }` builds
            // a record and `.Case` builds a sum, and only the type says which one
            // belongs here.
            (record @ (SomeRecord(_) | AnonRecord(_)), sum @ (SomeSum(_) | AnonSum(_)))
            | (sum @ (SomeSum(_) | AnonSum(_)), record @ (SomeRecord(_) | AnonRecord(_))) => {
                return Err(FloErr::RecordSumMismatch { record, sum, loc });
            }

            (a, b) if a == b => a,
            (a, b) => {
                // FIXME: This would print bad errors:
                // Eg: Fn([i32, bool], void) & Fn([i32, i8], void)
                // would give error: bool != i8
                return Err(FloErr::TypeMismatch {
                    expected: a,
                    got: b,
                    loc,
                });
            }
        };

        Ok(result)
    }

    /// An open record meeting a concrete one.
    ///
    /// The concrete side is the whole set, so the open side may only have fields
    /// it has. Fields the open side is *missing* are not an error: a record
    /// literal may leave a field out, and it gets zero initialized. Nothing about
    /// that is recorded here — which fields a literal left out is a property of
    /// that literal, not of the type several of them may share, so it is worked
    /// out per literal in [`crate::ast::Expr::resolve`].
    fn fit_record(
        &mut self,
        open: Record,
        closed: Record,
        whole: &Type,
        loc: Loc,
    ) -> FloResult<Record> {
        if !open.same_kind(&closed) {
            return Err(wrong_fields(&closed, &open, loc));
        }

        let given = owned_fields(&open);

        for (name, _) in &given {
            if closed.get(name).is_none() {
                return Err(FloErr::UnexpectedField {
                    ty: whole.clone(),
                    field: format!("{name:?}"),
                    loc,
                });
            }
        }

        let mut fields = Vec::with_capacity(closed.len());
        for (name, declared) in owned_fields(&closed) {
            let ty = match given.iter().find(|(n, _)| *n == name) {
                // The declared type first: it is the one that was *expected*, and
                // a mismatch is reported in that order.
                Some((_, provided)) => self.unify_types(declared, provided.clone(), loc)?,
                None => declared,
            };
            fields.push((name, ty));
        }

        Ok(Record::build(fields).expect("a shape taken from a record"))
    }

    /// The union of two open records. A field both sides know about has to be the
    /// same field on both; one only one side knows about joins the set.
    fn merge_records(&mut self, a: Record, b: Record, loc: Loc) -> FloResult<Record> {
        if !a.same_kind(&b) {
            return Err(wrong_fields(&a, &b, loc));
        }

        let a_fields = owned_fields(&a);
        let b_fields = owned_fields(&b);

        let mut merged: Vec<(FieldName, Type)> = Vec::with_capacity(a_fields.len() + b_fields.len());
        for (name, ty) in a_fields {
            let ty = match b_fields.iter().find(|(n, _)| *n == name) {
                Some((_, other)) => self.unify_types(ty, other.clone(), loc)?,
                None => ty,
            };
            merged.push((name, ty));
        }
        for (name, ty) in b_fields {
            if !merged.iter().any(|(n, _)| *n == name) {
                merged.push((name, ty));
            }
        }

        Ok(Record::build(merged).expect("a union of two records"))
    }

    /// Two concrete records. Both are closed over exactly their fields, so
    /// neither can give way and the sets have to be identical.
    fn unify_same_record(&mut self, a: Record, b: Record, loc: Loc) -> FloResult<Record> {
        if a.names() != b.names() {
            return Err(wrong_fields(&a, &b, loc));
        }

        let mut fields = Vec::with_capacity(a.len());
        for ((name, x), (_, y)) in owned_fields(&a).into_iter().zip(owned_fields(&b)) {
            fields.push((name, self.unify_types(x, y, loc)?));
        }

        Ok(Record::build(fields).expect("a shape taken from a record"))
    }

    /// An open sum meeting a concrete one: every case it is known to have has to
    /// be one of the concrete set's. Cases it does *not* have say nothing — a sum
    /// value is one case, and the open side simply has not seen the others.
    fn fit_cases(
        &mut self,
        open: Vec<SumCase>,
        closed: Vec<SumCase>,
        loc: Loc,
    ) -> FloResult<Vec<SumCase>> {
        let mut out = closed.clone();

        for case in open {
            let Some(at) = closed.iter().position(|c| c.name == case.name) else {
                return Err(FloErr::NoSuchCase {
                    ty: Type::AnonSum(closed.clone()),
                    case: case.name,
                    loc,
                });
            };
            out[at] = self.unify_case(case, closed[at].clone(), loc)?;
        }

        Ok(out)
    }

    /// The union of two sets of cases. A case both sides know about has to be
    /// the same case on both.
    fn merge_cases(
        &mut self,
        c1: Vec<SumCase>,
        c2: Vec<SumCase>,
        loc: Loc,
    ) -> FloResult<Vec<SumCase>> {
        let mut merged: Vec<SumCase> = Vec::with_capacity(c1.len() + c2.len());

        for case in c1 {
            match c2.iter().find(|c| c.name == case.name) {
                Some(other) => merged.push(self.unify_case(case, other.clone(), loc)?),
                None => merged.push(case),
            }
        }
        for case in c2 {
            if !merged.iter().any(|c| c.name == case.name) {
                merged.push(case);
            }
        }

        Ok(sorted_cases(merged))
    }

    /// Two concrete sums, which are the same type exactly when they have the same
    /// cases. Both sides are canonically ordered.
    fn unify_same_cases(
        &mut self,
        c1: Vec<SumCase>,
        c2: Vec<SumCase>,
        loc: Loc,
    ) -> FloResult<Vec<SumCase>> {
        let same_shape = c1.len() == c2.len() && c1.iter().zip(&c2).all(|(a, b)| a.name == b.name);
        if !same_shape {
            return Err(FloErr::TypeMismatch {
                expected: Type::AnonSum(c1),
                got: Type::AnonSum(c2),
                loc,
            });
        }

        let mut cases = Vec::with_capacity(c1.len());
        for (a, b) in c1.into_iter().zip(c2) {
            cases.push(self.unify_case(a, b, loc)?);
        }
        Ok(cases)
    }

    /// Unify two cases of the same name. A case either carries a payload or does
    /// not, and a literal has to say which — giving one to a case that carries
    /// nothing is as wrong as leaving one off a case that does.
    fn unify_case(&mut self, a: SumCase, b: SumCase, loc: Loc) -> FloResult<SumCase> {
        let payload = match (a.payload, b.payload) {
            (None, None) => None,
            (Some(x), Some(y)) => Some(self.unify_types(x, y, loc)?),
            (Some(_), None) => {
                return Err(FloErr::PayloadMismatch {
                    case: a.name,
                    expected: false,
                    loc,
                });
            }
            (None, Some(_)) => {
                return Err(FloErr::PayloadMismatch {
                    case: a.name,
                    expected: true,
                    loc,
                });
            }
        };

        Ok(SumCase {
            name: a.name,
            payload,
        })
    }

    pub(super) fn bind(&mut self, ty_id: usize, ty: Type, loc: Loc) -> FloResult<()> {
        self.bind_as(ty_id, ty, false, loc)
    }

    /// Bind a variable to a type.
    ///
    /// `incoming_is_expected` says which side of the original constraint `ty` came
    /// from. Every constraint is collected as (expected, got), and this is the one
    /// place that order would otherwise be lost: the variable may be on either
    /// side, so whether the type it already stood for is the expected one or the
    /// gotten one depends on which. It affects nothing but the wording of a
    /// mismatch — and getting that backwards is worse than saying nothing.
    pub(super) fn bind_as(
        &mut self,
        ty_id: usize,
        ty: Type,
        incoming_is_expected: bool,
        loc: Loc,
    ) -> FloResult<()> {
        let root = self.find(ty_id);

        if self.occurs(root, &ty) {
            return Err(FloErr::InfiniteType { loc });
        }

        let existing = self.bindings.remove(&root);
        let merged = match existing {
            Some(known) if incoming_is_expected => self.unify_types(ty, known, loc)?,
            Some(known) => self.unify_types(known, ty, loc)?,
            None => ty,
        };
        self.bindings.insert(root, merged);

        Ok(())
    }

    pub(super) fn resolve(&mut self, ty: &Type) -> Type {
        use Type::*;

        match ty {
            T(id) => {
                let root_id = self.find(*id);
                if let Some(ty) = self.bindings.get(&root_id).cloned() {
                    self.resolve(&ty)
                } else {
                    T(*id) // If binding doesn't exist, just return the type itself
                }
            }

            Fn(args, ret) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(self.resolve(arg));
                }
                let new_ret = Box::new(self.resolve(ret));
                Fn(new_args, new_ret)
            }

            User(name, args) => {
                let mut new_args = Vec::new();
                for arg in args {
                    new_args.push(self.resolve(arg));
                }
                User(name.clone(), new_args)
            }

            SomeRecord(record) => SomeRecord(self.resolve_record(record)),
            AnonRecord(record) => AnonRecord(self.resolve_record(record)),
            SomeSum(cases) => SomeSum(self.resolve_cases(cases)),
            AnonSum(cases) => AnonSum(self.resolve_cases(cases)),

            x => x.clone(),
        }
    }

    fn resolve_record(&mut self, record: &Record) -> Record {
        let fields = owned_fields(record)
            .into_iter()
            .map(|(name, ty)| (name, self.resolve(&ty)))
            .collect();
        Record::build(fields).expect("a shape taken from a record")
    }

    fn resolve_cases(&mut self, cases: &[SumCase]) -> Vec<SumCase> {
        cases
            .iter()
            .map(|case| SumCase {
                name: case.name.clone(),
                payload: case.payload.as_ref().map(|ty| self.resolve(ty)),
            })
            .collect()
    }

    /// True if the root of `var_root` occurs anywhere inside `ty` without
    /// crossing a pointer indirection. Pointer boundaries break cycles
    /// (a struct containing Ptr<Self> has finite size and is legitimate);
    /// direct embedding (struct/sum fields, fn args) does not.
    fn occurs(&mut self, var_root: usize, ty: &Type) -> bool {
        use Type::*;
        match ty {
            T(other) => {
                let other_root = self.find(*other);
                if other_root == var_root {
                    return true;
                }
                match self.bindings.get(&other_root).cloned() {
                    Some(bound) => self.occurs(var_root, &bound),
                    None => false,
                }
            }

            Fn(args, ret) => {
                args.iter().any(|a| self.occurs(var_root, a)) || self.occurs(var_root, ret)
            }

            User(_, args) => args.iter().any(|a| self.occurs(var_root, a)),

            // A field holds its type by value, so a variable reaching one of
            // these is embedded in it just as directly as in a `Fn` argument. A
            // case's payload is a record, and reached the same way.
            SomeRecord(record) | AnonRecord(record) => {
                record.types().any(|t| self.occurs(var_root, t))
            }
            SomeSum(cases) | AnonSum(cases) => cases.iter().any(|case| {
                case.payload
                    .as_ref()
                    .is_some_and(|t| self.occurs(var_root, t))
            }),

            _ => false,
        }
    }

    pub(super) fn default_types(&mut self) {
        self.map_bindings(&default_ty);
    }

    /// Close every type an inference left open. A record or sum that never met a
    /// concrete type becomes the anonymous type of exactly what it was known to
    /// have.
    pub(super) fn close_some_types(&mut self) {
        self.map_bindings(&close_ty);
    }

    fn map_bindings(&mut self, f: &dyn Fn(Type) -> Type) {
        let roots = self.bindings.keys().copied().collect::<Vec<_>>();
        for root in roots {
            let ty = self.bindings.remove(&root).unwrap();
            self.bindings.insert(root, f(ty));
        }
    }
}

/// A record's fields, owned, in canonical order. Needed wherever the fields are
/// walked while the solver is also being mutated.
fn owned_fields(record: &Record) -> Vec<(FieldName, Type)> {
    record.iter().map(|(name, ty)| (name, ty.clone())).collect()
}

fn wrong_fields(expected: &Record, got: &Record, loc: Loc) -> FloErr {
    FloErr::WrongFields {
        expected: expected.names().iter().map(|n| format!("{n:?}")).collect(),
        got: got.names().iter().map(|n| format!("{n:?}")).collect(),
        loc,
    }
}

/// A literal's default type, applied everywhere inside `ty` — including field
/// types, where a merge may have left an `{integer}` sitting inside a record.
fn default_ty(ty: Type) -> Type {
    use Type::*;
    match ty {
        Integer => I32,
        Decimal => F32,
        other => map_children(other, &default_ty),
    }
}

fn close_ty(ty: Type) -> Type {
    use Type::*;
    match ty {
        SomeRecord(record) => AnonRecord(record.map_types(&mut |t| close_ty(t.clone()))),
        SomeSum(cases) => AnonSum(sorted_cases(
            cases
                .into_iter()
                .map(|c| c.map_types(&mut |t| close_ty(t.clone())))
                .collect(),
        )),
        other => map_children(other, &close_ty),
    }
}

/// `ty` with `f` applied to every type it contains. Type variables are left
/// alone: what they stand for lives in its own binding, and is mapped there.
fn map_children(ty: Type, f: &dyn Fn(Type) -> Type) -> Type {
    use Type::*;
    match ty {
        Fn(args, ret) => Fn(args.into_iter().map(|a| f(a)).collect(), Box::new(f(*ret))),
        User(name, args) => User(name, args.into_iter().map(|a| f(a)).collect()),
        SomeRecord(record) => SomeRecord(record.map_types(&mut |t| f(t.clone()))),
        AnonRecord(record) => AnonRecord(record.map_types(&mut |t| f(t.clone()))),
        SomeSum(cases) => SomeSum(
            cases
                .into_iter()
                .map(|c| c.map_types(&mut |t| f(t.clone())))
                .collect(),
        ),
        AnonSum(cases) => AnonSum(
            cases
                .into_iter()
                .map(|c| c.map_types(&mut |t| f(t.clone())))
                .collect(),
        ),
        other => other,
    }
}
