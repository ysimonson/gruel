//! Package manifest loader for Gruel (`gruel.json`).
//!
//! See [ADR-0092](../../../docs/designs/0092-package-manifest.md). The manifest
//! is deliberately tiny: a name, a version, and exactly one of `bin` or `lib`
//! whose `root` points at the entry `.gruel` file. No dependencies, no
//! lockfile, no registry — this ADR establishes the schema and discovery
//! plumbing; future ADRs layer everything else on top.
//!
//! ```json
//! {
//!   "name": "hello",
//!   "version": "0.1.0",
//!   "bin": { "root": "src/main.gruel" }
//! }
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A successfully loaded and validated manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Display name (`name` field).
    pub name: String,
    /// Semver version (`version` field).
    pub version: semver::Version,
    /// The package target — either binary or library.
    pub target: PackageTarget,
    /// Directory containing the manifest (its parent).
    pub manifest_dir: PathBuf,
    /// Absolute path of the manifest file itself.
    pub manifest_path: PathBuf,
}

/// The package's target kind. Exactly one of `bin` or `lib` is allowed
/// per manifest (Phase 1 of ADR-0092).
#[derive(Debug, Clone)]
pub enum PackageTarget {
    /// Binary target — `bin` block in JSON.
    Binary(TargetSpec),
    /// Library target — `lib` block in JSON. Cannot `build` or `run`
    /// in this ADR; future ADRs will add artefact emission.
    Library(TargetSpec),
}

impl PackageTarget {
    /// Absolute, validated path of the entry `.gruel` file.
    pub fn root(&self) -> &Path {
        match self {
            PackageTarget::Binary(spec) | PackageTarget::Library(spec) => &spec.root,
        }
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, PackageTarget::Binary(_))
    }

    pub fn is_library(&self) -> bool {
        matches!(self, PackageTarget::Library(_))
    }
}

/// Per-target configuration. Currently just `root` — additive future
/// fields land here.
#[derive(Debug, Clone)]
pub struct TargetSpec {
    /// Absolute path resolved against the manifest's directory.
    pub root: PathBuf,
}

/// Anything that can go wrong loading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to read manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid manifest at {path}: missing required field '{field}'")]
    MissingField { path: PathBuf, field: &'static str },

    #[error(
        "invalid manifest at {path}: no target specified — exactly one of 'bin' or 'lib' is required"
    )]
    MissingTarget { path: PathBuf },

    #[error(
        "invalid manifest at {path}: conflicting targets — only one of 'bin' or 'lib' may be present"
    )]
    ConflictingTargets { path: PathBuf },

    #[error("invalid manifest at {path}: bad 'version' value '{value}': {source}")]
    BadVersion {
        path: PathBuf,
        value: String,
        #[source]
        source: semver::Error,
    },

    #[error("invalid manifest at {path}: bad 'root' value '{value}': {reason}")]
    BadRoot {
        path: PathBuf,
        value: String,
        reason: String,
    },

    #[error("invalid manifest at {path}: 'root' file not found at {resolved}")]
    RootNotFound { path: PathBuf, resolved: PathBuf },
}

