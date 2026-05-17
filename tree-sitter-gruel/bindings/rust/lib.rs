//! Rust bindings for the Gruel tree-sitter grammar (ADR-0090).
//!
//! ```no_run
//! use tree_sitter::Parser;
//! use tree_sitter_gruel::LANGUAGE;
//!
//! let mut parser = Parser::new();
//! parser.set_language(&LANGUAGE.into()).unwrap();
//! let tree = parser.parse("fn main() -> i32 { 0 }", None).unwrap();
//! assert!(!tree.root_node().has_error());
//! ```

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_gruel() -> Language;
}

/// The tree-sitter [`Language`](tree_sitter::Language) for Gruel.
///
/// Pass `&LANGUAGE.into()` to `tree_sitter::Parser::set_language`.
pub const LANGUAGE: LanguageFn = LanguageFn(tree_sitter_gruel);

/// Thin wrapper that lets us expose the raw FFI symbol through a `const`.
#[derive(Clone, Copy)]
pub struct LanguageFn(unsafe extern "C" fn() -> Language);

impl LanguageFn {
    /// Convert to a [`tree_sitter::Language`].
    pub fn into_language(self) -> Language {
        unsafe { (self.0)() }
    }
}

impl From<LanguageFn> for Language {
    fn from(value: LanguageFn) -> Self {
        value.into_language()
    }
}

/// Tree-sitter highlights query.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
/// Tree-sitter locals query.
pub const LOCALS_QUERY: &str = include_str!("../../queries/locals.scm");
/// Tree-sitter indents query.
pub const INDENTS_QUERY: &str = include_str!("../../queries/indents.scm");
/// Tree-sitter folds query.
pub const FOLDS_QUERY: &str = include_str!("../../queries/folds.scm");

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    #[test]
    fn loads_grammar() {
        let mut parser = Parser::new();
        parser
            .set_language(&LANGUAGE.into())
            .expect("language loaded");
        let tree = parser
            .parse("fn main() -> i32 { 0 }", None)
            .expect("parsed");
        assert!(!tree.root_node().has_error());
    }
}
