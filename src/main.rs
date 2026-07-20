use std::process::exit;

use crate::{parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// DONE: Add booleans
// DONE: Add scope
// TODO: Add If Else + early returns
// TODO: Add writing custom operator overloads
// TODO: Add variables
// TODO: Add While + Continue/Break
// TODO: Add |>
// TODO: Add defer
// TODO: Add other primitive types
// TODO: Add pointers
// TODO: Add Arrays & Slices
// TODO: Add Strings
// TODO: Add globals
// TODO: Add defining external functions/globals
// TODO: Add structs
// TODO: Add enums
// TODO: Add match
// TODO: Work on interpreter
// TODO: Add support for multiple files
// TODO: Implement compiler in flo

fn main() {
    let src = r#"
        -- This is a comment

        -- Arithmetic operators desugar to calls against built-in overloads and
        -- are type-checked like any other function. Bare literals default to i32
        -- (resolve -> default -> resolve), so no annotations are needed here.

        fn main() = 1 - i32(is_even(2));
 
        fn is_even(n) = n % 2 == 0;
        fn i32(b) = if b { 1 } else { 0 };
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
