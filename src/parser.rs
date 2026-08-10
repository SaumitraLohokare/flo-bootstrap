use std::collections::HashMap;

use crate::{
    ast::{Expr, ExprKind, FieldInit, Func, Module, Op, Statement, StmtKind, UseDecl},
    errors::{FloErr, FloResult},
    tokenizer::{Loc, Token, TokenKind, TokenValue},
    types::{CaseDecl, FieldDecl, Type, TypeDecl, TypeTable},
    util::Iota,
};

/// The names visible at a point in the source, mapping each to its variable id.
/// Only the mapping is scoped — a variable's type lives in [`Parser::var_types`],
/// keyed by the id, because ids are unique for the whole parse.
#[derive(Debug, Clone)]
struct Scope {
    vars: HashMap<String, usize>,

    /// The case names a `use` has brought in, and the type each was used from.
    /// A name in here may be written bare; one that is not is an unknown
    /// identifier. The type name is kept only so the `use` can be checked once
    /// every declaration is in — it does *not* pin the literal to that type,
    /// which is still inferred like any other.
    cases: HashMap<String, String>,
}

impl Scope {
    fn duplicate(&self) -> Self {
        Self {
            vars: self.vars.clone(),
            cases: self.cases.clone(),
        }
    }

    fn add_var(&mut self, name: String, id: usize) {
        self.vars.insert(name, id);
    }

    fn get_var(&self, name: &String) -> Option<usize> {
        self.vars.get(name).copied()
    }

    fn add_case(&mut self, case: String, type_name: String) {
        self.cases.insert(case, type_name);
    }

    fn knows_case(&self, name: &String) -> bool {
        self.cases.contains_key(name)
    }
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

    /// The case names a file-scope `use` brought in, and the type each was used
    /// from. Every function body starts from a copy of this.
    file_cases: HashMap<String, String>,

