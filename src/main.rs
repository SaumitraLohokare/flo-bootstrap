use std::process::exit;

use crate::{parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// TODO: Add arguments

fn main() {
    let src = r#"
        -- This is a comment
        fn main() -> i32 = 0;

        fn foo(a: i32) -> void = a;
    "#
    .to_string();

    let tokens = Tokenizer::new(&src).tokenize();
    let mut module = match Parser::new(tokens).parse() {
        Ok(module) => module,
        Err(err) => {
            err.pretty_print(src);
            exit(1);
        }
    };

    println!("{module:?}");

    if let Err(err) = TypeChecker::new().check(&mut module) {
        err.pretty_print(src);
        exit(1);
    }

    println!("{module:?}");
}
