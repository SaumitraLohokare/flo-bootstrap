use std::process::exit;

use crate::{parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// FIXME: Errors are printed randomly, because we check functions by iterating HashMap
// FIXME: Add bitwise shift/not & logical not
// FIXME: No warnings for using an uninitialized variable

// Then: While & Break/Continue -> Defer
// Then: Pointers -> Arrays & Slices -> Strings
// Then: Generics -> Sum Types -> is & Destructuring
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        fn main() -> i32 = let_ex(2);

        fn let_ex(n: i32) -> i32 = {
            let sqr_n = n * n;
            let sqr_n: i32 = sqr_n;

            if false {
                sqr_n = 0;
            };

            let acc;
            acc = sqr_n + 1;

            let copy = acc = acc * 2;
            copy
        };
    "#
    .to_string();

    let tokens = Tokenizer::new(&src).tokenize();
    let module = match Parser::new(tokens).parse() {
        Ok(module) => module,
        Err(err) => {
            err.pretty_print(&src);
            exit(1);
        }
    };

    println!("{module:?}");

    let module = match TypeChecker::new().check(module) {
        Ok(module) => module,
        Err(errs) => {
            for err in errs {
                err.pretty_print(&src);
            }
            exit(1);
        }
    };

    println!("{module:?}");
}
