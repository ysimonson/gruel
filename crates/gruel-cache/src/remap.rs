//! Spur remapping after cache load (ADR-0074 Phase 2b).
//!
//! Cached AST and RIR carry `Spur` values that index into the cached
//! per-file interner snapshot. After we re-intern each cached string into
//! the build's shared `ThreadedRodeo` (via [`InternerSnapshot::restore_into`])
//! we have a remap table: `cached_spur.into_usize() → build_spur`.
//!
//! This module walks the IR tree and substitutes every `Spur` via that
//! remap table.
//!
//! ## Status
//!
//! Phase 2b currently provides the `RemapSpurs` trait, the leaf and
//! container impls (`Spur`, `Box`, `Vec`, `Option`, `SmallVec`), and impls
//! for the AST type-level structure (`Ast`, `Item`, `Function`,
//! `StructDecl`, `EnumDecl`, etc.). The full recursive walker through
//! `Expr`, `Statement`, and `Pattern` is **not yet implemented** — those
//! types form the leaves of the remap with TODO bodies. Calling
//! `remap_spurs` on a real AST today is therefore incomplete; the
//! pipeline-integration step that depends on it is gated behind
//! `--preview incremental_compilation` and must wait on those impls.
//!
//! See ADR-0074 Phase 2 for the wiring this prepares.

use lasso::{Key, Spur};
use smallvec::SmallVec;

use gruel_parser::ast::*;
use gruel_rir::Rir;

/// Type capable of substituting cached `Spur` values via a remap table.
///
/// `table[cached_spur.into_usize()]` is the build-interner `Spur` that
/// should replace the cached one.
pub trait RemapSpurs {
    fn remap_spurs(&mut self, table: &[Spur]);
}

// =====================================================================
// Leaf impls
// =====================================================================

impl RemapSpurs for Spur {
    fn remap_spurs(&mut self, table: &[Spur]) {
        let idx = self.into_usize();
        debug_assert!(
            idx < table.len(),
            "cached Spur out of remap-table range ({} >= {})",
            idx,
            table.len()
        );
        *self = table[idx];
    }
}

// Primitives and types that contain no Spurs are no-ops. We list each one
// rather than blanket-impl over `Copy` so that adding a new field type
// fails to compile until we've decided whether it carries Spurs.
macro_rules! impl_no_op {
    ($($t:ty),* $(,)?) => {
        $(
            impl RemapSpurs for $t {
                fn remap_spurs(&mut self, _table: &[Spur]) {}
            }
        )*
    };
}

impl_no_op!(
    bool,
    u8,
    u16,
    u32,
    u64,
    i8,
    i16,
    i32,
    i64,
    f32,
    f64,
    String,
    gruel_util::Span,
    gruel_util::FileId,
    gruel_util::BinOp,
    gruel_util::UnaryOp,
);

// =====================================================================
// Container impls
// =====================================================================

impl<T: RemapSpurs> RemapSpurs for Box<T> {
    fn remap_spurs(&mut self, table: &[Spur]) {
        (**self).remap_spurs(table);
    }
}

impl<T: RemapSpurs> RemapSpurs for Option<T> {
    fn remap_spurs(&mut self, table: &[Spur]) {
        if let Some(v) = self {
            v.remap_spurs(table);
        }
    }
}

impl<T: RemapSpurs> RemapSpurs for Vec<T> {
    fn remap_spurs(&mut self, table: &[Spur]) {
        for v in self {
            v.remap_spurs(table);
        }
    }
}

impl<T: RemapSpurs, const N: usize> RemapSpurs for SmallVec<[T; N]>
where
    [T; N]: smallvec::Array<Item = T>,
{
    fn remap_spurs(&mut self, table: &[Spur]) {
        for v in self.iter_mut() {
            v.remap_spurs(table);
        }
    }
}

