use std::collections::HashMap;

use crate::{
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    types::{Type, TypeKind},
};

/// Union-find over type variables with concrete-type bindings and `Numeric`-style
/// kind bounds. Structural unification for compound (function) types keeps it
/// extensible for pointers/arrays later.
pub(super) struct UnionFind {
    parent: HashMap<usize, usize>,
    /// Representative -> the concrete type its set resolved to.
    binding: HashMap<usize, Type>,
    /// Representative -> its kind bound (and where the bound was introduced).
    kind: HashMap<usize, (TypeKind, Loc)>,
}

impl UnionFind {
    pub(super) fn new() -> Self {
        Self {
            parent: HashMap::new(),
            binding: HashMap::new(),
            kind: HashMap::new(),
        }
    }

    fn find(&mut self, id: usize) -> usize {
        let mut root = id;
        while let Some(&p) = self.parent.get(&root) {
            if p == root {
                break;
            }
            root = p;
        }
        // Path compression.
        let mut cur = id;
        while cur != root {
            let next = *self.parent.get(&cur).unwrap_or(&root);
            self.parent.insert(cur, root);
            cur = next;
        }
        root
    }

    /// The kind bound registered against a representative id, if any. Callers pass
    /// a canonical root (e.g. one produced by `resolve_head`).
    pub(super) fn kind_bound(&self, root: usize) -> Option<&(TypeKind, Loc)> {
        self.kind.get(&root)
    }

    /// Resolve a type to its head: a concrete type, or its free representative.
    pub(super) fn resolve_head(&mut self, ty: &Type) -> Type {
        match ty {
            Type::T(id) => {
                let r = self.find(*id);
                match self.binding.get(&r) {
                    Some(t) => t.clone(),
                    None => Type::T(r),
                }
            }
            other => other.clone(),
        }
    }

    pub(super) fn unify(&mut self, a: &Type, b: &Type, loc: Loc) -> FloResult<()> {
        let ra = self.resolve_head(a);
        let rb = self.resolve_head(b);

        match (ra, rb) {
            (Type::T(ia), Type::T(ib)) => {
                if ia == ib {
                    return Ok(());
                }
                // Neither is bound (else resolve_head returned concrete), so just
                // union and carry any kind bound over to the new root.
                self.parent.insert(ia, ib);
                if let Some(k) = self.kind.remove(&ia) {
                    self.kind.entry(ib).or_insert(k);
                }
                Ok(())
            }
            (Type::T(i), concrete) | (concrete, Type::T(i)) => self.bind(i, concrete, loc),
            (c1, c2) => self.unify_concrete(&c1, &c2, loc),
        }
    }

    fn bind(&mut self, var: usize, concrete: Type, loc: Loc) -> FloResult<()> {
        let root = self.find(var);
        if let Some(existing) = self.binding.get(&root).cloned() {
            return self.unify_concrete(&existing, &concrete, loc);
        }
        if let Some(&(kind, kloc)) = self.kind.get(&root) {
            if !kind.satisfies_type(&concrete) {
                return Err(FloErr::UnsatisfiedTypeKind {
                    ty: concrete,
                    ty_loc: loc,
                    kind,
                    loc: kloc,
                });
            }
        }
        self.binding.insert(root, concrete);
        Ok(())
    }

    fn unify_concrete(&mut self, a: &Type, b: &Type, loc: Loc) -> FloResult<()> {
        match (a, b) {
            (Type::Fn(pa, ra), Type::Fn(pb, rb)) => {
                if pa.len() != pb.len() {
                    return Err(FloErr::TypeMismatch {
                        t1: a.clone(),
                        loc1: loc,
                        t2: b.clone(),
                        loc2: loc,
                    });
                }
                for (x, y) in pa.iter().zip(pb) {
                    self.unify(x, y, loc)?;
                }
                self.unify(ra, rb, loc)
            }
            _ if a == b => Ok(()),
            _ => Err(FloErr::TypeMismatch {
                t1: a.clone(),
                loc1: loc,
                t2: b.clone(),
                loc2: loc,
            }),
        }
    }

    pub(super) fn add_kind(&mut self, ty: &Type, kind: TypeKind, loc: Loc) -> FloResult<()> {
        match self.resolve_head(ty) {
            Type::T(root) => {
                self.kind.entry(root).or_insert((kind, loc));
                Ok(())
            }
            concrete => {
                if kind.satisfies_type(&concrete) {
                    Ok(())
                } else {
                    Err(FloErr::UnsatisfiedTypeKind {
                        ty: concrete,
                        ty_loc: loc,
                        kind,
                        loc,
                    })
                }
            }
        }
    }

    /// Bind every still-free variable that carries a kind bound to that kind's
    /// default concrete type (`Numeric` → `i32`). Real information has already been
    /// unified in, so this only touches variables nothing else pinned down.
    /// Returns whether any variable was newly bound — the strict solver uses this
    /// to decide whether a stalled overload set is worth retrying.
    pub(super) fn default_free(&mut self) -> bool {
        let bounds: Vec<usize> = self.kind.keys().copied().collect();
        let mut bound_any = false;
        for var in bounds {
            let root = self.find(var);
            if !self.binding.contains_key(&root) {
                if let Some(&(kind, _)) = self.kind.get(&root) {
                    self.binding.insert(root, kind.default_type());
                    bound_any = true;
                }
            }
        }
        bound_any
    }
}
