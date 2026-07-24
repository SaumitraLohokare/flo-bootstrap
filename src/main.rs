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
// FIXME: Combine parse_func & parse_op_overload
// FIXME: Please remove var_iota from Scope

// TODO: Scope
//      - [ ] Add the tokens
//      - [ ] Add it to ExprKind
//      - [ ] write parse_scope (creates a new scope)
//      - [ ] fix errors in type checker
//          - [ ] Ensure type of scope is bound correctly

// Then: If & Return -> Variables & Globals -> While & Break/Continue -> Defer
// Then: Pointers -> Arrays & Slices -> Strings
// Then: Records & Sum Types -> Destructuring -> Match & Semantic Analysis
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        fn main() -> bool = {
            nop();
            true && false
        };

        fn nop() -> void = {};
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