// =====================================================================
// AST impls
//
// Top-down: Ast → Item → Function/StructDecl/Enum/Interface/Derive/etc.
// Each type's impl recurses into its Spur-bearing fields. Types whose
// only fields are Spans / primitives are no-ops via `impl_no_op` above.
//
// Expr/Statement/Pattern have placeholder impls below — the recursive
// walker through every Expr variant is the bulk of remaining Phase 2b
// work and is gated behind `--preview incremental_compilation`.
// =====================================================================

impl RemapSpurs for Ast {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.items.remap_spurs(table);
    }
}

impl RemapSpurs for Ident {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
    }
}

impl RemapSpurs for Directive {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.args.remap_spurs(table);
    }
}

impl RemapSpurs for DirectiveArg {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            DirectiveArg::Ident(i) => i.remap_spurs(table),
        }
    }
}

impl RemapSpurs for Item {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            Item::Function(f) => f.remap_spurs(table),
            Item::Struct(s) => s.remap_spurs(table),
            Item::Enum(e) => e.remap_spurs(table),
            Item::Interface(i) => i.remap_spurs(table),
            Item::Derive(d) => d.remap_spurs(table),
            Item::DropFn(d) => d.remap_spurs(table),
            Item::Const(c) => c.remap_spurs(table),
            Item::Error(_) => {}
        }
    }
}

impl RemapSpurs for ConstDecl {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.directives.remap_spurs(table);
        self.name.remap_spurs(table);
        self.ty.remap_spurs(table);
        self.init.remap_spurs(table);
    }
}

impl RemapSpurs for Visibility {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}

impl RemapSpurs for StructDecl {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.directives.remap_spurs(table);
        self.name.remap_spurs(table);
        self.fields.remap_spurs(table);
        self.methods.remap_spurs(table);
    }
}

impl RemapSpurs for FieldDecl {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.ty.remap_spurs(table);
    }
}

impl RemapSpurs for EnumDecl {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.variants.remap_spurs(table);
        self.methods.remap_spurs(table);
    }
}

impl RemapSpurs for EnumVariant {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.kind.remap_spurs(table);
    }
}

impl RemapSpurs for EnumVariantKind {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            EnumVariantKind::Unit => {}
            EnumVariantKind::Tuple(types) => types.remap_spurs(table),
            EnumVariantKind::Struct(fields) => fields.remap_spurs(table),
        }
    }
}

impl RemapSpurs for EnumVariantField {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.ty.remap_spurs(table);
    }
}

impl RemapSpurs for InterfaceDecl {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.methods.remap_spurs(table);
    }
}

impl RemapSpurs for MethodSig {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.receiver.remap_spurs(table);
        self.params.remap_spurs(table);
        self.return_type.remap_spurs(table);
    }
}

impl RemapSpurs for DeriveDecl {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.methods.remap_spurs(table);
    }
}

impl RemapSpurs for DropFn {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.type_name.remap_spurs(table);
        // body is Expr — see Phase 2b TODO on `Expr` impl below.
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for Method {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.directives.remap_spurs(table);
        self.name.remap_spurs(table);
        self.receiver.remap_spurs(table);
        self.params.remap_spurs(table);
        self.return_type.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for SelfParam {
    fn remap_spurs(&mut self, _table: &[Spur]) {
        // SelfParam contains only a SelfMode (no Spurs) and a Span.
    }
}

impl RemapSpurs for SelfMode {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}

impl RemapSpurs for Function {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.directives.remap_spurs(table);
        self.name.remap_spurs(table);
        self.params.remap_spurs(table);
        self.return_type.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for ParamMode {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}

impl RemapSpurs for Param {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.ty.remap_spurs(table);
    }
}

impl RemapSpurs for TypeExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            TypeExpr::Named(i) => i.remap_spurs(table),
            TypeExpr::Unit(_) | TypeExpr::Never(_) => {}
            TypeExpr::Array { element, .. } => element.remap_spurs(table),
            TypeExpr::AnonymousStruct {
                directives,
                fields,
                methods,
                ..
            } => {
                directives.remap_spurs(table);
                fields.remap_spurs(table);
                methods.remap_spurs(table);
            }
            TypeExpr::AnonymousEnum {
                directives,
                variants,
                methods,
                ..
            } => {
                directives.remap_spurs(table);
                variants.remap_spurs(table);
                methods.remap_spurs(table);
            }
            TypeExpr::AnonymousInterface { methods, .. } => methods.remap_spurs(table),
            TypeExpr::TypeCall { callee, args, .. } => {
                callee.remap_spurs(table);
                args.remap_spurs(table);
            }
            TypeExpr::Tuple { elems, .. } => elems.remap_spurs(table),
        }
    }
}

