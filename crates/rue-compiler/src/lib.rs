pub mod codegen;
pub mod error;
pub mod lexer;
pub mod parser;

pub use codegen::generate_elf;
pub use error::{CompileError, CompileResult, ErrorKind};
pub use lexer::{Lexer, Span, Token, TokenKind};
pub use parser::{Expr, Function, Parser, Program};
