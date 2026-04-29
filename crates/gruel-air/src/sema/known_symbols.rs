//! Pre-interned known symbols for fast comparison.
//!
//! This module provides `KnownSymbols`, a struct that holds pre-interned `Spur`
//! values for commonly compared strings together with a lookup table that maps
//! each intrinsic's name `Spur` to its stable `IntrinsicId` from the
//! `gruel-intrinsics` registry.
//!
//! # Performance
//!
//! Each `interner.resolve()` call involves a hash table lookup. While individual
//! lookups are fast, the cumulative cost across many intrinsic dispatches can be
//! significant. Pre-interning known symbols and looking up intrinsic ids
//! directly by `Spur` avoids repeated string resolution in hot paths.

use rustc_hash::FxHashMap as HashMap;

use gruel_intrinsics::{INTRINSICS, IntrinsicId};
use lasso::{Spur, ThreadedRodeo};

/// Pre-interned symbols for known strings.
///
/// Created once during `SemaContext` construction. Holds a `Spur → IntrinsicId`
/// lookup (the primary dispatch path) plus a small number of ad-hoc symbols
/// used outside of the main intrinsic-dispatch code — intrinsics that appear
/// by name in non-dispatch sites (comptime evaluator, address-of helpers,
/// import resolution) and non-intrinsic symbols (`String`, `main`).
#[derive(Debug, Clone)]
pub struct KnownSymbols {
    /// Maps each intrinsic's interned name `Spur` to its stable `IntrinsicId`.
    /// Built from the central `gruel-intrinsics` registry at sema startup;
    /// consumers use this for id-based dispatch instead of string matching.
    pub intrinsic_ids: HashMap<Spur, IntrinsicId>,

    // ---- Intrinsic symbols referenced outside the main dispatcher ----
    /// The `dbg` intrinsic symbol — used when lowering direct `dbg` calls and
    /// by the comptime evaluator.
    pub dbg: Spur,
    /// The `panic` intrinsic symbol — used when sema synthesizes a panic call.
    pub panic: Spur,
    /// The `assert` intrinsic symbol — used when sema synthesizes an assert call.
    pub assert: Spur,
    /// The `cast` intrinsic symbol — used by the comptime evaluator.
    pub cast: Spur,
    /// The `compile_error` intrinsic symbol — used by the comptime evaluator.
    pub compile_error: Spur,
    /// The `range` intrinsic symbol — recognized by for-loop lowering.
    pub range: Spur,
    /// The `raw` intrinsic symbol — address-of helper constructs AIR calls by name.
    pub raw: Spur,
    /// The `raw_mut` intrinsic symbol — same as `raw`, for mutable addresses.
    pub raw_mut: Spur,
    /// The `import` builtin symbol — used by the module-import path.
    pub import: Spur,

    // ---- Non-intrinsic symbols ----
    /// The `String` type name symbol.
    pub string_type: Spur,
    /// The `main` function name symbol.
    pub main_fn: Spur,
    /// The `Drop` interface name (ADR-0059). Compiler-recognized.
    pub drop_iface: Spur,
    /// The `Copy` interface name (ADR-0059). Compiler-recognized.
    pub copy_iface: Spur,
}

impl KnownSymbols {
    /// Create a new `KnownSymbols` by interning all known strings.
    ///
    /// This should be called once during `SemaContext` construction.
    pub fn new(interner: &ThreadedRodeo) -> Self {
        let intrinsic_ids = INTRINSICS
            .iter()
            .map(|d| (interner.get_or_intern_static(d.name), d.id))
            .collect();
        Self {
            intrinsic_ids,
            dbg: interner.get_or_intern_static("dbg"),
            panic: interner.get_or_intern_static("panic"),
            assert: interner.get_or_intern_static("assert"),
            cast: interner.get_or_intern_static("cast"),
            compile_error: interner.get_or_intern_static("compile_error"),
            range: interner.get_or_intern_static("range"),
            raw: interner.get_or_intern_static("raw"),
            raw_mut: interner.get_or_intern_static("raw_mut"),
            import: interner.get_or_intern_static("import"),
            string_type: interner.get_or_intern_static("String"),
            main_fn: interner.get_or_intern_static("main"),
            drop_iface: interner.get_or_intern_static("Drop"),
            copy_iface: interner.get_or_intern_static("Copy"),
        }
    }

    /// Look up an intrinsic's stable id by its interned name symbol.
    pub fn intrinsic_id(&self, sym: Spur) -> Option<IntrinsicId> {
        self.intrinsic_ids.get(&sym).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_symbols_creation() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        assert_eq!(interner.resolve(&known.dbg), "dbg");
        assert_eq!(interner.resolve(&known.panic), "panic");
        assert_eq!(interner.resolve(&known.assert), "assert");
        assert_eq!(interner.resolve(&known.cast), "cast");
        assert_eq!(interner.resolve(&known.compile_error), "compile_error");
        assert_eq!(interner.resolve(&known.range), "range");
        assert_eq!(interner.resolve(&known.raw), "raw");
        assert_eq!(interner.resolve(&known.raw_mut), "raw_mut");
        assert_eq!(interner.resolve(&known.import), "import");
        assert_eq!(interner.resolve(&known.string_type), "String");
        assert_eq!(interner.resolve(&known.main_fn), "main");
    }

    #[test]
    fn known_symbols_comparison() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Interning the same string should return the same Spur.
        let dbg_sym = interner.get_or_intern("dbg");
        assert_eq!(dbg_sym, known.dbg);

        let main_sym = interner.get_or_intern("main");
        assert_eq!(main_sym, known.main_fn);
    }

    #[test]
    fn intrinsic_id_lookup() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Every registered intrinsic must resolve.
        for d in INTRINSICS {
            let sym = interner.get_or_intern(d.name);
            assert_eq!(known.intrinsic_id(sym), Some(d.id));
        }

        // Non-intrinsic names yield None.
        let non_intrinsic = interner.get_or_intern("definitely_not_an_intrinsic");
        assert_eq!(known.intrinsic_id(non_intrinsic), None);
    }
}
