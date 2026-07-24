use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module, Op},
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

    fn duplicate(&self) -> Self {
        Self {
            var_iota: self.var_iota,
            vars: self.vars.clone(),
            var_types: self.var_types.clone(),
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

                TokenKind::Op => self.parse_op_overload()?,

                _ => {
                    return Err(FloErr::UnexpectedToken {
                        found: token.clone(),
                    });
                }
            }
        }

        // Register builtin ops
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

    fn parse_op_overload(&mut self) -> FloResult<()> {
        use TokenKind::*;
        self.expect(Op)?;

        let (_op, op_name, op_loc) = self.parse_operator()?;

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
                    start: op_loc.start,
                    end: r_paren_loc.end,
                },
            )
        };

        let loc = Loc {
            start: op_loc.start,
            end: ret_type_loc.end,
        };

        let ty = self.func_type(arg_types, ret_type);

        self.expect(Equal)?;

        let body = self.parse_expr(-1, &scope)?;

        self.expect(Semicolon)?;

        // It would be good to check for ambiguous overloads
        // here. It would give more consistent errors
        self.funcs
            .entry(op_name)
            .or_default()
            .push(Func { body, ty, loc });

        Ok(())
    }

    fn parse_operator(&mut self) -> FloResult<(Op, String, Loc)> {
        use TokenKind as TK;

        let tok = self.peek()?;
        let result = match tok.kind {
            TK::Plus => Ok((Op::Add, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Minus => Ok((Op::Sub, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Star => Ok((Op::Mul, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Slash => Ok((Op::Div, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Percent => Ok((Op::Mod, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Amp => Ok((Op::BitAnd, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Pipe => Ok((Op::BitOr, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Cap => Ok((Op::BitXor, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::AmpAmp => Ok((Op::And, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::PipePipe => Ok((Op::Or, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::EqualEqual => Ok((Op::Eq, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::BangEqual => Ok((Op::NEq, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::LessThan => Ok((Op::Lt, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::LessThanEqual => Ok((Op::Lte, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::GreaterThan => Ok((Op::Gt, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::GreaterThanEqual => Ok((Op::Gte, tok.kind.pretty_name().to_string(), tok.loc)),
            found => Err(FloErr::ExpectedOp {
                found,
                loc: tok.loc,
            })?,
        };

        self.skip();
        result
    }

    fn parse_expr(&mut self, precedence: i32, scope: &Scope) -> FloResult<Expr> {
        use ExprKind::*;
        use TokenKind::*;
        let mut lhs = self.parse_unary(scope)?;

        loop {
            let tok = self.peek()?;
            let op = tok.kind;

            if op.is_binary_op() {
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
            } else if op == PipeGreaterThan {
                self.skip();

                let func = self.expect_get(Ident)?;
                let TokenValue::String(func_name) = func.value else {
                    unreachable!()
                };
                let start = lhs.loc.start;
                let default_end = func.loc.end;

                let (mut args, end) = match self.parse_call_args(scope)? {
                    Some((args, end)) => (args, end),
                    None => (Vec::new(), default_end),
                };
                args.insert(0, lhs); // lhs becomes the first arg

                lhs = Expr {
                    kind: ExprKind::Call(func_name, args, None),
                    ty: self.fresh_type(),
                    loc: Loc { start, end },
                };
            } else {
                break;
            }
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

                match self.parse_call_args(scope)? {
                    Some((args, end)) => {
                        let kind = ExprKind::Call(name, args, None); // unresolved
                        Ok(Expr {
                            kind,
                            ty: self.fresh_type(),
                            loc: Loc {
                                start: name_loc.start,
                                end,
                            },
                        })
                    }
                    None => {
                        let var_id = scope.get_var(&name).ok_or(FloErr::UndefinedIdentifier {
                            name: name.clone(),
                            loc: name_loc,
                        })?;

                        Ok(Expr {
                            kind: ExprKind::Var(var_id),
                            ty: scope.get_var_type(var_id),
                            loc: name_loc,
                        })
                    }
                }
            }

            LCurly => self.parse_scope(scope),

            _ => Err(FloErr::UnexpectedToken {
                found: token.clone(),
            }),
        }
    }

    fn parse_scope(&mut self, scope: &Scope) -> FloResult<Expr> {
        use TokenKind::*;
        let l_curly = self.expect_get(LCurly)?;

        // We don't want variables created inside to affect outside
        let scope = scope.duplicate();

        let mut exprs = Vec::new();
        let mut tail = None;
        while self.peek_kind()? != RCurly {
            tail = Some(self.parse_expr(-1, &scope)?);

            if self.expect(Semicolon).is_err() {
                break;
            } else {
                let Some(expr) = tail else { unreachable!() };
                exprs.push(expr);
                tail = None;
            }
        }

        let r_curly = self.expect_get(RCurly)?;

        let ty = match &tail {
            Some(_) => self.fresh_type(),
            None => Type::Void,
        };
        let kind = ExprKind::Scope(exprs, tail.map(|e| Box::new(e)));
        let loc = Loc {
            start: l_curly.loc.start,
            end: r_curly.loc.end,
        };

        Ok(Expr { kind, ty, loc })
    }

    /// Parses an optional parenthesized, comma-separated argument list.
    /// parsing `::<T>` will go in here later.
    fn parse_call_args(&mut self, scope: &Scope) -> FloResult<Option<(Vec<Expr>, usize)>> {
        use TokenKind::*;

        if !matches!(self.peek_kind(), Ok(LParen)) {
            return Ok(None);
        }
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
        Ok(Some((args, r_paren.loc.end)))
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
        use Op::*;
        use Type::*;

        let plus_op = self.funcs.entry("+".to_string()).or_default();
        // binary
        plus_op.push(builtin_op(Add, vec![U8, U8], U8));
        plus_op.push(builtin_op(Add, vec![U16, U16], U16));
        plus_op.push(builtin_op(Add, vec![U32, U32], U32));
        plus_op.push(builtin_op(Add, vec![U64, U64], U64));
        plus_op.push(builtin_op(Add, vec![I8, I8], I8));
        plus_op.push(builtin_op(Add, vec![I16, I16], I16));
        plus_op.push(builtin_op(Add, vec![I32, I32], I32));
        plus_op.push(builtin_op(Add, vec![I64, I64], I64));
        plus_op.push(builtin_op(Add, vec![F32, F32], F32));
        plus_op.push(builtin_op(Add, vec![F64, F64], F64));
        // unary
        plus_op.push(builtin_op(Add, vec![U8], U8));
        plus_op.push(builtin_op(Add, vec![U16], U16));
        plus_op.push(builtin_op(Add, vec![U32], U32));
        plus_op.push(builtin_op(Add, vec![U64], U64));
        plus_op.push(builtin_op(Add, vec![I8], I8));
        plus_op.push(builtin_op(Add, vec![I16], I16));
        plus_op.push(builtin_op(Add, vec![I32], I32));
        plus_op.push(builtin_op(Add, vec![I64], I64));
        plus_op.push(builtin_op(Add, vec![F32], F32));
        plus_op.push(builtin_op(Add, vec![F64], F64));

        let minus_op = self.funcs.entry("-".to_string()).or_default();
        // binary
        minus_op.push(builtin_op(Sub, vec![U8, U8], U8));
        minus_op.push(builtin_op(Sub, vec![U16, U16], U16));
        minus_op.push(builtin_op(Sub, vec![U32, U32], U32));
        minus_op.push(builtin_op(Sub, vec![U64, U64], U64));
        minus_op.push(builtin_op(Sub, vec![I8, I8], I8));
        minus_op.push(builtin_op(Sub, vec![I16, I16], I16));
        minus_op.push(builtin_op(Sub, vec![I32, I32], I32));
        minus_op.push(builtin_op(Sub, vec![I64, I64], I64));
        minus_op.push(builtin_op(Sub, vec![F32, F32], F32));
        minus_op.push(builtin_op(Sub, vec![F64, F64], F64));
        // unary
        minus_op.push(builtin_op(Sub, vec![U8], U8));
        minus_op.push(builtin_op(Sub, vec![U16], U16));
        minus_op.push(builtin_op(Sub, vec![U32], U32));
        minus_op.push(builtin_op(Sub, vec![U64], U64));
        minus_op.push(builtin_op(Sub, vec![I8], I8));
        minus_op.push(builtin_op(Sub, vec![I16], I16));
        minus_op.push(builtin_op(Sub, vec![I32], I32));
        minus_op.push(builtin_op(Sub, vec![I64], I64));
        minus_op.push(builtin_op(Sub, vec![F32], F32));
        minus_op.push(builtin_op(Sub, vec![F64], F64));

        let star_op = self.funcs.entry("*".to_string()).or_default();
        star_op.push(builtin_op(Mul, vec![U8, U8], U8));
        star_op.push(builtin_op(Mul, vec![U16, U16], U16));
        star_op.push(builtin_op(Mul, vec![U32, U32], U32));
        star_op.push(builtin_op(Mul, vec![U64, U64], U64));
        star_op.push(builtin_op(Mul, vec![I8, I8], I8));
        star_op.push(builtin_op(Mul, vec![I16, I16], I16));
        star_op.push(builtin_op(Mul, vec![I32, I32], I32));
        star_op.push(builtin_op(Mul, vec![I64, I64], I64));
        star_op.push(builtin_op(Mul, vec![F32, F32], F32));
        star_op.push(builtin_op(Mul, vec![F64, F64], F64));

        let slash_op = self.funcs.entry("/".to_string()).or_default();
        slash_op.push(builtin_op(Div, vec![U8, U8], U8));
        slash_op.push(builtin_op(Div, vec![U16, U16], U16));
        slash_op.push(builtin_op(Div, vec![U32, U32], U32));
        slash_op.push(builtin_op(Div, vec![U64, U64], U64));
        slash_op.push(builtin_op(Div, vec![I8, I8], I8));
        slash_op.push(builtin_op(Div, vec![I16, I16], I16));
        slash_op.push(builtin_op(Div, vec![I32, I32], I32));
        slash_op.push(builtin_op(Div, vec![I64, I64], I64));
        slash_op.push(builtin_op(Div, vec![F32, F32], F32));
        slash_op.push(builtin_op(Div, vec![F64, F64], F64));

        let percent_op = self.funcs.entry("%".to_string()).or_default();
        percent_op.push(builtin_op(Mod, vec![U8, U8], U8));
        percent_op.push(builtin_op(Mod, vec![U16, U16], U16));
        percent_op.push(builtin_op(Mod, vec![U32, U32], U32));
        percent_op.push(builtin_op(Mod, vec![U64, U64], U64));
        percent_op.push(builtin_op(Mod, vec![I8, I8], I8));
        percent_op.push(builtin_op(Mod, vec![I16, I16], I16));
        percent_op.push(builtin_op(Mod, vec![I32, I32], I32));
        percent_op.push(builtin_op(Mod, vec![I64, I64], I64));
        percent_op.push(builtin_op(Mod, vec![F32, F32], F32));
        percent_op.push(builtin_op(Mod, vec![F64, F64], F64));

        let amp_op = self.funcs.entry("&".to_string()).or_default();
        amp_op.push(builtin_op(BitAnd, vec![U8, U8], U8));
        amp_op.push(builtin_op(BitAnd, vec![U16, U16], U16));
        amp_op.push(builtin_op(BitAnd, vec![U32, U32], U32));
        amp_op.push(builtin_op(BitAnd, vec![U64, U64], U64));
        amp_op.push(builtin_op(BitAnd, vec![I8, I8], I8));
        amp_op.push(builtin_op(BitAnd, vec![I16, I16], I16));
        amp_op.push(builtin_op(BitAnd, vec![I32, I32], I32));
        amp_op.push(builtin_op(BitAnd, vec![I64, I64], I64));
        amp_op.push(builtin_op(BitAnd, vec![Bool, Bool], Bool));

        let pipe_op = self.funcs.entry("|".to_string()).or_default();
        pipe_op.push(builtin_op(BitOr, vec![U8, U8], U8));
        pipe_op.push(builtin_op(BitOr, vec![U16, U16], U16));
        pipe_op.push(builtin_op(BitOr, vec![U32, U32], U32));
        pipe_op.push(builtin_op(BitOr, vec![U64, U64], U64));
        pipe_op.push(builtin_op(BitOr, vec![I8, I8], I8));
        pipe_op.push(builtin_op(BitOr, vec![I16, I16], I16));
        pipe_op.push(builtin_op(BitOr, vec![I32, I32], I32));
        pipe_op.push(builtin_op(BitOr, vec![I64, I64], I64));
        pipe_op.push(builtin_op(BitOr, vec![Bool, Bool], Bool));

        let cap_op = self.funcs.entry("^".to_string()).or_default();
        cap_op.push(builtin_op(BitXor, vec![U8, U8], U8));
        cap_op.push(builtin_op(BitXor, vec![U16, U16], U16));
        cap_op.push(builtin_op(BitXor, vec![U32, U32], U32));
        cap_op.push(builtin_op(BitXor, vec![U64, U64], U64));
        cap_op.push(builtin_op(BitXor, vec![I8, I8], I8));
        cap_op.push(builtin_op(BitXor, vec![I16, I16], I16));
        cap_op.push(builtin_op(BitXor, vec![I32, I32], I32));
        cap_op.push(builtin_op(BitXor, vec![I64, I64], I64));
        cap_op.push(builtin_op(BitXor, vec![Bool, Bool], Bool));

        let amp_amp_op = self.funcs.entry("&&".to_string()).or_default();
        amp_amp_op.push(builtin_op(And, vec![Bool, Bool], Bool));

        let pipe_pipe_op = self.funcs.entry("||".to_string()).or_default();
        pipe_pipe_op.push(builtin_op(Or, vec![Bool, Bool], Bool));

        let eq_eq_op = self.funcs.entry("==".to_string()).or_default();
        eq_eq_op.push(builtin_op(Eq, vec![U8, U8], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![U16, U16], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![U32, U32], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![U64, U64], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![I8, I8], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![I16, I16], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![I32, I32], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![I64, I64], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![F32, F32], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![F64, F64], Bool));
        eq_eq_op.push(builtin_op(Eq, vec![Bool, Bool], Bool));

        let bang_eq_op = self.funcs.entry("!=".to_string()).or_default();
        bang_eq_op.push(builtin_op(NEq, vec![U8, U8], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![U16, U16], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![U32, U32], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![U64, U64], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![I8, I8], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![I16, I16], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![I32, I32], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![I64, I64], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![F32, F32], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![F64, F64], Bool));
        bang_eq_op.push(builtin_op(NEq, vec![Bool, Bool], Bool));

        let lt_op = self.funcs.entry("<".to_string()).or_default();
        lt_op.push(builtin_op(Lt, vec![U8, U8], Bool));
        lt_op.push(builtin_op(Lt, vec![U16, U16], Bool));
        lt_op.push(builtin_op(Lt, vec![U32, U32], Bool));
        lt_op.push(builtin_op(Lt, vec![U64, U64], Bool));
        lt_op.push(builtin_op(Lt, vec![I8, I8], Bool));
        lt_op.push(builtin_op(Lt, vec![I16, I16], Bool));
        lt_op.push(builtin_op(Lt, vec![I32, I32], Bool));
        lt_op.push(builtin_op(Lt, vec![I64, I64], Bool));
        lt_op.push(builtin_op(Lt, vec![F32, F32], Bool));
        lt_op.push(builtin_op(Lt, vec![F64, F64], Bool));

        let gt_op = self.funcs.entry(">".to_string()).or_default();
        gt_op.push(builtin_op(Gt, vec![U8, U8], Bool));
        gt_op.push(builtin_op(Gt, vec![U16, U16], Bool));
        gt_op.push(builtin_op(Gt, vec![U32, U32], Bool));
        gt_op.push(builtin_op(Gt, vec![U64, U64], Bool));
        gt_op.push(builtin_op(Gt, vec![I8, I8], Bool));
        gt_op.push(builtin_op(Gt, vec![I16, I16], Bool));
        gt_op.push(builtin_op(Gt, vec![I32, I32], Bool));
        gt_op.push(builtin_op(Gt, vec![I64, I64], Bool));
        gt_op.push(builtin_op(Gt, vec![F32, F32], Bool));
        gt_op.push(builtin_op(Gt, vec![F64, F64], Bool));

        let le_op = self.funcs.entry("<=".to_string()).or_default();
        le_op.push(builtin_op(Lte, vec![U8, U8], Bool));
        le_op.push(builtin_op(Lte, vec![U16, U16], Bool));
        le_op.push(builtin_op(Lte, vec![U32, U32], Bool));
        le_op.push(builtin_op(Lte, vec![U64, U64], Bool));
        le_op.push(builtin_op(Lte, vec![I8, I8], Bool));
        le_op.push(builtin_op(Lte, vec![I16, I16], Bool));
        le_op.push(builtin_op(Lte, vec![I32, I32], Bool));
        le_op.push(builtin_op(Lte, vec![I64, I64], Bool));
        le_op.push(builtin_op(Lte, vec![F32, F32], Bool));
        le_op.push(builtin_op(Lte, vec![F64, F64], Bool));

        let ge_op = self.funcs.entry(">=".to_string()).or_default();
        ge_op.push(builtin_op(Gte, vec![U8, U8], Bool));
        ge_op.push(builtin_op(Gte, vec![U16, U16], Bool));
        ge_op.push(builtin_op(Gte, vec![U32, U32], Bool));
        ge_op.push(builtin_op(Gte, vec![U64, U64], Bool));
        ge_op.push(builtin_op(Gte, vec![I8, I8], Bool));
        ge_op.push(builtin_op(Gte, vec![I16, I16], Bool));
        ge_op.push(builtin_op(Gte, vec![I32, I32], Bool));
        ge_op.push(builtin_op(Gte, vec![I64, I64], Bool));
        ge_op.push(builtin_op(Gte, vec![F32, F32], Bool));
        ge_op.push(builtin_op(Gte, vec![F64, F64], Bool));
    }
}

fn builtin_op(op: Op, args: Vec<Type>, ret: Type) -> Func {
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
