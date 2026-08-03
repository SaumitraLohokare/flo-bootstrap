use std::fmt::Debug;

#[allow(unused)]
#[rustfmt::skip]
#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Type {
    // Type Var
    T(usize),

    // Fn(args, ret)
    Fn(Vec<Type>, Box<Type>),

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

impl Type {
    pub fn is_known(&self) -> bool {
        use Type::*;
        match self {
            T(_) => false,
            Fn(args, ret) => {
                let mut known = true;
                for arg in args {
                    known &= arg.is_known();
                }
                known & ret.is_known()
            }
            Integer => false,
            Decimal => false,
            Void | Never | Bool | U8 | U16 | U32 | U64 | I8 | I16 | I32 | I64 | F32 | F64 => true,
        }
    }

    pub fn satisfies_type(&self, other: &Type) -> bool {
        use Type::*;
        match (self, other) {
            (T(_), _) => true,
            // A NoReturn value satisfies any expected type (bottom type).
            (Never, _) => true,
            (Fn(args_1, ret_1), Fn(args_2, ret_2)) => {
                let mut satisfies = true;
                for (arg_1, arg_2) in args_1.iter().zip(args_2) {
                    satisfies &= arg_1.satisfies_type(arg_2);
                }
                satisfies & ret_1.satisfies_type(ret_2)
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
        }
    }
}
