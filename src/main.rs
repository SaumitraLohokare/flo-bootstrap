use std::process::exit;

use crate::{lower::lower, parser::Parser, tokenizer::Tokenizer, type_checker::TypeChecker};

mod ast;
mod errors;
mod lower;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// FIXME: Errors are printed randomly, because we check functions by iterating HashMap
// FIXME: Add bitwise shift/not & logical not
// FIXME: No warnings for using an uninitialized variable
// FIXME: Scopes, If, While need a `;` after them.

// Then: Pointers -> Arrays & Slices -> Strings
// Then: Generics -> Sum Types -> is & Destructuring
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        fn main() -> i32 = defer_ex(10);

        fn defer_ex(n: i32) -> i32 = {
            let acc = 0;
            let i = 0;

            while i < n {
                -- Runs at the end of every iteration, `continue` and `break`
                -- included, so the loop always makes progress.
                defer i = i + 1;

                if i % 2 == 0 {
                    continue;
                };

                if i > 7 {
                    break;
                };

                acc = acc + i;
            };

            acc
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

    // `defer` is the only thing lowering touches so far, so this runs on a
    // fully typed module and cannot fail.
    let module = lower(module);

    println!("{module:?}");
}