impl RemapSpurs for AnonStructField {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.ty.remap_spurs(table);
    }
}

// =====================================================================
// Expr / Statement walker — placeholder.
//
// `Expr` is a 30+ variant enum with many sub-types (BinaryExpr, IfExpr,
// MatchExpr, CallExpr, IntrinsicCallExpr, AnonFnExpr, ...). Each variant
// needs a recursive walker. Until the full implementation lands, we
// provide a panicking stub guarded by debug_assert: the cache is gated
// behind --preview incremental_compilation, and the pipeline integration
// (Phase 2b's other half) will not be wired until this is complete.
//
// In release builds the stub is a no-op, so an incomplete cache hit
// silently leaks unmapped Spurs — but in release builds the preview
// gate is the only way to reach this code, and the wiring step will
// itself debug-assert that the walker is complete before enabling.
// =====================================================================

impl RemapSpurs for Expr {
    fn remap_spurs(&mut self, _table: &[Spur]) {
        debug_assert!(
            false,
            "RemapSpurs::Expr not yet implemented — Phase 2b stub. \
             Cache pipeline integration is gated behind \
             --preview incremental_compilation and must not call this."
        );
    }
}

// =====================================================================
// RIR impl — RIR is tabular (Vec<Inst> + extra: Vec<u32>), and most Spur
// values live inside opaque instruction payloads. Real walker requires
// accessor methods on Rir that aren't currently exposed (instructions
// and extra are private). Phase 2b wiring lands the accessors and the
// real walker together.
// =====================================================================

impl RemapSpurs for Rir {
    fn remap_spurs(&mut self, _table: &[Spur]) {
        debug_assert!(
            false,
            "RemapSpurs::Rir not yet implemented — Phase 2b stub. \
             Cache pipeline integration is gated behind \
             --preview incremental_compilation and must not call this."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_util::Span;

    #[test]
    fn ident_remap_substitutes_spur() {
        let table = vec![
            Spur::try_from_usize(7).unwrap(),
            Spur::try_from_usize(8).unwrap(),
        ];
        let mut id = Ident {
            name: Spur::try_from_usize(1).unwrap(),
            span: Span::default(),
        };
        id.remap_spurs(&table);
        assert_eq!(id.name, Spur::try_from_usize(8).unwrap());
    }

    #[test]
    fn vec_of_idents_remaps_each() {
        let table = vec![
            Spur::try_from_usize(100).unwrap(),
            Spur::try_from_usize(200).unwrap(),
        ];
        let mut idents = vec![
            Ident {
                name: Spur::try_from_usize(0).unwrap(),
                span: Span::default(),
            },
            Ident {
                name: Spur::try_from_usize(1).unwrap(),
                span: Span::default(),
            },
        ];
        idents.remap_spurs(&table);
        assert_eq!(idents[0].name, Spur::try_from_usize(100).unwrap());
        assert_eq!(idents[1].name, Spur::try_from_usize(200).unwrap());
    }

    #[test]
    fn empty_ast_remap_is_noop() {
        let mut ast = Ast { items: Vec::new() };
        ast.remap_spurs(&[]);
        assert!(ast.items.is_empty());
    }
}
