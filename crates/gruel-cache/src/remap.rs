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
//! - **AST walker:** complete. Every `Expr`, `Statement`, `Pattern`,
//!   declaration-level type, and sub-type has a `RemapSpurs` impl;
//!   `remap_spurs` on a fully-deserialized AST substitutes every Spur
//!   via the remap table.
//! - **RIR walker:** stubbed. RIR's `extra` array packs heterogeneous
//!   payloads (call args, directives, match arms, …) whose Spur layout
//!   depends on which inst-data variant owns each region. A correct
//!   walker requires per-region typed access via Rir's existing
//!   `get_call_args`/`get_directives`/etc. accessors, threaded through
//!   each inst variant. The current pipeline never invokes this walker
//!   because RIR is regenerated from the cached AST rather than
//!   serialized; the stub remains as a guard against accidental use.

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
            DirectiveArg::String(s) => s.remap_spurs(table),
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
            Item::Const(c) => c.remap_spurs(table),
            Item::LinkExtern(b) => b.remap_spurs(table),
            Item::Error(_) => {}
        }
    }
}

impl RemapSpurs for gruel_parser::ast::LinkExternBlock {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.library.remap_spurs(table);
        for item in &mut self.items {
            item.remap_spurs(table);
        }
    }
}

impl RemapSpurs for gruel_parser::ast::ExternFn {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.directives.remap_spurs(table);
        self.name.remap_spurs(table);
        self.params.remap_spurs(table);
        self.return_type.remap_spurs(table);
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
        // SelfParam contains only a SelfReceiverKind (no Spurs) and a Span.
    }
}

impl RemapSpurs for SelfReceiverKind {
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
// Expr / Statement / Pattern walker.
//
// The bulk of this is mechanical: each Expr variant carries a sub-type
// whose own RemapSpurs impl recurses into its Spur-bearing fields. The
// types whose only Spur-bearing fields are an Ident (like FieldExpr or
// CallExpr) recurse via their `name`/`receiver`/etc. fields; the
// literal types (IntLit, FloatLit, BoolLit, UnitLit, NegIntLit, CharLit,
// SelfExpr, BreakExpr, ContinueExpr) carry no Spurs and are no-ops.
// =====================================================================

// Literal expressions and other zero-Spur types: explicit no-op impls
// so the macro-generated `impl_no_op!` doesn't grow huge.
impl RemapSpurs for IntLit {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for FloatLit {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for BoolLit {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for CharLit {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for UnitLit {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for NegIntLit {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for SelfExpr {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for BreakExpr {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for ContinueExpr {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for BinaryOp {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for UnaryOp {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}
impl RemapSpurs for ArgMode {
    fn remap_spurs(&mut self, _table: &[Spur]) {}
}

// StringLit carries a Spur for its interned string contents.
impl RemapSpurs for StringLit {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.value.remap_spurs(table);
    }
}

impl RemapSpurs for BinaryExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.left.remap_spurs(table);
        self.right.remap_spurs(table);
    }
}

impl RemapSpurs for UnaryExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.operand.remap_spurs(table);
    }
}

impl RemapSpurs for ParenExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.inner.remap_spurs(table);
    }
}

impl RemapSpurs for BlockExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.statements.remap_spurs(table);
        self.expr.remap_spurs(table);
    }
}

impl RemapSpurs for IfExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.cond.remap_spurs(table);
        self.then_block.remap_spurs(table);
        self.else_block.remap_spurs(table);
    }
}

impl RemapSpurs for MatchExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.scrutinee.remap_spurs(table);
        self.arms.remap_spurs(table);
    }
}

impl RemapSpurs for MatchArm {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.pattern.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for Pattern {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            Pattern::Wildcard(_) => {}
            Pattern::Ident { name, .. } => name.remap_spurs(table),
            Pattern::Int(_) | Pattern::NegInt(_) | Pattern::Bool(_) => {}
            Pattern::Path(p) => p.remap_spurs(table),
            Pattern::DataVariant {
                base,
                type_name,
                variant,
                fields,
                ..
            } => {
                base.remap_spurs(table);
                type_name.remap_spurs(table);
                variant.remap_spurs(table);
                fields.remap_spurs(table);
            }
            Pattern::StructVariant {
                base,
                type_name,
                variant,
                fields,
                ..
            } => {
                base.remap_spurs(table);
                type_name.remap_spurs(table);
                variant.remap_spurs(table);
                fields.remap_spurs(table);
            }
            Pattern::Struct {
                type_name, fields, ..
            } => {
                type_name.remap_spurs(table);
                fields.remap_spurs(table);
            }
            Pattern::Tuple { elems, .. } => elems.remap_spurs(table),
            Pattern::ComptimeUnrollArm {
                binding, iterable, ..
            } => {
                binding.remap_spurs(table);
                iterable.remap_spurs(table);
            }
        }
    }
}

impl RemapSpurs for TupleElemPattern {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            TupleElemPattern::Pattern(p) => p.remap_spurs(table),
            TupleElemPattern::Rest(_) => {}
        }
    }
}

impl RemapSpurs for FieldPattern {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.field_name.remap_spurs(table);
        self.sub.remap_spurs(table);
    }
}

