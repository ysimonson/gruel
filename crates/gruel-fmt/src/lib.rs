//! Source formatter for Gruel (ADR-0093).
//!
//! Parses source with the existing chumsky frontend, walks the AST, and emits
//! canonical text. Comment weaving is delegated to a side trivia scan
//! (ADR-0093 Phase 4).

use std::fmt;

use gruel_lexer::Lexer;
use gruel_parser::Parser;
use gruel_util::CompileErrors;

pub mod emit;
pub mod printer;
pub mod trivia;
pub use printer::Printer;

/// Top-level error returned from [`format_source`].
#[derive(Debug)]
pub enum FmtError {
    /// Source failed to lex or parse.
    Parse(CompileErrors),
}

impl fmt::Display for FmtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FmtError::Parse(errs) => write!(f, "{}", errs),
        }
    }
}

impl std::error::Error for FmtError {}

/// Format `src` to canonical Gruel form. Returns `Err` if the source does not
/// lex or parse.
pub fn format_source(src: &str) -> Result<String, FmtError> {
    let (tokens, interner) = Lexer::new(src)
        .tokenize()
        .map_err(|e| FmtError::Parse(e.into()))?;
    let (ast, interner) = Parser::new(tokens, interner)
        .with_source(src.to_string())
        .parse()
        .map_err(FmtError::Parse)?;

    let mut printer = Printer::new(&interner, src);
    emit::emit_ast(&mut printer, &ast);
    Ok(printer.finish())
}
