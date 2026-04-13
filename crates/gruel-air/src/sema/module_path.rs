//! Structured import path resolution.
//!
//! This module provides a structured approach to resolving import paths in Gruel.
//! Instead of ad-hoc string matching with many special cases, it uses a typed
//! representation of different import path kinds and explicit resolution order.
//!
//! # Resolution Order
//!
//! When resolving an import path like `@import("foo")`, we check in this order:
//!
//! 1. **Standard library** - if the path is exactly "std"
//! 2. **Exact path with extension** - if path includes ".gruel" extension
//! 3. **Simple file match** - look for `foo.gruel` or path ending with `foo.gruel`
//! 4. **Facade module** - look for `_foo.gruel` (directory module entry point)
//!
//! The first match wins, so `foo.gruel` takes precedence over `_foo.gruel`.

use std::path::Path;

/// Represents a parsed import path with its resolution strategy.
///
/// This enum categorizes import paths to determine how they should be resolved.
/// Each variant corresponds to a different resolution strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModulePath {
    /// Standard library import: `@import("std")`
    ///
    /// This is a special case that is currently not supported during const eval.
    Std,

    /// Import with explicit `.gruel` extension: `@import("foo.gruel")`
    ///
    /// The path is taken as-is and matched against loaded file paths.
    ExplicitRue { path: String },

    /// Simple module import: `@import("foo")` or `@import("utils/strings")`
    ///
    /// Resolution tries:
    /// 1. `{path}.gruel` - standard file
    /// 2. `_{basename}.gruel` - facade file for directory modules
    ///
    /// For nested paths like `utils/strings`, we look for `utils/strings.gruel`.
    Simple { path: String },
}

impl ModulePath {
    /// Parse an import path string into a structured `ModulePath`.
    ///
    /// This determines the kind of import based on the path format.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// ModulePath::parse("std") => ModulePath::Std
    /// ModulePath::parse("foo.gruel") => ModulePath::ExplicitRue { path: "foo.gruel" }
    /// ModulePath::parse("foo") => ModulePath::Simple { path: "foo" }
    /// ModulePath::parse("utils/strings") => ModulePath::Simple { path: "utils/strings" }
    /// ```
    pub fn parse(import_path: &str) -> Self {
        // Check for standard library
        if import_path == "std" {
            return ModulePath::Std;
        }

        // Check for explicit .gruel extension
        if import_path.ends_with(".gruel") {
            return ModulePath::ExplicitRue {
                path: import_path.to_string(),
            };
        }

        // Otherwise, it's a simple module import
        ModulePath::Simple {
            path: import_path.to_string(),
        }
    }

