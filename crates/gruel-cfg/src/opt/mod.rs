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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// No optimization (`-O0`).
    ///
    /// Produces unoptimized code that closely matches the source structure.
    /// Useful for debugging and faster compilation.
    #[default]
    O0,

    /// Basic optimizations (`-O1`).
    ///
    /// Runs LLVM's `default<O1>` pipeline.
    O1,

    /// Standard optimizations (`-O2`).
    ///
    /// Runs LLVM's `default<O2>` pipeline.
    O2,

    /// Aggressive optimizations (`-O3`).
    ///
    /// Runs LLVM's `default<O3>` pipeline.
    O3,
}

impl OptLevel {
    /// Returns the name of this optimization level (e.g., "O0", "O1").
    pub fn name(&self) -> &'static str {
        match self {
            OptLevel::O0 => "O0",
            OptLevel::O1 => "O1",
            OptLevel::O2 => "O2",
            OptLevel::O3 => "O3",
        }
    }

    /// Returns all available optimization levels.
    pub fn all() -> &'static [OptLevel] {
        &[OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::O3]
    }

    /// Returns a comma-separated string of all level names (for help text).
    pub fn all_names() -> &'static str {
        "-O0, -O1, -O2, -O3"
    }
}

/// Error returned when parsing an optimization level fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptLevelError(String);

impl std::fmt::Display for ParseOptLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown optimization level '{}'", self.0)
    }
}

impl std::error::Error for ParseOptLevelError {}

impl std::str::FromStr for OptLevel {
    type Err = ParseOptLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "O0" | "0" => Ok(OptLevel::O0),
            "O1" | "1" => Ok(OptLevel::O1),
            "O2" | "2" => Ok(OptLevel::O2),
            "O3" | "3" => Ok(OptLevel::O3),
            _ => Err(ParseOptLevelError(s.to_string())),
        }
    }
}

impl std::fmt::Display for OptLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "-{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opt_level_from_str() {
        assert_eq!("O0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("O1".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("O2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("O3".parse::<OptLevel>().unwrap(), OptLevel::O3);

        // Also accept just the number
        assert_eq!("0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("1".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("3".parse::<OptLevel>().unwrap(), OptLevel::O3);

        // Invalid
        assert!("O4".parse::<OptLevel>().is_err());
        assert!("fast".parse::<OptLevel>().is_err());
    }

    #[test]
    fn test_opt_level_display() {
        assert_eq!(format!("{}", OptLevel::O0), "-O0");
        assert_eq!(format!("{}", OptLevel::O1), "-O1");
        assert_eq!(format!("{}", OptLevel::O2), "-O2");
        assert_eq!(format!("{}", OptLevel::O3), "-O3");
    }

    #[test]
    fn test_opt_level_default() {
        assert_eq!(OptLevel::default(), OptLevel::O0);
    }
}
