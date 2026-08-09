use std::{collections::HashMap, rc::Rc};

use crate::{
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeCase, TypeTable, sorted_cases},
};

#[derive(Debug)]
pub(super) struct ReplaceSet {
    parents: HashMap<usize, usize>,
    bindings: HashMap<usize, Type>,
    /// Needed to unify an open [`Type::SomeType`] with a declared type: the
    /// `Type` only carries the declaration's name, and its cases have to be
    /// looked up to be checked against.
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
            (T(a), t2) => {
                self.bind(a, t2, loc)?;
                self.resolve(&T(a))
            }
            (t1, T(b)) => {
                self.bind(b, t1, loc)?;
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

            // An open type meeting the type it belongs to. This is where a
            // literal is finally pinned down: every case it was known to have
            // must be a case of the declaration, with exactly those fields.
            (SomeType(cases), User(name, args)) | (User(name, args), SomeType(cases)) => {
                self.check_against_decl(&cases, &name, &args, loc)?;
                User(name, args)
            }

            // Two open types: neither knows the whole set, so take the union.
            (SomeType(c1), SomeType(c2)) => SomeType(self.merge_cases(c1, c2, loc)?),

            // An open type meeting a closed one. The closed side is the whole
            // set, so the open side may only have cases it already has.
            (SomeType(open), Anon(closed)) | (Anon(closed), SomeType(open)) => {
                for case in &open {
                    match closed.iter().find(|c| c.name == case.name) {
                        None => {
                            return Err(FloErr::NoSuchCase {
                                ty: Anon(closed.clone()),
                                case: case.name.clone(),
                                loc,
                            });
                        }
                        Some(other) => {
                            self.unify_cases(case.clone(), other.clone(), loc)?;
                        }
                    }
                }
                Anon(closed)
            }

            // Structural, so two of them are the same type exactly when they
            // have the same cases. Both sides are canonically ordered.
            (Anon(c1), Anon(c2))
                if c1.len() == c2.len()
                    && c1.iter().zip(&c2).all(|(a, b)| {
                        a.name == b.name && a.field_names() == b.field_names()
                    }) =>
            {
                let mut cases = Vec::with_capacity(c1.len());
                for (a, b) in c1.into_iter().zip(c2) {
                    cases.push(self.unify_cases(a, b, loc)?);
                }
                Anon(cases)
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

    /// Check every case an open type is known to have against the declaration it
    /// turned out to belong to, unifying the field types as it goes.
    fn check_against_decl(
        &mut self,
        cases: &[TypeCase],
        name: &str,
        args: &[Type],
        loc: Loc,
    ) -> FloResult<()> {
        let types = Rc::clone(&self.types);
        let Some(decl) = types.get(name) else {
            // Reported by the declaration check; nothing useful to say here.
            return Ok(());
        };

        for case in cases {
            let Some(declared) = decl.case_at(&case.name, args) else {
                return Err(FloErr::NoSuchCase {
                    ty: Type::User(name.to_string(), args.to_vec()),
                    case: case.name.clone(),
                    loc,
                });
            };

            self.unify_cases(case.clone(), declared, loc)?;
        }

        Ok(())
    }

    /// The union of two sets of cases. A case both sides know about has to be
    /// the same case on both.
    fn merge_cases(
        &mut self,
        c1: Vec<TypeCase>,
        c2: Vec<TypeCase>,
        loc: Loc,
    ) -> FloResult<Vec<TypeCase>> {
        let mut merged: Vec<TypeCase> = Vec::with_capacity(c1.len() + c2.len());

        for case in c1 {
            match c2.iter().find(|c| c.name == case.name) {
                Some(other) => merged.push(self.unify_cases(case, other.clone(), loc)?),
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

    /// Unify two cases of the same name. A literal has to give a case's fields
    /// exactly, so anything but the same field names is an error — which is what
    /// reports a missing or misspelt field.
    fn unify_cases(&mut self, a: TypeCase, b: TypeCase, loc: Loc) -> FloResult<TypeCase> {
        if a.field_names() != b.field_names() {
            return Err(FloErr::WrongFields {
                expected: b.field_names().iter().map(|s| s.to_string()).collect(),
                got: a.field_names().iter().map(|s| s.to_string()).collect(),
                case: a.name,
                loc,
            });
        }

        let mut fields = Vec::with_capacity(a.fields.len());
        for ((name, x), (_, y)) in a.fields.into_iter().zip(b.fields) {
            fields.push((name, self.unify_types(x, y, loc)?));
        }

        Ok(TypeCase { name: a.name, fields })
    }

    pub(super) fn bind(&mut self, ty_id: usize, ty: Type, loc: Loc) -> FloResult<()> {
        let root = self.find(ty_id);

        if self.occurs(root, &ty) {
            return Err(FloErr::InfiniteType { loc });
        }

        let existing = self.bindings.remove(&root);
        let merged = match existing {
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

            SomeType(cases) => SomeType(self.resolve_cases(cases)),
            Anon(cases) => Anon(self.resolve_cases(cases)),

            x => x.clone(),
        }
    }

    fn resolve_cases(&mut self, cases: &[TypeCase]) -> Vec<TypeCase> {
        cases
            .iter()
            .map(|case| {
                let fields = case
                    .fields
                    .iter()
                    .map(|(n, t)| (n.clone(), self.resolve(t)))
                    .collect();
                TypeCase {
                    name: case.name.clone(),
                    fields,
                }
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
            // these is embedded in it just as directly as in a `Fn` argument.
            SomeType(cases) | Anon(cases) => cases.iter().any(|case| {
                case.fields
                    .iter()
                    .any(|(_, t)| self.occurs(var_root, t))
            }),

            _ => false,
        }
    }

    pub(super) fn default_types(&mut self) {
        self.map_bindings(&default_ty);
    }

    /// Close every type an inference left open. A [`Type::SomeType`] that never
    /// met a declared type becomes the anonymous type of exactly the cases it
    /// was known to have.
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

/// A literal's default type, applied everywhere inside `ty` — including field
/// types, where a merge may have left an `{integer}` sitting inside a case.
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
        SomeType(cases) => Anon(sorted_cases(
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
        Fn(args, ret) => Fn(
            args.into_iter().map(|a| f(a)).collect(),
            Box::new(f(*ret)),
        ),
        User(name, args) => User(name, args.into_iter().map(|a| f(a)).collect()),
        SomeType(cases) => SomeType(
            cases
                .into_iter()
                .map(|c| c.map_types(&mut |t| f(t.clone())))
                .collect(),
        ),
        Anon(cases) => Anon(
            cases
                .into_iter()
                .map(|c| c.map_types(&mut |t| f(t.clone())))
                .collect(),
        ),
        other => other,
    }
}
