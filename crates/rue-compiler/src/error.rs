use crate::lexer::Span;

/// A compilation error with source location information.
#[derive(Debug, Clone)]
pub struct CompileError {
    pub kind: ErrorKind,
    pub span: Span,
}

/// The kind of compilation error.
#[derive(Debug, Clone)]
pub enum ErrorKind {
    // Lexer errors
    UnexpectedCharacter(char),
    InvalidInteger,

    // Parser errors
    UnexpectedToken { expected: &'static str, found: String },
    UnexpectedEof { expected: &'static str },

    // Semantic errors
    NoMainFunction,
}

impl CompileError {
    pub fn new(kind: ErrorKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Get a human-readable message for this error.
    pub fn message(&self) -> String {
        match &self.kind {
            ErrorKind::UnexpectedCharacter(c) => format!("unexpected character: {}", c),
            ErrorKind::InvalidInteger => "invalid integer literal".to_string(),
            ErrorKind::UnexpectedToken { expected, found } => {
                format!("expected {}, found {}", expected, found)
            }
            ErrorKind::UnexpectedEof { expected } => {
                format!("unexpected end of file, expected {}", expected)
            }
            ErrorKind::NoMainFunction => "no main function found".to_string(),
        }
    }
}

/// Result type for compilation operations.
pub type CompileResult<T> = Result<T, CompileError>;
