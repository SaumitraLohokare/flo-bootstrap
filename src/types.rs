use std::fmt::Debug;

#[derive(Debug, Clone, Copy)]
pub enum TypeKind {
    Integral,
}

impl TypeKind {
    pub fn satisfies_type(&self, ty: &Type) -> bool {
        use Type::*;
        use TypeKind::*;

        match self {
            Integral => matches!(ty, I32),
        }
    }

    pub fn default_type(&self) -> Type {
        match self {
            TypeKind::Integral => Type::I32,
        }
    }
}

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
