use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, Func, Module, Op},
    errors::{FloErr, FloResult},
    tokenizer::{Loc, Token, TokenKind, TokenValue},
    types::Type,
    util::Iota,
};

/// The names visible at a point in the source, mapping each to its variable id.
/// Only the mapping is scoped — a variable's type lives in [`Parser::var_types`],
/// keyed by the id, because ids are unique for the whole parse.
#[derive(Debug, Clone)]
struct Scope {
    vars: HashMap<String, usize>,
}

impl Scope {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }

    fn duplicate(&self) -> Self {
        Self {
            vars: self.vars.clone(),
        }
    }

    fn add_var(&mut self, name: String, id: usize) {
        self.vars.insert(name, id);
    }

    fn get_var(&self, name: &String) -> Option<usize> {
        self.vars.get(name).copied()
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    idx: usize,

    /// Every variable's type, indexed by variable id. Also hands out the ids: a
    /// declaration pushes its type and takes the new index.
    var_types: Vec<Type>,
    type_iota: Iota,

    funcs: HashMap<String, Vec<Func>>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            idx: 0,
            var_types: Vec::new(),
            type_iota: Iota::new(),
            funcs: HashMap::new(),
        }
    }

    pub fn parse(mut self) -> FloResult<Module> {
        use TokenKind::*;

        while let Ok(token) = self.peek() {
            match token.kind {
                Fn | Op => self.parse_func()?,

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

        let (name, name_loc) = match self.peek_kind()? {
            Fn => {
                self.expect(Fn)?;
                let name_token = self.expect_get(Ident)?;
                let name_loc = name_token.loc;
                let TokenValue::String(name) = name_token.value.clone() else {
                    unreachable!()
                };
                (name, name_loc)
            }
            Op => {
                self.expect(Op)?;

                let (_op, op_name, op_loc) = self.parse_operator()?;

                (op_name, op_loc)
            }
            _ => unreachable!(),
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

            // Register arg in scope
            self.fresh_arg(arg_name, arg_type, arg.loc, &mut scope)?;

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

        let body = self.parse_expr(-1, &mut scope)?;

        self.expect(Semicolon)?;

        // It would be good to check for ambiguous overloads
        // here. It would give more consistent errors
        self.funcs
            .entry(name)
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

    fn parse_expr(&mut self, precedence: i32, scope: &mut Scope) -> FloResult<Expr> {
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

                // `=` is the only right-associative operator: recursing at its
                // own precedence (rather than one above) makes `a = b = c` group
                // as `a = (b = c)`.
                let rhs_precedence = if op == Equal {
                    op_precedence
                } else {
                    op_precedence + 1
                };

                let rhs = self.parse_expr(rhs_precedence, scope)?;
                let loc = Loc {
                    start: lhs.loc.start,
                    end: rhs.loc.end,
                };

                lhs = if op == Equal {
                    if !lhs.is_lvalue() {
                        return Err(FloErr::NotAssignable { loc: lhs.loc });
                    }
                    Expr {
                        kind: Assign(Box::new(lhs), Box::new(rhs)),
                        ty: self.fresh_type(),
                        loc,
                    }
                } else {
                    Expr {
                        kind: Call(format!("{}", op.pretty_name()), vec![lhs, rhs], None),
                        ty: self.fresh_type(),
                        loc,
                    }
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

    fn parse_unary(&mut self, scope: &mut Scope) -> FloResult<Expr> {
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
                            ty: self.var_types[var_id].clone(),
                            loc: name_loc,
                        })
                    }
                }
            }

            LParen => {
                let l_paren = self.expect_get(LParen)?;
                let mut expr = self.parse_expr(-1, scope)?;
                let r_paren = self.expect_get(RParen)?;

                // Grouping is purely syntactic, so it gets no node (and no type
                // var) of its own — only the inner expression's span is widened
                // to cover the parentheses, so errors underline what was written.
                expr.loc = Loc {
                    start: l_paren.loc.start,
                    end: r_paren.loc.end,
                };
                Ok(expr)
            }

            LCurly => self.parse_scope(scope),

            If => self.parse_if_expr(scope),

            Return => self.parse_return(scope),

            Let => self.parse_decl(scope),

            _ => Err(FloErr::UnexpectedToken {
                found: token.clone(),
            }),
        }
    }

    fn parse_scope(&mut self, scope: &Scope) -> FloResult<Expr> {
        use TokenKind::*;
        let l_curly = self.expect_get(LCurly)?;

        // We don't want variables created inside to affect outside
        let mut scope = scope.duplicate();

        let mut exprs = Vec::new();
        let mut tail = None;
        while self.peek_kind()? != RCurly {
            tail = Some(self.parse_expr(-1, &mut scope)?);

            if self.expect(Semicolon).is_err() {
                break;
            } else {
                let Some(expr) = tail else { unreachable!() };
                exprs.push(expr);
                tail = None;
            }
        }

        let r_curly = self.expect_get(RCurly)?;

        // Always a fresh var: the type checker decides whether the scope is its
        // tail's type, `void` (no tail), or `noreturn` (a statement/tail diverges).
        let ty = self.fresh_type();
        let kind = ExprKind::Scope(exprs, tail.map(|e| Box::new(e)));
        let loc = Loc {
            start: l_curly.loc.start,
            end: r_curly.loc.end,
        };

        Ok(Expr { kind, ty, loc })
    }

    fn parse_if_expr(&mut self, scope: &Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let if_tok = self.expect_get(If)?;
        let start = if_tok.loc.start;

        // Variables made in the condition should only be visible inside the then
        // branch. Such as using the `is` expr
        let cond_scope = &mut scope.duplicate();
        let cond = Box::new(self.parse_expr(-1, cond_scope)?);

        // Same here
        let then = Box::new(self.parse_expr(-1, cond_scope)?);
        let mut end = then.loc.end;

        // Same here
        let otherwise = if self.expect(Else).is_ok() {
            let else_scope = &mut scope.duplicate();
            let otherwise = self.parse_expr(-1, else_scope)?;
            end = otherwise.loc.end;
            Some(Box::new(otherwise))
        } else {
            None
        };

        let loc = Loc { start, end };
        let kind = ExprKind::If(cond, then, otherwise);
        let ty = self.fresh_type();
        Ok(Expr { kind, ty, loc })
    }

    fn parse_return(&mut self, scope: &mut Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let ret_tok = self.expect_get(Return)?;
        let start = ret_tok.loc.start;

        // The operand is optional: it is absent when the next token cannot start
        // an expression (a terminator, `else`, or EOF). Otherwise `return` grabs
        // the whole remaining expression, so `return a + b` is `return (a + b)`.
        let value = match self.peek_kind() {
            Ok(Semicolon | RCurly | RParen | Comma | Else) | Err(_) => None,
            _ => Some(Box::new(self.parse_expr(-1, scope)?)),
        };

        let end = match &value {
            Some(e) => e.loc.end,
            None => ret_tok.loc.end,
        };

        // A `return` expression is always NoReturn.
        Ok(Expr {
            kind: ExprKind::Return(value),
            ty: Type::Never,
            loc: Loc { start, end },
        })
    }

    /// Parses `let name [: Type] [= init]`. A declaration is an ordinary
    /// expression of type `void`, so it may appear anywhere an expression can —
    /// it just introduces its name into the enclosing scope as a side effect.
    fn parse_decl(&mut self, scope: &mut Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let let_tok = self.expect_get(Let)?;
        let start = let_tok.loc.start;

        let name_token = self.expect_get(Ident)?;
        let TokenValue::String(name) = name_token.value else {
            unreachable!()
        };
        let name_loc = name_token.loc;

        // Without an annotation the variable gets a fresh type var, left for the
        // initializer (or a later assignment) to pin down.
        let ty = if self.expect(Colon).is_ok() {
            self.parse_type()?.0
        } else {
            self.fresh_type()
        };

        // The initializer is parsed against the scope as it stands *before* the
        // new name is added, so `let a = a;` reads the outer `a` and shadowing
        // works.
        let init = if self.expect(Equal).is_ok() {
            Some(Box::new(self.parse_expr(-1, scope)?))
        } else {
            None
        };

        let end = match &init {
            Some(init) => init.loc.end,
            None => name_loc.end,
        };

        let id = self.fresh_var(name, ty.clone(), scope);

        Ok(Expr {
            kind: ExprKind::Let(id, ty, init),
            ty: Type::Void,
            loc: Loc { start, end },
        })
    }

    /// Parses an optional parenthesized, comma-separated argument list.
    /// parsing `::<T>` will go in here later.
    fn parse_call_args(&mut self, scope: &mut Scope) -> FloResult<Option<(Vec<Expr>, usize)>> {
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

    fn fresh_arg(&mut self, name: String, ty: Type, loc: Loc, scope: &mut Scope) -> FloResult<()> {
        if let Some(_) = scope.get_var(&name) {
            Err(FloErr::RedifinitionOfArgument {
                name: name,
                loc: loc,
            })
        } else {
            self.fresh_var(name, ty, scope);
            Ok(())
        }
    }

    /// Registers a new variable and returns its id. Unlike an argument, a `let`
    /// may reuse a name that is already in scope — that is shadowing, and the
    /// old id simply stops being reachable by name.
    fn fresh_var(&mut self, name: String, ty: Type, scope: &mut Scope) -> usize {
        let id = self.var_types.len();
        self.var_types.push(ty);
        scope.add_var(name, id);
        id
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

impl Expr {
    /// Whether this expression denotes a storage location, and so may appear on
    /// the left of an `=`. Pointer derefs, indexing and field access join this
    /// list when they land.
    fn is_lvalue(&self) -> bool {
        matches!(self.kind, ExprKind::Var(_))
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
            Equal |
            Plus | Minus | Star | Slash | Percent | Amp | Pipe | Cap | AmpAmp | PipePipe |
            EqualEqual | BangEqual | LessThan | GreaterThan | LessThanEqual | GreaterThanEqual
        )
    }

    fn precedence(&self) -> i32 {
        use TokenKind::*;

        match self {
            Equal => 0,
            PipePipe => 1,
            AmpAmp => 2,
            Pipe => 3,
            Cap => 4,
            Amp => 5,
            EqualEqual | BangEqual => 6,
            LessThan | LessThanEqual | GreaterThan | GreaterThanEqual => 7,
            Minus | Plus => 8,
            Star | Slash | Percent => 9,
            _ => unreachable!("Called TokenKind::precedence(`{self:?}`)"),
        }
    }
}
