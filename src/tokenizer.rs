#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Fn,

    True,
    False,

    Ident,
    Num,
    Flt,

    LParen,
    RParen,

    Colon,
    Comma,
    Arrow,

    PipeGreaterThan,

    Equal,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,

    Amp,
    Pipe,
    Cap,

    AmpAmp,
    PipePipe,

    EqualEqual,
    BangEqual,
    LessThan,
    GreaterThan,
    LessThanEqual,
    GreaterThanEqual,

    Semicolon,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenValue {
    None,
    String(String),
    Num(u64),
    Flt(f64),
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct Loc {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq)]
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

                ':' => tokens.push(self.tokenize_symbol(":", TokenKind::Colon)),
                ',' => tokens.push(self.tokenize_symbol(",", TokenKind::Comma)),

                '=' if self.peek_n(1) == Some('=') => {
                    tokens.push(self.tokenize_symbol("==", TokenKind::EqualEqual))
                }
                '=' => tokens.push(self.tokenize_symbol("=", TokenKind::Equal)),

                '!' if self.peek_n(1) == Some('=') => {
                    tokens.push(self.tokenize_symbol("!=", TokenKind::BangEqual))
                }

                '<' if self.peek_n(1) == Some('=') => {
                    tokens.push(self.tokenize_symbol("<=", TokenKind::LessThanEqual))
                }
                '<' => tokens.push(self.tokenize_symbol("<", TokenKind::LessThan)),

                '>' if self.peek_n(1) == Some('=') => {
                    tokens.push(self.tokenize_symbol(">=", TokenKind::GreaterThanEqual))
                }
                '>' => tokens.push(self.tokenize_symbol(">", TokenKind::GreaterThan)),

                '+' => tokens.push(self.tokenize_symbol("+", TokenKind::Plus)),
                '-' => tokens.push(self.tokenize_symbol("-", TokenKind::Minus)),
                '*' => tokens.push(self.tokenize_symbol("*", TokenKind::Star)),
                '/' => tokens.push(self.tokenize_symbol("/", TokenKind::Slash)),
                '%' => tokens.push(self.tokenize_symbol("%", TokenKind::Percent)),

                '&' if self.peek_n(1) == Some('&') => {
                    tokens.push(self.tokenize_symbol("&&", TokenKind::AmpAmp))
                }
                '&' => tokens.push(self.tokenize_symbol("&", TokenKind::Amp)),

                '|' if self.peek_n(1) == Some('|') => {
                    tokens.push(self.tokenize_symbol("||", TokenKind::PipePipe))
                }
                '|' if self.peek_n(1) == Some('>') => {
                    tokens.push(self.tokenize_symbol("|>", TokenKind::PipeGreaterThan))
                }
                '|' => tokens.push(self.tokenize_symbol("|", TokenKind::Pipe)),

                '^' => tokens.push(self.tokenize_symbol("^", TokenKind::Cap)),

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
            "true" => TokenKind::True,
            "false" => TokenKind::False,

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

        let mut is_decimal = false;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                num.push(ch);
                self.skip();
            } else if ch == '.' && !is_decimal {
                is_decimal = true;
                num.push(ch);
                self.skip();
            } else {
                break;
            }
        }

        let kind = if !is_decimal {
            TokenKind::Num
        } else {
            TokenKind::Flt
        };

        let value = if !is_decimal {
            TokenValue::Num(num.parse::<u64>().unwrap())
        } else {
            TokenValue::Flt(num.parse::<f64>().unwrap())
        };

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
