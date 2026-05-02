//! BLAKE3-based fingerprinting helpers.
//!
//! [`Hasher`] is a thin wrapper over `blake3::Hasher` that exposes the
//! `update`/`finalize` flow the rest of the compiler uses to build cache
//! keys. [`CacheKey`] is the finalized 32-byte hash plus a hex string for
//! filenames.

use std::fmt;

/// Compute a BLAKE3 hash of a single byte slice.
///
/// Convenience for callers who already have the full input in hand.
pub fn blake3_bytes(bytes: &[u8]) -> CacheKey {
    let mut h = Hasher::new();
    h.update(bytes);
    h.finalize()
}

/// Incremental BLAKE3 hasher.
///
/// Wraps `blake3::Hasher` so callers don't need to depend on the `blake3`
/// crate directly. Use [`Hasher::update`] in any order to mix in inputs;
/// the resulting [`CacheKey`] is deterministic for a given sequence of
/// updates.
#[derive(Default)]
pub struct Hasher {
    inner: blake3::Hasher,
}

impl Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
        self.inner.update(bytes);
        self
    }

    /// Mix in a `u32` in little-endian. Useful for embedding numeric
    /// discriminants (target arch, opt level, schema version, ...) into a
    /// key without committing to a string encoding.
    pub fn update_u32(&mut self, value: u32) -> &mut Self {
        self.inner.update(&value.to_le_bytes());
        self
    }

    /// Mix in a `u64` in little-endian.
    pub fn update_u64(&mut self, value: u64) -> &mut Self {
        self.inner.update(&value.to_le_bytes());
        self
    }

    /// Mix in a length-prefixed byte string. Use this instead of bare
    /// `update` when concatenating multiple variable-length fields, so that
    /// `("ab", "c")` and `("a", "bc")` don't collide.
    pub fn update_str(&mut self, s: &str) -> &mut Self {
        self.update_u64(s.len() as u64);
        self.inner.update(s.as_bytes());
        self
    }

    pub fn finalize(self) -> CacheKey {
        let hash = self.inner.finalize();
        CacheKey {
            bytes: *hash.as_bytes(),
        }
    }
}

/// A 32-byte BLAKE3 hash used as a cache identifier.
///
/// The hex form ([`CacheKey::hex`]) is the on-disk filename; the byte
/// form is what gets mixed into compound keys.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    bytes: [u8; 32],
}

impl CacheKey {
    /// Construct a key directly from a 32-byte hash. Mostly useful for
    /// tests and for re-hydrating keys read from disk; production code
    /// should produce keys via [`Hasher::finalize`].
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// 64-character lowercase hex encoding, used as the on-disk filename.
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for byte in &self.bytes {
            s.push_str(&format!("{:02x}", byte));
        }
        s
    }
}

impl fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show only the first 8 hex chars in Debug to keep logs readable.
        let hex = self.hex();
        write!(f, "CacheKey({}…)", &hex[..8])
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_stable_hash() {
        let k = blake3_bytes(b"");
        // BLAKE3("") is a well-known constant.
        assert_eq!(
            k.hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn update_str_is_length_prefixed() {
        // Without length-prefixing, ("ab", "c") and ("a", "bc") collide.
        // With it, they must not.
        let mut h1 = Hasher::new();
        h1.update_str("ab").update_str("c");
        let mut h2 = Hasher::new();
        h2.update_str("a").update_str("bc");
        assert_ne!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn raw_update_is_not_length_prefixed() {
        // Sanity check that update (raw) does NOT length-prefix, so it
        // collides on concatenation. This is intentional — callers use
        // raw update for fixed-size fields and update_str/update_u32
        // for variable-size fields.
        let mut h1 = Hasher::new();
        h1.update(b"abc");
        let mut h2 = Hasher::new();
        h2.update(b"a").update(b"bc");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn cache_key_hex_is_64_chars() {
        let k = blake3_bytes(b"some content");
        assert_eq!(k.hex().len(), 64);
        assert!(k.hex().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn debug_truncates() {
        let k = blake3_bytes(b"x");
        let dbg = format!("{:?}", k);
        assert!(dbg.starts_with("CacheKey("));
        assert!(dbg.ends_with("…)"));
    }
}
