use crate::error::{CompileError, CompileResult, ErrorKind};
use crate::lexer::{Span, Token, TokenKind};

/// A complete program (list of functions)
#[derive(Debug)]
pub struct Program {
    pub functions: Vec<Function>,
}

/// A function definition
#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub return_type: String,
    pub body: Expr,
    pub span: Span,
}

/// An expression
#[derive(Debug)]
pub enum Expr {
    /// Integer literal
    Int(i64, Span),
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(&mut self) -> CompileResult<Program> {
        let mut functions = Vec::new();

        while !self.is_at_end() {
            functions.push(self.parse_function()?);
        }

        Ok(Program { functions })
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
            span: Span { start, end },
        })
    }

    fn parse_expr(&mut self) -> CompileResult<Expr> {
        let token = self.advance();
        match &token.kind {
            TokenKind::Int(n) => Ok(Expr::Int(*n, token.span)),
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

    fn expect_ident(&mut self) -> CompileResult<String> {
        let token = self.advance();
        match token.kind {
            TokenKind::Ident(s) => Ok(s),
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
    use crate::lexer::Lexer;

    #[test]
    fn test_parse_main() {
        let mut lexer = Lexer::new("fn main() -> i32 { 42 }");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();

        assert_eq!(program.functions.len(), 1);
        let func = &program.functions[0];
        assert_eq!(func.name, "main");
        assert_eq!(func.return_type, "i32");
        assert!(matches!(func.body, Expr::Int(42, _)));
    }

    #[test]
    fn test_missing_return_type() {
        let mut lexer = Lexer::new("fn main() { 42 }");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let result = parser.parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnexpectedToken { expected: "'->'", .. }));
    }
}
