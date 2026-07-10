use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, FuncLocs, Module},
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
        let name_loc = name_token.loc;
        let TokenValue::String(name) = name_token.value.clone() else {
            unreachable!()
        };

        let mut scope = Scope::new();

        self.expect(LParen)?;

        let mut arg_types = Vec::new();
        let mut arg_locs = Vec::new();
        while self.peek()?.kind == Ident {
            let arg = self.expect_get(Ident)?;
            let TokenValue::String(arg_name) = arg.value else {
                unreachable!()
            };
            self.expect(Colon)?;
            let (arg_type, arg_type_loc) = self.parse_type()?;
            arg_types.push(arg_type.clone());
            arg_locs.push(arg_type_loc);

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

        let func_definition_loc = Loc {
            start: name_loc.start,
            end: ret_type_loc.end,
        };

        let loc = FuncLocs {
            definition: func_definition_loc,
            arg_types: arg_locs,
            ret_type: ret_type_loc,
        };

        let ty = self.func_type(arg_types, ret_type);

        self.expect(Equal)?;

        let body = self.parse_expr(-1, &scope)?;

        self.expect(Semicolon)?;

        if self.funcs.contains_key(&name) {
            return Err(FloErr::RedifinitionOfFunction {
                name,
                loc: loc.definition,
            });
        } else {
            self.funcs.insert(name, Func { body, ty, loc });
        }

        Ok(())
    }

    fn parse_expr(&mut self, _precedence: i32, scope: &Scope) -> FloResult<Expr> {
        let lhs = self.parse_atom(scope)?;

        // TODO

        Ok(lhs)
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

            Ident => {
                let TokenValue::String(name) = &token.value else {
                    unreachable!()
                };
                let var_id = scope.get_var(name).ok_or(FloErr::UndefinedIdentifier {
                    name: name.clone(),
                    loc: token.loc,
                })?;
                let loc = token.loc;

                self.skip();

                let kind = ExprKind::Var(var_id);
                Ok(Expr {
                    kind,
                    ty: scope.get_var_type(var_id),
                    loc,
                })
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
                    "i32" => Ok((Type::I32, loc)),
                    "void" => Ok((Type::Void, loc)),

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

    fn func_type(&self, arg_types: Vec<Type>, ret_type: Type) -> Type {
        Type::Fn(arg_types, Box::new(ret_type))
    }
}
