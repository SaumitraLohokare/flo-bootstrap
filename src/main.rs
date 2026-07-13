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

        -- fn nop() -> void = {};

        -- fn main() = nop();

        fn main() = add(id(1), id(1));
        fn id(a, b) = a;
        fn add(a: i32, b: u8) = b;
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
