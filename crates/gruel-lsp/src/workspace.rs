//! Workspace root discovery + file enumeration (ADR-0091).

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Enumerate every `*.gruel` file under `root`, respecting `.gitignore`
/// and skipping `.git`/`target`.
pub fn enumerate_gruel_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .standard_filters(true)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && name != "target"
        })
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("gruel") {
            out.push(path.to_path_buf());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_gruel_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.gruel"), "fn main() -> i32 { 0 }").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.gruel"), "fn helper() -> i32 { 1 }").unwrap();
        fs::write(root.join("notes.txt"), "not gruel").unwrap();
        let files = enumerate_gruel_files(root);
        let names: Vec<_> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(names.contains(&"a.gruel"));
        assert!(names.contains(&"b.gruel"));
        assert!(!names.contains(&"notes.txt"));
    }
}
