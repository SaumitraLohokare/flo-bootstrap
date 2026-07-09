use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module},
    errors::{FloErr, FloResult},
    tokenizer::{Token, TokenKind, TokenValue},
    types::Type,
    util::Iota,
};

pub struct Parser {
    tokens: Vec<Token>,
    idx: usize,

    type_iota: Iota,

    funcs: HashMap<String, Func>,
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

        if !self.funcs.contains_key("main") {
            Err(FloErr::MainFunctionNotFound)
        } else {
            Ok(Module { funcs: self.funcs })
        }
    }

    fn parse_func(&mut self) -> FloResult<()> {
        use TokenKind::*;
        self.expect(Fn)?;
        let name_token = self.expect_get(Ident)?;
        let TokenValue::String(name) = name_token.value.clone() else {
            unreachable!()
        };

        self.expect(LParen)?;
        self.expect(RParen)?;

        let ret_type = if self.expect(Arrow).is_ok() {
            self.parse_type()?
        } else {
            Type::Void
        };

        let ty = self.func_type(ret_type);

        self.expect(Equal)?;

        let body = self.parse_expr(-1)?;

        self.expect(Semicolon)?;

        if self.funcs.contains_key(&name) {
            return Err(FloErr::RedifinitionOfFunction { token: name_token });
        } else {
            self.funcs.insert(
                name,
                Func {
                    body,
                    ty,
                    loc: name_token.loc,
                },
            );
        }

        Ok(())
    }

    fn parse_expr(&mut self, _precedence: i32) -> FloResult<Expr> {
        let lhs = self.parse_atom()?;

        // TODO

        Ok(lhs)
    }

    fn parse_atom(&mut self) -> FloResult<Expr> {
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

            _ => Err(FloErr::UnexpectedToken {
                found: token.clone(),
            }),
        }
    }

    fn parse_type(&mut self) -> FloResult<Type> {
        use TokenKind::*;
        let token = self.peek()?;

        match token.kind {
            Ident => {
                let token = self.expect_get(Ident)?;
                let TokenValue::String(value) = &token.value else {
                    unreachable!()
                };

                match value.as_str() {
                    "i32" => Ok(Type::I32),
                    "void" => Ok(Type::Void),

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

    fn func_type(&self, ret_type: Type) -> Type {
        Type::Fn(Vec::new(), Box::new(ret_type))
    }
}
