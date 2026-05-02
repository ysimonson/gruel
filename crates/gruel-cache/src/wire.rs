//! Wire format for cached parse and RIR outputs (ADR-0074 Phase 2).
//!
//! The two top-level types — [`CachedParseOutput`] and [`CachedRirOutput`] —
//! bundle the serialized IR with a per-file interner snapshot. On cache hit,
//! the snapshot is re-interned into the build's shared `ThreadedRodeo` and
//! the IR's `Spur` values are remapped from the cached numbering to the
//! build's numbering.
//!
//! Per ADR-0074: "Spurs are file-local; on load they get re-interned into
//! the build-wide interner."
//!
//! The actual remapping (walking AST/RIR to substitute `Spur` values) is
//! implemented in `gruel-cache/src/remap.rs` and lives behind the same
//! preview feature as the rest of the cache. This module only handles the
//! envelope: serialize, deserialize, snapshot/restore the interner.

use lasso::{Key, Spur, ThreadedRodeo};
use serde::{Deserialize, Serialize};

use gruel_parser::ast::Ast;
use gruel_rir::Rir;

/// A per-file interner snapshot — `strings[i]` is the string value of the
/// `Spur` whose raw key equals `i`. Indexed by `Key::into_usize`, which for
/// lasso's default `Spur` is `NonZeroU32` minus one (i.e. the first
/// interned string has key 0, the second has key 1, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InternerSnapshot {
    pub strings: Vec<String>,
}

impl InternerSnapshot {
    /// Build a snapshot from a `ThreadedRodeo`, capturing every interned
    /// string in `Spur::into_usize` order.
    ///
    /// Used at cache *write* time, after a file has finished parsing into
    /// its own per-file interner.
    pub fn capture(interner: &ThreadedRodeo) -> Self {
        // ThreadedRodeo doesn't give us strings in Spur order directly; we
        // collect (Spur, &str) pairs and sort by Spur index.
        let mut pairs: Vec<(usize, String)> = interner
            .iter()
            .map(|(spur, s)| (spur.into_usize(), s.to_string()))
            .collect();
        pairs.sort_by_key(|(idx, _)| *idx);

        // Sanity check: the indices should form a contiguous range starting
        // at 0. If not, the cache assumes Spur ordering it can't honour.
        for (expected, (actual, _)) in pairs.iter().enumerate() {
            debug_assert_eq!(
                expected, *actual,
                "ThreadedRodeo Spurs not contiguous starting at 0; \
                 cache assumes lasso's standard packing"
            );
        }

        Self {
            strings: pairs.into_iter().map(|(_, s)| s).collect(),
        }
    }

    /// Re-intern every string into `target`, returning a remap table where
    /// `remap[i]` is the new `Spur` for the cached string at index `i`.
    ///
    /// Used at cache *read* time, after deserializing a `CachedParseOutput`
    /// or `CachedRirOutput`, before the AST/RIR's `Spur` values can be
    /// trusted against the build's shared interner.
    pub fn restore_into(&self, target: &ThreadedRodeo) -> Vec<Spur> {
        self.strings
            .iter()
            .map(|s| target.get_or_intern(s))
            .collect()
    }
}

/// Envelope around a parsed file's AST + interner snapshot, ready for
/// bincode serialization to the parse cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedParseOutput {
    pub interner: InternerSnapshot,
    pub ast: Ast,
}

impl CachedParseOutput {
    /// Serialize to the bincode wire format used by `CacheStore::put`.
    pub fn encode(&self) -> Result<Vec<u8>, bincode::error::EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::standard())
    }

    /// Deserialize from the bincode wire format. Pairs with
    /// [`CachedParseOutput::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        let (out, _read) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())?;
        Ok(out)
    }
}

/// Envelope around a per-file RIR + interner snapshot, ready for bincode
/// serialization to the RIR cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRirOutput {
    pub interner: InternerSnapshot,
    pub rir: Rir,
}

impl CachedRirOutput {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::error::EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::standard())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        let (out, _read) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_captures_strings_in_spur_order() {
        let interner = ThreadedRodeo::new();
        let s_a = interner.get_or_intern("alpha");
        let s_b = interner.get_or_intern("beta");
        let s_c = interner.get_or_intern("gamma");

        let snapshot = InternerSnapshot::capture(&interner);
        assert_eq!(snapshot.strings.len(), 3);
        assert_eq!(snapshot.strings[s_a.into_usize()], "alpha");
        assert_eq!(snapshot.strings[s_b.into_usize()], "beta");
        assert_eq!(snapshot.strings[s_c.into_usize()], "gamma");
    }

    #[test]
    fn snapshot_round_trips_through_bincode() {
        let interner = ThreadedRodeo::new();
        interner.get_or_intern("hello");
        interner.get_or_intern("world");
        let snap = InternerSnapshot::capture(&interner);

        let encoded = bincode::serde::encode_to_vec(&snap, bincode::config::standard()).unwrap();
        let (decoded, _): (InternerSnapshot, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(decoded.strings, snap.strings);
    }

    #[test]
    fn restore_reinterns_strings_into_target() {
        // Source interner has three strings.
        let src = ThreadedRodeo::new();
        let s_x = src.get_or_intern("x");
        let s_y = src.get_or_intern("y");
        let s_z = src.get_or_intern("z");
        let snap = InternerSnapshot::capture(&src);

        // Target interner already has "y" interned at some Spur.
        let tgt = ThreadedRodeo::new();
        let pre_y = tgt.get_or_intern("y");

        let remap = snap.restore_into(&tgt);

        // "y" in the cache maps to the *existing* Spur in the target.
        assert_eq!(remap[s_y.into_usize()], pre_y);
        // "x" and "z" got Spurs in the target whose strings resolve back
        // to the cached values. We don't assert the Spur values themselves
        // — they may coincidentally match the source interner's Spurs
        // depending on order, and only the string mapping is load-bearing.
        assert_eq!(tgt.resolve(&remap[s_x.into_usize()]), "x");
        assert_eq!(tgt.resolve(&remap[s_z.into_usize()]), "z");
    }

    #[test]
    fn empty_ast_round_trips() {
        let cached = CachedParseOutput {
            interner: InternerSnapshot::default(),
            ast: Ast { items: Vec::new() },
        };
        let bytes = cached.encode().unwrap();
        let decoded = CachedParseOutput::decode(&bytes).unwrap();
        assert!(decoded.ast.items.is_empty());
        assert!(decoded.interner.strings.is_empty());
    }
}