    /// Resolve this import path against a collection of loaded file paths.
    ///
    /// Returns `Some(resolved_path)` if a match is found, or `None` if the
    /// module cannot be found.
    ///
    /// The resolution order is:
    /// 1. Exact match (for ExplicitRue)
    /// 2. Standard file match (`{path}.gruel`)
    /// 3. Path suffix match (for nested paths)
    /// 4. Facade file match (`_{basename}.gruel`)
    pub fn resolve<'a, I>(&self, loaded_paths: I) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        match self {
            ModulePath::Std => {
                // Standard library not supported yet
                None
            }
            ModulePath::ExplicitRue { path } => self.resolve_explicit(path, loaded_paths),
            ModulePath::Simple { path } => self.resolve_simple(path, loaded_paths),
        }
    }

    /// Resolve an explicit `.gruel` path.
    fn resolve_explicit<'a, I>(&self, import_path: &str, loaded_paths: I) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        let collected: Vec<_> = loaded_paths.collect();

        // Priority 1: Exact match
        for file_path in &collected {
            if *file_path == import_path {
                return Some((*file_path).clone());
            }
        }

        // Priority 2: Path ends with import_path
        // This handles cases like "foo.gruel" matching "/path/to/foo.gruel"
        for file_path in &collected {
            if file_path.ends_with(import_path) {
                // Verify it's a proper path boundary (preceded by / or start of string)
                let prefix_len = file_path.len() - import_path.len();
                if prefix_len == 0 || file_path.as_bytes()[prefix_len - 1] == b'/' {
                    return Some((*file_path).clone());
                }
            }
        }

        None
    }

    /// Resolve a simple (no extension) import path.
    fn resolve_simple<'a, I>(&self, import_path: &str, loaded_paths: I) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        let import_with_gruel = format!("{}.gruel", import_path);
        let collected: Vec<_> = loaded_paths.collect();

        // Extract the basename for facade file matching
        let basename = Path::new(import_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(import_path);
        let facade_name = format!("_{}.gruel", basename);

        // Priority 1: Look for exact {path}.gruel
        for file_path in &collected {
            if *file_path == &import_with_gruel {
                return Some((*file_path).clone());
            }
        }

        // Priority 2: Look for path ending with {path}.gruel
        // This handles "utils/strings" matching "/project/utils/strings.gruel"
        for file_path in &collected {
            if file_path.ends_with(&import_with_gruel) {
                // Verify it's a proper path boundary
                let prefix_len = file_path.len() - import_with_gruel.len();
                if prefix_len == 0 || file_path.as_bytes()[prefix_len - 1] == b'/' {
                    return Some((*file_path).clone());
                }
            }
        }

        // Priority 3: Look for files matching just the basename (e.g., "math" matches "src/math.gruel")
        for file_path in &collected {
            if let Some(file_name) = Path::new(file_path.as_str())
                .file_stem()
                .and_then(|s| s.to_str())
            {
                if file_name == basename {
                    return Some((*file_path).clone());
                }
            }
        }

        // Priority 4: Look for facade file (_foo.gruel)
        for file_path in &collected {
            if let Some(file_name) = Path::new(file_path.as_str())
                .file_name()
                .and_then(|s| s.to_str())
            {
                if file_name == facade_name {
                    return Some((*file_path).clone());
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Parsing tests
    // =========================================================================

    #[test]
    fn test_parse_std() {
        assert_eq!(ModulePath::parse("std"), ModulePath::Std);
    }

    #[test]
    fn test_parse_explicit_gruel() {
        assert_eq!(
            ModulePath::parse("foo.gruel"),
            ModulePath::ExplicitRue {
                path: "foo.gruel".to_string()
            }
        );
        assert_eq!(
            ModulePath::parse("utils/strings.gruel"),
            ModulePath::ExplicitRue {
                path: "utils/strings.gruel".to_string()
            }
        );
    }

    #[test]
    fn test_parse_simple() {
        assert_eq!(
            ModulePath::parse("foo"),
            ModulePath::Simple {
                path: "foo".to_string()
            }
        );
        assert_eq!(
            ModulePath::parse("utils/strings"),
            ModulePath::Simple {
                path: "utils/strings".to_string()
            }
        );
    }

    // =========================================================================
    // Resolution tests - Standard library
    // =========================================================================

    #[test]
    fn test_resolve_std_not_supported() {
        let paths = vec!["main.gruel".to_string()];
        let module = ModulePath::Std;
        assert_eq!(module.resolve(paths.iter()), None);
    }

    // =========================================================================
    // Resolution tests - Explicit .gruel extension
    // =========================================================================

    #[test]
    fn test_resolve_explicit_exact_match() {
        let paths = vec!["foo.gruel".to_string(), "bar.gruel".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "foo.gruel".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), Some("foo.gruel".to_string()));
    }

    #[test]
    fn test_resolve_explicit_suffix_match() {
        let paths = vec!["/project/src/foo.gruel".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "foo.gruel".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/src/foo.gruel".to_string())
        );
    }

    #[test]
    fn test_resolve_explicit_no_false_substring_match() {
        // "foo.gruel" should NOT match "xfoo.gruel" (no path boundary)
        let paths = vec!["xfoo.gruel".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "foo.gruel".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }

    #[test]
    fn test_resolve_explicit_nested_path() {
        let paths = vec!["/project/utils/strings.gruel".to_string()];
        let module = ModulePath::ExplicitRue {
            path: "utils/strings.gruel".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/utils/strings.gruel".to_string())
        );
    }

    // =========================================================================
    // Resolution tests - Simple (no extension)
    // =========================================================================

    #[test]
    fn test_resolve_simple_exact_match() {
        let paths = vec!["foo.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), Some("foo.gruel".to_string()));
    }

    #[test]
    fn test_resolve_simple_suffix_match() {
        let paths = vec!["/project/src/foo.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/src/foo.gruel".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_nested_path() {
        let paths = vec!["/project/utils/strings.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "utils/strings".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("/project/utils/strings.gruel".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_basename_match() {
        // "math" should match "src/math.gruel" by basename
        let paths = vec!["src/math.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "math".to_string(),
        };
        assert_eq!(
            module.resolve(paths.iter()),
            Some("src/math.gruel".to_string())
        );
    }

    #[test]
    fn test_resolve_simple_facade_file() {
        let paths = vec!["_utils.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "utils".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), Some("_utils.gruel".to_string()));
    }

    #[test]
    fn test_resolve_simple_prefers_regular_over_facade() {
        // When both "foo.gruel" and "_foo.gruel" exist, prefer "foo.gruel"
        let paths = vec!["_foo.gruel".to_string(), "foo.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), Some("foo.gruel".to_string()));
    }

    #[test]
    fn test_resolve_simple_no_false_substring_match() {
        // "math" should NOT match "mathematics.gruel"
        let paths = vec!["mathematics.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "math".to_string(),
        };
        // The basename "mathematics" != "math", so no match
        assert_eq!(module.resolve(paths.iter()), None);
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn test_resolve_not_found() {
        let paths = vec!["other.gruel".to_string()];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }

    #[test]
    fn test_resolve_empty_paths() {
        let paths: Vec<String> = vec![];
        let module = ModulePath::Simple {
            path: "foo".to_string(),
        };
        assert_eq!(module.resolve(paths.iter()), None);
    }
}
