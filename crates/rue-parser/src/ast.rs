//! Abstract Syntax Tree types for Rue.
//!
//! The AST represents the syntactic structure of the source code.
//! It closely mirrors the source syntax and preserves all information
//! needed for error reporting.

use rue_span::Span;

/// A complete source file (list of items).
#[derive(Debug)]
pub struct Ast {
    pub items: Vec<Item>,
}

/// A top-level item in a source file.
#[derive(Debug)]
pub enum Item {
    Function(Function),
}

/// A function definition.
#[derive(Debug)]
pub struct Function {
    /// Function name
    pub name: Ident,
    /// Return type
    pub return_type: Ident,
    /// Function body
    pub body: Expr,
    /// Span covering the entire function
    pub span: Span,
}

/// An identifier.
#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// An expression.
#[derive(Debug)]
pub enum Expr {
    /// Integer literal
    Int(IntLit),
    /// Binary operation (e.g., `a + b`)
    Binary(BinaryExpr),
    /// Unary operation (e.g., `-x`)
    Unary(UnaryExpr),
    /// Parenthesized expression (e.g., `(a + b)`)
    Paren(ParenExpr),
}

/// An integer literal.
#[derive(Debug)]
pub struct IntLit {
    pub value: i64,
    pub span: Span,
}

/// A binary expression.
#[derive(Debug)]
pub struct BinaryExpr {
    pub left: Box<Expr>,
    pub op: BinaryOp,
    pub right: Box<Expr>,
    pub span: Span,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add, // +
    Sub, // -
    Mul, // *
    Div, // /
    Mod, // %
}

/// A unary expression.
#[derive(Debug)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg, // -
}

/// A parenthesized expression.
#[derive(Debug)]
pub struct ParenExpr {
    pub inner: Box<Expr>,
    pub span: Span,
}

impl Expr {
    /// Get the span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(lit) => lit.span,
            Expr::Binary(bin) => bin.span,
            Expr::Unary(un) => un.span,
            Expr::Paren(paren) => paren.span,
        }
    }
}
