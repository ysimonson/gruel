//! Parser and AST for the Rue programming language.

pub mod ast;
mod parser;

pub use ast::{
    Ast, BinaryExpr, BinaryOp, Expr, Function, Ident, IntLit, Item, ParenExpr, UnaryExpr, UnaryOp,
};
pub use parser::Parser;
