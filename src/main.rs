use std::process::exit;

use crate::{parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod util;
mod parser;
mod tokenizer;
mod type_checker;
mod types;

fn main() {
    let src = r#"
        -- This is a comment
        fn main() -> i32 = 0;
        fn foo() -> i32 = 0;
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
    
    match TypeChecker::new().check(&mut module) {
        Err(err) => {
            err.pretty_print(src);
            exit(1);
        }
        _ => {}
    }

    println!("INFO: Type checking done.\n");
    println!("{module:?}");
}
