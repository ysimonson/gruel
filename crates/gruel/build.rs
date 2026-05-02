//! Build-time metadata for the `gruel` binary.
//!
//! Embeds the current git SHA and a `dirty` flag so `gruel --version` can
//! tell users which compiler build they're running. Per ADR-0074, these
//! are NOT part of the cache key — the binary-bytes hash already captures
//! anything they encode — but they make "why did my cache invalidate?"
//! answerable.

use std::process::Command;

fn main() {
    // Re-run if any of these change, but DON'T re-run on every source file
    // change (that would be too aggressive — we'd rerun on every edit).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GRUEL_GIT_SHA");
    println!("cargo:rerun-if-env-changed=GRUEL_GIT_DIRTY");
    // The git directory's HEAD ref is the right "this commit changed"
    // signal. .git/HEAD itself moves when you switch branches; the
    // referenced ref file moves on commit.
    rerun_on_git_head();

    let sha = env_or("GRUEL_GIT_SHA", git_sha);
    let dirty = env_or("GRUEL_GIT_DIRTY", git_dirty);
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());

    let dirty_marker = if dirty == "1" { " +dirty" } else { "" };
    let full_version = format!("{} ({}{})", pkg_version, sha, dirty_marker);

    println!("cargo:rustc-env=GRUEL_GIT_SHA={}", sha);
    println!("cargo:rustc-env=GRUEL_GIT_DIRTY={}", dirty);
    println!("cargo:rustc-env=GRUEL_VERSION={}", full_version);
}

fn env_or(name: &str, fallback: impl FnOnce() -> String) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback())
}

fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn git_dirty() -> String {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| if o.stdout.is_empty() { "0" } else { "1" })
        .unwrap_or("unknown")
        .to_string()
}

fn rerun_on_git_head() {
    // .git/HEAD changes on branch switches; the ref it points at changes
    // on commits. Tell cargo to watch both.
    let head_path = std::path::Path::new("../../.git/HEAD");
    if !head_path.exists() {
        return;
    }
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    if let Ok(head) = std::fs::read_to_string(head_path)
        && let Some(rest) = head.strip_prefix("ref: ")
    {
        let ref_path = format!("../../.git/{}", rest.trim());
        if std::path::Path::new(&ref_path).exists() {
            println!("cargo:rerun-if-changed={}", ref_path);
        }
    }
}