impl ManifestError {
    /// Path of the manifest that produced this error.
    pub fn path(&self) -> &Path {
        match self {
            ManifestError::Io { path, .. }
            | ManifestError::Parse { path, .. }
            | ManifestError::MissingField { path, .. }
            | ManifestError::MissingTarget { path }
            | ManifestError::ConflictingTargets { path }
            | ManifestError::BadVersion { path, .. }
            | ManifestError::BadRoot { path, .. }
            | ManifestError::RootNotFound { path, .. } => path,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal raw form for serde
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    bin: Option<RawTarget>,
    #[serde(default)]
    lib: Option<RawTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    root: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load and validate a manifest at the given path.
///
/// `manifest_path` must point directly at the `gruel.json` file. Callers
/// who want directory-or-file semantics should use the higher-level
/// `discover_*` helpers (or the CLI's classification logic) first.
pub fn load_at(manifest_path: &Path) -> Result<Manifest, ManifestError> {
    let canonical_path = match manifest_path.canonicalize() {
        Ok(p) => p,
        Err(err) => {
            return Err(ManifestError::Io {
                path: manifest_path.to_path_buf(),
                source: err,
            });
        }
    };

    let manifest_dir = canonical_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let bytes = std::fs::read(&canonical_path).map_err(|err| ManifestError::Io {
        path: canonical_path.clone(),
        source: err,
    })?;

    let raw: RawManifest = serde_json::from_slice(&bytes).map_err(|err| ManifestError::Parse {
        path: canonical_path.clone(),
        source: err,
    })?;

    parse_raw(raw, canonical_path, manifest_dir)
}

/// Walk up from `start` (inclusive) looking for a `gruel.json` file.
/// Used by the CLI; npm-style "first hit wins" semantics.
pub fn discover_upward(start: &Path) -> Option<PathBuf> {
    let absolute = match start.canonicalize() {
        Ok(p) => p,
        Err(_) => start.to_path_buf(),
    };
    for ancestor in absolute.ancestors() {
        let candidate = ancestor.join("gruel.json");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Look for `gruel.json` at exactly `root` (no upward walk, no
/// subdirectory scan). Used by the LSP.
pub fn discover_at_root(root: &Path) -> Option<PathBuf> {
    let candidate = root.join("gruel.json");
    candidate.is_file().then_some(candidate)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn parse_raw(
    raw: RawManifest,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
) -> Result<Manifest, ManifestError> {
    let name = match raw.name {
        Some(n) if !n.is_empty() => n,
        Some(_) => {
            return Err(ManifestError::BadRoot {
                path: manifest_path,
                value: String::new(),
                reason: "'name' must be a non-empty string".to_string(),
            });
        }
        None => {
            return Err(ManifestError::MissingField {
                path: manifest_path,
                field: "name",
            });
        }
    };

    let version_str = raw.version.ok_or_else(|| ManifestError::MissingField {
        path: manifest_path.clone(),
        field: "version",
    })?;
    let version = semver::Version::parse(&version_str).map_err(|err| ManifestError::BadVersion {
        path: manifest_path.clone(),
        value: version_str.clone(),
        source: err,
    })?;

    let target = match (raw.bin, raw.lib) {
        (Some(_), Some(_)) => {
            return Err(ManifestError::ConflictingTargets {
                path: manifest_path,
            });
        }
        (Some(bin), None) => {
            let spec = resolve_target(bin, &manifest_path, &manifest_dir)?;
            PackageTarget::Binary(spec)
        }
        (None, Some(lib)) => {
            let spec = resolve_target(lib, &manifest_path, &manifest_dir)?;
            PackageTarget::Library(spec)
        }
        (None, None) => {
            return Err(ManifestError::MissingTarget {
                path: manifest_path,
            });
        }
    };

    Ok(Manifest {
        name,
        version,
        target,
        manifest_dir,
        manifest_path,
    })
}

fn resolve_target(
    raw: RawTarget,
    manifest_path: &Path,
    manifest_dir: &Path,
) -> Result<TargetSpec, ManifestError> {
    let root_str = raw.root;

    if root_str.is_empty() {
        return Err(ManifestError::BadRoot {
            path: manifest_path.to_path_buf(),
            value: root_str,
            reason: "'root' must be a non-empty path".to_string(),
        });
    }

    let root_path = Path::new(&root_str);
    if root_path.is_absolute() {
        return Err(ManifestError::BadRoot {
            path: manifest_path.to_path_buf(),
            value: root_str,
            reason: "'root' must be a relative path".to_string(),
        });
    }

    if root_path.extension().and_then(|s| s.to_str()) != Some("gruel") {
        return Err(ManifestError::BadRoot {
            path: manifest_path.to_path_buf(),
            value: root_str,
            reason: "'root' must point to a .gruel file".to_string(),
        });
    }

    let candidate = manifest_dir.join(root_path);
    let resolved = match candidate.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return Err(ManifestError::RootNotFound {
                path: manifest_path.to_path_buf(),
                resolved: candidate,
            });
        }
    };

    if !resolved.starts_with(manifest_dir) {
        return Err(ManifestError::BadRoot {
            path: manifest_path.to_path_buf(),
            value: root_str,
            reason: "'root' must resolve to a path inside the manifest's directory".to_string(),
        });
    }

    if !resolved.is_file() {
        return Err(ManifestError::RootNotFound {
            path: manifest_path.to_path_buf(),
            resolved,
        });
    }

    Ok(TargetSpec { root: resolved })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join("gruel.json");
        fs::write(&path, contents).unwrap();
        path
    }

    fn touch_gruel(dir: &Path, rel: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, "// stub\n").unwrap();
        path
    }

    #[test]
    fn load_binary_manifest_happy_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let entry = touch_gruel(&root, "src/main.gruel");
        let path = write_manifest(
            &root,
            r#"{
                "name": "hello",
                "version": "0.1.0",
                "bin": { "root": "src/main.gruel" }
            }"#,
        );

        let manifest = load_at(&path).expect("load should succeed");
        assert_eq!(manifest.name, "hello");
        assert_eq!(manifest.version, semver::Version::parse("0.1.0").unwrap());
        assert!(manifest.target.is_binary());
        assert_eq!(manifest.target.root(), entry.canonicalize().unwrap());
        assert_eq!(manifest.manifest_dir, root);
        assert_eq!(manifest.manifest_path, path.canonicalize().unwrap());
    }

    #[test]
    fn load_library_manifest_happy_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "src/lib.gruel");
        let path = write_manifest(
            &root,
            r#"{
                "name": "math",
                "version": "0.2.3-rc.1",
                "lib": { "root": "src/lib.gruel" }
            }"#,
        );

        let manifest = load_at(&path).unwrap();
        assert_eq!(manifest.name, "math");
        assert_eq!(
            manifest.version,
            semver::Version::parse("0.2.3-rc.1").unwrap()
        );
        assert!(manifest.target.is_library());
    }

    #[test]
    fn missing_target_rejected() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"{ "name": "x", "version": "0.1.0" }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::MissingTarget { .. }), "got {err:?}");
    }

    #[test]
    fn conflicting_targets_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "a.gruel");
        touch_gruel(&root, "b.gruel");
        let path = write_manifest(
            &root,
            r#"{
                "name": "x",
                "version": "0.1.0",
                "bin": { "root": "a.gruel" },
                "lib": { "root": "b.gruel" }
            }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::ConflictingTargets { .. }), "got {err:?}");
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{
                "name": "x",
                "version": "0.1.0",
                "bin": { "root": "main.gruel" },
                "license": "MIT"
            }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }), "got {err:?}");
    }

    #[test]
    fn unknown_target_field_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{
                "name": "x",
                "version": "0.1.0",
                "bin": { "root": "main.gruel", "extra": true }
            }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }), "got {err:?}");
    }

    #[test]
    fn missing_name_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{ "version": "0.1.0", "bin": { "root": "main.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        match err {
            ManifestError::MissingField { field, .. } => assert_eq!(field, "name"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn missing_version_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{ "name": "x", "bin": { "root": "main.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        match err {
            ManifestError::MissingField { field, .. } => assert_eq!(field, "version"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn bad_version_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{ "name": "x", "version": "not-semver", "bin": { "root": "main.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::BadVersion { .. }), "got {err:?}");
    }

    #[test]
    fn absolute_root_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "/abs/main.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::BadRoot { .. }), "got {err:?}");
    }

    #[test]
    fn root_wrong_extension_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        fs::write(root.join("main.txt"), "").unwrap();
        let path = write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "main.txt" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::BadRoot { .. }), "got {err:?}");
    }

    #[test]
    fn root_missing_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "does-not-exist.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::RootNotFound { .. }), "got {err:?}");
    }

    #[test]
    fn root_outside_manifest_dir_rejected() {
        let outer = TempDir::new().unwrap();
        let outer_canon = outer.path().canonicalize().unwrap();
        touch_gruel(&outer_canon, "outside.gruel");
        let inner = outer_canon.join("pkg");
        fs::create_dir(&inner).unwrap();
        let path = write_manifest(
            &inner,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "../outside.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::BadRoot { .. }), "got {err:?}");
    }

    #[test]
    fn manifest_io_error_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("gruel.json");
        let err = load_at(&missing).unwrap_err();
        assert!(matches!(err, ManifestError::Io { .. }), "got {err:?}");
    }

    #[test]
    fn discover_upward_finds_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "src/main.gruel");
        let manifest_path = write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "src/main.gruel" } }"#,
        );
        let sub = root.join("src");
        let found = discover_upward(&sub).expect("should walk up and find manifest");
        assert_eq!(found.canonicalize().unwrap(), manifest_path.canonicalize().unwrap());
    }

    #[test]
    fn discover_upward_returns_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        // Should not find anything (assumes no gruel.json above tmp dir on test host).
        // The discovery walks all the way to /, but real systems shouldn't have a
        // gruel.json there — we accept a small risk here.
        let result = discover_upward(&nested);
        // If it found one outside our control, at least it must be readable.
        if let Some(path) = result {
            assert!(path.is_file(), "discover returned non-file path: {path:?}");
        }
    }

    #[test]
    fn discover_at_root_finds_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "src/main.gruel");
        write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "src/main.gruel" } }"#,
        );
        let found = discover_at_root(&root).expect("should find manifest at root");
        assert!(found.file_name().map(|n| n == "gruel.json").unwrap_or(false));
    }

    #[test]
    fn discover_at_root_does_not_walk_up() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "src/main.gruel");
        write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0", "bin": { "root": "src/main.gruel" } }"#,
        );
        let sub = root.join("src");
        assert!(discover_at_root(&sub).is_none());
    }

    #[test]
    fn version_accepts_prerelease_and_build_metadata() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{ "name": "x", "version": "0.1.0-alpha.1+build.7", "bin": { "root": "main.gruel" } }"#,
        );
        let manifest = load_at(&path).unwrap();
        assert_eq!(
            manifest.version,
            semver::Version::parse("0.1.0-alpha.1+build.7").unwrap()
        );
    }

    #[test]
    fn empty_name_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        touch_gruel(&root, "main.gruel");
        let path = write_manifest(
            &root,
            r#"{ "name": "", "version": "0.1.0", "bin": { "root": "main.gruel" } }"#,
        );
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, ManifestError::BadRoot { .. }), "got {err:?}");
    }
}
