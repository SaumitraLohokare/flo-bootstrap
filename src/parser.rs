use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::{Loc, Token, TokenKind, TokenValue},
    types::Type,
    util::Iota,
};

struct Scope {
    var_iota: Iota,
    vars: HashMap<String, usize>,
    var_types: HashMap<usize, Type>,
}

impl Scope {
    fn new() -> Self {
        Self {
            var_iota: Iota::new(),
            vars: HashMap::new(),
            var_types: HashMap::new(),
        }
    }

    fn add_arg(&mut self, name: String, ty: Type) -> bool {
        if self.vars.contains_key(&name) {
            return false;
        }

        let var_id = self.var_iota.next();
        self.vars.insert(name, var_id);
        self.var_types.insert(var_id, ty);
        true
    }

    fn get_var(&self, name: &String) -> Option<usize> {
        self.vars.get(name).copied()
    }

    fn get_var_type(&self, id: usize) -> Type {
        self.var_types[&id].clone()
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    idx: usize,

    type_iota: Iota,

    funcs: HashMap<String, Vec<Func>>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            idx: 0,
            type_iota: Iota::new(),
            funcs: HashMap::new(),
        }
    }

    pub fn parse(mut self) -> FloResult<Module> {
        while let Ok(token) = self.peek() {
            match token.kind {
                TokenKind::Fn => self.parse_func()?,

                _ => {
                    return Err(FloErr::UnexpectedToken {
                        found: token.clone(),
                    });
                }
            }
        }

        // Register builtin ops (body is a Nop expr with ty = ret_ty)
        self.register_builtin_ops();

        match self.funcs.entry("main".to_string()).or_default().len() {
            0 => Err(FloErr::MainFunctionNotFound),
            1 => Ok(Module { funcs: self.funcs }),
            _ => Err(FloErr::MultipleMainFunction),
        }
    }

    fn parse_func(&mut self) -> FloResult<()> {
        use TokenKind::*;
        self.expect(Fn)?;
        let name_token = self.expect_get(Ident)?;
        let name_loc = name_token.loc;
        let TokenValue::String(name) = name_token.value.clone() else {
            unreachable!()
        };

        let mut scope = Scope::new();

        self.expect(LParen)?;

        let mut arg_types = Vec::new();
        while self.peek()?.kind == Ident {
            let arg = self.expect_get(Ident)?;
            let TokenValue::String(arg_name) = arg.value else {
                unreachable!()
            };
            self.expect(Colon)?;
            let (arg_type, _) = self.parse_type()?;
            arg_types.push(arg_type.clone());

            // EW: clone might be unneccessary
            if !scope.add_arg(arg_name.clone(), arg_type) {
                return Err(FloErr::RedifinitionOfArgument {
                    name: arg_name,
                    loc: arg.loc,
                });
            }

            if self.expect(Comma).is_err() {
                break;
            }
        }

        let r_paren_token = self.expect_get(RParen)?;
        let r_paren_loc = r_paren_token.loc;

        let (ret_type, ret_type_loc) = if self.expect(Arrow).is_ok() {
            self.parse_type()?
        } else {
            (
                Type::Void,
                Loc {
                    start: name_loc.start,
                    end: r_paren_loc.end,
                },
            )
        };

        let loc = Loc {
            start: name_loc.start,
            end: ret_type_loc.end,
        };

        let ty = self.func_type(arg_types, ret_type);

        self.expect(Equal)?;

        let body = self.parse_expr(-1, &scope)?;

        self.expect(Semicolon)?;

        // It would be good to check for ambiguous overloads
        // here. It would give more consistent errors
        self.funcs
            .entry(name)
            .or_default()
            .push(Func { body, ty, loc });

        Ok(())
    }

    fn parse_expr(&mut self, precedence: i32, scope: &Scope) -> FloResult<Expr> {
        use ExprKind::*;
        let mut lhs = self.parse_unary(scope)?;

        loop {
            let tok = self.peek()?;
            let op = tok.kind;

            if !op.is_binary_op() {
                break;
            }

            let op_precedence = op.precedence();
            if op_precedence < precedence {
                break;
            }

            self.skip();

            let rhs = self.parse_expr(op_precedence + 1, scope)?;
            let loc = Loc {
                start: lhs.loc.start,
                end: rhs.loc.end,
            };
            lhs = Expr {
                kind: Call(format!("{}", op.pretty_name()), vec![lhs, rhs], None),
                ty: self.fresh_type(),
                loc,
            };
        }

        Ok(lhs)
    }

    fn parse_unary(&mut self, scope: &Scope) -> FloResult<Expr> {
        use ExprKind::*;

        let token = self.peek()?;
        let op = token.kind;
        if op.is_unary_op() {
            let start = token.loc.start;
            self.skip();

            let operand = self.parse_unary(scope)?;
            let end = operand.loc.end;
            let kind = Call(format!("{}", op.pretty_name()), vec![operand], None);
            let loc = Loc { start, end };
            Ok(Expr {
                kind,
                ty: self.fresh_type(),
                loc,
            })
        } else {
            self.parse_atom(scope)
        }
    }

    fn parse_atom(&mut self, scope: &Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let token = self.peek()?;
        match token.kind {
            Num => {
                let TokenValue::Num(num) = token.value else {
                    unreachable!()
                };
                let loc = token.loc;
                self.skip();

                let kind = ExprKind::Num(num);
                Ok(Expr {
                    kind,
                    ty: self.fresh_type(),
                    loc,
                })
            }

            Flt => {
                let TokenValue::Flt(num) = token.value else {
                    unreachable!()
                };
                let loc = token.loc;
                self.skip();

                let kind = ExprKind::Flt(num);
                Ok(Expr {
                    kind,
                    ty: self.fresh_type(),
                    loc,
                })
            }

            True | False => {
                let value = token.kind == True;
                let loc = token.loc;
                self.skip();

                let kind = ExprKind::Bool(value);
                Ok(Expr {
                    kind,
                    ty: self.fresh_type(),
                    loc,
                })
            }

            Ident => {
                let TokenValue::String(name) = token.value.clone() else {
                    unreachable!()
                };
                let name_loc = token.loc;

                self.skip();

                if let Ok(LParen) = self.peek_kind() {
                    self.skip();

                    let mut args = Vec::new();
                    while let Ok(next_kind) = self.peek_kind()
                        && next_kind != RParen
                    {
                        args.push(self.parse_expr(-1, scope)?);
                        if self.expect(Comma).is_err() {
                            break;
                        }
                    }

                    let r_paren = self.expect_get(RParen)?;

                    // All calls are unresolved initially
                    let kind = ExprKind::Call(name, args, None);
                    Ok(Expr {
                        kind,
                        ty: self.fresh_type(),
                        loc: Loc {
                            start: name_loc.start,
                            end: r_paren.loc.end,
                        },
                    })
                } else {
                    let var_id = scope.get_var(&name).ok_or(FloErr::UndefinedIdentifier {
                        name: name.clone(),
                        loc: name_loc,
                    })?;

                    let kind = ExprKind::Var(var_id);
                    Ok(Expr {
                        kind,
                        ty: scope.get_var_type(var_id),
                        loc: name_loc,
                    })
                }
            }

            _ => Err(FloErr::UnexpectedToken {
                found: token.clone(),
            }),
        }
    }

    fn parse_type(&mut self) -> FloResult<(Type, Loc)> {
        use TokenKind::*;
        let token = self.peek()?;
        let loc = token.loc;

        match token.kind {
            Ident => {
                let token = self.expect_get(Ident)?;
                let TokenValue::String(value) = &token.value else {
                    unreachable!()
                };

                match value.as_str() {
                    "u8" => Ok((Type::U8, loc)),
                    "u16" => Ok((Type::U16, loc)),
                    "u32" => Ok((Type::U32, loc)),
                    "u64" => Ok((Type::U64, loc)),
                    "i8" => Ok((Type::I8, loc)),
                    "i16" => Ok((Type::I16, loc)),
                    "i32" => Ok((Type::I32, loc)),
                    "i64" => Ok((Type::I64, loc)),
                    "f32" => Ok((Type::F32, loc)),
                    "f64" => Ok((Type::F64, loc)),
                    "void" => Ok((Type::Void, loc)),
                    "bool" => Ok((Type::Bool, loc)),

                    _ => Err(FloErr::NotAType { token }),
                }
            }

            _ => Err(FloErr::NotAType {
                token: token.clone(),
            }),
        }
    }

    fn peek(&self) -> FloResult<&Token> {
        self.tokens.get(self.idx).ok_or(FloErr::UnexpectedEOF)
    }

    fn peek_kind(&self) -> FloResult<TokenKind> {
        self.tokens
            .get(self.idx)
            .map(|t| t.kind)
            .ok_or(FloErr::UnexpectedEOF)
    }

    fn skip(&mut self) {
        self.idx += 1;
    }

    fn expect(&mut self, expected: TokenKind) -> FloResult<()> {
        let token = self.peek()?;
        if token.kind != expected {
            Err(FloErr::ExpectedTokenNotFound {
                expected,
                found: token.clone(),
            })
        } else {
            self.skip();
            Ok(())
        }
    }

    fn expect_get(&mut self, expected: TokenKind) -> FloResult<Token> {
        let token = self.peek()?;
        if token.kind != expected {
            Err(FloErr::ExpectedTokenNotFound {
                expected,
                found: token.clone(),
            })
        } else {
            let token = token.clone();
            self.skip();
            Ok(token)
        }
    }

    fn fresh_type(&mut self) -> Type {
        Type::T(self.type_iota.next())
    }

    fn func_type(&self, arg_types: Vec<Type>, ret_type: Type) -> Type {
        Type::Fn(arg_types, Box::new(ret_type))
    }

    fn register_builtin_ops(&mut self) {
        use TokenKind::*;
        use Type::*;

        let plus_op = self.funcs.entry("+".to_string()).or_default();
        // binary
        plus_op.push(builtin_op(Plus, vec![U8, U8], U8));
        plus_op.push(builtin_op(Plus, vec![U16, U16], U16));
        plus_op.push(builtin_op(Plus, vec![U32, U32], U32));
        plus_op.push(builtin_op(Plus, vec![U64, U64], U64));
        plus_op.push(builtin_op(Plus, vec![I8, I8], I8));
        plus_op.push(builtin_op(Plus, vec![I16, I16], I16));
        plus_op.push(builtin_op(Plus, vec![I32, I32], I32));
        plus_op.push(builtin_op(Plus, vec![I64, I64], I64));
        plus_op.push(builtin_op(Plus, vec![F32, F32], F32));
        plus_op.push(builtin_op(Plus, vec![F64, F64], F64));
        // unary
        plus_op.push(builtin_op(Plus, vec![U8], U8));
        plus_op.push(builtin_op(Plus, vec![U16], U16));
        plus_op.push(builtin_op(Plus, vec![U32], U32));
        plus_op.push(builtin_op(Plus, vec![U64], U64));
        plus_op.push(builtin_op(Plus, vec![I8], I8));
        plus_op.push(builtin_op(Plus, vec![I16], I16));
        plus_op.push(builtin_op(Plus, vec![I32], I32));
        plus_op.push(builtin_op(Plus, vec![I64], I64));
        plus_op.push(builtin_op(Plus, vec![F32], F32));
        plus_op.push(builtin_op(Plus, vec![F64], F64));

        let minus_op = self.funcs.entry("-".to_string()).or_default();
        // binary
        minus_op.push(builtin_op(Minus, vec![U8, U8], U8));
        minus_op.push(builtin_op(Minus, vec![U16, U16], U16));
        minus_op.push(builtin_op(Minus, vec![U32, U32], U32));
        minus_op.push(builtin_op(Minus, vec![U64, U64], U64));
        minus_op.push(builtin_op(Minus, vec![I8, I8], I8));
        minus_op.push(builtin_op(Minus, vec![I16, I16], I16));
        minus_op.push(builtin_op(Minus, vec![I32, I32], I32));
        minus_op.push(builtin_op(Minus, vec![I64, I64], I64));
        minus_op.push(builtin_op(Minus, vec![F32, F32], F32));
        minus_op.push(builtin_op(Minus, vec![F64, F64], F64));
        // unary
        minus_op.push(builtin_op(Minus, vec![U8], U8));
        minus_op.push(builtin_op(Minus, vec![U16], U16));
        minus_op.push(builtin_op(Minus, vec![U32], U32));
        minus_op.push(builtin_op(Minus, vec![U64], U64));
        minus_op.push(builtin_op(Minus, vec![I8], I8));
        minus_op.push(builtin_op(Minus, vec![I16], I16));
        minus_op.push(builtin_op(Minus, vec![I32], I32));
        minus_op.push(builtin_op(Minus, vec![I64], I64));
        minus_op.push(builtin_op(Minus, vec![F32], F32));
        minus_op.push(builtin_op(Minus, vec![F64], F64));

        let star_op = self.funcs.entry("*".to_string()).or_default();
        star_op.push(builtin_op(Star, vec![U8, U8], U8));
        star_op.push(builtin_op(Star, vec![U16, U16], U16));
        star_op.push(builtin_op(Star, vec![U32, U32], U32));
        star_op.push(builtin_op(Star, vec![U64, U64], U64));
        star_op.push(builtin_op(Star, vec![I8, I8], I8));
        star_op.push(builtin_op(Star, vec![I16, I16], I16));
        star_op.push(builtin_op(Star, vec![I32, I32], I32));
        star_op.push(builtin_op(Star, vec![I64, I64], I64));
        star_op.push(builtin_op(Star, vec![F32, F32], F32));
        star_op.push(builtin_op(Star, vec![F64, F64], F64));

        let slash_op = self.funcs.entry("/".to_string()).or_default();
        slash_op.push(builtin_op(Slash, vec![U8, U8], U8));
        slash_op.push(builtin_op(Slash, vec![U16, U16], U16));
        slash_op.push(builtin_op(Slash, vec![U32, U32], U32));
        slash_op.push(builtin_op(Slash, vec![U64, U64], U64));
        slash_op.push(builtin_op(Slash, vec![I8, I8], I8));
        slash_op.push(builtin_op(Slash, vec![I16, I16], I16));
        slash_op.push(builtin_op(Slash, vec![I32, I32], I32));
        slash_op.push(builtin_op(Slash, vec![I64, I64], I64));
        slash_op.push(builtin_op(Slash, vec![F32, F32], F32));
        slash_op.push(builtin_op(Slash, vec![F64, F64], F64));

        let percent_op = self.funcs.entry("%".to_string()).or_default();
        percent_op.push(builtin_op(Percent, vec![U8, U8], U8));
        percent_op.push(builtin_op(Percent, vec![U16, U16], U16));
        percent_op.push(builtin_op(Percent, vec![U32, U32], U32));
        percent_op.push(builtin_op(Percent, vec![U64, U64], U64));
        percent_op.push(builtin_op(Percent, vec![I8, I8], I8));
        percent_op.push(builtin_op(Percent, vec![I16, I16], I16));
        percent_op.push(builtin_op(Percent, vec![I32, I32], I32));
        percent_op.push(builtin_op(Percent, vec![I64, I64], I64));
        percent_op.push(builtin_op(Percent, vec![F32, F32], F32));
        percent_op.push(builtin_op(Percent, vec![F64, F64], F64));

        let amp_op = self.funcs.entry("&".to_string()).or_default();
        amp_op.push(builtin_op(Amp, vec![U8, U8], U8));
        amp_op.push(builtin_op(Amp, vec![U16, U16], U16));
        amp_op.push(builtin_op(Amp, vec![U32, U32], U32));
        amp_op.push(builtin_op(Amp, vec![U64, U64], U64));
        amp_op.push(builtin_op(Amp, vec![I8, I8], I8));
        amp_op.push(builtin_op(Amp, vec![I16, I16], I16));
        amp_op.push(builtin_op(Amp, vec![I32, I32], I32));
        amp_op.push(builtin_op(Amp, vec![I64, I64], I64));
        amp_op.push(builtin_op(Amp, vec![Bool, Bool], Bool));

        let pipe_op = self.funcs.entry("|".to_string()).or_default();
        pipe_op.push(builtin_op(Pipe, vec![U8, U8], U8));
        pipe_op.push(builtin_op(Pipe, vec![U16, U16], U16));
        pipe_op.push(builtin_op(Pipe, vec![U32, U32], U32));
        pipe_op.push(builtin_op(Pipe, vec![U64, U64], U64));
        pipe_op.push(builtin_op(Pipe, vec![I8, I8], I8));
        pipe_op.push(builtin_op(Pipe, vec![I16, I16], I16));
        pipe_op.push(builtin_op(Pipe, vec![I32, I32], I32));
        pipe_op.push(builtin_op(Pipe, vec![I64, I64], I64));
        pipe_op.push(builtin_op(Pipe, vec![Bool, Bool], Bool));

        let cap_op = self.funcs.entry("^".to_string()).or_default();
        cap_op.push(builtin_op(Cap, vec![U8, U8], U8));
        cap_op.push(builtin_op(Cap, vec![U16, U16], U16));
        cap_op.push(builtin_op(Cap, vec![U32, U32], U32));
        cap_op.push(builtin_op(Cap, vec![U64, U64], U64));
        cap_op.push(builtin_op(Cap, vec![I8, I8], I8));
        cap_op.push(builtin_op(Cap, vec![I16, I16], I16));
        cap_op.push(builtin_op(Cap, vec![I32, I32], I32));
        cap_op.push(builtin_op(Cap, vec![I64, I64], I64));
        cap_op.push(builtin_op(Cap, vec![Bool, Bool], Bool));

        let amp_amp_op = self.funcs.entry("&&".to_string()).or_default();
        amp_amp_op.push(builtin_op(AmpAmp, vec![Bool, Bool], Bool));

        let pipe_pipe_op = self.funcs.entry("||".to_string()).or_default();
        pipe_pipe_op.push(builtin_op(PipePipe, vec![Bool, Bool], Bool));

        let eq_eq_op = self.funcs.entry("==".to_string()).or_default();
        eq_eq_op.push(builtin_op(EqualEqual, vec![U8, U8], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![U16, U16], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![U32, U32], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![U64, U64], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![I8, I8], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![I16, I16], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![I32, I32], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![I64, I64], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![F32, F32], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![F64, F64], Bool));
        eq_eq_op.push(builtin_op(EqualEqual, vec![Bool, Bool], Bool));

        let bang_eq_op = self.funcs.entry("!=".to_string()).or_default();
        bang_eq_op.push(builtin_op(BangEqual, vec![U8, U8], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![U16, U16], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![U32, U32], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![U64, U64], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![I8, I8], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![I16, I16], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![I32, I32], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![I64, I64], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![F32, F32], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![F64, F64], Bool));
        bang_eq_op.push(builtin_op(BangEqual, vec![Bool, Bool], Bool));

        let lt_op = self.funcs.entry("<".to_string()).or_default();
        lt_op.push(builtin_op(LessThan, vec![U8, U8], Bool));
        lt_op.push(builtin_op(LessThan, vec![U16, U16], Bool));
        lt_op.push(builtin_op(LessThan, vec![U32, U32], Bool));
        lt_op.push(builtin_op(LessThan, vec![U64, U64], Bool));
        lt_op.push(builtin_op(LessThan, vec![I8, I8], Bool));
        lt_op.push(builtin_op(LessThan, vec![I16, I16], Bool));
        lt_op.push(builtin_op(LessThan, vec![I32, I32], Bool));
        lt_op.push(builtin_op(LessThan, vec![I64, I64], Bool));
        lt_op.push(builtin_op(LessThan, vec![F32, F32], Bool));
        lt_op.push(builtin_op(LessThan, vec![F64, F64], Bool));

        let gt_op = self.funcs.entry(">".to_string()).or_default();
        gt_op.push(builtin_op(GreaterThan, vec![U8, U8], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![U16, U16], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![U32, U32], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![U64, U64], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![I8, I8], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![I16, I16], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![I32, I32], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![I64, I64], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![F32, F32], Bool));
        gt_op.push(builtin_op(GreaterThan, vec![F64, F64], Bool));

        let le_op = self.funcs.entry("<=".to_string()).or_default();
        le_op.push(builtin_op(LessThanEqual, vec![U8, U8], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![U16, U16], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![U32, U32], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![U64, U64], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![I8, I8], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![I16, I16], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![I32, I32], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![I64, I64], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![F32, F32], Bool));
        le_op.push(builtin_op(LessThanEqual, vec![F64, F64], Bool));

        let ge_op = self.funcs.entry(">=".to_string()).or_default();
        ge_op.push(builtin_op(GreaterThanEqual, vec![U8, U8], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![U16, U16], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![U32, U32], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![U64, U64], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![I8, I8], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![I16, I16], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![I32, I32], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![I64, I64], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![F32, F32], Bool));
        ge_op.push(builtin_op(GreaterThanEqual, vec![F64, F64], Bool));
    }
}

