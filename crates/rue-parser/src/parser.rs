//! Parser for the Rue programming language.
//!
//! Converts a sequence of tokens into an AST.

use crate::ast::{
    Ast, BinaryExpr, BinaryOp, Expr, Function, Ident, IntLit, Item, ParenExpr, UnaryExpr, UnaryOp,
};
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

    /// Parse an expression (entry point).
    fn parse_expr(&mut self) -> CompileResult<Expr> {
        self.parse_additive()
    }

    /// Parse additive expressions (+, -).
    /// Lower precedence than multiplicative.
    fn parse_additive(&mut self) -> CompileResult<Expr> {
        let mut left = self.parse_multiplicative()?;

        while matches!(self.current().kind, TokenKind::Plus | TokenKind::Minus) {
            let op_token = self.advance();
            let op = match op_token.kind {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => unreachable!(),
            };

            let right = self.parse_multiplicative()?;
            let span = Span::new(left.span().start, right.span().end);

            left = Expr::Binary(BinaryExpr {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            });
        }

        Ok(left)
    }

    /// Parse multiplicative expressions (*, /, %).
    /// Higher precedence than additive.
    fn parse_multiplicative(&mut self) -> CompileResult<Expr> {
        let mut left = self.parse_unary()?;

        while matches!(
            self.current().kind,
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent
        ) {
            let op_token = self.advance();
            let op = match op_token.kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Mod,
                _ => unreachable!(),
            };

            let right = self.parse_unary()?;
            let span = Span::new(left.span().start, right.span().end);

            left = Expr::Binary(BinaryExpr {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            });
        }

        Ok(left)
    }

    /// Parse unary expressions (-x).
    /// Highest precedence (binds tightest).
    fn parse_unary(&mut self) -> CompileResult<Expr> {
        if matches!(self.current().kind, TokenKind::Minus) {
            let op_token = self.advance();
            let operand = self.parse_unary()?; // Recursive for --x
            let span = Span::new(op_token.span.start, operand.span().end);

            Ok(Expr::Unary(UnaryExpr {
                op: UnaryOp::Neg,
                operand: Box::new(operand),
                span,
            }))
        } else {
            self.parse_primary()
        }
    }

    /// Parse primary expressions (literals, parenthesized expressions).
    fn parse_primary(&mut self) -> CompileResult<Expr> {
        let token = self.current().clone();

        match &token.kind {
            TokenKind::Int(n) => {
                let value = *n;
                self.advance();
                Ok(Expr::Int(IntLit {
                    value,
                    span: token.span,
                }))
            }
            TokenKind::LParen => {
                let start = token.span.start;
                self.advance(); // consume '('
                let inner = self.parse_expr()?;
                let close = self.expect(TokenKind::RParen)?;
                let span = Span::new(start, close.span.end);

                Ok(Expr::Paren(ParenExpr {
                    inner: Box::new(inner),
                    span,
                }))
            }
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

    fn parse(source: &str) -> CompileResult<Ast> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize()?;
        let mut parser = Parser::new(tokens);
        parser.parse()
    }

    fn parse_expr(source: &str) -> CompileResult<Expr> {
        let ast = parse(&format!("fn main() -> i32 {{ {} }}", source))?;
        match ast.items.into_iter().next().unwrap() {
            Item::Function(f) => Ok(f.body),
        }
    }

    #[test]
    fn test_parse_main() {
        let ast = parse("fn main() -> i32 { 42 }").unwrap();

        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.name.name, "main");
                assert_eq!(f.return_type.name, "i32");
                match &f.body {
                    Expr::Int(lit) => assert_eq!(lit.value, 42),
                    _ => panic!("expected Int"),
                }
            }
        }
    }

    #[test]
    fn test_missing_return_type() {
        let result = parse("fn main() { 42 }");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_addition() {
        let expr = parse_expr("1 + 2").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Add));
                match (*bin.left, *bin.right) {
                    (Expr::Int(l), Expr::Int(r)) => {
                        assert_eq!(l.value, 1);
                        assert_eq!(r.value, 2);
                    }
                    _ => panic!("expected Int operands"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let expr = parse_expr("1 + 2 * 3").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Add));
                match *bin.left {
                    Expr::Int(l) => assert_eq!(l.value, 1),
                    _ => panic!("expected Int"),
                }
                match *bin.right {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::Mul));
                    }
                    _ => panic!("expected Binary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_parens() {
        // (1 + 2) * 3 should parse as (1 + 2) * 3
        let expr = parse_expr("(1 + 2) * 3").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Mul));
                match *bin.left {
                    Expr::Paren(p) => match *p.inner {
                        Expr::Binary(inner) => {
                            assert!(matches!(inner.op, BinaryOp::Add));
                        }
                        _ => panic!("expected Binary inside paren"),
                    },
                    _ => panic!("expected Paren"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_unary_negation() {
        let expr = parse_expr("-42").unwrap();
        match expr {
            Expr::Unary(un) => {
                assert!(matches!(un.op, UnaryOp::Neg));
                match *un.operand {
                    Expr::Int(lit) => assert_eq!(lit.value, 42),
                    _ => panic!("expected Int"),
                }
            }
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn test_parse_double_negation() {
        let expr = parse_expr("--5").unwrap();
        match expr {
            Expr::Unary(outer) => {
                assert!(matches!(outer.op, UnaryOp::Neg));
                match *outer.operand {
                    Expr::Unary(inner) => {
                        assert!(matches!(inner.op, UnaryOp::Neg));
                        match *inner.operand {
                            Expr::Int(lit) => assert_eq!(lit.value, 5),
                            _ => panic!("expected Int"),
                        }
                    }
                    _ => panic!("expected Unary"),
                }
            }
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn test_parse_negation_precedence() {
        // -2 * 3 should parse as (-2) * 3
        let expr = parse_expr("-2 * 3").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Mul));
                match *bin.left {
                    Expr::Unary(un) => {
                        assert!(matches!(un.op, UnaryOp::Neg));
                    }
                    _ => panic!("expected Unary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_all_operators() {
        // Test all binary operators
        assert!(parse_expr("1 + 2").is_ok());
        assert!(parse_expr("1 - 2").is_ok());
        assert!(parse_expr("1 * 2").is_ok());
        assert!(parse_expr("1 / 2").is_ok());
        assert!(parse_expr("1 % 2").is_ok());
    }

    #[test]
    fn test_left_associativity() {
        // 10 - 3 - 2 should parse as (10 - 3) - 2
        let expr = parse_expr("10 - 3 - 2").unwrap();
        match expr {
            Expr::Binary(outer) => {
                assert!(matches!(outer.op, BinaryOp::Sub));
                match *outer.right {
                    Expr::Int(lit) => assert_eq!(lit.value, 2),
                    _ => panic!("expected Int"),
                }
                match *outer.left {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::Sub));
                    }
                    _ => panic!("expected Binary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }
}
