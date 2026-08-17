use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, FieldInit, Func, Module, Op, Statement, StmtKind, TypeQuery},
    errors::{FloErr, FloResult},
    tokenizer::{Loc, Token, TokenKind, TokenValue},
    types::{
        CaseDecl, DeclKind, FieldDecl, FieldName, RecordDecl, SumCase, Type, TypeDecl, TypeTable,
        sorted_cases,
    },
    util::Iota,
};

/// The names visible at a point in the source, mapping each to its variable id.
/// Only the mapping is scoped — a variable's type lives in [`Parser::var_types`],
/// keyed by the id, because ids are unique for the whole parse.
///
/// Variables are the only thing a scope holds. A case name is only ever written
/// after a `.`, where nothing else it could be, so there is nothing to bring
/// into scope to make one writable.
#[derive(Debug, Clone)]
struct Scope {
    vars: HashMap<String, usize>,
}

impl Scope {
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

/// What one alternative of a type position turned out to be.
///
/// A `{ .. }` is a record and a `Name { .. }` is a sum case, but a bare `Name`
/// is either a mention of a declared type or a payload-less case of an anonymous
/// sum — and which it is only becomes clear when the next token either is or is
/// not a `|`. So the decision is deferred to [`Parser::parse_type`] rather than
/// guessed at here.
enum TypeAtom {
    Ty(Type),
    Case(SumCase),
    Name(String),
}

pub struct Parser {
    tokens: Vec<Token>,
    idx: usize,

    /// Every variable's type, indexed by variable id. Also hands out the ids: a
    /// declaration pushes its type and takes the new index.
    var_types: Vec<Type>,
    type_iota: Iota,

    /// How many `while` bodies enclose the expression being parsed. `break` and
    /// `continue` are only legal when this is non-zero.
    loop_depth: usize,

    /// The type parameters of the function being parsed, mapped to the type
    /// variable id standing for each. Cleared between functions, so a `T` in one
    /// signature has nothing to do with a `T` in the next.
    type_params: HashMap<String, usize>,