fn builtin_op(op: TokenKind, args: Vec<Type>, ret: Type) -> Func {
    use ExprKind::*;
    use Type::*;

    let ty = Fn(args, Box::new(ret.clone()));
    let body = Expr {
        kind: BuiltinOp(op),
        ty: ret,
        loc: Loc { start: 0, end: 0 },
    };

    Func {
        body,
        ty,
        loc: Loc { start: 0, end: 0 },
    }
}

impl TokenKind {
    fn is_unary_op(&self) -> bool {
        use TokenKind::*;
        matches!(self, Plus | Minus)
    }

    #[rustfmt::skip]
    fn is_binary_op(&self) -> bool {
        use TokenKind::*;
        matches!(self,
            Plus | Minus | Star | Slash | Percent | Amp | Pipe | Cap | AmpAmp | PipePipe |
            EqualEqual | BangEqual | LessThan | GreaterThan | LessThanEqual | GreaterThanEqual
        )
    }

    fn precedence(&self) -> i32 {
        use TokenKind::*;

        match self {
            PipePipe => 0,
            AmpAmp => 1,
            Pipe => 2,
            Cap => 3,
            Amp => 4,
            EqualEqual | BangEqual => 5,
            LessThan | LessThanEqual | GreaterThan | GreaterThanEqual => 6,
            Minus | Plus => 7,
            Star | Slash | Percent => 8,
            _ => unreachable!("Called TokenKind::precedence(`{self:?}`)"),
        }
    }
}
