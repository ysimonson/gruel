//! Parser for the Rue programming language.
//!
//! Converts a sequence of tokens into an AST.

use crate::ast::{
    AssignStatement, Ast, BinaryExpr, BinaryOp, BlockExpr, BoolLit, BreakExpr, CallExpr,
    ContinueExpr, Expr, Function, Ident, IfExpr, IntLit, Item, LetStatement, Param, ParenExpr,
    Statement, UnaryExpr, UnaryOp, WhileExpr,
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

        // ( params )
        self.expect(TokenKind::LParen)?;
        let params = self.parse_params()?;
        self.expect(TokenKind::RParen)?;

        // -> Type
        self.expect(TokenKind::Arrow)?;
        let return_type = self.expect_ident()?;

        // { body }
        let body = self.parse_block()?;

        let end = self.tokens[self.pos.saturating_sub(1)].span.end;

        Ok(Function {
            name,
            params,
            return_type,
            body,
            span: Span::new(start, end),
        })
    }

    /// Parse function parameters: `name: type, name: type, ...`
    fn parse_params(&mut self) -> CompileResult<Vec<Param>> {
        let mut params = Vec::new();

        // Check for empty parameter list
        if self.check(&TokenKind::RParen) {
            return Ok(params);
        }

        // Parse first parameter
        params.push(self.parse_param()?);

        // Parse remaining parameters
        while self.check(&TokenKind::Comma) {
            self.advance(); // consume comma
            params.push(self.parse_param()?);
        }

        Ok(params)
    }

    /// Parse a single parameter: `name: type`
    fn parse_param(&mut self) -> CompileResult<Param> {
        let start = self.current().span.start;

        let name = self.expect_ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.expect_ident()?;

        let end = self.tokens[self.pos.saturating_sub(1)].span.end;

        Ok(Param {
            name,
            ty,
            span: Span::new(start, end),
        })
    }

    /// Parse a block: `{ statements... expr }`
    fn parse_block(&mut self) -> CompileResult<Expr> {
        let start = self.current().span.start;
        self.expect(TokenKind::LBrace)?;

        let mut statements = Vec::new();

        // Parse statements and final expression
        loop {
            // Check for end of block
            if self.check(&TokenKind::RBrace) {
                // Empty block or block ending after statements - error
                return Err(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "expression",
                        found: self.current().kind.name().to_string(),
                    },
                    self.current().span,
                ));
            }

            // Try to parse a let statement
            if self.check(&TokenKind::Let) {
                statements.push(self.parse_let_statement()?);
                continue;
            }

            // Parse an expression
            let expr = self.parse_expr()?;

            // Check what follows the expression
            if self.check(&TokenKind::Semi) {
                // Expression statement - consume semicolon and continue
                self.advance();
                statements.push(Statement::Expr(expr));
            } else if self.check(&TokenKind::RBrace) {
                // Final expression - end of block
                self.expect(TokenKind::RBrace)?;
                let end = self.tokens[self.pos.saturating_sub(1)].span.end;

                return Ok(Expr::Block(BlockExpr {
                    statements,
                    expr: Box::new(expr),
                    span: Span::new(start, end),
                }));
            } else if self.check(&TokenKind::Eq) {
                // Assignment: we parsed the LHS as an expression, check it's an identifier
                match expr {
                    Expr::Ident(target) => {
                        let assign = self.parse_assignment_rest(target)?;
                        statements.push(Statement::Assign(assign));
                    }
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::UnexpectedToken {
                                expected: "';'",
                                found: self.current().kind.name().to_string(),
                            },
                            self.current().span,
                        ));
                    }
                }
            } else if matches!(&expr, Expr::If(_) | Expr::While(_) | Expr::Break(_) | Expr::Continue(_)) {
                // If, while, break, and continue don't require semicolon when used as statements
                statements.push(Statement::Expr(expr));
            } else {
                return Err(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "';'",
                        found: self.current().kind.name().to_string(),
                    },
                    self.current().span,
                ));
            }
        }
    }

    /// Parse a let statement: `let [mut] name [: type] = expr;`
    fn parse_let_statement(&mut self) -> CompileResult<Statement> {
        let start = self.current().span.start;
        self.expect(TokenKind::Let)?;

        // Check for 'mut'
        let is_mut = if self.check(&TokenKind::Mut) {
            self.advance();
            true
        } else {
            false
        };

        // Variable name
        let name = self.expect_ident()?;

        // Optional type annotation
        let ty = if self.check(&TokenKind::Colon) {
            self.advance();
            Some(self.expect_ident()?)
        } else {
            None
        };

        // = expr
        self.expect(TokenKind::Eq)?;
        let init = self.parse_expr()?;

        // ;
        self.expect(TokenKind::Semi)?;

        let end = self.tokens[self.pos.saturating_sub(1)].span.end;

        Ok(Statement::Let(LetStatement {
            is_mut,
            name,
            ty,
            init: Box::new(init),
            span: Span::new(start, end),
        }))
    }

    /// Parse a while expression: `while cond { body }`
    fn parse_while_expr(&mut self) -> CompileResult<Expr> {
        let start = self.current().span.start;
        self.expect(TokenKind::While)?;

        // Parse condition
        let cond = self.parse_expr()?;

        // Parse body block using the same method as if-then blocks
        let body = self.parse_maybe_unit_block()?;

        let end = body.span.end;

        Ok(Expr::While(WhileExpr {
            cond: Box::new(cond),
            body,
            span: Span::new(start, end),
        }))
    }

    /// Parse the rest of an assignment after we've already parsed the target identifier.
    fn parse_assignment_rest(&mut self, target: Ident) -> CompileResult<AssignStatement> {
        let start = target.span.start;

        // = expr
        self.expect(TokenKind::Eq)?;
        let value = self.parse_expr()?;

        // ;
        self.expect(TokenKind::Semi)?;

        let end = self.tokens[self.pos.saturating_sub(1)].span.end;

        Ok(AssignStatement {
            name: target,
            value: Box::new(value),
            span: Span::new(start, end),
        })
    }

    /// Parse an expression (entry point).
    fn parse_expr(&mut self) -> CompileResult<Expr> {
        self.parse_or()
    }

    /// Parse logical OR expressions (||).
    /// Lowest precedence among binary operators.
    fn parse_or(&mut self) -> CompileResult<Expr> {
        let mut left = self.parse_and()?;

        while matches!(self.current().kind, TokenKind::PipePipe) {
            self.advance();
            let right = self.parse_and()?;
            let span = Span::new(left.span().start, right.span().end);

            left = Expr::Binary(BinaryExpr {
                left: Box::new(left),
                op: BinaryOp::Or,
                right: Box::new(right),
                span,
            });
        }

        Ok(left)
    }

    /// Parse logical AND expressions (&&).
    /// Higher precedence than OR, lower than comparison.
    fn parse_and(&mut self) -> CompileResult<Expr> {
        let mut left = self.parse_comparison()?;

        while matches!(self.current().kind, TokenKind::AmpAmp) {
            self.advance();
            let right = self.parse_comparison()?;
            let span = Span::new(left.span().start, right.span().end);

            left = Expr::Binary(BinaryExpr {
                left: Box::new(left),
                op: BinaryOp::And,
                right: Box::new(right),
                span,
            });
        }

        Ok(left)
    }

    /// Parse comparison expressions (==, !=, <, >, <=, >=).
    /// Higher precedence than logical operators, lower than additive.
    ///
    /// Note: This allows chaining like `1 < 2 < 3`, which parses as `(1 < 2) < 3`.
    /// This will type-error (bool vs int), but a dedicated "comparison chaining"
    /// error message would provide better UX. Future work.
    fn parse_comparison(&mut self) -> CompileResult<Expr> {
        let mut left = self.parse_additive()?;

        while matches!(
            self.current().kind,
            TokenKind::EqEq
                | TokenKind::BangEq
                | TokenKind::Lt
                | TokenKind::Gt
                | TokenKind::LtEq
                | TokenKind::GtEq
        ) {
            let op_token = self.advance();
            let op = match op_token.kind {
                TokenKind::EqEq => BinaryOp::Eq,
                TokenKind::BangEq => BinaryOp::Ne,
                TokenKind::Lt => BinaryOp::Lt,
                TokenKind::Gt => BinaryOp::Gt,
                TokenKind::LtEq => BinaryOp::Le,
                TokenKind::GtEq => BinaryOp::Ge,
                _ => unreachable!(),
            };

            let right = self.parse_additive()?;
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

    /// Parse unary expressions (-x, !x).
    /// Highest precedence (binds tightest).
    fn parse_unary(&mut self) -> CompileResult<Expr> {
        if matches!(self.current().kind, TokenKind::Minus | TokenKind::Bang) {
            let op_token = self.advance();
            let op = match op_token.kind {
                TokenKind::Minus => UnaryOp::Neg,
                TokenKind::Bang => UnaryOp::Not,
                _ => unreachable!(),
            };
            let operand = self.parse_unary()?; // Recursive for --x, !!x
            let span = Span::new(op_token.span.start, operand.span().end);

            Ok(Expr::Unary(UnaryExpr {
                op,
                operand: Box::new(operand),
                span,
            }))
        } else {
            self.parse_primary()
        }
    }

    /// Parse primary expressions (literals, identifiers, parenthesized expressions).
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
            TokenKind::True => {
                self.advance();
                Ok(Expr::Bool(BoolLit {
                    value: true,
                    span: token.span,
                }))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Bool(BoolLit {
                    value: false,
                    span: token.span,
                }))
            }
            TokenKind::Ident(name) => {
                let name_str = name.clone();
                let ident_span = token.span;
                self.advance();

                // Check for function call
                if self.check(&TokenKind::LParen) {
                    let call = self.parse_call(Ident {
                        name: name_str,
                        span: ident_span,
                    })?;
                    Ok(Expr::Call(call))
                } else {
                    Ok(Expr::Ident(Ident {
                        name: name_str,
                        span: ident_span,
                    }))
                }
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
            TokenKind::LBrace => {
                // Nested block expression
                self.parse_block()
            }
            TokenKind::If => {
                self.parse_if_expr()
            }
            TokenKind::While => {
                self.parse_while_expr()
            }
            TokenKind::Break => {
                self.advance();
                Ok(Expr::Break(BreakExpr { span: token.span }))
            }
            TokenKind::Continue => {
                self.advance();
                Ok(Expr::Continue(ContinueExpr { span: token.span }))
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

    /// Parse an if expression: `if cond { then } [else { else }]`
    fn parse_if_expr(&mut self) -> CompileResult<Expr> {
        let start = self.current().span.start;

        // Consume 'if'
        self.expect(TokenKind::If)?;

        // Parse condition
        let cond = self.parse_expr()?;

        // Parse then block - use maybe_unit_block to allow statements without final expression
        let then_block = self.parse_maybe_unit_block()?;

        // Optionally parse else block
        let else_block = if self.check(&TokenKind::Else) {
            self.advance(); // consume 'else'
            Some(self.parse_maybe_unit_block()?)
        } else {
            None
        };

        let end = if let Some(ref else_b) = else_block {
            else_b.span.end
        } else {
            then_block.span.end
        };

        Ok(Expr::If(IfExpr {
            cond: Box::new(cond),
            then_block,
            else_block,
            span: Span::new(start, end),
        }))
    }

    /// Parse a block that may end with statements (producing Unit) or with an expression.
    /// This is used for if-then blocks and while bodies where the final value may be discarded.
    fn parse_maybe_unit_block(&mut self) -> CompileResult<BlockExpr> {
        let start = self.current().span.start;
        self.expect(TokenKind::LBrace)?;

        let mut statements = Vec::new();

        // Parse statements and maybe a final expression
        loop {
            // Check for end of block
            if self.check(&TokenKind::RBrace) {
                self.expect(TokenKind::RBrace)?;
                let end = self.tokens[self.pos.saturating_sub(1)].span.end;

                // Block ends after statements - use a dummy placeholder expression
                return Ok(BlockExpr {
                    statements,
                    expr: Box::new(Expr::Bool(BoolLit {
                        value: false,
                        span: Span::new(end, end),
                    })),
                    span: Span::new(start, end),
                });
            }

            // Try to parse a let statement
            if self.check(&TokenKind::Let) {
                statements.push(self.parse_let_statement()?);
                continue;
            }

            // Parse an expression
            let expr = self.parse_expr()?;

            // Check what follows the expression
            if self.check(&TokenKind::Semi) {
                // Expression statement - consume semicolon and continue
                self.advance();
                statements.push(Statement::Expr(expr));
            } else if self.check(&TokenKind::RBrace) {
                // Final expression - end of block
                self.expect(TokenKind::RBrace)?;
                let end = self.tokens[self.pos.saturating_sub(1)].span.end;

                return Ok(BlockExpr {
                    statements,
                    expr: Box::new(expr),
                    span: Span::new(start, end),
                });
            } else if self.check(&TokenKind::Eq) {
                // Assignment: we parsed the LHS as an expression, check it's an identifier
                match expr {
                    Expr::Ident(target) => {
                        let assign = self.parse_assignment_rest(target)?;
                        statements.push(Statement::Assign(assign));
                    }
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::UnexpectedToken {
                                expected: "';'",
                                found: self.current().kind.name().to_string(),
                            },
                            self.current().span,
                        ));
                    }
                }
            } else if matches!(expr, Expr::If(_) | Expr::While(_) | Expr::Block(_) | Expr::Break(_) | Expr::Continue(_)) {
                // If, while, block, break, and continue don't need semicolon
                statements.push(Statement::Expr(expr));
            } else {
                return Err(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "';'",
                        found: self.current().kind.name().to_string(),
                    },
                    self.current().span,
                ));
            }
        }
    }

    /// Parse a function call: `name(args...)`
    /// The name identifier has already been parsed.
    fn parse_call(&mut self, name: Ident) -> CompileResult<CallExpr> {
        let start = name.span.start;

        // (
        self.expect(TokenKind::LParen)?;

        // Parse arguments
        let args = self.parse_args()?;

        // )
        let close = self.expect(TokenKind::RParen)?;

        Ok(CallExpr {
            name,
            args,
            span: Span::new(start, close.span.end),
        })
    }

    /// Parse function call arguments: `expr, expr, ...`
    fn parse_args(&mut self) -> CompileResult<Vec<Expr>> {
        let mut args = Vec::new();

        // Check for empty argument list
        if self.check(&TokenKind::RParen) {
            return Ok(args);
        }

        // Parse first argument
        args.push(self.parse_expr()?);

        // Parse remaining arguments
        while self.check(&TokenKind::Comma) {
            self.advance(); // consume comma
            args.push(self.parse_expr()?);
        }

        Ok(args)
    }

    /// Parse a block expression and return the BlockExpr directly (not wrapped in Expr).
    fn parse_block_expr(&mut self) -> CompileResult<BlockExpr> {
        let start = self.current().span.start;
        self.expect(TokenKind::LBrace)?;

        let mut statements = Vec::new();

        // Parse statements and final expression
        loop {
            // Check for end of block
            if self.check(&TokenKind::RBrace) {
                // Empty block or block ending after statements - error
                return Err(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "expression",
                        found: self.current().kind.name().to_string(),
                    },
                    self.current().span,
                ));
            }

            // Try to parse a let statement
            if self.check(&TokenKind::Let) {
                statements.push(self.parse_let_statement()?);
                continue;
            }

            // Parse an expression
            let expr = self.parse_expr()?;

            // Check what follows the expression
            if self.check(&TokenKind::Semi) {
                // Expression statement - consume semicolon and continue
                self.advance();
                statements.push(Statement::Expr(expr));
            } else if self.check(&TokenKind::RBrace) {
                // Final expression - end of block
                self.expect(TokenKind::RBrace)?;
                let end = self.tokens[self.pos.saturating_sub(1)].span.end;

                return Ok(BlockExpr {
                    statements,
                    expr: Box::new(expr),
                    span: Span::new(start, end),
                });
            } else if self.check(&TokenKind::Eq) {
                // Assignment: we parsed the LHS as an expression, check it's an identifier
                match expr {
                    Expr::Ident(target) => {
                        let assign = self.parse_assignment_rest(target)?;
                        statements.push(Statement::Assign(assign));
                    }
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::UnexpectedToken {
                                expected: "';'",
                                found: self.current().kind.name().to_string(),
                            },
                            self.current().span,
                        ));
                    }
                }
            } else {
                return Err(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "';'",
                        found: self.current().kind.name().to_string(),
                    },
                    self.current().span,
                ));
            }
        }
    }

    fn current(&self) -> &Token {
        // Safety: We rely on the lexer always producing an EOF token at the end.
        // This assertion provides a clear error message if that invariant is violated,
        // rather than panicking with an unclear index-out-of-bounds error.
        debug_assert!(
            self.pos < self.tokens.len(),
            "parser position {} exceeds token count {}; lexer should always produce EOF token",
            self.pos,
            self.tokens.len()
        );
        // Use get() with a fallback to the last token (EOF) for safety in release builds
        self.tokens.get(self.pos).unwrap_or_else(|| {
            self.tokens.last().expect("token stream should never be empty")
        })
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind)
    }

    fn advance(&mut self) -> Token {
        let token = self.current().clone();
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
            Item::Function(f) => {
                // Unwrap the block to get the final expression
                match f.body {
                    Expr::Block(block) => Ok(*block.expr),
                    other => Ok(other),
                }
            }
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
                // Body is now wrapped in a Block
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::Int(lit) => assert_eq!(lit.value, 42),
                        _ => panic!("expected Int"),
                    },
                    _ => panic!("expected Block"),
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

    #[test]
    fn test_parse_identifier() {
        let expr = parse_expr("x").unwrap();
        match expr {
            Expr::Ident(ident) => assert_eq!(ident.name, "x"),
            _ => panic!("expected Ident"),
        }
    }

    #[test]
    fn test_parse_let_binding() {
        let ast = parse("fn main() -> i32 { let x = 42; x }").unwrap();
        match &ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    assert_eq!(block.statements.len(), 1);
                    match &block.statements[0] {
                        Statement::Let(let_stmt) => {
                            assert!(!let_stmt.is_mut);
                            assert_eq!(let_stmt.name.name, "x");
                            assert!(let_stmt.ty.is_none());
                            match let_stmt.init.as_ref() {
                                Expr::Int(lit) => assert_eq!(lit.value, 42),
                                _ => panic!("expected Int"),
                            }
                        }
                        _ => panic!("expected Let"),
                    }
                    match block.expr.as_ref() {
                        Expr::Ident(ident) => assert_eq!(ident.name, "x"),
                        _ => panic!("expected Ident"),
                    }
                }
                _ => panic!("expected Block"),
            },
        }
    }

    #[test]
    fn test_parse_let_mut() {
        let ast = parse("fn main() -> i32 { let mut x = 10; x }").unwrap();
        match &ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match &block.statements[0] {
                    Statement::Let(let_stmt) => {
                        assert!(let_stmt.is_mut);
                        assert_eq!(let_stmt.name.name, "x");
                    }
                    _ => panic!("expected Let"),
                },
                _ => panic!("expected Block"),
            },
        }
    }

    #[test]
    fn test_parse_let_with_type() {
        let ast = parse("fn main() -> i32 { let x: i32 = 42; x }").unwrap();
        match &ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match &block.statements[0] {
                    Statement::Let(let_stmt) => {
                        assert_eq!(let_stmt.name.name, "x");
                        assert!(let_stmt.ty.is_some());
                        assert_eq!(let_stmt.ty.as_ref().unwrap().name, "i32");
                    }
                    _ => panic!("expected Let"),
                },
                _ => panic!("expected Block"),
            },
        }
    }

    #[test]
    fn test_parse_assignment() {
        let ast = parse("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();
        match &ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    assert_eq!(block.statements.len(), 2);
                    match &block.statements[1] {
                        Statement::Assign(assign) => {
                            assert_eq!(assign.name.name, "x");
                            match assign.value.as_ref() {
                                Expr::Int(lit) => assert_eq!(lit.value, 20),
                                _ => panic!("expected Int"),
                            }
                        }
                        _ => panic!("expected Assign"),
                    }
                }
                _ => panic!("expected Block"),
            },
        }
    }

    #[test]
    fn test_parse_multiple_statements() {
        let ast = parse("fn main() -> i32 { let x = 1; let y = 2; x + y }").unwrap();
        match &ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    assert_eq!(block.statements.len(), 2);
                    match block.expr.as_ref() {
                        Expr::Binary(bin) => assert!(matches!(bin.op, BinaryOp::Add)),
                        _ => panic!("expected Binary"),
                    }
                }
                _ => panic!("expected Block"),
            },
        }
    }

    #[test]
    fn test_parse_logical_not() {
        let expr = parse_expr("!true").unwrap();
        match expr {
            Expr::Unary(un) => {
                assert!(matches!(un.op, UnaryOp::Not));
                match *un.operand {
                    Expr::Bool(lit) => assert!(lit.value),
                    _ => panic!("expected Bool"),
                }
            }
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn test_parse_double_not() {
        let expr = parse_expr("!!false").unwrap();
        match expr {
            Expr::Unary(outer) => {
                assert!(matches!(outer.op, UnaryOp::Not));
                match *outer.operand {
                    Expr::Unary(inner) => {
                        assert!(matches!(inner.op, UnaryOp::Not));
                    }
                    _ => panic!("expected Unary"),
                }
            }
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn test_parse_logical_and() {
        let expr = parse_expr("true && false").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::And));
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_logical_or() {
        let expr = parse_expr("true || false").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Or));
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_and_or_precedence() {
        // true || false && false should parse as true || (false && false)
        let expr = parse_expr("true || false && false").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Or));
                match *bin.right {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::And));
                    }
                    _ => panic!("expected Binary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_not_binds_tighter_than_and() {
        // !true && false should parse as (!true) && false
        let expr = parse_expr("!true && false").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::And));
                match *bin.left {
                    Expr::Unary(un) => {
                        assert!(matches!(un.op, UnaryOp::Not));
                    }
                    _ => panic!("expected Unary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_parse_comparison_binds_tighter_than_and() {
        // 1 < 2 && 3 < 4 should parse as (1 < 2) && (3 < 4)
        let expr = parse_expr("1 < 2 && 3 < 4").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::And));
                match *bin.left {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::Lt));
                    }
                    _ => panic!("expected Binary"),
                }
                match *bin.right {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::Lt));
                    }
                    _ => panic!("expected Binary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }
}
