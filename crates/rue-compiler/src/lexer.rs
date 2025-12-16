use crate::error::{CompileError, CompileResult, ErrorKind};

/// Source span for error reporting
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Keywords
    Fn,

    // Literals
    Int(i64),

    // Identifiers
    Ident(String),

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Arrow, // ->
    Colon,
    Semi,

    // Special
    Eof,
}

impl TokenKind {
    /// Get a human-readable name for this token kind.
    pub fn name(&self) -> &'static str {
        match self {
            TokenKind::Fn => "'fn'",
            TokenKind::Int(_) => "integer",
            TokenKind::Ident(_) => "identifier",
            TokenKind::LParen => "'('",
            TokenKind::RParen => "')'",
            TokenKind::LBrace => "'{'",
            TokenKind::RBrace => "'}'",
            TokenKind::Arrow => "'->'",
            TokenKind::Colon => "':'",
            TokenKind::Semi => "';'",
            TokenKind::Eof => "end of file",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub struct Lexer<'a> {
    source: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self { source, pos: 0 }
    }

    pub fn tokenize(&mut self) -> CompileResult<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            let is_eof = token.kind == TokenKind::Eof;
            tokens.push(token);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn next_token(&mut self) -> CompileResult<Token> {
        self.skip_whitespace();

        let start = self.pos;

        let Some(c) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span { start, end: start },
            });
        };

        let kind = match c {
            '(' => {
                self.advance();
                TokenKind::LParen
            }
            ')' => {
                self.advance();
                TokenKind::RParen
            }
            '{' => {
                self.advance();
                TokenKind::LBrace
            }
            '}' => {
                self.advance();
                TokenKind::RBrace
            }
            ':' => {
                self.advance();
                TokenKind::Colon
            }
            ';' => {
                self.advance();
                TokenKind::Semi
            }
            '-' => {
                self.advance();
                if self.peek() == Some('>') {
                    self.advance();
                    TokenKind::Arrow
                } else {
                    return Err(CompileError::new(
                        ErrorKind::UnexpectedCharacter('-'),
                        Span {
                            start,
                            end: self.pos,
                        },
                    ));
                }
            }
            '0'..='9' => self.lex_number()?,
            'a'..='z' | 'A'..='Z' | '_' => self.lex_ident_or_keyword(),
            _ => {
                self.advance();
                return Err(CompileError::new(
                    ErrorKind::UnexpectedCharacter(c),
                    Span {
                        start,
                        end: self.pos,
                    },
                ));
            }
        };

        Ok(Token {
            kind,
            span: Span {
                start,
                end: self.pos,
            },
        })
    }

    fn lex_number(&mut self) -> CompileResult<TokenKind> {
        let start = self.pos;
        while let Some('0'..='9') = self.peek() {
            self.advance();
        }
        let text = &self.source[start..self.pos];
        let value = text.parse().map_err(|_| {
            CompileError::new(
                ErrorKind::InvalidInteger,
                Span {
                    start,
                    end: self.pos,
                },
            )
        })?;
        Ok(TokenKind::Int(value))
    }

    fn lex_ident_or_keyword(&mut self) -> TokenKind {
        let start = self.pos;
        while let Some('a'..='z' | 'A'..='Z' | '0'..='9' | '_') = self.peek() {
            self.advance();
        }
        let text = &self.source[start..self.pos];
        match text {
            "fn" => TokenKind::Fn,
            _ => TokenKind::Ident(text.to_string()),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.advance();
            } else if c == '/' && self.peek_next() == Some('/') {
                // Line comment
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_next(&self) -> Option<char> {
        let mut chars = self.source[self.pos..].chars();
        chars.next();
        chars.next()
    }

    fn advance(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokens() {
        let mut lexer = Lexer::new("fn main() -> i32 { 42 }");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "main"));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert!(matches!(tokens[3].kind, TokenKind::RParen));
        assert!(matches!(tokens[4].kind, TokenKind::Arrow));
        assert!(matches!(tokens[5].kind, TokenKind::Ident(ref s) if s == "i32"));
        assert!(matches!(tokens[6].kind, TokenKind::LBrace));
        assert!(matches!(tokens[7].kind, TokenKind::Int(42)));
        assert!(matches!(tokens[8].kind, TokenKind::RBrace));
        assert!(matches!(tokens[9].kind, TokenKind::Eof));
    }

    #[test]
    fn test_unexpected_character() {
        let mut lexer = Lexer::new("fn main() { @ }");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnexpectedCharacter('@')));
    }
}
