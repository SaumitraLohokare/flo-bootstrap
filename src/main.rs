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

// TODO: If & Return
//      - [ ] register new tokens
//      - [ ] Parse if exprs in parse_atom
//      - [ ] in type checker ensure types are assigned correctly
// Return:
//      - [ ] Add return token
//      - [ ] parse return expr in parse_atom
//      - [ ] Add new NoReturn Type
//      - [ ] Add proper type checking for return

// Then: Variables & Globals -> While & Break/Continue -> Defer
// Then: Pointers -> Arrays & Slices -> Strings
// Then: Records & Sum Types -> Destructuring -> Match & Semantic Analysis
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        fn main() -> i32 = true |> i32;
        fn i32(b: bool) -> i32 = if b { 1 } else 0;
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
