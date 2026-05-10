//! Verify that every `@<name>` token mentioned in the human-written
//! specification (`docs/spec/src/`) corresponds to an actual builtin.
//!
//! Catches drift like the spec describing `@intCast` while the registry
//! only knows `@cast` (the kind of bug that traceability + spec tests
//! cannot see, because they only check that paragraph IDs are covered
//! and that test sources compile — not that the prose is accurate).
//!
//! Usage: `cargo run -p gruel-intrinsics --bin gruel-check-spec [<spec-dir>]`
//!
//! Default `<spec-dir>` is `<workspace-root>/docs/spec/src`. Wired into
//! `make check` via the `check-spec-builtins` target.
//!
//! A name is allowed if it is:
//! - In the intrinsics registry (`gruel_intrinsics::lookup_by_name`)
//! - A known directive (`allow`, `derive`, `lang`, `mark`)
//! - Listed under FILE_EXCEPTIONS for the file in which it appears
//!   (used for retired/typo names that the spec deliberately mentions)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gruel_intrinsics::lookup_by_name;

const KNOWN_DIRECTIVES: &[&str] = &["allow", "derive", "lang", "mark"];

/// Per-file allowlist for names the spec deliberately documents but which
/// are not real builtins. Path is relative to the spec root.
const FILE_EXCEPTIONS: &[(&str, &[&str])] = &[
    // 2.5:32 documents typos as examples of suggestion-aware diagnostics;
    // 2.5:33 lists retired directive names with the ADRs that removed them.
    (
        "02-lexical-structure/05-builtins.md",
        &["allwo", "dervie", "copy", "handle"],
    ),
    // ADR-0047 retired @compileLog in favor of @dbg. The retirement is
    // documented under stable paragraph IDs.
    ("04-expressions/14-comptime.md", &["compileLog"]),
];

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let spec_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_spec_dir);

    if !spec_dir.is_dir() {
        eprintln!("spec dir does not exist: {}", spec_dir.display());
        return ExitCode::FAILURE;
    }

    let mut findings: Vec<Finding> = Vec::new();
    walk_md(&spec_dir, &mut |path| {
        scan_file(&spec_dir, path, &mut findings)
    });

    if findings.is_empty() {
        return ExitCode::SUCCESS;
    }

    findings.sort_by(|a, b| {
        a.rel_path
            .cmp(&b.rel_path)
            .then(a.line.cmp(&b.line))
            .then(a.name.cmp(&b.name))
    });

    eprintln!("Spec references unknown @builtins:");
    for f in &findings {
        eprintln!("  {}:{}: @{}", f.rel_path.display(), f.line, f.name);
    }
    eprintln!();
    eprintln!("If the name is real, register it in crates/gruel-intrinsics/src/lib.rs.");
    eprintln!(
        "If the spec is wrong, fix the prose. If the name is intentionally\n\
         documented (retired/typo example), add it to FILE_EXCEPTIONS in\n\
         crates/gruel-intrinsics/src/bin/check_spec.rs."
    );
    ExitCode::FAILURE
}

struct Finding {
    rel_path: PathBuf,
    line: usize,
    name: String,
}

fn scan_file(spec_root: &Path, path: &Path, findings: &mut Vec<Finding>) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    let rel_path = path.strip_prefix(spec_root).unwrap_or(path).to_path_buf();

    for (idx, line) in content.lines().enumerate() {
        for name in extract_at_idents(line) {
            if is_allowed(&rel_path, name) {
                continue;
            }
            findings.push(Finding {
                rel_path: rel_path.clone(),
                line: idx + 1,
                name: name.to_string(),
            });
        }
    }
}

fn is_allowed(rel_path: &Path, name: &str) -> bool {
    if lookup_by_name(name).is_some() {
        return true;
    }
    if KNOWN_DIRECTIVES.contains(&name) {
        return true;
    }
    let rel_str = rel_path.to_string_lossy();
    for (file, names) in FILE_EXCEPTIONS {
        if rel_str.replace('\\', "/").ends_with(file) && names.contains(&name) {
            return true;
        }
    }
    false
}

/// Extract identifiers that follow `@`. Skips `@/...` (Zola link refs),
/// any `@` not followed by an identifier-start character, and any `@`
/// preceded by an alphanumeric (so email-like patterns are not picked up).
fn extract_at_idents(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let preceded_by_word =
                i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            let start = i + 1;
            if !preceded_by_word
                && start < bytes.len()
                && (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_')
            {
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                out.push(&line[start..end]);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn walk_md(path: &Path, f: &mut dyn FnMut(&Path)) {
    if path.is_file() {
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            f(path);
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        walk_md(&entry.path(), f);
    }
}

fn default_spec_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root: &Path = Path::new(manifest_dir)
        .ancestors()
        .nth(2)
        .expect("workspace root must exist two levels above the crate manifest");
    workspace_root.join("docs/spec/src")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_simple_intrinsic() {
        assert_eq!(extract_at_idents("call `@dbg(x)`"), vec!["dbg"]);
    }

    #[test]
    fn extracts_multiple_per_line() {
        let line = "Use `@cast` or `@parse_i32` instead of `@intCast`.";
        assert_eq!(
            extract_at_idents(line),
            vec!["cast", "parse_i32", "intCast"]
        );
    }

    #[test]
    fn skips_link_references() {
        // `@/04-expressions/...` is a Zola link path, not a builtin.
        let line = "See [`@dbg`](@/04-expressions/13-intrinsics.md)";
        assert_eq!(extract_at_idents(line), vec!["dbg"]);
    }

    #[test]
    fn skips_isolated_at() {
        assert_eq!(extract_at_idents("foo@bar"), Vec::<&str>::new());
        assert_eq!(extract_at_idents("@ ident"), Vec::<&str>::new());
    }

    #[test]
    fn registry_names_are_allowed() {
        assert!(is_allowed(Path::new("any.md"), "cast"));
        assert!(is_allowed(Path::new("any.md"), "dbg"));
    }

    #[test]
    fn directives_are_allowed() {
        assert!(is_allowed(Path::new("any.md"), "allow"));
        assert!(is_allowed(Path::new("any.md"), "derive"));
        assert!(is_allowed(Path::new("any.md"), "lang"));
    }

    #[test]
    fn file_exceptions_are_scoped() {
        let scoped = Path::new("02-lexical-structure/05-builtins.md");
        let other = Path::new("04-expressions/13-intrinsics.md");
        assert!(is_allowed(scoped, "allwo"));
        assert!(!is_allowed(other, "allwo"));
    }

    #[test]
    fn unknown_names_rejected() {
        assert!(!is_allowed(Path::new("any.md"), "intCast"));
        assert!(!is_allowed(Path::new("any.md"), "compileError"));
        assert!(!is_allowed(Path::new("any.md"), "transmute"));
    }
}
