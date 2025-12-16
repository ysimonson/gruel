//! Parser and AST for the Rue programming language.

pub mod ast;
mod parser;

pub use ast::{Ast, Expr, Function, Ident, IntLit, Item};
pub use parser::Parser;
