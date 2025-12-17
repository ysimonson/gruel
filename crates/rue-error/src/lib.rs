//! Error types for the Rue compiler.
//!
//! This crate provides the error infrastructure used throughout the compilation
//! pipeline. Errors carry source location information for diagnostic rendering.

use rue_span::Span;
use std::fmt;

/// A compilation error with optional source location information.
///
/// Some errors (like `NoMainFunction` or `LinkError`) don't have a meaningful
/// source location. Use `has_span()` to check before rendering location info.
#[derive(Debug, Clone)]
pub struct CompileError {
    pub kind: ErrorKind,
    span: Option<Span>,
}

/// The kind of compilation error.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    UndefinedFunction(String),
    AssignToImmutable(String),
    UnknownType(String),
    TypeMismatch {
        expected: String,
        found: String,
    },
    WrongArgumentCount {
        expected: usize,
        found: usize,
    },

    // Control flow errors
    BreakOutsideLoop,
    ContinueOutsideLoop,

    // Linker errors
    LinkError(String),
}

impl CompileError {
    /// Create a new error with the given kind and span.
    #[inline]
    pub fn new(kind: ErrorKind, span: Span) -> Self {
        Self {
            kind,
            span: Some(span),
        }
    }

    /// Create an error without a source location.
    ///
    /// Use this for errors that don't correspond to a specific source location,
    /// such as "no main function found" or linker errors.
    #[inline]
    pub fn without_span(kind: ErrorKind) -> Self {
        Self { kind, span: None }
    }

    /// Create an error at a specific position (zero-length span).
    #[inline]
    pub fn at(kind: ErrorKind, pos: u32) -> Self {
        Self {
            kind,
            span: Some(Span::point(pos)),
        }
    }

    /// Returns true if this error has source location information.
    #[inline]
    pub fn has_span(&self) -> bool {
        self.span.is_some()
    }

    /// Get the span, if present.
    #[inline]
    pub fn span(&self) -> Option<Span> {
        self.span
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for CompileError {}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnexpectedCharacter(c) => write!(f, "unexpected character: {}", c),
            ErrorKind::InvalidInteger => write!(f, "invalid integer literal"),
            ErrorKind::UnexpectedToken { expected, found } => {
                write!(f, "expected {}, found {}", expected, found)
            }
            ErrorKind::UnexpectedEof { expected } => {
                write!(f, "unexpected end of file, expected {}", expected)
            }
            ErrorKind::NoMainFunction => write!(f, "no main function found"),
            ErrorKind::UndefinedVariable(name) => write!(f, "undefined variable '{}'", name),
            ErrorKind::UndefinedFunction(name) => write!(f, "undefined function '{}'", name),
            ErrorKind::AssignToImmutable(name) => {
                write!(f, "cannot assign to immutable variable '{}'", name)
            }
            ErrorKind::UnknownType(name) => write!(f, "unknown type '{}'", name),
            ErrorKind::TypeMismatch { expected, found } => {
                write!(f, "type mismatch: expected {}, found {}", expected, found)
            }
            ErrorKind::WrongArgumentCount { expected, found } => {
                if *expected == 1 {
                    write!(f, "expected {} argument, found {}", expected, found)
                } else {
                    write!(f, "expected {} arguments, found {}", expected, found)
                }
            }
            ErrorKind::BreakOutsideLoop => write!(f, "'break' outside of loop"),
            ErrorKind::ContinueOutsideLoop => write!(f, "'continue' outside of loop"),
            ErrorKind::LinkError(msg) => write!(f, "link error: {}", msg),
        }
    }
}

/// Result type for compilation operations.
pub type CompileResult<T> = Result<T, CompileError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_with_span() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::InvalidInteger, span);

        assert!(error.has_span());
        assert_eq!(error.span(), Some(span));
        assert_eq!(error.to_string(), "invalid integer literal");
    }

    #[test]
    fn test_error_without_span() {
        let error = CompileError::without_span(ErrorKind::NoMainFunction);

        assert!(!error.has_span());
        assert_eq!(error.span(), None);
        assert_eq!(error.to_string(), "no main function found");
    }

    #[test]
    fn test_error_at_position() {
        let error = CompileError::at(ErrorKind::InvalidInteger, 42);

        assert!(error.has_span());
        assert_eq!(error.span(), Some(Span::point(42)));
    }

    #[test]
    fn test_unexpected_character_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedCharacter('@'));
        assert_eq!(error.to_string(), "unexpected character: @");
    }

    #[test]
    fn test_unexpected_token_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedToken {
            expected: "identifier",
            found: "'+'".to_string(),
        });
        assert_eq!(error.to_string(), "expected identifier, found '+'");
    }

    #[test]
    fn test_unexpected_eof_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedEof {
            expected: "'}'",
        });
        assert_eq!(error.to_string(), "unexpected end of file, expected '}'");
    }

    #[test]
    fn test_undefined_variable_message() {
        let error = CompileError::without_span(ErrorKind::UndefinedVariable("foo".to_string()));
        assert_eq!(error.to_string(), "undefined variable 'foo'");
    }

    #[test]
    fn test_undefined_function_message() {
        let error = CompileError::without_span(ErrorKind::UndefinedFunction("bar".to_string()));
        assert_eq!(error.to_string(), "undefined function 'bar'");
    }

    #[test]
    fn test_assign_to_immutable_message() {
        let error = CompileError::without_span(ErrorKind::AssignToImmutable("x".to_string()));
        assert_eq!(error.to_string(), "cannot assign to immutable variable 'x'");
    }

    #[test]
    fn test_unknown_type_message() {
        let error = CompileError::without_span(ErrorKind::UnknownType("Foo".to_string()));
        assert_eq!(error.to_string(), "unknown type 'Foo'");
    }

    #[test]
    fn test_type_mismatch_message() {
        let error = CompileError::without_span(ErrorKind::TypeMismatch {
            expected: "i32".to_string(),
            found: "bool".to_string(),
        });
        assert_eq!(error.to_string(), "type mismatch: expected i32, found bool");
    }

    #[test]
    fn test_wrong_argument_count_singular() {
        let error = CompileError::without_span(ErrorKind::WrongArgumentCount {
            expected: 1,
            found: 3,
        });
        assert_eq!(error.to_string(), "expected 1 argument, found 3");
    }

    #[test]
    fn test_wrong_argument_count_plural() {
        let error = CompileError::without_span(ErrorKind::WrongArgumentCount {
            expected: 2,
            found: 0,
        });
        assert_eq!(error.to_string(), "expected 2 arguments, found 0");
    }

    #[test]
    fn test_link_error_message() {
        let error = CompileError::without_span(ErrorKind::LinkError("undefined symbol".to_string()));
        assert_eq!(error.to_string(), "link error: undefined symbol");
    }

    #[test]
    fn test_error_kind_equality() {
        assert_eq!(ErrorKind::InvalidInteger, ErrorKind::InvalidInteger);
        assert_eq!(ErrorKind::NoMainFunction, ErrorKind::NoMainFunction);
        assert_ne!(ErrorKind::InvalidInteger, ErrorKind::NoMainFunction);
    }

    #[test]
    fn test_error_implements_std_error() {
        fn assert_error<T: std::error::Error>() {}
        assert_error::<CompileError>();
    }
}
