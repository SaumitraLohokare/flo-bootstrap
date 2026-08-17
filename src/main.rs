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

// Then: match & Patterns & Exhaustiveness
// Then: Pointers -> Arrays & Slices -> Strings
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        type Vec2 = { x: i32, y: i32 };         -- a record

        type Option<T> = Some { T } | None;     -- a sum, with a positional payload

        type Player = {
            pos: { x: i32, y: i32 },            -- an anonymous record
            dim: { w: i32, h: i32 },
            status: Alive | Dead,               -- an anonymous sum
        };

        op +(a: Vec2, b: Vec2) -> Vec2 = .{ x: a.x + b.x, y: a.y + b.y };

        fn main() -> i32 = {
            let a = .{ x: 1, y: 2 };            -- inferred from the `+` below
            let b = Vec2.{ y: 4, x: 3 };        -- qualified; fields in any order
            let sum = a + b;

            let some = wrap(sum.x);             -- Option<i32>, inferred
            let none: Option<i32> = .None;
            let two: Option<i32> = Option.Some.{ sum.y };

            -- `dim` is left out, so it is zero initialized
            let p: Player = .{ pos: .{ x: 0, y: 0 }, status: .Alive };

            sum.x + sum.y + p.pos.x
        };

        fn wrap<T>(v: T) -> Option<T> = .Some .{ v };
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
