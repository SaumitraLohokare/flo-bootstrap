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

    /// A feature the syntax reserves but the compiler does not implement yet.
    NotImplemented {
        what: &'static str,
        loc: Loc,
    },

    /// A bare name that is not a variable in scope. There is nothing else it
    /// could be: a case is only ever written after a `.`, and a type name only as
    /// a literal's qualifier, which needs a `.` after it too.
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

    /// A `type` case that is neither `Name` nor `Name { .. }`.
    ExpectedCase {
        found: Token,
    },

    /// A `|` in a type position whose alternative could not be a case: a
    /// primitive, a type parameter, a generic mention, or a record.
    NotACase {
        ty: Type,
        loc: Loc,
    },

    /// `Name { .. }` written in a type position with no `|` after it. It can only
    /// have been meant as a case, and a one-case sum is a record written the long
    /// way round.
    SingleCaseAnonSum {
        case: String,
        loc: Loc,
    },

    /// The same case named twice in one anonymous sum.
    DuplicateCaseInAnonSum {
        case: String,
        loc: Loc,
    },

    /// A record with both named and positional fields. It is addressed one way or
    /// the other, so it has to be written one way or the other.
    MixedFieldKinds {
        loc: Loc,
    },

    /// `_0`, `_1`, ... name positional fields, so they cannot be written as the
    /// name of one.
    ReservedFieldName {
        field: String,
        loc: Loc,
    },

    /// A `.` after something that is neither a field name nor a `{`.
    ExpectedLiteral {
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

    /// A literal qualified with a name that is neither a type nor a variable in
    /// scope. Whether a name is a type cannot be known while parsing — the
    /// declaration may be further down the file — so `foo.bar` is parsed as a
    /// qualified literal and reported here instead.
    NotATypeOrVariable {
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

    /// Two concrete records that are not the same record. Neither side can give
    /// way — a record written down is closed over exactly its fields — so unlike
    /// [`FloErr::UnexpectedField`] this is not about a literal at all.
    WrongFields {
        expected: Vec<String>,
        got: Vec<String>,
        loc: Loc,
    },

    /// A record literal gave a field that the record it turned out to be does not
    /// have. Fields it *left out* are not an error: those are zero initialized.
    UnexpectedField {
        ty: Type,
        field: String,
        loc: Loc,
    },

    /// A record where a sum belongs, or the other way round. Its own error rather
    /// than a plain mismatch because it is the easiest thing to get wrong: the two
    /// are built with different syntax, and only the type says which is wanted.
    RecordSumMismatch {
        record: Type,
        sum: Type,
        loc: Loc,
    },

    /// A case given a payload it does not take, or not given the one it does.
    /// `expected` is whether the case has a payload.
    PayloadMismatch {
        case: String,
        expected: bool,
        loc: Loc,
    },

    UnknownField {
        ty: Type,
        field: String,
        loc: Loc,
    },

    /// Field access on a sum. Which case a value holds is a runtime question, so
    /// a sum's payload is reached through `match` and never through `.` — and
    /// that holds for a one-case sum too, which is a choice of one and not a
    /// record.
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
                eprintln!("    a case is `Name` or `Name {{ .. }}`");
                eprintln!("    a `{{ .. }}` straight after the `=` declares a record instead");
                print_src(src, &[found.loc]);
            }
            NotACase { ty, loc } => {
                eprintln!("`{ty:?}` cannot be a case of a sum");
                eprintln!("    a case is a name, optionally with a `{{ .. }}` payload");
                print_src(src, &[loc]);
            }
            SingleCaseAnonSum { case, loc } => {
                eprintln!("`{case} {{ .. }}` is a sum of one case, which is not a type you can write");
                eprintln!("    write `{{ .. }}` for a record, or add a `| OtherCase` to make it a sum");
                print_src(src, &[loc]);
            }
            DuplicateCaseInAnonSum { case, loc } => {
                eprintln!("The case `{case}` appears more than once in this sum");
                print_src(src, &[loc]);
            }
            MixedFieldKinds { loc } => {
                eprintln!("This record mixes named and positional fields");
                eprintln!("    a record is addressed by name or by position, so it is written one way or the other");
                print_src(src, &[loc]);
            }
            ReservedFieldName { field, loc } => {
                eprintln!("`{field}` is the name of a positional field, so it cannot be given to one");
                print_src(src, &[loc]);
            }
            ExpectedLiteral { found } => {
                eprintln!(
                    "Expected a field name or `{{` after `.`, but found `{}`",
                    found.kind.pretty_name()
                );
                print_src(src, &[found.loc]);
            }
            NotImplemented { what, loc } => {
                eprintln!("`{what}` is not implemented yet");
                print_src(src, &[loc]);
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
                field,
                loc,
                prev_loc,
            } => {
                eprintln!("The field `{field}` is declared more than once");
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
            NotATypeOrVariable { name, loc } => {
                eprintln!("`{name}` is not a type, and no variable of that name is in scope");
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
                expected,
                got,
                loc,
            } => {
                eprintln!("This record has the fields {}", named(&expected));
                eprintln!("    but the other one has {}", named(&got));
                print_src(src, &[loc]);
            }
            UnexpectedField { ty, field, loc } => {
                eprintln!("`{ty:?}` has no field `{field}`");
                eprintln!("    a field left *out* of a literal is zero initialized, but one it does not have cannot go anywhere");
                print_src(src, &[loc]);
            }
            RecordSumMismatch { record, sum, loc } => {
                eprintln!("A record and a sum are never the same type");
                eprintln!("    `{record:?}` is a record -- built with `.{{ .. }}`");
                eprintln!("    `{sum:?}` is a sum -- built with `.Case`");
                print_src(src, &[loc]);
            }
            PayloadMismatch {
                case,
                expected,
                loc,
            } => {
                if expected {
                    eprintln!("The case `{case}` carries a payload, which this literal does not give");
                } else {
                    eprintln!("The case `{case}` carries nothing, so it takes no payload");
                }
                print_src(src, &[loc]);
            }
            UnknownField { ty, field, loc } => {
                eprintln!("Type `{ty:?}` has no field `{field}`");
                print_src(src, &[loc]);
            }
            FieldAccessOnSumType { ty, field, loc } => {
                eprintln!("Cannot read `{field}` of `{ty:?}`, which is a sum");
                eprintln!("    which case a sum holds is a runtime question, so its payload is reached with `match`");
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
            UnknownIdentifier { name, loc } => {
                eprintln!("Unknown identifier `{name}`");
                eprintln!(
                    "    it is not a variable in scope; a case of a type is written `.{name}`, or `Type.{name}`"
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
            TokenKind::Match => "match",
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
