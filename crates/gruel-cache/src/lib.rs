//! On-disk content-addressed cache for incremental compilation (ADR-0074).
//!
//! This crate provides the storage and fingerprinting primitives the compiler
//! driver uses to skip work for files (and per-function bitcode) whose inputs
//! haven't changed since the last build. It does **not** know about Gruel
//! source, AST, or AIR — those are serialized by their owning crates and
//! handed to this crate as opaque byte blobs keyed by [`CacheKey`]s.
//!
//! Architecture:
//!
//! - [`CacheStore`] is a content-addressed on-disk store rooted at some cache
//!   directory (typically `target/gruel-cache/`). All writes are atomic
//!   (write to `tmp/`, then `rename`). All filenames are content-hashes
//!   (BLAKE3, hex-encoded) so concurrent invocations cannot corrupt each
//!   other.
//! - [`Hasher`] / [`CacheKey`] wrap BLAKE3 to give the rest of the compiler
//!   a typed-key surface instead of raw byte slices.
//! - [`compiler_fp`] hashes the running compiler binary, memoized across
//!   invocations by `(path, mtime, size)`. This is the load-bearing
//!   fingerprint that invalidates the entire cache when the compiler itself
//!   changes — including local `cargo build` cycles during compiler dev.
//!
//! See ADR-0074 for the full design rationale.

mod compiler_fp;
mod fingerprint;
mod store;
mod wire;

pub use compiler_fp::{compiler_fingerprint, current_binary_path};
pub use fingerprint::{CacheKey, Hasher, blake3_bytes};
pub use store::{CacheKind, CacheStats, CacheStore};
pub use wire::{CachedParseOutput, CachedRirOutput, InternerSnapshot};

/// On-disk cache schema version. Bump when the layout of any cached blob
/// changes in an incompatible way. The store wipes the cache directory on
/// startup if the persisted version doesn't match this constant.
///
/// History:
/// - 1: initial layout (parse, air, llvm-ir).
pub const CACHE_SCHEMA_VERSION: u32 = 1;
