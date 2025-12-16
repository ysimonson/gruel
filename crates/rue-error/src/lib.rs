//! Error types for the Rue compiler.
//!
//! This crate provides the error infrastructure used throughout the compilation
//! pipeline. Errors carry source location information for diagnostic rendering.

use rue_span::Span;

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
    UnexpectedToken {
        expected: &'static str,
        found: String,
    },
    UnexpectedEof {
        expected: &'static str,
    },

    // Semantic errors
    NoMainFunction,
    UndefinedVariable(String),
    TypeMismatch {
        expected: String,
        found: String,
    },
}

impl CompileError {
    /// Create a new error with the given kind and span.
    #[inline]
    pub fn new(kind: ErrorKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Create an error at a specific position (zero-length span).
    #[inline]
    pub fn at(kind: ErrorKind, pos: u32) -> Self {
        Self {
            kind,
            span: Span::point(pos),
        }
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
            ErrorKind::UndefinedVariable(name) => format!("undefined variable: {}", name),
            ErrorKind::TypeMismatch { expected, found } => {
                format!("type mismatch: expected {}, found {}", expected, found)
            }
        }
    }
}

/// Result type for compilation operations.
pub type CompileResult<T> = Result<T, CompileError>;

/// A collection of compilation errors.
///
/// Some phases may collect multiple errors before failing.
#[derive(Debug, Default)]
pub struct Errors {
    errors: Vec<CompileError>,
}

impl Errors {
    /// Create a new empty error collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an error to the collection.
    pub fn push(&mut self, error: CompileError) {
        self.errors.push(error);
    }

    /// Returns true if there are no errors.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of errors.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Iterate over the errors.
    pub fn iter(&self) -> impl Iterator<Item = &CompileError> {
        self.errors.iter()
    }

    /// Convert to a Result, failing if there are any errors.
    pub fn into_result<T>(self, value: T) -> CompileResult<T> {
        if let Some(first) = self.errors.into_iter().next() {
            Err(first)
        } else {
            Ok(value)
        }
    }
}

impl IntoIterator for Errors {
    type Item = CompileError;
    type IntoIter = std::vec::IntoIter<CompileError>;

    fn into_iter(self) -> Self::IntoIter {
        self.errors.into_iter()
    }
}
