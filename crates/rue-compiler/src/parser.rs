use crate::lexer::{Token, TokenKind, Span};

/// A complete program (list of functions)
#[derive(Debug)]
pub struct Program {
    pub functions: Vec<Function>,
}

/// A function definition
#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub return_type: Option<String>,
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

    pub fn parse(&mut self) -> Program {
        let mut functions = Vec::new();

        while !self.is_at_end() {
            functions.push(self.parse_function());
        }

        Program { functions }
    }

    fn parse_function(&mut self) -> Function {
        let start = self.current().span.start;

        // fn
        self.expect(TokenKind::Fn);

        // name
        let name = self.expect_ident();

        // ()
        self.expect(TokenKind::LParen);
        self.expect(TokenKind::RParen);

        // optional -> Type
        let return_type = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.expect_ident())
        } else {
            None
        };

        // { body }
        self.expect(TokenKind::LBrace);
        let body = self.parse_expr();
        self.expect(TokenKind::RBrace);

        let end = self.tokens[self.pos.saturating_sub(1)].span.end;

        Function {
            name,
            return_type,
            body,
            span: Span { start, end },
        }
    }

    fn parse_expr(&mut self) -> Expr {
        let token = self.advance();
        match &token.kind {
            TokenKind::Int(n) => Expr::Int(*n, token.span),
            _ => panic!("expected expression, got {:?}", token.kind),
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

    fn expect(&mut self, expected: TokenKind) {
        if !self.check(&expected) {
            panic!("expected {:?}, got {:?}", expected, self.current().kind);
        }
        self.advance();
    }

    fn expect_ident(&mut self) -> String {
        let token = self.advance();
        match token.kind {
            TokenKind::Ident(s) => s,
            _ => panic!("expected identifier, got {:?}", token.kind),
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
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse();

        assert_eq!(program.functions.len(), 1);
        let func = &program.functions[0];
        assert_eq!(func.name, "main");
        assert_eq!(func.return_type, Some("i32".to_string()));
        assert!(matches!(func.body, Expr::Int(42, _)));
    }
}