impl RemapSpurs for PathPattern {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.type_name.remap_spurs(table);
        self.variant.remap_spurs(table);
    }
}

impl RemapSpurs for CallArg {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.expr.remap_spurs(table);
    }
}

impl RemapSpurs for CallExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.args.remap_spurs(table);
    }
}

impl RemapSpurs for IntrinsicArg {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            IntrinsicArg::Expr(e) => e.remap_spurs(table),
            IntrinsicArg::Type(t) => t.remap_spurs(table),
        }
    }
}

impl RemapSpurs for IntrinsicCallExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.args.remap_spurs(table);
    }
}

impl RemapSpurs for StructLitExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.name.remap_spurs(table);
        self.fields.remap_spurs(table);
    }
}

impl RemapSpurs for FieldInit {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.name.remap_spurs(table);
        self.value.remap_spurs(table);
    }
}

impl RemapSpurs for TupleExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.elems.remap_spurs(table);
    }
}

impl RemapSpurs for AnonFnExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.params.remap_spurs(table);
        self.return_type.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for TupleIndexExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
    }
}

impl RemapSpurs for FieldExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.field.remap_spurs(table);
    }
}

impl RemapSpurs for MethodCallExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.receiver.remap_spurs(table);
        self.method.remap_spurs(table);
        self.args.remap_spurs(table);
    }
}

impl RemapSpurs for ArrayLitExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.elements.remap_spurs(table);
    }
}

impl RemapSpurs for IndexExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.index.remap_spurs(table);
    }
}

impl RemapSpurs for RangeExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.lo.remap_spurs(table);
        self.hi.remap_spurs(table);
    }
}

impl RemapSpurs for PathExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.type_name.remap_spurs(table);
        self.variant.remap_spurs(table);
    }
}

impl RemapSpurs for EnumStructLitExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.type_name.remap_spurs(table);
        self.variant.remap_spurs(table);
        self.fields.remap_spurs(table);
    }
}

impl RemapSpurs for AssocFnCallExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.base.remap_spurs(table);
        self.type_name.remap_spurs(table);
        self.type_args.remap_spurs(table);
        self.function.remap_spurs(table);
        self.args.remap_spurs(table);
    }
}

impl RemapSpurs for ComptimeBlockExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.expr.remap_spurs(table);
    }
}

impl RemapSpurs for ComptimeUnrollForExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.binding.remap_spurs(table);
        self.iterable.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for CheckedBlockExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.expr.remap_spurs(table);
    }
}

impl RemapSpurs for TypeLitExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.type_expr.remap_spurs(table);
    }
}

impl RemapSpurs for WhileExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.cond.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for ForExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.binding.remap_spurs(table);
        self.iterable.remap_spurs(table);
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for LoopExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.body.remap_spurs(table);
    }
}

impl RemapSpurs for ReturnExpr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.value.remap_spurs(table);
    }
}

impl RemapSpurs for Expr {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Char(_) | Expr::Unit(_) => {}
            Expr::String(s) => s.remap_spurs(table),
            Expr::Ident(i) => i.remap_spurs(table),
            Expr::Binary(b) => b.remap_spurs(table),
            Expr::Unary(u) => u.remap_spurs(table),
            Expr::Paren(p) => p.remap_spurs(table),
            Expr::Block(b) => b.remap_spurs(table),
            Expr::If(i) => i.remap_spurs(table),
            Expr::Match(m) => m.remap_spurs(table),
            Expr::While(w) => w.remap_spurs(table),
            Expr::For(f) => f.remap_spurs(table),
            Expr::Loop(l) => l.remap_spurs(table),
            Expr::Call(c) => c.remap_spurs(table),
            Expr::Break(_) | Expr::Continue(_) => {}
            Expr::Return(r) => r.remap_spurs(table),
            Expr::StructLit(s) => s.remap_spurs(table),
            Expr::Field(f) => f.remap_spurs(table),
            Expr::MethodCall(m) => m.remap_spurs(table),
            Expr::IntrinsicCall(i) => i.remap_spurs(table),
            Expr::ArrayLit(a) => a.remap_spurs(table),
            Expr::Index(i) => i.remap_spurs(table),
            Expr::Path(p) => p.remap_spurs(table),
            Expr::EnumStructLit(e) => e.remap_spurs(table),
            Expr::AssocFnCall(a) => a.remap_spurs(table),
            Expr::SelfExpr(_) => {}
            Expr::Comptime(c) => c.remap_spurs(table),
            Expr::ComptimeUnrollFor(c) => c.remap_spurs(table),
            Expr::Checked(c) => c.remap_spurs(table),
            Expr::TypeLit(t) => t.remap_spurs(table),
            Expr::Tuple(t) => t.remap_spurs(table),
            Expr::TupleIndex(t) => t.remap_spurs(table),
            Expr::Range(r) => r.remap_spurs(table),
            Expr::AnonFn(a) => a.remap_spurs(table),
            Expr::Error(_) => {}
        }
    }
}

