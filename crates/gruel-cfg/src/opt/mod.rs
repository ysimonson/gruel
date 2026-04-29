//! Optimization level configuration for the Gruel compiler.
//!
//! CFG-level optimization passes were removed in ADR-0034. Optimization is now
//! handled entirely by LLVM's mid-end pipeline (`default<OX>`), which is invoked
//! when generating object code or LLVM IR at `-O1` and above.
//!
//! This module retains the `OptLevel` enum so that the CLI and `CompileOptions`
//! continue to express the user's requested optimization level.

/// Optimization level, following standard compiler conventions.
///
/// Controls the LLVM mid-end optimization pipeline invoked during code
/// generation. At `-O0` no LLVM passes are run; at `-O1+` the full
/// `default<OX>` pipeline runs (InstCombine, GVN, SCCP, ADCE, SimplifyCFG, …).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, strum::Display, strum::EnumString, strum::EnumIter,
)]
pub enum OptLevel {
    /// No optimization (`-O0`).
    #[default]
    #[strum(to_string = "0", serialize = "O0")]
    O0,

    /// Basic optimizations (`-O1`).
    #[strum(to_string = "1", serialize = "O1")]
    O1,

    /// Standard optimizations (`-O2`).
    #[strum(to_string = "2", serialize = "O2")]
    O2,

    /// Aggressive optimizations (`-O3`).
    #[strum(to_string = "3", serialize = "O3")]
    O3,
}

impl OptLevel {
    /// Returns all available optimization levels.
    pub fn all() -> Vec<OptLevel> {
        use strum::IntoEnumIterator;
        OptLevel::iter().collect()
    }

    /// Returns a comma-separated string of all level names (for help text).
    pub fn all_names() -> String {
        Self::all()
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opt_level_from_str() {
        // Numeric form (canonical for --opt-level=N)
        assert_eq!("0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("1".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("3".parse::<OptLevel>().unwrap(), OptLevel::O3);

        // O-prefixed form is still accepted for compatibility.
        assert_eq!("O0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("O3".parse::<OptLevel>().unwrap(), OptLevel::O3);

        // Invalid
        assert!("4".parse::<OptLevel>().is_err());
        assert!("fast".parse::<OptLevel>().is_err());
    }

    #[test]
    fn test_opt_level_display() {
        assert_eq!(format!("{}", OptLevel::O0), "0");
        assert_eq!(format!("{}", OptLevel::O3), "3");
    }

    #[test]
    fn test_opt_level_default() {
        assert_eq!(OptLevel::default(), OptLevel::O0);
    }
}
