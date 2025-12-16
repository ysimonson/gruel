pub mod lexer;
pub mod parser;
pub mod codegen;

pub use lexer::{Lexer, Token, TokenKind};
pub use parser::{Parser, Program, Function, Expr};
pub use codegen::generate_elf;
