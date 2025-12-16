//! Parser for the Rue programming language.
//!
//! Converts a sequence of tokens into an AST.

use crate::ast::{Ast, Expr, Function, Ident, IntLit, Item};
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_lexer::{Token, TokenKind};
use rue_span::Span;

/// Parser that converts tokens into an AST.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    /// Create a new parser for the given tokens.
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    /// Parse the tokens into an AST.
    pub fn parse(&mut self) -> CompileResult<Ast> {
        let mut items = Vec::new();

        while !self.is_at_end() {
            items.push(self.parse_item()?);
        }

        Ok(Ast { items })
    }

    fn parse_item(&mut self) -> CompileResult<Item> {
        // Currently only functions are supported
        Ok(Item::Function(self.parse_function()?))
    }

    fn parse_function(&mut self) -> CompileResult<Function> {
        let start = self.current().span.start;

        // fn
        self.expect(TokenKind::Fn)?;

        // name
        let name = self.expect_ident()?;

        // ()
        self.expect(TokenKind::LParen)?;
        self.expect(TokenKind::RParen)?;

        // -> Type
        self.expect(TokenKind::Arrow)?;
        let return_type = self.expect_ident()?;

        // { body }
        self.expect(TokenKind::LBrace)?;
        let body = self.parse_expr()?;
        self.expect(TokenKind::RBrace)?;

        let end = self.tokens[self.pos.saturating_sub(1)].span.end;

        Ok(Function {
            name,
            return_type,
            body,
            span: Span::new(start, end),
        })
    }

    fn parse_expr(&mut self) -> CompileResult<Expr> {
        let token = self.advance();
        match &token.kind {
            TokenKind::Int(n) => Ok(Expr::Int(IntLit {
                value: *n,
                span: token.span,
            })),
            _ => Err(CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: "expression",
                    found: token.kind.name().to_string(),
                },
                token.span,
            )),
        }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind)
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if !self.is_at_end() {
            self.pos += 1;
        }
        token
    }

    fn expect(&mut self, expected: TokenKind) -> CompileResult<Token> {
        if self.is_at_end() {
            return Err(CompileError::new(
                ErrorKind::UnexpectedEof {
                    expected: expected.name(),
                },
                self.current().span,
            ));
        }
        if !self.check(&expected) {
            let current = self.current();
            return Err(CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: expected.name(),
                    found: current.kind.name().to_string(),
                },
                current.span,
            ));
        }
        Ok(self.advance())
    }

    fn expect_ident(&mut self) -> CompileResult<Ident> {
        let token = self.advance();
        match token.kind {
            TokenKind::Ident(name) => Ok(Ident {
                name,
                span: token.span,
            }),
            TokenKind::Eof => Err(CompileError::new(
                ErrorKind::UnexpectedEof {
                    expected: "identifier",
                },
                token.span,
            )),
            _ => Err(CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: "identifier",
                    found: token.kind.name().to_string(),
                },
                token.span,
            )),
        }
    }

    fn is_at_end(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    #[test]
    fn test_parse_main() {
        let mut lexer = Lexer::new("fn main() -> i32 { 42 }");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.name.name, "main");
                assert_eq!(f.return_type.name, "i32");
                match &f.body {
                    Expr::Int(lit) => assert_eq!(lit.value, 42),
                }
            }
        }
    }

    #[test]
    fn test_missing_return_type() {
        let mut lexer = Lexer::new("fn main() { 42 }");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let result = parser.parse();
        assert!(result.is_err());
    }
}
