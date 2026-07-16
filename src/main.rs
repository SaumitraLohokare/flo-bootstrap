use std::process::exit;

use crate::{parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// TODO: Allow parsing these functions

fn main() {
    let src = r#"
        -- This is a comment

        -- Arithmetic operators desugar to calls against built-in overloads and
        -- are type-checked like any other function. Bare literals default to i32
        -- (resolve -> default -> resolve), so no annotations are needed here.

        fn main() = double(1 + 2 * 3) - -4;
        fn double(x) = x + is_even(x);

        fn is_even(n) = n % 2;
    "#
    .to_string();

    let tokens = Tokenizer::new(&src).tokenize();
    let mut module = match Parser::new(tokens).parse() {
        Ok(module) => module,
        Err(err) => {
            err.pretty_print(&src);
            exit(1);
        }
    };

    println!("{module:?}");

    let resolved = match TypeChecker::new(&mut module).check() {
        Ok(resolved) => resolved,
        Err(errs) => {
            for err in errs {
                err.pretty_print(&src);
            }
            exit(1);
        }
    };

    println!("{resolved:?}");
}
