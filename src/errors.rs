use crate::{
    tokenizer::{Loc, Token, TokenKind},
    types::Type,
};

pub type FloResult<T> = Result<T, FloErr>;
#[derive(Debug)]
pub enum FloErr {
    UnexpectedEOF,

    UnexpectedToken {
        found: Token,
    },

    ExpectedTokenNotFound {
        expected: TokenKind,
        found: Token,
    },

    RedifinitionOfArgument {
        name: String,
        loc: Loc,
    },

    MainFunctionNotFound,

    MultipleMainFunction,

    InvalidMainSignature {
        ty: Type,
        loc: Loc,
    },

    LetOutsideStatementPosition {
        loc: Loc,
    },

    UseOutsideStatementPosition {
        loc: Loc,
    },

    /// A bare name that is neither a variable in scope nor a case brought in by
    /// a `use`. Without one of those there is nothing it could mean: a case name
    /// says nothing about which type it belongs to, so an unknown one cannot
    /// just be taken as a literal.
    UnknownIdentifier {
        name: String,
        loc: Loc,
    },

    RedifinitionOfTypeParam {
        name: String,
        loc: Loc,
    },

    EmptyTypeParamList {
        loc: Loc,
    },

    /// A type parameter that nothing at the call site pins down.
    CannotInferTypeParam {
        name: String,
        loc: Loc,
    },

    /// Something went wrong inside a generic function's body, for one
    /// particular instantiation of it. `cause` is reported at its own location
    /// inside the generic; this wrapper adds the call site that asked for it.
    InGenericInstantiation {
        name: String,
        type_args: Vec<Type>,
        call_loc: Loc,
        cause: Box<FloErr>,
    },

    /// Guards against a generic that instantiates itself without ever bottoming
    /// out, which would otherwise loop forever.
    MonomorphizationLimit {
        name: String,
        limit: usize,
        loc: Loc,
    },

    NotAType {
        token: Token,
    },

    /// A `type` case that is neither `Name`, `Name { .. }` nor `{ .. }`.
    ExpectedCase {
        found: Token,
    },

    DuplicateType {
        name: String,
        loc: Loc,
        prev_loc: Loc,
    },

    DuplicateCase {
        type_name: String,
        case: String,
        loc: Loc,
        prev_loc: Loc,
    },

    DuplicateField {
        case: String,
        field: String,
        loc: Loc,
        prev_loc: Loc,
    },

    /// The same field given twice in one type literal.
    DuplicateFieldInit {
        field: String,
        loc: Loc,
        prev_loc: Loc,
    },

    /// A type annotation naming a type that nothing declares.
    UnknownType {
        name: String,
        loc: Loc,
    },

    TypeArityMismatch {
        name: String,
        expected: usize,
        got: usize,
        loc: Loc,
    },

    /// `void` where a builtin needed a type with bits: either side of a
    /// `@cast`, or the argument of `@sizeof` / `@alignof`.
    TypeHasNoSize {
        ty: Type,
        loc: Loc,
    },

    /// An `@name` that is not one of the builtins.
    UnknownBuiltin {
        name: String,
        loc: Loc,
    },

    /// A type that contains itself with nothing to break the cycle, so it has
    /// no size. `cycle` is the path back to the type, in order.
    RecursiveType {
        name: String,
        cycle: Vec<String>,
        loc: Loc,
    },

    /// A literal of a case the type it was pinned to does not have.
    NoSuchCase {
        ty: Type,
        case: String,
        loc: Loc,
    },

    /// A literal that gave a case the wrong fields. A literal must give every
    /// field of its case and no others, so this covers missing and unknown ones
    /// alike.
    WrongFields {
        case: String,
        expected: Vec<String>,
        got: Vec<String>,
        loc: Loc,
    },

    UnknownField {
        ty: Type,
        field: String,
        loc: Loc,
    },

    /// Field access on a type with more than one case. Which case a value holds
    /// is not known without asking, so its fields are reached through `is`.
    FieldAccessOnSumType {
        ty: Type,
        field: String,
        loc: Loc,
    },

    /// Field access on something that has no fields at all, like `1.x`.
    NotAStruct {
        ty: Type,
        field: String,
        loc: Loc,
    },

    UndefinedFunction {
        name: String,
        loc: Loc,
    },

    ExpectedOp {
        found: TokenKind,
        loc: Loc,
    },

    /// `op &&(..)` / `op ||(..)`. The two short-circuiting operators are the only
    /// ones that are not calls, so there is nothing for an overload to hook into.
    OpNotOverloadable {
        op: TokenKind,
        loc: Loc,
    },

    CallArityMismatch {
        expected: usize,
        got: usize,
        loc: Loc,
    },

