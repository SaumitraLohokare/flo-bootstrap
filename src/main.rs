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

// TODO: Variables
//      - [ ] Add let/mut token
//      - [ ] parse variable declaration
//      - [ ] Need to maintain somewhere that a certain variable is mutable or not
//      - [ ] Immutable variable cannot appear on lhs of assign (is_l_value())
//      - [ ] I might leave it as a declaration expression in the AST (Will help with type checking)
//            because we can store the expected type.
//      - [ ] Also need to add the assign expr (Or it could just be a binary op too)
//      - [ ] let & mut expressions return bool, if they were able to assign or not.
//            This way we get if let and while let for free

// Then: Globals -> While & Break/Continue -> Defer
// Then: Pointers -> Arrays & Slices -> Strings
// Then: Generics -> Sum Types -> Destructuring -> Switch
// Then: Modules & Project Structure
// Then: C Transpiling -> External Funcs -> Compiler Directives (@windows/@linux/@macos/@extern/@link)

fn main() {
    let src = r#"
        fn main() -> i32 = let_ex(2);

        fn let_ex(n: i32) -> i32 = {
            let sqr_n = n * n;
            let sqr_n: i32 = sqr_n;
            sqr_n
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
