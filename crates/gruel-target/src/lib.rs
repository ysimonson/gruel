//! Target architecture and OS definitions for the Gruel compiler.
//!
//! Backed by [`target_lexicon::Triple`], so any triple LLVM understands can be
//! parsed and used. The compiler-internal `Arch` and `Os` enums are derived
//! from the lexicon's fields rather than hardcoded.
//!
//! See ADR-0077 for the design.

use std::fmt;
use std::str::FromStr;

use target_lexicon::{Architecture, BinaryFormat, OperatingSystem, Triple};

/// A compilation target, identified by an LLVM-style triple.
///
/// `Target` wraps [`target_lexicon::Triple`] so the parser, validator, and
/// host-detection logic come "for free" from the upstream crate. Anything
/// LLVM understands is accepted; only the targets in [`Target::all()`] are
/// "blessed" (i.e. tested and supported).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    triple: Triple,
}

impl Target {
    /// The host machine's target, evaluated at compile time.
    pub fn host() -> Self {
        Self {
            triple: Triple::host(),
        }
    }

    /// Construct a target from a [`target_lexicon::Triple`].
    pub fn from_triple(triple: Triple) -> Self {
        Self { triple }
    }

    /// The underlying [`target_lexicon::Triple`].
    pub fn triple(&self) -> &Triple {
        &self.triple
    }

    /// The triple as a string suitable for passing to LLVM (e.g.
    /// `"x86_64-unknown-linux-gnu"`).
    pub fn triple_string(&self) -> String {
        self.triple.to_string()
    }

    /// The CPU architecture component of this target.
    pub fn arch(&self) -> Arch {
        Arch::from_lexicon(self.triple.architecture)
    }

    /// The operating system component of this target.
    pub fn os(&self) -> Os {
        Os::from_lexicon(self.triple.operating_system)
    }

    /// Whether this target uses ELF object format.
    pub fn is_elf(&self) -> bool {
        self.triple.binary_format == BinaryFormat::Elf
    }

    /// Whether this target uses Mach-O object format.
    pub fn is_macho(&self) -> bool {
        self.triple.binary_format == BinaryFormat::Macho
    }

    /// The curated list of "blessed" targets — those Gruel explicitly tests
    /// and supports. Other LLVM-known triples are accepted (`from_str`
    /// succeeds) but unblessed.
    pub fn all() -> Vec<Target> {
        BLESSED_TRIPLES
            .iter()
            .map(|s| Target::from_str(s).expect("blessed triple must parse"))
            .collect()
    }

    /// Whether the triple is in the blessed list.
    pub fn is_blessed(&self) -> bool {
        Self::all().iter().any(|t| t == self)
    }

    /// Comma-separated string of all blessed target names for help text.
    pub fn all_names() -> String {
        Self::all()
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Blessed target triples — fully tested in CI.
const BLESSED_TRIPLES: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
];

impl FromStr for Target {
    type Err = TargetParseError;

    /// Parse a target from any LLVM-understood triple. Accepts a few
    /// short-form aliases (`x86_64-linux`, `aarch64-linux`,
    /// `aarch64-macos`, `arm64-linux`, `arm64-macos`) used historically by
    /// the CLI.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = match s {
            "x86_64-linux" | "x86-64-linux" => "x86_64-unknown-linux-gnu",
            "aarch64-linux" | "arm64-linux" => "aarch64-unknown-linux-gnu",
            "aarch64-macos" | "arm64-macos" => "aarch64-apple-darwin",
            other => other,
        };
        let triple = Triple::from_str(normalized).map_err(|e| TargetParseError {
            input: s.to_string(),
            message: e.to_string(),
        })?;
        Ok(Target { triple })
    }
}

/// Error returned when a target triple fails to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetParseError {
    pub input: String,
    pub message: String,
}

impl fmt::Display for TargetParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid target '{}': {}", self.input, self.message)
    }
}

impl std::error::Error for TargetParseError {}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.triple)
    }
}

impl Default for Target {
    fn default() -> Self {
        Self::host()
    }
}

/// The CPU architecture of a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    X86,
    X86_64,
    Arm,
    Aarch64,
    Riscv32,
    Riscv64,
    Wasm32,
    Wasm64,
    /// Any architecture we don't model individually (`target-lexicon` may know
    /// it, but Gruel hasn't classified it).
    Unknown,
}

