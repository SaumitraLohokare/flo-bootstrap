use std::process::exit;

use crate::{
    parser::{Parser, check_entry_point},
    tokenizer::Tokenizer,
    type_checker::TypeChecker,
};

mod ast;
mod errors;
mod parser;
mod tokenizer;
mod type_checker;
mod types;
mod util;

// FIXME: Errors are printed randomly, because we check functions by iterating HashMap
// FIXME: No warnings for using an uninitialized variable
// FIXME: No warning when @cast's two types are of different sizes (needs a warning sink)
// FIXME: Tokenizer panics instead of reporting an error (unknown char, bad number)

// Then: Pointers -> Arrays & Slices -> Strings
// Then: is & Destructuring
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        type Vec2 = { x: i32, y: i32 };         -- one case, named after the type

        type Option<T> = Some { val: T } | None;

        use Vec2::Vec2;                         -- file scope, and order does not
        use Option::None;                       -- matter: `wrap` below uses `Some`

        op +(a: Vec2, b: Vec2) -> Vec2 = Vec2 { x: a.x + b.x, y: a.y + b.y };

        fn main() -> i32 = {
            let a = Vec2 { x: 1, y: 2 };
            let b = Vec2 { y: 4, x: 3 };        -- fields in any order
            let sum = a + b;

            let some = wrap::<u8>(7);           -- Option<u8>
            let none: Option<i32> = None;       -- the case alone gives the type

            -- Written out, so no `use` is needed. The type argument is left to
            -- inference, which takes it from the annotation.
            let two: Option<i32> = Option::Some { val: sum.x };

            sum.x + sum.y
        };

        use Option::Some;

        fn wrap<T>(v: T) -> Option<T> = Some { val: v };
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

    if let Err(err) = check_entry_point(&module) {
        err.pretty_print(&src);
        exit(1);
    }

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