    funcs: HashMap<String, Vec<Func>>,
    types: TypeTable,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            idx: 0,
            var_types: Vec::new(),
            type_iota: Iota::new(),
            loop_depth: 0,
            type_params: HashMap::new(),
            funcs: HashMap::new(),
            types: TypeTable::new(),
        }
    }

    pub fn parse(mut self) -> FloResult<Module> {
        use TokenKind::*;

        while let Ok(token) = self.peek() {
            match token.kind {
                Fn | Op => self.parse_func()?,
                TypeKw => self.parse_type_decl()?,

                _ => {
                    return Err(FloErr::UnexpectedToken {
                        found: token.clone(),
                    });
                }
            }
        }

        // Register builtin ops
        self.register_builtin_ops();

        Ok(Module {
            funcs: self.funcs,
            types: self.types,
            var_count: self.var_types.len(),
            type_var_count: self.type_iota.count(),
        })
    }

    /// The scope every function body starts from. Nothing is visible at file
    /// scope that a body has to be told about: functions and types are looked up
    /// by name after parsing, and there are no file-level variables yet.
    fn root_scope(&self) -> Scope {
        Scope {
            vars: HashMap::new(),
        }
    }

    /// Parses `type Name<T> = { field: T };` or
    /// `type Name<T> = Case { T } | Other;`
    ///
    /// What follows the `=` decides which kind of type this is: a `{` makes it a
    /// record, a case name makes it a sum. There is no shorthand between the
    /// two — a one-case sum and a record of that case's fields are different
    /// types, and only the record supports field access.
    ///
    /// Nothing here checks that the field types name real types, or that the
    /// type is not recursive: a declaration may mention one written further down
    /// the file, so both have to wait until every declaration is in (see
    /// [`crate::type_checker::check_type_decls`]).
    fn parse_type_decl(&mut self) -> FloResult<()> {
        use TokenKind::*;

        self.expect(TypeKw)?;

        let name_token = self.expect_get(Ident)?;
        let TokenValue::String(name) = name_token.value.clone() else {
            unreachable!()
        };

        // As for a function, the parameters are registered before the body is
        // read, so a `T` in a field type resolves to the variable standing for it.
        let type_params = self.parse_type_params()?;

        self.expect(Equal)?;

        let kind = if self.peek_kind()? == LCurly {
            DeclKind::Record(self.parse_record_decl()?)
        } else {
            let mut cases: Vec<CaseDecl> = Vec::new();
            loop {
                let case = self.parse_case_decl()?;

                if let Some(prev) = cases.iter().find(|c| c.name == case.name) {
                    return Err(FloErr::DuplicateCase {
                        type_name: name,
                        case: case.name,
                        loc: case.loc,
                        prev_loc: prev.loc,
                    });
                }
                cases.push(case);

                if self.expect(Pipe).is_err() {
                    break;
                }
            }
            DeclKind::Sum(cases)
        };

        let semicolon = self.expect_get(Semicolon)?;
        let loc = Loc {
            start: name_token.loc.start,
            end: semicolon.loc.end,
        };

        if let Some(prev) = self.types.get(&name) {
            return Err(FloErr::DuplicateType {
                name,
                loc,
                prev_loc: prev.loc,
            });
        }

        self.types.insert(
            name.clone(),
            TypeDecl {
                name,
                type_params,
                kind,
                loc,
            },
        );

        Ok(())
    }

    /// Parses a record body: `{ x: i32, y: i32 }`, `{ i32, bool }` or `{}`.
    ///
    /// A field is named when an identifier is followed by a `:`; anything else is
    /// a positional field, whose name is the index it was written at. The two
    /// cannot be mixed — a record is addressed by name or by position, not both —
    /// which is checked here rather than later, because here is where the spans
    /// to point at are.
    fn parse_record_decl(&mut self) -> FloResult<RecordDecl> {
        use TokenKind::*;

        let l_curly = self.expect_get(LCurly)?;

        let mut fields: Vec<FieldDecl> = Vec::new();
        let mut positional = 0usize;

        while self.peek_kind()? != RCurly {
            let field_start = self.peek()?.loc;

            let (name, name_loc) = if self.peek_kind()? == Ident
                && self.peek_kind_n(1) == Some(Colon)
            {
                let tok = self.expect_get(Ident)?;
                let TokenValue::String(written) = tok.value else {
                    unreachable!()
                };
                self.expect(Colon)?;

                // `_0`, `_1`, ... are the names of positional fields, so they
                // are not available as written ones.
                let name = FieldName::parse(&written);
                if name.is_positional() {
                    return Err(FloErr::ReservedFieldName {
                        field: written,
                        loc: tok.loc,
                    });
                }
                (name, tok.loc)
            } else {
                let name = FieldName::Pos(positional);
                positional += 1;
                (name, field_start)
            };

            let (ty, ty_loc) = self.parse_type()?;

            if let Some(prev) = fields.iter().find(|f| f.name == name) {
                return Err(FloErr::DuplicateField {
                    field: format!("{name:?}"),
                    loc: name_loc,
                    prev_loc: prev.loc,
                });
            }

            fields.push(FieldDecl {
                name,
                ty,
                loc: Loc {
                    start: name_loc.start,
                    end: ty_loc.end,
                },
            });

            if self.expect(Comma).is_err() {
                break;
            }
        }

        let r_curly = self.expect_get(RCurly)?;
        let loc = Loc {
            start: l_curly.loc.start,
            end: r_curly.loc.end,
        };

        if positional != 0 && positional != fields.len() {
            return Err(FloErr::MixedFieldKinds { loc });
        }

        Ok(RecordDecl { fields, loc })
    }

    /// One case of a sum: `Name { fields }` or a bare `Name`.
    fn parse_case_decl(&mut self) -> FloResult<CaseDecl> {
        use TokenKind::*;

        let start_token = self.peek()?.clone();
        if start_token.kind != Ident {
            return Err(FloErr::ExpectedCase {
                found: start_token,
            });
        }

        let tok = self.expect_get(Ident)?;
        let TokenValue::String(name) = tok.value else {
            unreachable!()
        };

        let payload = if matches!(self.peek_kind(), Ok(LCurly)) {
            Some(self.parse_record_decl()?)
        } else {
            None
        };

        let end = match &payload {
            Some(record) => record.loc.end,
            None => tok.loc.end,
        };

        Ok(CaseDecl {
            name,
            payload,
            loc: Loc {
                start: start_token.loc.start,
                end,
            },
        })
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

        // Type parameters are registered before anything else in the signature
        // is read, so that they are already in scope for the argument types and
        // the return type.
        let type_params = self.parse_type_params()?;

        let mut scope = self.root_scope();

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

        self.funcs.entry(name).or_default().push(Func {
            body,
            ty,
            loc,
            type_params,
        });

        Ok(())
    }

    /// Parses an optional `<T, U>` after a function's name, registering each
    /// parameter so that [`Parser::parse_type`] will resolve it, and returning
    /// the type variable ids standing for them.
    ///
    /// The set is cleared first: parameters belong to one signature only, so a
    /// `T` here is unrelated to the `T` of the previous function.
    fn parse_type_params(&mut self) -> FloResult<Vec<(String, usize)>> {
        use TokenKind::*;

        self.type_params.clear();

        if self.expect(LessThan).is_err() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::new();
        while self.peek_kind()? == Ident {
            let tok = self.expect_get(Ident)?;
            let TokenValue::String(name) = tok.value.clone() else {
                unreachable!()
            };

            let Type::T(id) = self.fresh_type() else {
                unreachable!()
            };

            if self.type_params.insert(name.clone(), id).is_some() {
                return Err(FloErr::RedifinitionOfTypeParam { name, loc: tok.loc });
            }
            ids.push((name, id));

            if self.expect(Comma).is_err() {
                break;
            }
        }

        self.expect(GreaterThan)?;

        if ids.is_empty() {
            return Err(FloErr::EmptyTypeParamList {
                loc: self.tokens[self.idx - 1].loc,
            });
        }

        Ok(ids)
    }

    fn parse_operator(&mut self) -> FloResult<(Op, String, Loc)> {
        use TokenKind as TK;

        // Before the single-token operators: a shift is two of them (see
        // `peek_shift`), so `op <<(..)` would otherwise read as `op <` followed
        // by a stray `<`.
        if let Some((op, name, loc)) = self.peek_shift() {
            self.skip();
            self.skip();
            return Ok((op, name.to_string(), loc));
        }

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
            TK::Tilde => Ok((Op::BitNot, tok.kind.pretty_name().to_string(), tok.loc)),
            TK::Bang => Ok((Op::Not, tok.kind.pretty_name().to_string(), tok.loc)),
            // `&&` and `||` short-circuit, so they are not calls and there is
            // nothing to overload. See `ExprKind::Logical`.
            TK::AmpAmp | TK::PipePipe => Err(FloErr::OpNotOverloadable {
                op: tok.kind,
                loc: tok.loc,
            })?,
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

            // Checked before the single-token operators: `<` and `>` are binary
            // operators in their own right, so a shift has to be recognised
            // first or it would parse as a comparison against nothing.
            if let Some((_, name, _)) = self.peek_shift() {
                if SHIFT_PRECEDENCE < precedence {
                    break;
                }

                self.skip();
                self.skip();

                let rhs = self.parse_expr(SHIFT_PRECEDENCE + 1, scope)?;
                let loc = Loc {
                    start: lhs.loc.start,
                    end: rhs.loc.end,
                };

                lhs = Expr {
                    kind: Call(name.to_string(), vec![lhs, rhs], None),
                    ty: self.fresh_type(),
                    loc,
                };
                continue;
            }

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
                } else if op == AmpAmp || op == PipePipe {
                    // Not a call, unlike every other operator: these short-circuit,
                    // so they cannot be overloaded and their type is fixed rather
                    // than inferred (see `ExprKind::Logical`).
                    // Spelled out because `use TokenKind::*` above shadows `Op`
                    // with the `op` keyword's token kind.
                    let logical_op = if op == AmpAmp {
                        crate::ast::Op::And
                    } else {
                        crate::ast::Op::Or
                    };
                    Expr {
                        kind: Logical(logical_op, Box::new(lhs), Box::new(rhs)),
                        ty: Type::Bool,
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
                let mut end = func.loc.end;

                let mut args = match self.parse_call_args(scope)? {
                    Some((args, args_end)) => {
                        end = args_end;
                        args
                    }
                    None => Vec::new(),
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
        if op == TokenKind::At {
            return self.parse_builtin(scope);
        }
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
            self.parse_postfix(scope)
        }
    }

    /// Parses one of the `@` builtins: `@cast(T) expr`, `@sizeof(T)` or
    /// `@alignof(T)`.
    ///
    /// None of them is a call, and none can be: each takes a *type* where a call
    /// takes values. Which also means there is nothing to overload and nothing
    /// to infer — a builtin's type is the one written into it.
    ///
    /// Parsed at unary level, so a cast binds tighter than every binary operator
    /// the way C's does: `@cast(u8) a + b` is `(@cast(u8) a) + b`.
    fn parse_builtin(&mut self, scope: &mut Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let at = self.expect_get(At)?;
        let name_token = self.expect_get(Ident)?;
        let TokenValue::String(name) = name_token.value.clone() else {
            unreachable!()
        };
        let name_loc = Loc {
            start: at.loc.start,
            end: name_token.loc.end,
        };

        let query = match name.as_str() {
            "cast" => None,
            "sizeof" => Some(TypeQuery::Size),
            "alignof" => Some(TypeQuery::Align),
            _ => {
                return Err(FloErr::UnknownBuiltin {
                    name,
                    loc: name_loc,
                });
            }
        };

        self.expect(LParen)?;
        let (ty, ty_loc) = self.parse_type()?;
        let r_paren = self.expect_get(RParen)?;

        // All three work on the bits of a value of the type, and `void` has
        // none. Caught here because the type is written down; a cast's *operand*
        // can also turn out to be void, which only the checker can see.
        if matches!(ty, Type::Void) {
            return Err(FloErr::TypeHasNoSize { ty, loc: ty_loc });
        }

        if let Some(query) = query {
            return Ok(Expr {
                kind: ExprKind::TypeInfo(query, ty),
                // Fixed by the spec, not inferred: a size is a u64.
                ty: Type::U64,
                loc: Loc {
                    start: at.loc.start,
                    end: r_paren.loc.end,
                },
            });
        }

        let value = self.parse_unary(scope)?;
        let loc = Loc {
            start: at.loc.start,
            end: value.loc.end,
        };

        Ok(Expr {
            kind: ExprKind::Cast(ty.clone(), Box::new(value)),
            // A cast yields exactly the type it names, so its own type is that
            // type — there is no fresh variable for the checker to solve.
            ty,
            loc,
        })
    }

    /// Parses an atom and any `.field` chained onto it. Field access binds
    /// tighter than every operator, unary ones included, so `-a.b` is `-(a.b)`
    /// and `&a.b` will be `&(a.b)`.
    fn parse_postfix(&mut self, scope: &mut Scope) -> FloResult<Expr> {
        let expr = self.parse_atom(scope)?;

        // A block-shaped expression is not a receiver. An expression may *begin*
        // with `.` (every literal does), so a `.` after a `}` would otherwise
        // swallow the statement that follows: `if c { } .Alive` would read as one
        // field access instead of an `if` and a literal. Stopping here leaves the
        // `.` for the caller, and the missing `;` gets reported as exactly that.
        // `({ .. }).x` still works — the parenthesized form does its own chain.
        if expr.is_block_like() {
            return Ok(expr);
        }

        self.parse_field_chain(expr)
    }

    /// The `.field` chain hanging off an already-parsed receiver.
    fn parse_field_chain(&mut self, mut expr: Expr) -> FloResult<Expr> {
        use TokenKind::*;

        while matches!(self.peek_kind(), Ok(Dot)) {
            self.skip();

            let name_token = self.expect_get(Ident)?;
            let TokenValue::String(field) = name_token.value else {
                unreachable!()
            };

            let loc = Loc {
                start: expr.loc.start,
                end: name_token.loc.end,
            };
            expr = Expr {
                kind: ExprKind::Field(Box::new(expr), FieldName::parse(&field)),
                ty: self.fresh_type(),
                loc,
            };
        }

        Ok(expr)
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

                // A `.` after a name is either field access on a variable of that
                // name, or the qualifier of a literal — `Vec2.{ .. }`,
                // `Option.Some`. The variable wins, as it does everywhere: a name
                // in scope is that variable, whatever else it might also be. That
                // this is a type name is not checked here at all; it cannot be,
                // since the declaration may be further down the file.
                if matches!(self.peek_kind(), Ok(Dot)) && scope.get_var(&name).is_none() {
                    self.expect(Dot)?;
                    return self.parse_lit_after_dot(Some(name), name_loc.start, scope);
                }

                // `::` is the module separator and nothing else — a type's case is
                // reached with `.`. Caught here so that old `Type::Case` and
                // `f::<T>()` spellings say what is wrong rather than reporting the
                // name as unknown.
                if matches!(self.peek_kind(), Ok(ColonColon)) {
                    return Err(FloErr::NotImplemented {
                        what: "module paths (`::`)",
                        loc: self.peek()?.loc,
                    });
                }

                // Parentheses make it a call. Without them there are no type
                // arguments to give it away, so a bare name is a variable or
                // nothing at all.
                match self.parse_call_args(scope)? {
                    Some((args, end)) => Ok(Expr {
                        kind: ExprKind::Call(name, args, None), // unresolved
                        ty: self.fresh_type(),
                        loc: Loc {
                            start: name_loc.start,
                            end,
                        },
                    }),
                    None => match scope.get_var(&name) {
                        Some(var_id) => Ok(Expr {
                            kind: ExprKind::Var(var_id),
                            ty: self.var_types[var_id].clone(),
                            loc: name_loc,
                        }),
                        // There is nothing else a bare name could denote: a case
                        // is only ever written after a `.`, and a type name only
                        // as a qualifier, which the `.` above would have caught.
                        None => Err(FloErr::UnknownIdentifier {
                            name,
                            loc: name_loc,
                        }),
                    },
                }
            }

            // A leading `.` starts a literal, and only ever a literal — that is
            // what makes one unmistakable wherever it is written.
            Dot => {
                let dot = self.expect_get(Dot)?;
                self.parse_lit_after_dot(None, dot.loc.start, scope)
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

                // Parenthesizing is how a block-shaped expression becomes a
                // receiver: `({ .. }).x`. `parse_postfix` will not chain onto one
                // (see there), so the chain is taken here, where the parentheses
                // have already made the receiver unambiguous.
                self.parse_field_chain(expr)
            }

            LCurly => self.parse_scope(scope),

            If => self.parse_if_expr(scope),

            While => self.parse_while_expr(scope),

            Break | Continue => self.parse_loop_jump(),

            Return => self.parse_return(scope),

            // Reserved, so that a program using it as a name breaks now rather
            // than when the expression lands.
            Match => Err(FloErr::NotImplemented {
                what: "match",
                loc: token.loc,
            }),

            // Not an expression, and only legal as a statement directly inside a
            // scope, where `parse_scope` handles it. Reaching one here means it
            // was written as an operand — `1 + (let a = 2)` and the like.
            Let => Err(FloErr::LetOutsideStatementPosition { loc: token.loc }),

            _ => Err(FloErr::UnexpectedToken {
                found: token.clone(),
            }),
        }
    }

    /// Parses a literal, with the cursor just past the `.` that starts it:
    /// `.{ .. }`, `.Case`, or `.Case .{ .. }`. `qualifier` is the type name
    /// written before the dot, if there was one, and `start` is where the whole
    /// literal begins.
    fn parse_lit_after_dot(
        &mut self,
        qualifier: Option<String>,
        start: usize,
        scope: &mut Scope,
    ) -> FloResult<Expr> {
        use TokenKind::*;

        match self.peek_kind()? {
            LCurly => {
                let (fields, end) = self.parse_record_lit(scope)?;
                Ok(Expr {
                    kind: ExprKind::RecordLit(qualifier, fields),
                    ty: self.fresh_type(),
                    loc: Loc { start, end },
                })
            }

            Ident => {
                let case_token = self.expect_get(Ident)?;
                let TokenValue::String(case) = case_token.value else {
                    unreachable!()
                };
                let mut end = case_token.loc.end;

                // Only a `{` after the next `.` makes it a payload. A name there
                // is field access on this literal instead, which the caller's
                // field chain picks up — and which the checker then rejects,
                // because a sum has no fields to reach.
                let payload = if matches!(self.peek_kind(), Ok(Dot))
                    && self.peek_kind_n(1) == Some(LCurly)
                {
                    self.skip();
                    let payload_start = self.peek()?.loc.start;
                    let (fields, payload_end) = self.parse_record_lit(scope)?;
                    end = payload_end;
                    Some(Box::new(Expr {
                        kind: ExprKind::RecordLit(None, fields),
                        ty: self.fresh_type(),
                        loc: Loc {
                            start: payload_start,
                            end: payload_end,
                        },
                    }))
                } else {
                    None
                };

                Ok(Expr {
                    kind: ExprKind::CaseLit(qualifier, case, payload),
                    ty: self.fresh_type(),
                    loc: Loc { start, end },
                })
            }

            _ => Err(FloErr::ExpectedLiteral {
                found: self.peek()?.clone(),
            }),
        }
    }

    fn parse_scope(&mut self, scope: &Scope) -> FloResult<Expr> {
        use TokenKind::*;
        let l_curly = self.expect_get(LCurly)?;

        // We don't want variables created inside to affect outside
        let mut scope = scope.duplicate();

        let mut stmts: Vec<Statement> = Vec::new();
        let mut tail = None;
        while self.peek_kind()? != RCurly {
            // A scope is the only place a `let` may appear, and it is not an
            // expression, so it is parsed here rather than in `parse_atom`.
            if self.peek_kind()? == Let {
                stmts.push(self.parse_let(&mut scope)?);
                continue;
            }

            let expr = self.parse_expr(-1, &mut scope)?;

            // Every statement ends in a `;`, block-shaped ones included: there is
            // no implicit separator. An expression may *begin* with `.`, so
            // without the `;` a `.` after a `}` would be read as field access on
            // the block rather than as the start of what follows. The tail is the
            // one expression that goes without, and that is how the two are told
            // apart.
            if self.expect(Semicolon).is_ok() {
                let loc = expr.loc;
                stmts.push(Statement {
                    kind: StmtKind::Expr(expr),
                    loc,
                });
                continue;
            }

            // No `;`, so this was the tail — and nothing may follow the tail. If
            // something does, the `;` is what is missing, which says far more
            // than "expected `}`" would.
            if !matches!(self.peek_kind(), Ok(RCurly)) {
                return Err(FloErr::ExpectedTokenNotFound {
                    expected: Semicolon,
                    found: self.peek()?.clone(),
                });
            }

            tail = Some(expr);
            break;
        }

        let r_curly = self.expect_get(RCurly)?;

        // Always a fresh var: the type checker decides whether the scope is its
        // tail's type, `void` (no tail), or `noreturn` (a statement/tail diverges).
        let ty = self.fresh_type();
        let kind = ExprKind::Scope(stmts, tail.map(|e| Box::new(e)));
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

        // The branch must be a scope, so that a `{` after the condition is
        // never read as the start of something else.
        let then = Box::new(self.parse_scope(cond_scope)?);
        let mut end = then.loc.end;

        // The else branch is a scope too, or another `if` — that second form is
        // all `else if` is.
        let otherwise = if self.expect(Else).is_ok() {
            let else_scope = &mut scope.duplicate();
            let otherwise = if self.peek_kind()? == If {
                self.parse_if_expr(else_scope)?
            } else {
                self.parse_scope(else_scope)?
            };
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

    /// Parses `while <cond> <body>`. A loop yields no value, so — like a `let` —
    /// its type is `void` outright rather than a fresh type var: there is nothing
    /// for the checker to infer, and it never diverges (the condition may be
    /// false on the very first check).
    fn parse_while_expr(&mut self, scope: &Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let while_tok = self.expect_get(While)?;
        let start = while_tok.loc.start;

        // As with `if`, variables introduced by the condition (via a future `is`
        // expr) are visible in the body but not after the loop.
        let cond_scope = &mut scope.duplicate();
        let cond = Box::new(self.parse_expr(-1, cond_scope)?);

        // Only the body counts as being "inside" the loop: the condition is
        // evaluated before each iteration, so a `break` there has nothing to
        // jump out of yet. Like an `if` branch, it has to be a scope.
        self.loop_depth += 1;
        let body = self.parse_scope(cond_scope);
        self.loop_depth -= 1;
        let body = Box::new(body?);

        let loc = Loc {
            start,
            end: body.loc.end,
        };
        Ok(Expr {
            kind: ExprKind::While(cond, body),
            ty: Type::Void,
            loc,
        })
    }

    /// Parses a bare `break` or `continue`. Both are NoReturn, like `return`, and
    /// neither takes an operand yet.
    fn parse_loop_jump(&mut self) -> FloResult<Expr> {
        use TokenKind::*;

        let tok = self.peek()?.clone();
        self.skip();

        if self.loop_depth == 0 {
            return Err(match tok.kind {
                Break => FloErr::BreakOutsideLoop { loc: tok.loc },
                _ => FloErr::ContinueOutsideLoop { loc: tok.loc },
            });
        }

        let kind = if tok.kind == Break {
            ExprKind::Break
        } else {
            ExprKind::Continue
        };

        Ok(Expr {
            kind,
            ty: Type::Never,
            loc: tok.loc,
        })
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

    /// Parses `let name [: Type] [= init];`. A declaration is a statement, not
    /// an expression: it yields no value, so it can only appear directly inside
    /// a scope and never as the scope's tail.
    fn parse_let(&mut self, scope: &mut Scope) -> FloResult<Statement> {
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
            Some(self.parse_expr(-1, scope)?)
        } else {
            None
        };

        let end = match &init {
            Some(init) => init.loc.end,
            None => name_loc.end,
        };

        // The name only comes into scope once the initializer has been parsed,
        // but before the `;`, so a redeclaration in the same scope shadows from
        // the next statement on.
        let id = self.fresh_var(name, ty.clone(), scope);

        self.expect(Semicolon)?;

        Ok(Statement {
            kind: StmtKind::Let(id, ty, init),
            loc: Loc { start, end },
        })
    }

    /// Parses an optional parenthesized, comma-separated argument list.
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

    /// Parses the `{ .. }` of a record literal, and where it ends.
    ///
    /// As in a declaration, a field is named when an identifier is followed by a
    /// `:`, and positional otherwise — its name being the index it was written
    /// at. Named fields may come in any order, since which record this is has not
    /// even been decided yet; positional ones are in the only order they have.
    ///
    /// Fields may be left out. What that means is not this pass's business: the
    /// missing ones are filled in with zeroes once the checker has settled which
    /// record this is.
    fn parse_record_lit(&mut self, scope: &mut Scope) -> FloResult<(Vec<FieldInit>, usize)> {
        use TokenKind::*;

        let l_curly = self.expect_get(LCurly)?;

        let mut fields: Vec<FieldInit> = Vec::new();
        let mut positional = 0usize;

        while self.peek_kind()? != RCurly {
            let value_start = self.peek()?.loc;

            let (name, name_loc) =
                if self.peek_kind()? == Ident && self.peek_kind_n(1) == Some(Colon) {
                    let tok = self.expect_get(Ident)?;
                    let TokenValue::String(written) = tok.value else {
                        unreachable!()
                    };
                    self.expect(Colon)?;

                    let name = FieldName::parse(&written);
                    if name.is_positional() {
                        return Err(FloErr::ReservedFieldName {
                            field: written,
                            loc: tok.loc,
                        });
                    }
                    (name, tok.loc)
                } else {
                    let name = FieldName::Pos(positional);
                    positional += 1;
                    (name, value_start)
                };

            let value = self.parse_expr(-1, scope)?;

            if let Some(prev) = fields.iter().find(|f| f.name == name) {
                return Err(FloErr::DuplicateFieldInit {
                    field: format!("{name:?}"),
                    loc: name_loc,
                    prev_loc: prev.loc,
                });
            }

            fields.push(FieldInit {
                name,
                value,
                loc: name_loc,
            });

            if self.expect(Comma).is_err() {
                break;
            }
        }

        let r_curly = self.expect_get(RCurly)?;

        if positional != 0 && positional != fields.len() {
            return Err(FloErr::MixedFieldKinds {
                loc: Loc {
                    start: l_curly.loc.start,
                    end: r_curly.loc.end,
                },
            });
        }

        Ok((fields, r_curly.loc.end))
    }

    /// Parses a type: a primitive, a type parameter, a declared type, an
    /// anonymous record, or an anonymous sum.
    ///
    /// The one thing that needs deciding is a bare name, which is a declared type
    /// everywhere except as an alternative of an anonymous sum, where it is a
    /// payload-less case. What follows it is what says which: a `|` makes the
    /// whole thing a sum, and nothing else can. Which also means an anonymous sum
    /// always has at least two cases — with one there would be no `|`, and no way
    /// to tell it from a mention of a type by that name.
    fn parse_type(&mut self) -> FloResult<(Type, Loc)> {
        use TokenKind::*;

        let (first, first_loc) = self.parse_type_atom()?;

        if !matches!(self.peek_kind(), Ok(Pipe)) {
            return match first {
                TypeAtom::Ty(ty) => Ok((ty, first_loc)),
                TypeAtom::Name(name) => {
                    // Taken on trust: the declaration may be further down the
                    // file, so whether the name exists and takes this many
                    // arguments is checked once the whole program is parsed.
                    Ok((Type::User(name, Vec::new()), first_loc))
                }
                // It had a payload, so it can only have been meant as a case —
                // but there is no `|`, so there is no sum for it to be a case of.
                TypeAtom::Case(case) => Err(FloErr::SingleCaseAnonSum {
                    case: case.name,
                    loc: first_loc,
                }),
            };
        }

        let mut cases = vec![atom_as_case(first, first_loc)?];
        let mut end = first_loc.end;

        while self.expect(Pipe).is_ok() {
            let (atom, atom_loc) = self.parse_type_atom()?;
            let case = atom_as_case(atom, atom_loc)?;

            if let Some(prev) = cases.iter().find(|c| c.name == case.name) {
                return Err(FloErr::DuplicateCaseInAnonSum {
                    case: prev.name.clone(),
                    loc: atom_loc,
                });
            }

            cases.push(case);
            end = atom_loc.end;
        }

        Ok((
            Type::AnonSum(sorted_cases(cases)),
            Loc {
                start: first_loc.start,
                end,
            },
        ))
    }

    /// One alternative of a type position. See [`TypeAtom`] for why a bare name
    /// cannot be resolved here.
    fn parse_type_atom(&mut self) -> FloResult<(TypeAtom, Loc)> {
        use TokenKind::*;

        let token = self.peek()?.clone();
        let loc = token.loc;

        match token.kind {
            // An anonymous record. Written down means concrete, so this is an
            // `AnonRecord` and not something still being inferred.
            LCurly => {
                let record = self.parse_record_decl()?;
                let ty = Type::AnonRecord(record.at(&HashMap::new()));
                Ok((TypeAtom::Ty(ty), record.loc))
            }

            Ident => {
                let token = self.expect_get(Ident)?;
                let TokenValue::String(value) = &token.value else {
                    unreachable!()
                };

                // A type parameter of the function being parsed shadows
                // nothing — none of the primitives are valid parameter names —
                // but it is checked first all the same, so a `T` resolves to the
                // variable standing for it.
                if let Some(&id) = self.type_params.get(value) {
                    return Ok((TypeAtom::Ty(Type::T(id)), loc));
                }

                let primitive = match value.as_str() {
                    "u8" => Some(Type::U8),
                    "u16" => Some(Type::U16),
                    "u32" => Some(Type::U32),
                    "u64" => Some(Type::U64),
                    "i8" => Some(Type::I8),
                    "i16" => Some(Type::I16),
                    "i32" => Some(Type::I32),
                    "i64" => Some(Type::I64),
                    "f32" => Some(Type::F32),
                    "f64" => Some(Type::F64),
                    "void" => Some(Type::Void),
                    "bool" => Some(Type::Bool),
                    _ => None,
                };
                if let Some(ty) = primitive {
                    return Ok((TypeAtom::Ty(ty), loc));
                }

                let name = value.clone();

                // A `{` makes it a case with a payload, which no type mention can
                // be. A `<` makes it a generic type, which no case can be.
                if matches!(self.peek_kind(), Ok(LCurly)) {
                    let payload = self.parse_record_decl()?;
                    let case = SumCase::new(
                        name,
                        Some(Type::AnonRecord(payload.at(&HashMap::new()))),
                    );
                    return Ok((
                        TypeAtom::Case(case),
                        Loc {
                            start: loc.start,
                            end: payload.loc.end,
                        },
                    ));
                }

                let (args, args_end) = self.parse_type_args()?;
                match args_end {
                    Some(end) => Ok((
                        TypeAtom::Ty(Type::User(name, args)),
                        Loc {
                            start: loc.start,
                            end,
                        },
                    )),
                    None => Ok((TypeAtom::Name(name), loc)),
                }
            }

            _ => Err(FloErr::NotAType {
                token: token.clone(),
            }),
        }
    }

    /// Parses an optional `<T, U>` after a type's name. Unlike the turbofish
    /// this needs no `::`: inside a type there is nothing for `<` to mean other
    /// than the start of an argument list.
    fn parse_type_args(&mut self) -> FloResult<(Vec<Type>, Option<usize>)> {
        use TokenKind::*;

        if self.expect(LessThan).is_err() {
            return Ok((Vec::new(), None));
        }

        let mut args = Vec::new();
        while self.peek_kind()? != GreaterThan {
            args.push(self.parse_type()?.0);
            if self.expect(Comma).is_err() {
                break;
            }
        }

        let close = self.expect_get(GreaterThan)?;

        if args.is_empty() {
            return Err(FloErr::EmptyTypeParamList { loc: close.loc });
        }

        Ok((args, Some(close.loc.end)))
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

    fn peek_kind_n(&self, n: usize) -> Option<TokenKind> {
        self.tokens.get(self.idx + n).map(|t| t.kind)
    }

    /// The shift operator at the cursor, if there is one: its `Op`, the symbol
    /// it is written with, and the span of both halves.
    ///
    /// `<<` and `>>` are two tokens each, not one. They have to be, because
    /// `View<View<i32>>` closes two argument lists with two `>` in a row and the
    /// tokenizer cannot know which of the two it is looking at — so joining them
    /// is the grammar's job, and it only happens where a shift is what was
    /// meant. Adjacency in the source is what says so: `a >> b` is a shift,
    /// `Foo<Bar<i32>>` is not, and `a > > b` is neither (it stays the error it
    /// always was).
    fn peek_shift(&self) -> Option<(Op, &'static str, Loc)> {
        use TokenKind::*;

        let first = self.tokens.get(self.idx)?;
        let second = self.tokens.get(self.idx + 1)?;

        // Both are one character wide, so "nothing between them" is exactly
        // this. Anything else — a space, a comment, a newline — is not a shift.
        if second.loc.start != first.loc.end + 1 {
            return None;
        }

        // Spelled out because `use TokenKind::*` above shadows `Op` with the
        // `op` keyword's token kind.
        let (op, name) = match (first.kind, second.kind) {
            (LessThan, LessThan) => (crate::ast::Op::Shl, "<<"),
            (GreaterThan, GreaterThan) => (crate::ast::Op::Shr, ">>"),
            _ => return None,
        };

        let loc = Loc {
            start: first.loc.start,
            end: second.loc.end,
        };
        Some((op, name, loc))
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

        let tilde_op = self.funcs.entry("~".to_string()).or_default();
        tilde_op.push(builtin_op(BitNot, vec![U8], U8));
        tilde_op.push(builtin_op(BitNot, vec![U16], U16));
        tilde_op.push(builtin_op(BitNot, vec![U32], U32));
        tilde_op.push(builtin_op(BitNot, vec![U64], U64));
        tilde_op.push(builtin_op(BitNot, vec![I8], I8));
        tilde_op.push(builtin_op(BitNot, vec![I16], I16));
        tilde_op.push(builtin_op(BitNot, vec![I32], I32));
        tilde_op.push(builtin_op(BitNot, vec![I64], I64));
        // A bool overload, like the other bitwise operators have.
        tilde_op.push(builtin_op(BitNot, vec![Bool], Bool));

        // `!` is logical negation and nothing else: there is no truthiness in
        // the language, so an integer is not something that can be negated.
        let bang_op = self.funcs.entry("!".to_string()).or_default();
        bang_op.push(builtin_op(Not, vec![Bool], Bool));

        // The shifts are the only operators whose operands need not agree: what
        // is being shifted decides the result type, and the shift amount only
        // says how far — so any integer width will do for it. Hence the square
        // rather than the single row every other table above has.
        let int_types = [U8, U16, U32, U64, I8, I16, I32, I64];
        for (name, op) in [("<<", Shl), (">>", Shr)] {
            let shift_op = self.funcs.entry(name.to_string()).or_default();
            for value in &int_types {
                for amount in &int_types {
                    shift_op.push(builtin_op(
                        op,
                        vec![value.clone(), amount.clone()],
                        value.clone(),
                    ));
                }
            }
        }

        // No `&&` / `||` entries: they short-circuit, so they are not calls at
        // all and there is no function for a call site to resolve to.

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

/// Check that the module has an entry point, and that it looks like one.
///
/// This is a property of a whole *program*, not of a parse: a module that is
/// only ever imported has no `main`, and with several source files the entry
/// point may not be in the one being parsed. So it lives here rather than in
/// [`Parser::parse`], and only the driver runs it.
pub fn check_entry_point(module: &Module) -> FloResult<()> {
    let mains = match module.funcs.get("main") {
        Some(mains) if !mains.is_empty() => mains,
        _ => return Err(FloErr::MainFunctionNotFound),
    };

    if mains.len() > 1 {
        return Err(FloErr::MultipleMainFunction);
    }

    let main = &mains[0];
    let Type::Fn(args, ret) = &main.ty else {
        unreachable!()
    };

    // `fn main()` or `fn main(args: []string)`, returning `void` or `i32`. The
    // argument form has to wait for slices and `string` to exist.
    let args_ok = args.is_empty();
    let ret_ok = matches!(**ret, Type::Void | Type::I32);

    if args_ok && ret_ok {
        Ok(())
    } else {
        Err(FloErr::InvalidMainSignature {
            ty: main.ty.clone(),
            loc: main.loc,
        })
    }
}

/// The atom read as one case of an anonymous sum.
///
/// A bare name becomes a payload-less case, which is the whole reason the
/// decision waits for the `|`. Anything that is definitely a type — a primitive,
/// a type parameter, a generic mention, a record — is not something a case could
/// be, and says the `|` was a mistake.
fn atom_as_case(atom: TypeAtom, loc: Loc) -> FloResult<SumCase> {
    match atom {
        TypeAtom::Case(case) => Ok(case),
        TypeAtom::Name(name) => Ok(SumCase::new(name, None)),
        TypeAtom::Ty(ty) => Err(FloErr::NotACase { ty, loc }),
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
        type_params: Vec::new(),
    }
}

impl Expr {
    /// Whether this expression denotes a storage location, and so may appear on
    /// the left of an `=`. Pointer derefs and indexing join this list when they
    /// land.
    fn is_lvalue(&self) -> bool {
        match &self.kind {
            ExprKind::Var(_) => true,
            // A field is only storage if what it belongs to is: `foo.bar = 1`
            // assigns, `make_foo().bar = 1` has nowhere to put the result.
            ExprKind::Field(recv, _) => recv.is_lvalue(),
            _ => false,
        }
    }

    /// Whether this expression is written as a brace-delimited block. Those
    /// carry an implicit `;` in statement position, so a scope may hold several
    /// of them in a row with no separator.
    fn is_block_like(&self) -> bool {
        use ExprKind::*;
        matches!(self.kind, Scope(..) | If(..) | While(..))
    }
}

/// Where `<<` and `>>` sit in the ladder below — between the additive operators
/// and the relational ones, as in C. It is a constant rather than an arm of
/// [`TokenKind::precedence`] because a shift is not a token (see
/// [`Parser::peek_shift`]); keep it in step with that ladder.
const SHIFT_PRECEDENCE: i32 = 8;

impl TokenKind {
    fn is_unary_op(&self) -> bool {
        use TokenKind::*;
        // `!` and `~` are unary only. `+` and `-` are both, which the parser
        // does not have to distinguish: the arity of the `Call` it builds is
        // what picks the overload.
        matches!(self, Plus | Minus | Bang | Tilde)
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
            // 8 is the shifts; see `SHIFT_PRECEDENCE`.
            Minus | Plus => 9,
            Star | Slash | Percent => 10,
            _ => unreachable!("Called TokenKind::precedence(`{self:?}`)"),
        }
    }
}
