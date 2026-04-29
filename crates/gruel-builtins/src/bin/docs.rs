//! Regenerate the checked-in built-in types reference page.
//!
//! Usage: `cargo run -p gruel-builtins --bin gruel-builtins-docs -- [path]`
//!
//! With no arguments, writes to `docs/generated/builtins-reference.md`
//! relative to the workspace root. With one argument, writes to that path.
//! `make check-builtins-docs` runs this into a temp file and diffs against
//! the checked-in copy to catch drift.

use std::fs;
use std::path::{Path, PathBuf};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let out_path: PathBuf = match args.next() {
        Some(p) => PathBuf::from(p),
        None => default_output_path(),
    };

    let content = gruel_builtins::render_reference_markdown();

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out_path, content)?;
    eprintln!("wrote {}", out_path.display());
    Ok(())
}

fn default_output_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root: &Path = Path::new(manifest_dir)
        .ancestors()
        .nth(2)
        .expect("workspace root must exist two levels above the crate manifest");
    workspace_root.join("docs/generated/builtins-reference.md")
}
