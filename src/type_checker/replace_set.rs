use std::collections::HashMap;

use crate::{
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::Type,
};

#[derive(Debug)]
pub(super) struct ReplaceSet {
    parents: HashMap<usize, usize>,
    bindings: HashMap<usize, Type>,
}

impl ReplaceSet {
    pub(super) fn new() -> Self {
        Self {
            parents: HashMap::new(),
            bindings: HashMap::new(),
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

            x => x.clone(),
        }
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

            _ => false,
        }
    }

    pub(super) fn default_types(&mut self) {
        use Type::*;

        for (_, ty) in self.bindings.iter_mut() {
            match ty {
                Integer => *ty = I32,
                Decimal => *ty = F32,
                _ => {}
            }
        }
    }
}
