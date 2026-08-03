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

    NotAType {
        token: Token,
    },

    UndefinedIdentifier {
        name: String,
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

    AmbiguousOverload {
        name: String,
        found_loc: Loc,
        previous_loc: Loc,
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
            UndefinedIdentifier { name, loc } => {
                eprintln!("Undefined identifier `{name}`");
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
            AmbiguousOverload {
                name,
                found_loc,
                previous_loc,
            } => {
                eprintln!("Ambiguous overload `{name}`");
                print_src(src, &[found_loc, previous_loc]);
            }
            ExpectedOp { found, loc } => {
                eprintln!("Expected an operator but found: `{}`", found.pretty_name());
                print_src(src, &[loc]);
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
            TokenKind::Comma => ",",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Amp => "&",
            TokenKind::Pipe => "|",
            TokenKind::Cap => "^",
            TokenKind::AmpAmp => "&&",
            TokenKind::PipePipe => "||",
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
            TokenKind::Defer => "defer",
            TokenKind::Let => "let",
        }
    }
}