impl Arch {
    fn from_lexicon(a: Architecture) -> Self {
        match a {
            Architecture::X86_32(_) => Arch::X86,
            Architecture::X86_64 | Architecture::X86_64h => Arch::X86_64,
            Architecture::Arm(_) => Arch::Arm,
            Architecture::Aarch64(_) => Arch::Aarch64,
            Architecture::Riscv32(_) => Arch::Riscv32,
            Architecture::Riscv64(_) => Arch::Riscv64,
            Architecture::Wasm32 => Arch::Wasm32,
            Architecture::Wasm64 => Arch::Wasm64,
            _ => Arch::Unknown,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Arch::X86 => "x86",
            Arch::X86_64 => "x86-64",
            Arch::Arm => "arm",
            Arch::Aarch64 => "aarch64",
            Arch::Riscv32 => "riscv32",
            Arch::Riscv64 => "riscv64",
            Arch::Wasm32 => "wasm32",
            Arch::Wasm64 => "wasm64",
            Arch::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// The operating system of a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Linux,
    Macos,
    Windows,
    /// No OS — bare metal / freestanding.
    Freestanding,
    /// WebAssembly System Interface.
    Wasi,
    /// Any OS we don't model individually.
    Unknown,
}

impl Os {
    fn from_lexicon(o: OperatingSystem) -> Self {
        match o {
            OperatingSystem::Linux => Os::Linux,
            OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_) | OperatingSystem::IOS(_) => {
                // We treat all Apple OSes as Macos for our intrinsic; iOS/etc.
                // are unusual targets and currently uninteresting.
                Os::Macos
            }
            OperatingSystem::Windows => Os::Windows,
            OperatingSystem::Wasi | OperatingSystem::WasiP1 | OperatingSystem::WasiP2 => Os::Wasi,
            OperatingSystem::None_ | OperatingSystem::Unknown => Os::Freestanding,
            _ => Os::Unknown,
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
            Os::Freestanding => "freestanding",
            Os::Wasi => "wasi",
            Os::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Target {
        s.parse().unwrap()
    }

    #[test]
    fn parses_canonical_triples() {
        assert_eq!(t("x86_64-unknown-linux-gnu").arch(), Arch::X86_64);
        assert_eq!(t("x86_64-unknown-linux-gnu").os(), Os::Linux);
        assert_eq!(t("aarch64-unknown-linux-gnu").arch(), Arch::Aarch64);
        assert_eq!(t("aarch64-unknown-linux-gnu").os(), Os::Linux);
        assert_eq!(t("aarch64-apple-darwin").arch(), Arch::Aarch64);
        assert_eq!(t("aarch64-apple-darwin").os(), Os::Macos);
    }

    #[test]
    fn parses_short_aliases() {
        assert_eq!(t("x86_64-linux"), t("x86_64-unknown-linux-gnu"));
        assert_eq!(t("x86-64-linux"), t("x86_64-unknown-linux-gnu"));
        assert_eq!(t("aarch64-linux"), t("aarch64-unknown-linux-gnu"));
        assert_eq!(t("arm64-linux"), t("aarch64-unknown-linux-gnu"));
        assert_eq!(t("aarch64-macos"), t("aarch64-apple-darwin"));
        assert_eq!(t("arm64-macos"), t("aarch64-apple-darwin"));
    }

    #[test]
    fn parses_extended_triples() {
        // These are not "blessed" but must parse.
        assert_eq!(t("riscv64gc-unknown-linux-gnu").arch(), Arch::Riscv64);
        assert_eq!(t("riscv32imc-unknown-none-elf").arch(), Arch::Riscv32);
        assert_eq!(t("wasm32-unknown-unknown").arch(), Arch::Wasm32);
        assert_eq!(t("x86_64-pc-windows-msvc").os(), Os::Windows);
        assert_eq!(t("aarch64-unknown-none").os(), Os::Freestanding);
        assert_eq!(t("wasm32-wasi").os(), Os::Wasi);
    }

    #[test]
    fn rejects_garbage() {
        assert!("not-a-triple-at-all".parse::<Target>().is_err());
    }

    #[test]
    fn binary_format() {
        assert!(t("x86_64-unknown-linux-gnu").is_elf());
        assert!(t("aarch64-unknown-linux-gnu").is_elf());
        assert!(!t("aarch64-unknown-linux-gnu").is_macho());
        assert!(t("aarch64-apple-darwin").is_macho());
        assert!(!t("aarch64-apple-darwin").is_elf());
    }

    #[test]
    fn default_is_host() {
        assert_eq!(Target::default(), Target::host());
    }

    #[test]
    fn blessed_targets_round_trip() {
        for target in Target::all() {
            let s = target.to_string();
            let parsed: Target = s.parse().expect("Display output should re-parse");
            assert_eq!(target, parsed);
            assert!(target.is_blessed());
        }
    }

    #[test]
    fn unblessed_target_is_not_blessed() {
        let t = "wasm32-wasi".parse::<Target>().unwrap();
        assert!(!t.is_blessed());
    }

    #[test]
    fn arch_display_matches_legacy() {
        assert_eq!(Arch::X86_64.to_string(), "x86-64");
        assert_eq!(Arch::Aarch64.to_string(), "aarch64");
    }

    #[test]
    fn os_display_matches_legacy() {
        assert_eq!(Os::Linux.to_string(), "linux");
        assert_eq!(Os::Macos.to_string(), "macos");
    }
}
