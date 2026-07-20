use std::collections::HashMap;

use crate::{
    errors::{FloErr, FloResult},
    tokenizer::Loc,
    type_checker::TypeLoc,
    types::Type,
};

#[derive(Debug)]
pub struct ReplaceSet {
    pub ty_to_id: HashMap<TypeLoc, usize>,
    pub id_to_ty: Vec<TypeLoc>,

    parents: Vec<usize>,
}

impl ReplaceSet {
    pub fn new() -> Self {
        Self {
            ty_to_id: HashMap::new(),
            id_to_ty: Vec::new(),

            parents: Vec::new(),
        }
    }

    // We should add all known types first
    pub fn add(&mut self, ty: Type, loc: Loc) -> usize {
        let type_loc = (ty, loc);
        if let Some(&id) = self.ty_to_id.get(&type_loc) {
            return id;
        }
        let id = self.id_to_ty.len();
        self.id_to_ty.push(type_loc.clone());
        self.ty_to_id.insert(type_loc, id);
        self.parents.push(id);
        id
    }

    pub fn find(&mut self, type_id: usize) -> usize {
        // All types should already be added
        debug_assert!(self.id_to_ty.len() > type_id);

        let mut root = type_id;
        while self.parents[root] != root {
            root = self.parents[root];
        }

        // Path Compression
        let mut cur = type_id;
        while cur != root {
            let next = self.parents[cur];
            self.parents[cur] = root;
            cur = next;
        }

        root
    }

    pub fn union(&mut self, a: usize, b: usize) -> FloResult<()> {
        let ra = self.find(a);
        let rb = self.find(b);

        let (ra_type, ra_loc) = &self.id_to_ty[ra];
        let (rb_type, rb_loc) = &self.id_to_ty[rb];

        match (ra_type.is_known(), rb_type.is_known()) {
            (true, true) => {
                if ra_type != rb_type {
                    Err(FloErr::TypeMismatch {
                        t1: ra_type.clone(),
                        loc1: *ra_loc,
                        t2: rb_type.clone(),
                        loc2: *rb_loc,
                    })
                } else {
                    Ok(())
                }
            }
            (false, _) => {
                self.parents[ra] = rb;
                Ok(())
            }
            (_, false) => {
                self.parents[rb] = ra;
                Ok(())
            }
        }
    }

    pub fn resolve(&mut self, ty: Type, loc: Loc) -> FloResult<Type> {
        let type_loc = (ty, loc);

        let Some(&id) = self.ty_to_id.get(&type_loc) else {
            unreachable!()
        };
        let root = self.find(id);
        let (root_type, _) = self.id_to_ty[root].clone();

        if root_type.is_known() {
            Ok(root_type.clone())
        } else {
            Err(FloErr::UnresolvedType { loc })
        }
    }
}
