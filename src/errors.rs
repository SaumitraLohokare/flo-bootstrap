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

    RedifinitionOfArgument {
        name: String,
        loc: Loc,
    },

    MainFunctionNotFound,
    /// `main` is the single specialize root and stays unmangled, so it can never
    /// be overloaded. Carries every `main` definition's loc so all are shown.
    MultipleMainDefinitions {
        locs: Vec<Loc>,
    },

    /// A call whose overload set could not be narrowed to a single candidate:
    /// two or more overloads remained possible after pruning.
    AmbiguousCall {
        name: String,
        loc: Loc,
        candidates: usize,
    },
    /// A call for which no overload matched (wrong argument types / kinds).
    NoMatchingOverload {
        name: String,
        loc: Loc,
    },

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
            FloErr::MultipleMainDefinitions { locs } => {
                eprintln!("`main` is defined more than once and cannot be overloaded");
                print_src(src, &locs);
            }
            FloErr::AmbiguousCall {
                name,
                loc,
                candidates,
            } => {
                if is_operator(&name) {
                    eprintln!(
                        "Operator `{name}` is ambiguous here: {candidates} overloads match"
                    );
                } else {
                    eprintln!("Call to `{name}` is ambiguous: {candidates} overloads match");
                }
                print_src(src, &[loc]);
            }
            FloErr::NoMatchingOverload { name, loc } => {
                if is_operator(&name) {
                    eprintln!("No overload of operator `{name}` matches these operands");
                } else {
                    eprintln!("No overload of `{name}` matches this call");
                }
                print_src(src, &[loc]);
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

/// Whether a resolved function name is actually a built-in operator. Operator
/// overloads are keyed by their symbol (`+`, `-`, …); user identifiers are always
/// alphanumeric/underscore, so a purely-symbolic name can only be an operator.
/// Lets overload errors on desugared operator calls read as operator errors.
fn is_operator(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| !c.is_ascii_alphanumeric() && c != '_')
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
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Ident => "identifier",
            TokenKind::Num => "number",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Arrow => "->",
            TokenKind::Equal => "=",
            TokenKind::Semicolon => ";",
            TokenKind::Colon => ":",
            TokenKind::Comma => ",",
            TokenKind::SingleQuote => "'",
            TokenKind::Exclamation => "!",
            TokenKind::AmpAmp => "&&",
            TokenKind::PipePipe => "||",
        }
    }
}
