use std::{collections::HashMap, fmt::Debug};

#[allow(unused)]
#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Type {
    /// Type Var
    T(usize),

    /// Fn(args, ret)
    Fn(Vec<Type>, Box<Type>),

    /// void
    Void,

    /// i32
    I32,
}

impl Type {
    pub fn is_known(&self) -> bool {
        match self {
            Type::T(_) => false,

            Type::Fn(args, ret) => {
                let mut known = true;
                for arg in args {
                    known &= arg.is_known();
                }
                known && ret.is_known()
            }

            Type::Void | Type::I32 => true,
        }
    }

    pub fn replace_types(&mut self, replace_map: &HashMap<Type, Type>) {
        match self {
            Type::T(_) => {
                if let Some(replacement) = replace_map.get(self) {
                    *self = replacement.clone();
                }
            }
            Type::Fn(args, ret) => {
                for arg in args {
                    arg.replace_types(replace_map);
                }
                ret.replace_types(replace_map);
            }
            _ => {}
        }
    }
}

impl Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::T(n) => write!(f, "t{n}"),
            Self::Fn(args, ret) => {
                let arg_list = args
                    .iter()
                    .map(|a| format!("{a:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "({arg_list}) -> {ret:?}")
            }
            Self::Void => write!(f, "void"),
            Self::I32 => write!(f, "i32"),
        }
    }
}
