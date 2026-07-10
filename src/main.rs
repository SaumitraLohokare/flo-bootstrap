use std::process::exit;

use crate::{parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// TODO: Errors are printed randomly, because we check functions by iterating HashMap
// TODO: Pipe, Scope + Variables, Operators

fn main() {
    let src = r#"
        -- This is a comment
        fn main() -> i32 = foo(0);

        fn foo(a: i32) -> void = id(a);

        fn id(a: i32) -> void = a;
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

    let errs = TypeChecker::new().check(&mut module);
    if !errs.is_empty() {
        for err in errs {
            err.pretty_print(&src);
        }
        exit(1);
    }

    println!("{module:?}");
}
