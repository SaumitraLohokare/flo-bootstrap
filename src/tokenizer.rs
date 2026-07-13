#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Fn,

    Ident,
    Num,

    LParen,
    RParen,

    Colon,
    Comma,
    Arrow,

    SingleQuote,

    Equal,

    Semicolon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenValue {
    None,
    String(String),
    Num(u64),
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct Loc {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub value: TokenValue,
    pub loc: Loc,
}

// ---------------------------

pub struct Tokenizer {
    chars: Vec<char>,
    idx: usize,
}

impl Tokenizer {
    pub fn new(src: &str) -> Self {
        Self {
            chars: src.chars().collect(),
            idx: 0,
        }
    }

    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();

        while let Some(ch) = self.peek() {
            match ch {
                x if x.is_whitespace() => self.skip(),

                x if x.is_ascii_alphabetic() || x == '_' => tokens.push(self.tokenize_word()),
                x if x.is_ascii_digit() => tokens.push(self.tokenize_number()),

                '-' if self.peek_n(1) == Some('-') => self.skip_comment(),

                '-' if self.peek_n(1) == Some('>') => {
                    tokens.push(self.tokenize_symbol("->", TokenKind::Arrow))
                }

                '(' => tokens.push(self.tokenize_symbol("(", TokenKind::LParen)),
                ')' => tokens.push(self.tokenize_symbol(")", TokenKind::RParen)),

                '\'' => tokens.push(self.tokenize_symbol("'", TokenKind::SingleQuote)),

                ':' => tokens.push(self.tokenize_symbol(":", TokenKind::Colon)),
                ',' => tokens.push(self.tokenize_symbol(",", TokenKind::Comma)),

                '=' => tokens.push(self.tokenize_symbol("=", TokenKind::Equal)),

                ';' => tokens.push(self.tokenize_symbol(";", TokenKind::Semicolon)),

                _ => panic!("Unknown char: {ch}"),
            }
        }

        tokens
    }

    fn tokenize_word(&mut self) -> Token {
        let mut word = String::new();
        let start = self.idx;

        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                word.push(ch);
                self.skip();
            } else {
                break;
            }
        }

        let kind = match word.as_str() {
            "fn" => TokenKind::Fn,

            _ => TokenKind::Ident,
        };

        let value = if kind == TokenKind::Ident {
            TokenValue::String(word)
        } else {
            TokenValue::None
        };

        let loc = Loc {
            start,
            end: self.idx - 1,
        };

        Token { kind, value, loc }
    }

    fn tokenize_number(&mut self) -> Token {
        let mut num = String::new();
        let start = self.idx;

        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                num.push(ch);
                self.skip();
            } else {
                break;
            }
        }

        let kind = TokenKind::Num;
        let value = TokenValue::Num(num.parse::<u64>().unwrap());
        let loc = Loc {
            start,
            end: self.idx - 1,
        };

        Token { kind, value, loc }
    }

    fn tokenize_symbol(&mut self, symbol: &str, kind: TokenKind) -> Token {
        let start = self.idx;
        self.skip_n(symbol.len());
        let loc = Loc {
            start,
            end: self.idx - 1,
        };
        Token {
            kind,
            value: TokenValue::None,
            loc,
        }
    }

    fn skip_comment(&mut self) {
        while let Some(ch) = self.peek()
            && ch != '\n'
        {
            self.skip();
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.idx).copied()
    }

    fn peek_n(&self, n: usize) -> Option<char> {
        self.chars.get(self.idx + n).copied()
    }

    fn skip(&mut self) {
        self.idx += 1;
    }

    fn skip_n(&mut self, n: usize) {
        self.idx += n;
    }
}