    TypeMismatch {
        expected: Type,
        got: Type,
        loc: Loc,
    },

    UnresolvedType {
        ty: Type,
        loc: Loc,
    },

    InfiniteType {
        loc: Loc,
    },

    NoPossibleOverloads {
        name: String,
        known_ty: Type,
        loc: Loc,
    },

    MultiplePossibleOverloads {
        name: String,
        possible_tys: Vec<Type>,
        loc: Loc,
    },

    NotAssignable {
        loc: Loc,
    },

    BreakOutsideLoop {
        loc: Loc,
    },

    ContinueOutsideLoop {
        loc: Loc,
    },
}

impl FloErr {
    pub fn pretty_print(self, src: &String) {
        use FloErr::*;

        // This one has no message of its own. The cause is the error, and it
        // prints at its own location inside the generic; all this adds is a note
        // saying which instantiation exposed it. Handled before the `error:`
        // prefix so the cause is the thing labelled as the error.
        if let InGenericInstantiation {
            name,
            type_args,
            call_loc,
            cause,
        } = self
        {
            cause.pretty_print(src);
            let args = type_args
                .iter()
                .map(|t| format!("{t:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("\x1b[1;36mnote\x1b[0m: while instantiating `{name}::<{args}>`");
            print_src(src, &[call_loc]);
            return;
        }

        eprint!("\x1b[1;31merror\x1b[0m: ");

        match self {
            UnexpectedEOF => eprintln!("Unexpected EOF"),
            UnexpectedToken { found } => {
                eprintln!("Unexpected token `{}`", found.kind.pretty_name());
                print_src(src, &[found.loc]);
            }
            ExpectedTokenNotFound { expected, found } => {
                eprintln!(
                    "Expected `{}`, but found `{}`",
                    expected.pretty_name(),
                    found.kind.pretty_name()
                );
                print_src(src, &[found.loc]);
            }
            RedifinitionOfArgument { name, loc } => {
                eprintln!("Redifinition of argument `{name}`");
                print_src(src, &[loc]);
            }
            NotAType { token } => {
                eprintln!("`{}` is not a type", token.kind.pretty_name(),);
                print_src(src, &[token.loc]);
            }
            ExpectedCase { found } => {
                eprintln!(
                    "Expected a case of the type, but found `{}`",
                    found.kind.pretty_name()
                );
                eprintln!("    a case is `Name`, `Name {{ .. }}`, or `{{ .. }}` to reuse the type's name");
                print_src(src, &[found.loc]);
            }
            DuplicateType {
                name,
                loc,
                prev_loc,
            } => {
                eprintln!("Type `{name}` is declared more than once");
                print_src(src, &[prev_loc, loc]);
            }
            DuplicateCase {
                type_name,
                case,
                loc,
                prev_loc,
            } => {
                eprintln!("Type `{type_name}` declares the case `{case}` more than once");
                print_src(src, &[prev_loc, loc]);
            }
            DuplicateField {
                case,
                field,
                loc,
                prev_loc,
            } => {
                eprintln!("Case `{case}` declares the field `{field}` more than once");
                print_src(src, &[prev_loc, loc]);
            }
            DuplicateFieldInit {
                field,
                loc,
                prev_loc,
            } => {
                eprintln!("Field `{field}` is given more than once");
                print_src(src, &[prev_loc, loc]);
            }
            UnknownType { name, loc } => {
                eprintln!("Unknown type `{name}`");
                print_src(src, &[loc]);
            }
            TypeArityMismatch {
                name,
                expected,
                got,
                loc,
            } => {
                eprintln!(
                    "Type `{name}` takes {expected} type argument(s), but {got} were given"
                );
                print_src(src, &[loc]);
            }
            TypeHasNoSize { ty, loc } => {
                eprintln!("`{ty:?}` has no size");
                eprintln!(
                    "    `@cast`, `@sizeof` and `@alignof` all work on the bits of a value, and `{ty:?}` has none"
                );
                print_src(src, &[loc]);
            }
            UnknownBuiltin { name, loc } => {
                eprintln!("Unknown builtin `@{name}`");
                eprintln!("    the builtins are `@cast`, `@sizeof` and `@alignof`");
                print_src(src, &[loc]);
            }
            RecursiveType { name, cycle, loc } => {
                eprintln!("Type `{name}` contains itself, so it has no fixed size");
                eprintln!("    {}", cycle.join(" -> "));
                print_src(src, &[loc]);
            }
            NoSuchCase { ty, case, loc } => {
                eprintln!("Type `{ty:?}` has no case `{case}`");
                print_src(src, &[loc]);
            }
            WrongFields {
                case,
                expected,
                got,
                loc,
            } => {
                eprintln!("Case `{case}` needs the fields {}", named(&expected));
                eprintln!("    but was given {}", named(&got));
                print_src(src, &[loc]);
            }
            UnknownField { ty, field, loc } => {
                eprintln!("Type `{ty:?}` has no field `{field}`");
                print_src(src, &[loc]);
            }
            FieldAccessOnSumType { ty, field, loc } => {
                eprintln!("Cannot read `{field}` of `{ty:?}`, which has more than one case");
                eprintln!("    which case it holds has to be established with `is` first");
                print_src(src, &[loc]);
            }
            NotAStruct { ty, field, loc } => {
                eprintln!("Cannot read `{field}` of `{ty:?}`, which has no fields");
                print_src(src, &[loc]);
            }
            TypeMismatch {
                expected: t1,
                got: t2,
                loc,
            } => {
                eprintln!("Expected `{t1:?}` but got `{t2:?}`");
                print_src(src, &[loc]);
            }
            MainFunctionNotFound => {
                eprintln!("Main function not found.");
            }
            MultipleMainFunction => {
                eprintln!("Multiple definitions of `main` function found.");
            }
            InvalidMainSignature { ty, loc } => {
                eprintln!("`main` cannot have the type `{ty:?}`");
                eprintln!(
                    "    it must be `fn main()` or `fn main(args: []string)`, returning `void` or `i32`"
                );
                print_src(src, &[loc]);
            }
            LetOutsideStatementPosition { loc } => {
                eprintln!("`let` may only appear as a statement inside a scope");
                print_src(src, &[loc]);
            }
            UseOutsideStatementPosition { loc } => {
                eprintln!("`use` may only appear at file scope, or as a statement inside a scope");
                print_src(src, &[loc]);
            }
            UnknownIdentifier { name, loc } => {
                eprintln!("Unknown identifier `{name}`");
                eprintln!(
                    "    it is not a variable in scope; if it is a case of a type, bring it in with `use Type::{name};` or write it as `Type::{name}`"
                );
                print_src(src, &[loc]);
            }
            RedifinitionOfTypeParam { name, loc } => {
                eprintln!("Redifinition of type parameter `{name}`");
                print_src(src, &[loc]);
            }
            EmptyTypeParamList { loc } => {
                eprintln!("Empty type parameter list");
                print_src(src, &[loc]);
            }
            CannotInferTypeParam { name, loc } => {
                eprintln!("Cannot infer type parameter `{name}` at this call");
                eprintln!("    give it explicitly with a turbofish, eg `f::<i32>(..)`");
                print_src(src, &[loc]);
            }
            // Handled above, before the `error:` prefix.
            InGenericInstantiation { .. } => unreachable!(),
            MonomorphizationLimit { name, limit, loc } => {
                eprintln!("`{name}` was instantiated more than {limit} times");
                eprintln!("    it is probably generic over itself without a base case");
                print_src(src, &[loc]);
            }
            UndefinedFunction { name, loc } => {
                eprintln!("Undefined function `{name}`");
                print_src(src, &[loc]);
            }
            CallArityMismatch { expected, got, loc } => {
                eprintln!("Function call expected {expected} arguments, but got {got} instead");
                print_src(src, &[loc]);
            }
            InfiniteType { loc } => {
                eprintln!("Infinite Type");
                print_src(src, &[loc]);
            }
            UnresolvedType { ty, loc } => {
                eprintln!("Unresolved type `{ty:?}`");
                print_src(src, &[loc]);
            }
            NoPossibleOverloads {
                name,
                known_ty,
                loc,
            } => {
                eprintln!("No possible overloads found for `{name}{known_ty:?}`");
                print_src(src, &[loc]);
            }
            MultiplePossibleOverloads {
                name,
                loc,
                possible_tys,
            } => {
                let possible_tys = possible_tys
                    .iter()
                    .map(|t| format!("{name}{t:?}"))
                    .collect::<Vec<String>>()
                    .join(", ");
                eprintln!("Multiple possible overloads found for `{name}`:");
                eprintln!("    {possible_tys}");
                print_src(src, &[loc]);
            }
            ExpectedOp { found, loc } => {
                eprintln!("Expected an operator but found: `{}`", found.pretty_name());
                print_src(src, &[loc]);
            }
            OpNotOverloadable { op, loc } => {
                let op = op.pretty_name();
                eprintln!("`{op}` cannot be overloaded, because it short-circuits");
                print_src(src, &[loc]);
                eprintln!(
                    "\x1b[1;36mnote\x1b[0m: `{op}` only evaluates its right operand when it has to, \
                     which a function call cannot do -- its arguments are always evaluated first"
                );
            }
            NotAssignable { loc } => {
                eprintln!("Cannot assign to this expression");
                print_src(src, &[loc]);
            }
            BreakOutsideLoop { loc } => {
                eprintln!("`break` outside of a loop");
                print_src(src, &[loc]);
            }
            ContinueOutsideLoop { loc } => {
                eprintln!("`continue` outside of a loop");
                print_src(src, &[loc]);
            }
        }
    }
}

/// A list of names for a message: `none`, `` `a` ``, `` `a`, `b` ``.
fn named(names: &[String]) -> String {
    if names.is_empty() {
        return "none".to_string();
    }
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

use std::collections::BTreeMap;

// DISCLAIMER: This function is written by Claude
fn print_src(src: &String, locs: &[Loc]) {
    if locs.is_empty() {
        return;
    }

    let mut lines_info: BTreeMap<usize, (&str, usize, Vec<(usize, usize)>)> = BTreeMap::new();

    // Locs are char indices (the tokenizer counts chars, not bytes), so all
    // offsets here are measured in chars to stay consistent.
    let src_len = src.chars().count();

    let mut all_lines = Vec::new();
    let mut offset = 0;
    for (i, line) in src.split('\n').enumerate() {
        let line_len = line.chars().count();
        let line_end = offset + line_len;
        all_lines.push((i, offset, line_end, line, line_len));
        offset = line_end + 1;
    }

    for loc in locs {
        let start = loc.start.min(src_len);
        let end = loc.end.min(src_len);

        for &(line_num, line_start, line_end, line_text, line_len) in &all_lines {
            if line_end < start || line_start > end {
                continue;
            }

            let seg_start = start.max(line_start) - line_start;
            let seg_end = end.min(line_end) - line_start;
            let seg_start = seg_start.min(line_len);
            let seg_end = seg_end.max(seg_start).min(line_len);

            let entry = lines_info
                .entry(line_num)
                .or_insert_with(|| (line_text, line_start, Vec::new()));
            entry.2.push((seg_start, seg_end));
        }
    }

    // Width of the largest line number, so the "..." separator can align under the " | "
    let max_line_num = lines_info.keys().last().copied().unwrap_or(0) + 1;
    let num_width = max_line_num.to_string().len();

    let mut prev_line_num: Option<usize> = None;

    for (line_num, (line_text, _line_start, mut segs)) in lines_info {
        // If we skipped one or more lines since the last one printed, show a gap marker
        if let Some(prev) = prev_line_num {
            if line_num > prev + 1 {
                eprintln!("{:>width$} | ...", "", width = num_width);
            }
        }

        let prefix = format!("{:>width$} | ", line_num + 1, width = num_width);
        eprintln!("{prefix}{line_text}");

        segs.sort_by_key(|&(s, _)| s);

        let mut underline = String::new();
        let mut cursor = 0;
        for (seg_start, seg_end) in segs {
            if seg_start < cursor {
                continue;
            }
            underline.push_str(&" ".repeat(seg_start - cursor));
            let len = seg_end - seg_start + 1;
            underline.push_str(&"^".repeat(len));
            cursor = seg_start + len;
        }

        eprintln!("{}{}", " ".repeat(prefix.len()), underline);

        prev_line_num = Some(line_num);
    }
}

impl TokenKind {
    pub fn pretty_name(&self) -> &str {
        match self {
            TokenKind::Fn => "fn",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Ident => "identifier",
            TokenKind::Num => "number",
            TokenKind::Flt => "decimal",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::Arrow => "->",
            TokenKind::Equal => "=",
            TokenKind::Semicolon => ";",
            TokenKind::Colon => ":",
            TokenKind::ColonColon => "::",
            TokenKind::Comma => ",",
            TokenKind::Dot => ".",
            TokenKind::TypeKw => "type",
            TokenKind::Use => "use",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Amp => "&",
            TokenKind::Pipe => "|",
            TokenKind::Cap => "^",
            TokenKind::Tilde => "~",
            TokenKind::AmpAmp => "&&",
            TokenKind::PipePipe => "||",
            TokenKind::Bang => "!",
            TokenKind::At => "@",
            TokenKind::EqualEqual => "==",
            TokenKind::BangEqual => "!=",
            TokenKind::LessThan => "<",
            TokenKind::GreaterThan => ">",
            TokenKind::LessThanEqual => "<=",
            TokenKind::GreaterThanEqual => ">=",
            TokenKind::PipeGreaterThan => "|>",
            TokenKind::Op => "op",
            TokenKind::LCurly => "{",
            TokenKind::RCurly => "}",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::While => "while",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Return => "return",
            TokenKind::Let => "let",
        }
    }
}
