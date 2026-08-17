type Foo struct {
    bar: i32,
    baz: i32,
}

void main() {
    println("Hello, World!");
}

i32 divide(a: i32, b: i32) errors {
    if b == 0 error;
    return a / b;
}

type IOErr struct {
    msg: string,
}

File open(path: string) errors[IOErr] {
    // ...
}

type ParseErr struct {
    msg: string,
    loc: struct { row: i32, col: i32 },
}

def parse(file: File) -> Vec<Token> errors[ParseErr] {
    // ...
}

def compile(path: string) -> Vec<Token> errors[ParseErr] {
    var file = open(path) or |e| log_abort(e);
    var tokens = parse(file)?;
    return tokens;
}
