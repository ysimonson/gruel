//! Type system for Rue.
//!
//! Currently very minimal - just i32. Will be extended as the language grows.

/// A type in the Rue type system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Type {
    /// 32-bit signed integer
    I32,
    /// The unit type (for functions that don't return a value)
    #[default]
    Unit,
    /// An error type (used during type checking to continue after errors)
    Error,
}

impl Type {
    /// Get a human-readable name for this type.
    pub fn name(&self) -> &'static str {
        match self {
            Type::I32 => "i32",
            Type::Unit => "()",
            Type::Error => "<error>",
        }
    }

    /// Check if this is an error type.
    pub fn is_error(&self) -> bool {
        matches!(self, Type::Error)
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}
