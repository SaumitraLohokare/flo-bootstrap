use std::fmt::Debug;

#[allow(unused)]
#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Type {
    /// Type Var
    T(usize),

    /// Fn(args, ret)
    Fn(Vec<Type>, Box<Type>),

    /// void
    Void,

    /// {integer}
    Integer,

    /// i32
    I32,
}

impl Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Type::*;
        match self {
            T(n) => write!(f, "t{n}"),
            Void => write!(f, "void"),
            I32 => write!(f, "i32"),
            Integer => write!(f, "{{integer}}"),
            Fn(args, ret) => {
                let arg_list = args
                    .iter()
                    .map(|a| format!("{a:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "({arg_list}) -> {ret:?}")
            }
        }
    }
}
