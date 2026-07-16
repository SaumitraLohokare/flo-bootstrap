use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, FuncLocs, Module},
    errors::{FloErr, FloResult},
    tokenizer::{Loc, Token, TokenKind, TokenValue},
    types::Type,
    util::Iota,
};

#[derive(Debug, Clone)]
struct Scope {
    vars: HashMap<String, usize>,
    var_types: HashMap<usize, Type>,

    type_vars: HashMap<String, Type>,
}

impl Scope {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
            var_types: HashMap::new(),
            type_vars: HashMap::new(),
        }
    }

    fn add_var(&mut self, name: String, id: usize, ty: Type) {
        self.vars.insert(name, id);
        self.var_types.insert(id, ty);
    }

    fn get_var(&self, name: &String) -> Option<usize> {
        self.vars.get(name).copied()
    }

    fn get_var_type(&self, id: usize) -> Type {
        self.var_types[&id].clone()
    }

    fn add_type_var(&mut self, name: String, ty: Type) {
        self.type_vars.insert(name, ty);
    }

    fn get_type_var(&self, name: &String) -> Option<Type> {
        self.type_vars.get(name).cloned()
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    idx: usize,

    var_iota: Iota,
    type_iota: Iota,

    funcs: HashMap<String, Vec<Func>>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            idx: 0,
            var_iota: Iota::new(),
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

        match self.funcs.get("main") {
            None => Err(FloErr::MainFunctionNotFound),
            // `main` is the single specialize root and stays unmangled, so it can
            // never be overloaded.
            Some(mains) if mains.len() > 1 => Err(FloErr::MultipleMainDefinitions {
                locs: mains.iter().map(|f| f.loc.definition).collect(),
            }),
            Some(_) => Ok(Module { funcs: self.funcs }),
        }
    }

    fn parse_func(&mut self) -> FloResult<()> {
        use TokenKind::*;
        self.var_iota.reset();

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
            // Parse arg name
            let arg = self.expect_get(Ident)?;
            let TokenValue::String(arg_name) = arg.value else {
                unreachable!()
            };

            // Parse arg type
            let (arg_type, arg_type_loc) = if self.expect(Colon).is_ok() {
                self.parse_type(&mut scope)?
            } else {
                (self.fresh_type(), arg.loc)
            };

            // Store arg types and loc (Do we need these?)
            arg_types.push(arg_type.clone());
            arg_locs.push(arg_type_loc);

            // Register arg in scope
            self.fresh_arg(arg_name, arg_type, arg.loc, &mut scope)?;

            if self.expect(Comma).is_err() {
                break;
            }
        }

        let r_paren_token = self.expect_get(RParen)?;
        let r_paren_loc = r_paren_token.loc;

        let (ret_type, ret_type_loc) = if self.expect(Arrow).is_ok() {
            self.parse_type(&mut scope)?
        } else {
            (
                self.fresh_type(),
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

        let body = self.parse_expr(-1, &mut scope)?;

        self.expect(Semicolon)?;

        // Overloads are permitted: every definition is kept. An overload set that
        // can't be narrowed to a single candidate at a call site becomes an
        // ambiguity error during type checking, not a redefinition error here.
        self.funcs
            .entry(name)
            .or_default()
            .push(Func { body, ty, loc });

        Ok(())
    }

    /// Precedence-climbing parser. `min_prec` is the lowest binary-operator
    /// precedence this call is allowed to consume; callers starting a fresh
    /// expression (function body, call argument) pass `-1` so every operator is in
    /// range. Operators desugar straight into `Call` nodes named by their symbol
    /// (`+`, `-`, …) — names no user identifier can collide with — so the type
    /// checker resolves them exactly like any other overloaded call.
    fn parse_expr(&mut self, min_prec: i32, scope: &mut Scope) -> FloResult<Expr> {
        let mut lhs = self.parse_unary(scope)?;

        while let Some((op, prec)) = self.peek_binop() {
            if prec < min_prec {
                break;
            }
            self.skip(); // consume the operator token

            // Left-associative: the right operand only binds operators strictly
            // tighter than this one, so `a - b - c` parses as `(a - b) - c`.
            let rhs = self.parse_expr(prec + 1, scope)?;

            let loc = Loc {
                start: lhs.loc.start,
                end: rhs.loc.end,
            };
            lhs = Expr {
                kind: ExprKind::Call(op.to_string(), vec![lhs, rhs]),
                ty: self.fresh_type(),
                loc,
            };
        }

        Ok(lhs)
    }

    /// Parse a unary-prefix expression. Unary `-` binds tighter than any binary
    /// operator and desugars to a one-argument `Call` on `-` (told apart from
    /// binary `-` by arity during overload resolution).
    fn parse_unary(&mut self, scope: &mut Scope) -> FloResult<Expr> {
        use TokenKind::*;

        if self.peek()?.kind == Minus {
            let minus = self.expect_get(Minus)?;
            let operand = self.parse_unary(scope)?;
            let loc = Loc {
                start: minus.loc.start,
                end: operand.loc.end,
            };
            return Ok(Expr {
                kind: ExprKind::Call("-".to_string(), vec![operand]),
                ty: self.fresh_type(),
                loc,
            });
        }

        self.parse_atom(scope)
    }

    /// If the next token is a binary operator, its symbol name and precedence.
    /// `* / %` bind tighter than `+ -`.
    fn peek_binop(&self) -> Option<(&'static str, i32)> {
        use TokenKind::*;
        match self.peek_kind().ok()? {
            Plus => Some(("+", 1)),
            Minus => Some(("-", 1)),
            Star => Some(("*", 2)),
            Slash => Some(("/", 2)),
            Percent => Some(("%", 2)),
            _ => None,
        }
    }

    fn parse_atom(&mut self, scope: &mut Scope) -> FloResult<Expr> {
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

                    let kind = ExprKind::Call(name, args);
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

    fn parse_type(&mut self, scope: &mut Scope) -> FloResult<(Type, Loc)> {
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
                    "u8" => Ok((Type::U8, loc)),
                    "void" => Ok((Type::Void, loc)),

                    _ => Err(FloErr::NotAType { token }),
                }
            }

            SingleQuote => {
                let mut loc = token.loc;
                self.skip();
                let ident = self.expect_get(Ident)?;
                let TokenValue::String(type_name) = ident.value else {
                    unreachable!()
                };
                loc.end = ident.loc.end;

                let ty = match scope.get_type_var(&type_name) {
                    Some(ty) => ty,
                    None => {
                        let ty = self.fresh_type();
                        scope.add_type_var(type_name, ty.clone());
                        ty
                    }
                };

                Ok((ty, loc))
            }

            _ => Err(FloErr::NotAType {
                token: token.clone(),
            }),
        }
    }

    fn fresh_arg(&mut self, name: String, ty: Type, loc: Loc, scope: &mut Scope) -> FloResult<()> {
        if let Some(_) = scope.get_var(&name) {
            Err(FloErr::RedifinitionOfArgument {
                name: name,
                loc: loc,
            })
        } else {
            let id = self.var_iota.next();
            scope.add_var(name, id, ty);
            Ok(())
        }
    }

    fn fresh_type(&mut self) -> Type {
        Type::T(self.type_iota.next())
    }

    fn func_type(&self, arg_types: Vec<Type>, ret_type: Type) -> Type {
        Type::Fn(arg_types, Box::new(ret_type))
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
}
