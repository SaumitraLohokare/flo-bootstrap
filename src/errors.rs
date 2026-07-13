use crate::{
    tokenizer::{Loc, Token, TokenKind},
    types::{Type, TypeKind},
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

    RedifinitionOfFunction {
        name: String,
        loc: Loc,
    },

    RedifinitionOfArgument {
        name: String,
        loc: Loc,
    },

    MainFunctionNotFound,

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

    CallArityMismatch {
        expected: usize,
        got: usize,
        loc: Loc,
    },

    UnsatisfiedTypeKind {
        ty: Type,
        ty_loc: Loc,
        kind: TypeKind,
        loc: Loc,
    },
    TypeMismatch {
        t1: Type,
        loc1: Loc,
        t2: Type,
        loc2: Loc,
    },
    UnresolvedType {
        loc: Loc,
    },
}

impl FloErr {
    pub fn pretty_print(self, src: &String) {
        eprint!("Error: ");

        match self {
            FloErr::UnexpectedEOF => eprintln!("Unexpected EOF"),
            FloErr::UnexpectedToken { found } => {
                eprintln!("Unexpected token `{}`", found.kind.pretty_name());
                print_src(src, &[found.loc]);
            }
            FloErr::ExpectedTokenNotFound { expected, found } => {
                eprintln!(
                    "Expected `{}`, but found `{}`",
                    expected.pretty_name(),
                    found.kind.pretty_name()
                );
                print_src(src, &[found.loc]);
            }
            FloErr::RedifinitionOfFunction { name, loc } => {
                eprintln!("Redifinition of function `{name}`");
                print_src(src, &[loc]);
            }
            FloErr::RedifinitionOfArgument { name, loc } => {
                eprintln!("Redifinition of argument `{name}`");
                print_src(src, &[loc]);
            }
            FloErr::NotAType { token } => {
                eprintln!("`{}` is not a type", token.kind.pretty_name(),);
                print_src(src, &[token.loc]);
            }
            FloErr::UnsatisfiedTypeKind {
                ty,
                ty_loc,
                kind,
                loc,
            } => {
                eprintln!("Type `{kind:?}` does not match `{ty:?}`");
                print_src(src, &[ty_loc, loc]);
            }
            FloErr::TypeMismatch { t1, loc1, t2, loc2 } => {
                eprintln!("Type `{t1:?}` does not match `{t2:?}`");
                print_src(src, &[loc1, loc2]);
            }
            FloErr::UnresolvedType { loc } => {
                eprintln!("Type of expression could not be resolved");
                print_src(src, &[loc]);
            }
            FloErr::MainFunctionNotFound => {
                eprintln!("Main function not found.");
            }
            FloErr::UndefinedIdentifier { name, loc } => {
                eprintln!("Undefined identifier `{name}`");
                print_src(src, &[loc]);
            }
            FloErr::UndefinedFunction { name, loc } => {
                eprintln!("Undefined function `{name}`");
                print_src(src, &[loc]);
            }
            FloErr::CallArityMismatch { expected, got, loc } => {
                eprintln!("Function call expected {expected} arguments, but got {got} instead");
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
    fn pretty_name(&self) -> &str {
        match self {
            TokenKind::Fn => "fn",
            TokenKind::Ident => "identifier",
            TokenKind::Num => "number",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::Arrow => "->",
            TokenKind::Equal => "=",
            TokenKind::Semicolon => ";",
            TokenKind::Colon => ":",
            TokenKind::Comma => ",",
            TokenKind::SingleQuote => "'",
        }
    }
}
