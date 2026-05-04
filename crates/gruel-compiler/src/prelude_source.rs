//! Prelude source assembly (ADR-0078).
//!
//! The Gruel prelude was originally a single embedded raw-string constant in
//! `unit.rs`. ADR-0078 Phase 1 moves the prelude onto disk under
//! `std/prelude/*.gruel`, with a per-topic split (option, result, char,
//! string, …). The compiler still parses one virtual prelude source under
//! `FileId::PRELUDE`, but the *content* is now assembled by concatenating
//! the on-disk files in a stable order.
//!
//! Resolution order, mirroring `resolve_std_import` in
//! `crates/gruel-air/src/sema/analysis.rs`:
//!
//! 1. `$GRUEL_STD_PATH/prelude/*.gruel` if `GRUEL_STD_PATH` is set
//! 2. Walk up from the binary's manifest directory looking for
//!    `std/prelude/*.gruel`
//! 3. Embedded fallback via `include_str!` — guaranteed to compile and
//!    distribute in the binary, used when no on-disk stdlib is found
//!
//! The fallback exists because (a) hosts running `gruel` outside a checked-out
//! repo have no on-disk stdlib, and (b) `Sema`-direct unit tests should not
//! depend on filesystem layout.
//!
//! The list of prelude files is closed and ordered. To add a new prelude file,
//! append it to `PRELUDE_FILES`.

use std::path::{Path, PathBuf};

/// Embedded copies of every prelude file. Order matters: files are
/// concatenated in this order to form the virtual prelude source.
///
/// Each entry is `(filename, content)`. The filename is used to locate the
/// corresponding on-disk file under `std/prelude/`.
const PRELUDE_FILES: &[(&str, &str)] = &[
    (
        "interfaces.gruel",
        include_str!("../../../std/prelude/interfaces.gruel"),
    ),
    (
        "target.gruel",
        include_str!("../../../std/prelude/target.gruel"),
    ),
    ("cmp.gruel", include_str!("../../../std/prelude/cmp.gruel")),
    (
        "option.gruel",
        include_str!("../../../std/prelude/option.gruel"),
    ),
    (
        "result.gruel",
        include_str!("../../../std/prelude/result.gruel"),
    ),
    (
        "char.gruel",
        include_str!("../../../std/prelude/char.gruel"),
    ),
    (
        "string.gruel",
        include_str!("../../../std/prelude/string.gruel"),
    ),
];

/// Build the virtual prelude source string by concatenating prelude files.
///
/// Tries the on-disk `std/prelude/` first (via `GRUEL_STD_PATH` or a small
/// upward search); falls back to the embedded copies on miss. The result is
/// a single Gruel source string suitable for parsing under
/// `FileId::PRELUDE`.
pub fn assemble_prelude_source() -> String {
    if let Some(dir) = locate_prelude_dir()
        && let Some(source) = read_prelude_dir(&dir)
    {
        return source;
    }
    embedded_prelude_source()
}

/// Concatenated embedded prelude (the `include_str!` fallback).
///
/// Public so unit-test fixtures that bypass `CompilationUnit` (e.g.
/// `Sema`-direct tests) can use it without disk I/O.
pub fn embedded_prelude_source() -> String {
    let mut out = String::new();
    for (_, content) in PRELUDE_FILES {
        out.push_str(content);
        out.push('\n');
    }
    out
}

/// Try to locate `std/prelude/` on disk. Returns the directory path on hit.
fn locate_prelude_dir() -> Option<PathBuf> {
    if let Ok(gruel_std) = std::env::var("GRUEL_STD_PATH") {
        let candidate = Path::new(&gruel_std).join("prelude");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // Walk up from CARGO_MANIFEST_DIR looking for std/prelude/. Stops at the
    // filesystem root. This works for in-repo builds (`cargo run -p gruel`).
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut current: PathBuf = PathBuf::from(manifest);
    loop {
        let candidate = current.join("std").join("prelude");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Read every file listed in `PRELUDE_FILES` from `dir` and concatenate.
/// Returns `None` if any file is missing — the caller falls back to
/// embedded copies in that case so a partial on-disk stdlib doesn't
/// silently shadow the embedded one.
fn read_prelude_dir(dir: &Path) -> Option<String> {
    let mut out = String::new();
    for (filename, _) in PRELUDE_FILES {
        let path = dir.join(filename);
        let content = std::fs::read_to_string(&path).ok()?;
        out.push_str(&content);
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_source_contains_canonical_types() {
        let source = embedded_prelude_source();
        assert!(source.contains("fn Option(comptime T: type)"));
        assert!(source.contains("fn Result(comptime T: type, comptime E: type)"));
        assert!(source.contains("fn char__encode_utf8"));
        assert!(source.contains("fn String__from_utf8"));
    }

    #[test]
    fn assembled_source_matches_embedded_when_disk_missing() {
        // `assemble_prelude_source` should at minimum return the embedded
        // copy. With an in-repo build the disk copy is used, but the disk
        // copy is generated from the same files via `include_str!`, so the
        // contents are byte-equivalent.
        let assembled = assemble_prelude_source();
        let embedded = embedded_prelude_source();
        assert_eq!(assembled, embedded);
    }
}
