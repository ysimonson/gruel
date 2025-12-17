//! Parser and AST for the Rue programming language.

pub mod ast;
mod parser;

pub use ast::{
    AssignStatement, Ast, BinaryExpr, BinaryOp, BlockExpr, CallExpr, Expr, Function, Ident,
    IntLit, Item, LetStatement, Param, ParenExpr, Statement, UnaryExpr, UnaryOp, WhileExpr,
};
pub use parser::Parser;
