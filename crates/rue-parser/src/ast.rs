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
    /// Identifier reference (variable)
    Ident(Ident),
    /// Binary operation (e.g., `a + b`)
    Binary(BinaryExpr),
    /// Unary operation (e.g., `-x`)
    Unary(UnaryExpr),
    /// Parenthesized expression (e.g., `(a + b)`)
    Paren(ParenExpr),
    /// Block with statements and final expression
    Block(BlockExpr),
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

/// A block expression containing statements and a final expression.
#[derive(Debug)]
pub struct BlockExpr {
    /// Statements in the block
    pub statements: Vec<Statement>,
    /// Final expression (the value of the block)
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A statement (does not produce a value).
#[derive(Debug)]
pub enum Statement {
    /// Let binding: `let x = expr;` or `let mut x = expr;`
    Let(LetStatement),
    /// Assignment: `x = expr;`
    Assign(AssignStatement),
    /// Expression statement: `expr;`
    Expr(Expr),
}

/// A let binding statement.
#[derive(Debug)]
pub struct LetStatement {
    /// Whether the binding is mutable
    pub is_mut: bool,
    /// Variable name
    pub name: Ident,
    /// Optional type annotation
    pub ty: Option<Ident>,
    /// Initializer expression
    pub init: Box<Expr>,
    pub span: Span,
}

/// An assignment statement.
#[derive(Debug)]
pub struct AssignStatement {
    /// Target variable
    pub name: Ident,
    /// Value expression
    pub value: Box<Expr>,
    pub span: Span,
}

impl Expr {
    /// Get the span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(lit) => lit.span,
            Expr::Ident(ident) => ident.span,
            Expr::Binary(bin) => bin.span,
            Expr::Unary(un) => un.span,
            Expr::Paren(paren) => paren.span,
            Expr::Block(block) => block.span,
        }
    }
}