    funcs: HashMap<String, Vec<Func>>,
    types: TypeTable,
    uses: Vec<UseDecl>,
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
            file_cases: HashMap::new(),
            funcs: HashMap::new(),
            types: TypeTable::new(),
            uses: Vec::new(),
        }
    }

    pub fn parse(mut self) -> FloResult<Module> {
        use TokenKind::*;

        // A file-scope `use` applies to the whole file, not just to what comes
        // after it — the same as a `fn` or a `type`. Since resolving a name
        // happens while parsing, the set has to be known before the first body
        // is read, which is what this pre-pass is for.
        self.prescan_file_uses();

        while let Ok(token) = self.peek() {
            match token.kind {
                Fn | Op => self.parse_func()?,
                TypeKw => self.parse_type_decl()?,
                Use => {
                    // Already accounted for by the pre-pass; parsed again here
                    // so that a malformed one is reported, and recorded so it
                    // can be checked against the declarations.
                    let use_decl = self.parse_use()?;
                    self.uses.push(use_decl);
                }

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
            uses: self.uses,
            var_count: self.var_types.len(),
            type_var_count: self.type_iota.count(),
        })
    }

    /// Find every file-scope `use` up front, so one written at the bottom of the
    /// file is in scope at the top.
    ///
    /// "File scope" is just brace depth zero: a `use` is either a top-level
    /// declaration or a statement, and a statement is always inside the braces
    /// of some scope. Nothing is reported here — a malformed `use` is simply not
    /// recognised, and the real parse reports it a moment later.
    fn prescan_file_uses(&mut self) {
        use TokenKind::*;

        let mut found = Vec::new();
        let mut depth = 0usize;

        for (i, token) in self.tokens.iter().enumerate() {
            match token.kind {
                LCurly => depth += 1,
                RCurly => depth = depth.saturating_sub(1),
                Use if depth == 0 => {
                    let kind_at = |n: usize| self.tokens.get(i + n).map(|t| t.kind);
                    if kind_at(1) != Some(Ident)
                        || kind_at(2) != Some(ColonColon)
                        || kind_at(3) != Some(Ident)
                    {
                        continue;
                    }

                    let (TokenValue::String(type_name), TokenValue::String(case)) =
                        (&self.tokens[i + 1].value, &self.tokens[i + 3].value)
                    else {
                        unreachable!()
                    };
                    found.push((case.clone(), type_name.clone()));
                }
                _ => {}
            }
        }

        self.file_cases.extend(found);
    }

    /// Parses `use Type::Case;`. The same form at file scope and as a statement.
    ///
    /// Whether the type exists and has the case cannot be answered yet — it may
    /// be declared further down the file — so that waits for
    /// [`crate::type_checker::check_type_decls`].
    fn parse_use(&mut self) -> FloResult<UseDecl> {
        use TokenKind::*;

        let use_tok = self.expect_get(Use)?;

        let type_token = self.expect_get(Ident)?;
        let TokenValue::String(type_name) = type_token.value else {
            unreachable!()
        };

        self.expect(ColonColon)?;

        let case_token = self.expect_get(Ident)?;
        let TokenValue::String(case) = case_token.value else {
            unreachable!()
        };

        self.expect(Semicolon)?;

        Ok(UseDecl {
            type_name,
            case,
            loc: Loc {
                start: use_tok.loc.start,
                end: case_token.loc.end,
            },
        })
    }

    /// A scope's copy of the file-level names. Every function body starts here.
    fn root_scope(&self) -> Scope {
        Scope {
            vars: HashMap::new(),
            cases: self.file_cases.clone(),
        }
    }

    /// Parses `type Name<T> = Case { field: T } | Other;`
    ///
    /// A case may leave out its name, in which case it takes the type's own —
    /// which is what makes `type Vec<T> = { .. };` the one-case shorthand for
    /// `type Vec<T> = Vec { .. };`.
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

        // As for a function, the parameters are registered before the cases are
        // read, so a `T` in a field type resolves to the variable standing for it.
        let type_params = self.parse_type_params()?;

        self.expect(Equal)?;

        let mut cases: Vec<CaseDecl> = Vec::new();
        loop {
            let case = self.parse_case_decl(&name)?;

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
                cases,
                loc,
            },
        );

        Ok(())
    }

    /// One case of a `type`: `Name { fields }`, a bare `Name`, or `{ fields }`
    /// with the name left off — which means `type_name`.
    fn parse_case_decl(&mut self, type_name: &str) -> FloResult<CaseDecl> {
        use TokenKind::*;

        let start_token = self.peek()?.clone();

        let name = if self.peek_kind()? == Ident {
            let tok = self.expect_get(Ident)?;
            let TokenValue::String(name) = tok.value else {
                unreachable!()
            };
            name
        } else if self.peek_kind()? == LCurly {
            type_name.to_string()
        } else {
            return Err(FloErr::ExpectedCase {
                found: start_token,
            });
        };

        let mut fields: Vec<FieldDecl> = Vec::new();
        let mut end = self.tokens[self.idx - 1].loc.end;

        if self.expect(LCurly).is_ok() {
            while self.peek_kind()? == Ident {
                let field_token = self.expect_get(Ident)?;
                let TokenValue::String(field_name) = field_token.value else {
                    unreachable!()
                };

                self.expect(Colon)?;
                let (ty, _) = self.parse_type()?;

                if let Some(prev) = fields.iter().find(|f| f.name == field_name) {
                    return Err(FloErr::DuplicateField {
                        case: name,
                        field: field_name,
                        loc: field_token.loc,
                        prev_loc: prev.loc,
                    });
                }

                fields.push(FieldDecl {
                    name: field_name,
                    ty,
                    loc: field_token.loc,
                });

                if self.expect(Comma).is_err() {
                    break;
                }
            }

            end = self.expect_get(RCurly)?.loc.end;
        }

        Ok(CaseDecl {
            name,
            fields,
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

    /// Parses an optional `::<T, U>` turbofish at a call site. Empty when there
    /// is none, which is the usual case — the instantiation is inferred.
    ///
    /// A `::` not followed by `<` is left alone: that is the qualifier of a type
    /// literal, `Foo::Foo { .. }`.
    fn parse_turbofish(&mut self) -> FloResult<Vec<Type>> {
        use TokenKind::*;

        if !matches!(self.peek_kind(), Ok(ColonColon)) || self.peek_kind_n(1) != Some(LessThan) {
            return Ok(Vec::new());
        }
        self.skip();

        self.expect(LessThan)?;

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

        Ok(args)
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
                        kind: Call(
                            format!("{}", op.pretty_name()),
                            Vec::new(),
                            vec![lhs, rhs],
                            None,
                        ),
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

                let type_args = self.parse_turbofish()?;

                let mut args = match self.parse_call_args(scope)? {
                    Some((args, args_end)) => {
                        end = args_end;
                        args
                    }
                    None => Vec::new(),
                };
                args.insert(0, lhs); // lhs becomes the first arg

                lhs = Expr {
                    kind: ExprKind::Call(func_name, type_args, args, None),
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
            let kind = Call(
                format!("{}", op.pretty_name()),
                Vec::new(),
                vec![operand],
                None,
            );
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

    /// Parses an atom and any `.field` chained onto it. Field access binds
    /// tighter than every operator, unary ones included, so `-a.b` is `-(a.b)`
    /// and `&a.b` will be `&(a.b)`.
    fn parse_postfix(&mut self, scope: &mut Scope) -> FloResult<Expr> {
        use TokenKind::*;

        let mut expr = self.parse_atom(scope)?;

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
                kind: ExprKind::Field(Box::new(expr), field),
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

                let type_args = self.parse_turbofish()?;

                // A second `::` means this name was the *type* of a literal:
                // `Vec::<i32>::Vec { .. }`. Nothing else can follow a name that
                // way, so it is decided before the call forms below.
                if matches!(self.peek_kind(), Ok(ColonColon)) {
                    self.skip();

                    let case_token = self.expect_get(Ident)?;
                    let TokenValue::String(case) = case_token.value else {
                        unreachable!()
                    };

                    let (fields, fields_end) = self.parse_field_inits(scope)?;

                    return Ok(Expr {
                        kind: ExprKind::CaseLit(Some((name, type_args)), case, fields),
                        ty: self.fresh_type(),
                        loc: Loc {
                            start: name_loc.start,
                            end: fields_end.unwrap_or(case_token.loc.end),
                        },
                    });
                }

                let args = self.parse_call_args(scope)?;

                // Parentheses make it a call, and so does a turbofish on its
                // own, since no variable can carry type arguments.
                match (args, type_args.is_empty()) {
                    (Some((args, end)), _) => {
                        let kind = ExprKind::Call(name, type_args, args, None); // unresolved
                        Ok(Expr {
                            kind,
                            ty: self.fresh_type(),
                            loc: Loc {
                                start: name_loc.start,
                                end,
                            },
                        })
                    }
                    (None, false) => {
                        let kind = ExprKind::Call(name, type_args, Vec::new(), None);
                        Ok(Expr {
                            kind,
                            ty: self.fresh_type(),
                            loc: Loc {
                                start: name_loc.start,
                                end: self.tokens[self.idx - 1].loc.end,
                            },
                        })
                    }
                    // A name in scope is that variable. The variable always
                    // wins: `if foo { .. }` reads `foo` as the condition, not as
                    // the start of a literal of a case that happens to be
                    // called `foo`.
                    (None, true) if scope.get_var(&name).is_some() => {
                        let var_id = scope.get_var(&name).unwrap();
                        Ok(Expr {
                            kind: ExprKind::Var(var_id),
                            ty: self.var_types[var_id].clone(),
                            loc: name_loc,
                        })
                    }
                    // Failing that, a case a `use` brought in: `Foo { bar: 0 }`,
                    // or written bare when the case carries no fields. The `use`
                    // only makes the name legal — which type this is remains
                    // inferred, exactly as for the qualified form.
                    (None, true) if scope.knows_case(&name) => {
                        let (fields, fields_end) = self.parse_field_inits(scope)?;
                        Ok(Expr {
                            kind: ExprKind::CaseLit(None, name, fields),
                            ty: self.fresh_type(),
                            loc: Loc {
                                start: name_loc.start,
                                end: fields_end.unwrap_or(name_loc.end),
                            },
                        })
                    }
                    // Neither, so there is nothing the name could denote. A case
                    // name on its own says nothing about which type it belongs
                    // to, so it has to have been introduced by a `use` or be
                    // written out as `Type::Case`.
                    (None, true) => Err(FloErr::UnknownIdentifier {
                        name,
                        loc: name_loc,
                    }),
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

            While => self.parse_while_expr(scope),

            Break | Continue => self.parse_loop_jump(),

            Return => self.parse_return(scope),

            // Neither is an expression, and both are only legal as a statement
            // directly inside a scope, where `parse_scope` handles them.
            // Reaching one here means it was written as an operand —
            // `1 + (let a = 2)` and the like.
            Let => Err(FloErr::LetOutsideStatementPosition { loc: token.loc }),
            Use => Err(FloErr::UseOutsideStatementPosition { loc: token.loc }),

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

        let mut stmts: Vec<Statement> = Vec::new();
        let mut tail = None;
        while self.peek_kind()? != RCurly {
            // A scope is the only place `let` and `use` may appear, and neither
            // is an expression, so both are parsed here rather than in
            // `parse_atom`. Both always end in a `;`: a statement has no value,
            // so neither can be the scope's tail.
            match self.peek_kind()? {
                Let => {
                    stmts.push(self.parse_let(&mut scope)?);
                    continue;
                }
                Use => {
                    let use_decl = self.parse_use()?;
                    scope.add_case(use_decl.case.clone(), use_decl.type_name.clone());
                    stmts.push(Statement {
                        kind: StmtKind::Use(
                            use_decl.type_name.clone(),
                            use_decl.case.clone(),
                        ),
                        loc: use_decl.loc,
                    });
                    self.uses.push(use_decl);
                    continue;
                }
                _ => {}
            }

            let starts_block = matches!(self.peek_kind()?, LCurly | If | While);
            let expr = self.parse_expr(-1, &mut scope)?;

            // A block-shaped expression carries an implicit `;` when something
            // follows it, so `if c { } print();` needs no separator. Written
            // last it is still the scope's tail, which is what makes
            // `{ if c { 0 } else { 1 } }` yield a value.
            //
            // Both halves of the test matter. The statement has to have *begun*
            // with a block, so `({ 0 }) - 1` stays one subtraction; and it has
            // to have *stayed* one, so `if c { 0 } else { 1 } - 1` does too —
            // there the trailing operator was folded in and the result is a
            // call, not an `if`.
            let implicit_semi =
                starts_block && expr.is_block_like() && !matches!(self.peek_kind(), Ok(RCurly));

            if self.expect(Semicolon).is_ok() || implicit_semi {
                let loc = expr.loc;
                stmts.push(Statement {
                    kind: StmtKind::Expr(expr),
                    loc,
                });
            } else {
                tail = Some(expr);
                break;
            }
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

    /// Parses the optional `{ name: value, .. }` of a type literal. The end
    /// offset is `None` when there was no brace list at all — a case with no
    /// fields is written bare.
    ///
    /// Every field must be named and they may come in any order, since which
    /// case this is has not even been decided yet.
    fn parse_field_inits(&mut self, scope: &mut Scope) -> FloResult<(Vec<FieldInit>, Option<usize>)> {
        use TokenKind::*;

        if self.expect(LCurly).is_err() {
            return Ok((Vec::new(), None));
        }

        let mut fields: Vec<FieldInit> = Vec::new();
        while self.peek_kind()? == Ident {
            let name_token = self.expect_get(Ident)?;
            let TokenValue::String(name) = name_token.value else {
                unreachable!()
            };

            self.expect(Colon)?;
            let value = self.parse_expr(-1, scope)?;

            if let Some(prev) = fields.iter().find(|f| f.name == name) {
                return Err(FloErr::DuplicateFieldInit {
                    field: name,
                    loc: name_token.loc,
                    prev_loc: prev.loc,
                });
            }

            fields.push(FieldInit {
                name,
                value,
                loc: name_token.loc,
            });

            if self.expect(Comma).is_err() {
                break;
            }
        }

        let r_curly = self.expect_get(RCurly)?;
        Ok((fields, Some(r_curly.loc.end)))
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

                // A type parameter of the function being parsed shadows
                // nothing — none of the primitives are valid parameter names —
                // but it is checked first all the same, so a `T` resolves to the
                // variable standing for it.
                if let Some(&id) = self.type_params.get(value) {
                    return Ok((Type::T(id), loc));
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
                    return Ok((ty, loc));
                }

                // Anything else names a declared type. It is taken on trust: the
                // declaration may be further down the file, so whether the name
                // exists and takes this many arguments is checked once the whole
                // program is parsed.
                let name = value.clone();
                let (args, end) = self.parse_type_args()?;

                Ok((
                    Type::User(name, args),
                    Loc {
                        start: loc.start,
                        end: end.unwrap_or(loc.end),
                    },
                ))
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