impl RemapSpurs for Statement {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            Statement::Let(l) => l.remap_spurs(table),
            Statement::Assign(a) => a.remap_spurs(table),
            Statement::Expr(e) => e.remap_spurs(table),
        }
    }
}

impl RemapSpurs for LetStatement {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.directives.remap_spurs(table);
        self.pattern.remap_spurs(table);
        self.ty.remap_spurs(table);
        self.init.remap_spurs(table);
    }
}

impl RemapSpurs for AssignStatement {
    fn remap_spurs(&mut self, table: &[Spur]) {
        self.target.remap_spurs(table);
        self.value.remap_spurs(table);
    }
}

impl RemapSpurs for AssignTarget {
    fn remap_spurs(&mut self, table: &[Spur]) {
        match self {
            AssignTarget::Var(i) => i.remap_spurs(table),
            AssignTarget::Field(f) => f.remap_spurs(table),
            AssignTarget::Index(i) => i.remap_spurs(table),
        }
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
             Cache pipeline currently regenerates RIR from cached AST \
             rather than serializing RIR, so this walker should not \
             be reached."
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

    #[test]
    fn nested_expr_remap_substitutes_idents_at_every_level() {
        use gruel_parser::ast::{
            BinaryExpr, BinaryOp, BlockExpr, CallArg, CallExpr, Function, IntLit, Param, ParamMode,
            Statement, TypeExpr, Visibility,
        };
        use smallvec::smallvec;

        // Hand-build a small AST to exercise the deep walker:
        //
        //   fn add(a: i32, b: i32) -> i32 {
        //       sum(a, b)
        //   }
        //
        // Spurs: 0=add, 1=a, 2=i32, 3=b, 4=sum
        let s = |i: usize| Spur::try_from_usize(i).unwrap();
        let id = |sp: Spur| Ident {
            name: sp,
            span: Span::default(),
        };

        let body_call = Expr::Call(CallExpr {
            name: id(s(4)),
            args: vec![
                CallArg {
                    mode: ArgMode::Normal,
                    expr: Expr::Ident(id(s(1))),
                    span: Span::default(),
                },
                CallArg {
                    mode: ArgMode::Normal,
                    expr: Expr::Ident(id(s(3))),
                    span: Span::default(),
                },
            ],
            span: Span::default(),
        });

        let body = Box::new(Expr::Block(BlockExpr {
            statements: vec![Statement::Expr(Expr::Binary(BinaryExpr {
                left: Box::new(Expr::Ident(id(s(1)))),
                op: BinaryOp::Add,
                right: Box::new(Expr::Int(IntLit {
                    value: 1,
                    span: Span::default(),
                })),
                span: Span::default(),
            }))],
            expr: Box::new(body_call),
            span: Span::default(),
        }));

        let mut ast = Ast {
            items: vec![Item::Function(Function {
                directives: smallvec![],
                visibility: Visibility::Public,
                is_unchecked: false,
                name: id(s(0)),
                params: vec![
                    Param {
                        is_comptime: false,
                        mode: ParamMode::Normal,
                        name: id(s(1)),
                        ty: TypeExpr::Named(id(s(2))),
                        span: Span::default(),
                    },
                    Param {
                        is_comptime: false,
                        mode: ParamMode::Normal,
                        name: id(s(3)),
                        ty: TypeExpr::Named(id(s(2))),
                        span: Span::default(),
                    },
                ],
                return_type: Some(TypeExpr::Named(id(s(2)))),
                body: Expr::Block(BlockExpr {
                    statements: Vec::new(),
                    expr: body,
                    span: Span::default(),
                }),
                span: Span::default(),
            })],
        };

        // Remap: shift every Spur by +10 (so 0→10, 1→11, 2→12, 3→13, 4→14).
        let table: Vec<Spur> = (0..5).map(|i| s(i + 10)).collect();
        ast.remap_spurs(&table);

        // Walk the resulting AST and check that every Ident.name is in
        // the new range (10..=14).
        fn collect_idents(e: &Expr, out: &mut Vec<Spur>) {
            match e {
                Expr::Ident(i) => out.push(i.name),
                Expr::Binary(b) => {
                    collect_idents(&b.left, out);
                    collect_idents(&b.right, out);
                }
                Expr::Call(c) => {
                    out.push(c.name.name);
                    for a in &c.args {
                        collect_idents(&a.expr, out);
                    }
                }
                Expr::Block(b) => {
                    for s in &b.statements {
                        if let Statement::Expr(e) = s {
                            collect_idents(e, out);
                        }
                    }
                    collect_idents(&b.expr, out);
                }
                _ => {}
            }
        }

        let Item::Function(f) = &ast.items[0] else {
            panic!()
        };
        assert_eq!(f.name.name, s(10));
        assert_eq!(f.params[0].name.name, s(11));
        assert_eq!(f.params[1].name.name, s(13));
        let mut idents = Vec::new();
        collect_idents(&f.body, &mut idents);
        // We expect to see Spurs 11, 14, 11, 13 (from the body Binary + Call)
        // — i.e. all in the remapped 10+ range.
        assert!(
            idents.iter().all(|sp| sp.into_usize() >= 10),
            "expected every Spur to be in remapped range, got {:?}",
            idents
        );
    }
}
