# Flo Programming Language 1.0

This document specifies what features Flo will have at 1.0 & how I'd implement it.

## Features

- Functions & Overloading
- Multi-File & import & pub
- Types & switch & match & .
- use
- Generics
- Scopes & Statements (Let & Mut)
- If-Else & While & For
- Pointers & Mut
- Arrays & Destructuring
- Slices & Destructuring
- Strings
- Operators & Operator Functions
- Interpreter
- Globals

## How I'd Implement It

### Stage 1:

Start with a single file support. It reads the file, tokenizes it, parses it, type checks it, then lowers it to our intermediate representation, then lowers it to assembly based on the target & uses Clang to compile the assembly.

This means we would need to build a Tokenizer that produces a list of tokens for a given file.
Then implement a Parser that parses the tokens and produces an AST.
Then implement a TypeChecker that type checks the AST with support for inference.
Then we lower the AST down to an IR. This IR still has concepts of variables & compound types. It's only use is for optimization.
Then we implement a Codegen pass that lowers the IR to windows-x64, linux-x64, and macos-arm64.
Then calls clang on the generated assembly.

Source String -> Tokens -> AST -> IR -> ASM -> Exe

Token:
    - kind: TokenKind
    - value: 
        - NoValue = For symbols and keywords
        - String  = For identifiers
        - uint64  = For integer literals

Func:
    - Internal(expr)      = Functions defined in current file
    - External(file_name) = Functions defined in other file
    - ExternalUnknown     = Functions not defined in our program

AST:
    - funcs: map(string -> []Func)   = Because we allow overloading
    - types: map(string -> Type)     = Named types
    - imports: map(string -> string) = namespace to source file

AST.Expr:
    - Int(n)                                                                      = For integer literals
    - Var(optional(namespace), name)                                              = For args & variables
    - Call(optional(namespace), name, []args)                                     = For function calls
    - Enum(optional(namespace), optional(typename), casename, map(field -> expr)) = For type literals
    - Switch(cond, [] destructure -> expr)                                        = Switch
    - Match(cond, [] destructure -> expr)                                         = Match
    - Access(expr, field)                                                         = Field access

AST.Type:
    - Internal(TypeKind)  = Types defined in our file
    - External(file_name) = Types defined in other file

Program:
    - modules: map(string -> AST) = File Name to AST

TypeChecker.Constraints:
    - Equal(type_1, type_2)                    = To unify types
    - Overload(func_name, arg_types, ret_type) = To resolve a call to it's correct overload
    - Struct(type)                             = Field access says it's a struct (single case)

TypeChecker.TypeKind:
    - Integer         = For unknown integer literal
    - I8-I64          = For signed integers
    - U8-U64          = For unsigned integers
    - Void            = C void
    - Fn([]args, ret) = For functions
    - SomeEnum([]Case { map(field -> TypeKind )})  = For unknown types
    - AnonEnum([]Case { map(field -> TypeKind )})  = For anonymous types
    - KnownEnum([]Case { map(field -> TypeKind )}) = For named types

IR.Inst:
    - set dest, src
    - call dest, name, [args]
    - ret optional(src)