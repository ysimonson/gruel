//! Wire format for cached AIR (ADR-0074 Phase 4).
//!
//! See `wire.rs` for the parse/RIR cache wire types. This module covers
//! the typed-IR side: a [`CachedAirOutput`] envelope holding everything
//! a downstream consumer needs to skip running sema on a file:
//!
//! - The per-file `InternerSnapshot` (same as parse/RIR) so cached
//!   `Spur`s can be remapped into the build's shared interner.
//! - Each file's [`AnalyzedFunction`]s with their typed `Air`.
//! - The serializable parts of `TypeInternPool` (the canonical
//!   `Vec<TypeData>`; structural-dedup maps reconstruct on load).
//! - The string and byte literal tables (`strings`, `bytes`) that
//!   AIR's `*Const` instructions index into.
//! - The interface definitions and vtable witnesses produced by sema.
//! - The comptime `@dbg` output buffer collected during sema, so cache
//!   hits can replay it to stderr and remain observably identical to
//!   cache misses (ADR-0074 "Comptime side-effects replay" subsection).
//!
//! ## What this does NOT cache
//!
//! Compile *warnings* (`Vec<CompileWarning>`) are not yet serialized
//! — `DiagnosticWrapper<WarningKind>` carries `Box<Diagnostic>` with
//! complex label/note/help machinery whose serialization is its own
//! focused implementation pass. On AIR cache hit, the build won't
//! surface the cached warnings until that's done. This is a known
//! regression that the integration code documents and the next
//! follow-up will address.
//!
//! ## TypeId remapping
//!
//! The cached `TypeInternPool` is loaded into a fresh pool (replace,
//! not merge). Cross-file caching needs a `TypeId` remap walker that
//! visits every `Type`/`InternedType`/`StructId`/`EnumId`/`InterfaceId`
//! field in the AIR and remaps it from cached numbering to the build's.
//! That walker is the one piece of Phase 4 still missing; until it
//! lands, AIR caching is single-file-only (sufficient for an end-to-end
//! demo but not for a multi-file build).

use serde::{Deserialize, Serialize};

use gruel_air::{AnalyzedFunction, InterfaceDef, InterfaceVtables, TypeInternPool};

use crate::wire::InternerSnapshot;

/// Envelope around a per-file AIR + interner snapshot, ready for
/// bincode serialization to the AIR cache.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedAirOutput {
    /// Per-file interner snapshot (same role as in `CachedParseOutput`).
    pub interner: InternerSnapshot,
    /// All analyzed functions in this file with their typed AIR.
    pub functions: Vec<AnalyzedFunction>,
    /// Type intern pool snapshot. See `TypeInternPool`'s custom serde
    /// impl: only the canonical `types: Vec<TypeData>` is captured;
    /// the structural-dedup HashMaps reconstruct on load.
    pub type_pool: TypeInternPool,
    /// String literals, indexed by `AirInstData::StringConst` index.
    pub strings: Vec<String>,
    /// Byte-blob literals (`@embed_file`), indexed by `BytesConst` index.
    pub bytes: Vec<Vec<u8>>,
    /// Interface definitions, indexed by `InterfaceId.0`.
    pub interface_defs: Vec<InterfaceDef>,
    /// Vtable witnesses keyed by `(StructId, InterfaceId)`.
    pub interface_vtables: InterfaceVtables,
    /// Lines of `@dbg` output collected during comptime evaluation.
    /// Replayed verbatim to stderr on cache hit so the build is
    /// observably identical to a cold build.
    pub comptime_dbg_output: Vec<String>,
}

impl CachedAirOutput {
    /// Serialize to the bincode wire format used by `CacheStore::put`.
    pub fn encode(&self) -> Result<Vec<u8>, bincode::error::EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::standard())
    }

    /// Deserialize from the bincode wire format.
    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        let (out, _read) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap;

    #[test]
    fn empty_air_output_round_trips() {
        let cached = CachedAirOutput {
            interner: InternerSnapshot::default(),
            functions: Vec::new(),
            type_pool: TypeInternPool::new(),
            strings: Vec::new(),
            bytes: Vec::new(),
            interface_defs: Vec::new(),
            interface_vtables: FxHashMap::default(),
            comptime_dbg_output: Vec::new(),
        };
        let bytes = cached.encode().expect("encode");
        let decoded = CachedAirOutput::decode(&bytes).expect("decode");
        assert!(decoded.functions.is_empty());
        assert!(decoded.strings.is_empty());
        assert!(decoded.comptime_dbg_output.is_empty());
    }
}
