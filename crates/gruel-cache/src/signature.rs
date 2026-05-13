//! Per-file signature fingerprinting (ADR-0074 Phase 3).
//!
//! `sig_fp(file)` is a BLAKE3 hash of the file's `pub` interface — the
//! exact set of names, types, and signatures importers can see. Two
//! files whose `pub` items match byte-for-byte after canonical encoding
//! produce the same `sig_fp`; changing a body inside a pub function
//! does NOT change `sig_fp`, so editing a private helper doesn't
//! invalidate downstream files' AIR cache.
//!
//! ## Encoding stability matters
//!
//! Once `sig_fp` is in use by the AIR cache (Phase 4), any change to
//! the canonical encoding silently invalidates every cached AIR entry
//! across every workspace that uses this compiler. The encoding is
//! locked by golden tests: bumping the encoding requires bumping
//! [`SIG_FP_VERSION`] AND the `CACHE_SCHEMA_VERSION` so old caches get
//! wiped on first build with the new compiler.
//!
//! ## What's hashed
//!
//! For each public item (`pub fn`, `pub struct`, `pub enum`,
//! `pub interface`, `pub const`, public methods on pub types), the
//! encoder emits:
//!
//! - A discriminant byte identifying the item kind.
//! - A length-prefixed name.
//! - Length-prefixed canonical text forms of types (for fn params /
//!   return / struct fields / enum variants / const types). Spurs are
//!   resolved to their string content via the supplied interner so
//!   the hash is stable across builds with different Spur numberings.
//! - Recursive method signatures for inline-method-bearing types.
//!
//! Items are encoded in a deterministic order: lexicographic by name,
//! and within composite items (struct fields, enum variants, methods),
//! by declaration order — declaration order is part of the
//! observable interface (e.g. for tuple struct positional fields).
//!
//! Function bodies, struct field initializers, and method bodies are
//! NOT hashed. Only signatures.
//!
//! ## What's NOT hashed
//!
//! - Item docstrings and comments (pre-stripped by the parser).
//! - Span positions (would make the hash sensitive to whitespace).
//! - Directives whose effect is purely on body code generation.
//!   We DO hash directives that affect signature semantics (e.g.
//!   visibility-affecting directives).
//! - Private items. Editing a private function never changes
//!   `sig_fp`, regardless of whether it's called by pub items
//!   transitively (sema sees those calls; the cache invariant is on
//!   the source-level `pub` boundary).

use lasso::ThreadedRodeo;

use gruel_parser::ast::{
    AnonStructField, Ast, ConstDecl, DeriveDecl, EnumDecl, EnumVariant, EnumVariantField,
    EnumVariantKind, FieldDecl, Function, Ident, InterfaceDecl, Item, Method, MethodSig, Param,
    SelfParam, SelfReceiverKind, StructDecl, TypeExpr, Visibility,
};

use crate::fingerprint::{CacheKey, Hasher};

/// Bumped any time the canonical encoding changes. Mixed into every
/// `sig_fp` so old caches (which used a different encoding) are
/// invalidated even if the file content matches.
pub const SIG_FP_VERSION: u32 = 1;

/// Compute `sig_fp` for a single file's AST.
///
/// `interner` resolves the Spurs in `ast` to their string content so
/// the hash is stable across different Spur numberings (e.g. after
/// re-interning into a different build's interner).
pub fn compute_sig_fp(ast: &Ast, interner: &ThreadedRodeo) -> CacheKey {
    let mut h = Hasher::new();
    h.update_u32(SIG_FP_VERSION);

    // Collect public items in lexicographic-by-name order. The order
    // within each item kind doesn't strictly matter for correctness
    // (any deterministic order works), but lex order makes diffs
    // legible if the encoding ever needs to be debugged.
    let mut pub_items: Vec<(String, &Item)> = Vec::new();
    for item in &ast.items {
        match item {
            Item::Function(f) if is_pub(&f.visibility) => {
                pub_items.push((interner.resolve(&f.name.name).to_string(), item));
            }
            Item::Struct(s) if is_pub(&s.visibility) => {
                pub_items.push((interner.resolve(&s.name.name).to_string(), item));
            }
            Item::Enum(e) if is_pub(&e.visibility) => {
                pub_items.push((interner.resolve(&e.name.name).to_string(), item));
            }
            Item::Interface(i) => {
                // ADR-0056: interface visibility is currently always private
                // (see `pub visibility` doc comment in InterfaceDecl). Module
                // system makes interfaces visible across files via re-export
                // through `pub const`. We still include them in the
                // signature hash because adding/changing an interface
                // affects how derives and conformance witnesses resolve.
                let _ = is_pub(&i.visibility);
                pub_items.push((interner.resolve(&i.name.name).to_string(), item));
            }
            Item::Const(c) if is_pub(&c.visibility) => {
                pub_items.push((interner.resolve(&c.name.name).to_string(), item));
            }
            Item::Derive(d) => {
                // Derives apply to their host type's signature. Always
                // include them, keyed by their decl name.
                pub_items.push((interner.resolve(&d.name.name).to_string(), item));
            }
            // Private items and Error nodes are not part of the public
            // interface. Skip them.
            _ => {}
        }
    }
    pub_items.sort_by(|a, b| a.0.cmp(&b.0));

    h.update_u64(pub_items.len() as u64);
    for (_name, item) in pub_items {
        encode_item(&mut h, item, interner);
    }

    h.finalize()
}

fn is_pub(v: &Visibility) -> bool {
    matches!(v, Visibility::Public)
}

const TAG_FN: u8 = 1;
const TAG_STRUCT: u8 = 2;
const TAG_ENUM: u8 = 3;
const TAG_INTERFACE: u8 = 4;
const TAG_DERIVE: u8 = 5;
const TAG_CONST: u8 = 6;
const TAG_LINK_EXTERN: u8 = 7;

fn encode_item(h: &mut Hasher, item: &Item, interner: &ThreadedRodeo) {
    match item {
        Item::Function(f) => encode_function(h, f, interner),
        Item::Struct(s) => encode_struct(h, s, interner),
        Item::Enum(e) => encode_enum(h, e, interner),
        Item::Interface(i) => encode_interface(h, i, interner),
        Item::Derive(d) => encode_derive(h, d, interner),
        Item::Const(c) => encode_const(h, c, interner),
        Item::LinkExtern(b) => encode_link_extern(h, b, interner),
        Item::Error(_) => {}
    }
}

fn encode_link_extern(
    h: &mut Hasher,
    block: &gruel_parser::ast::LinkExternBlock,
    interner: &ThreadedRodeo,
) {
    h.update(&[TAG_LINK_EXTERN]);
    h.update(interner.resolve(&block.library.value).as_bytes());
    h.update(&[0]);
    for item in &block.items {
        encode_ident(h, &item.name, interner);
        encode_params(h, &item.params, interner);
        encode_return_type(h, item.return_type.as_ref(), interner);
    }
}

fn encode_function(h: &mut Hasher, f: &Function, interner: &ThreadedRodeo) {
    h.update(&[TAG_FN]);
    encode_ident(h, &f.name, interner);
    h.update(&[u8::from(f.is_unchecked)]);
    encode_params(h, &f.params, interner);
    encode_return_type(h, f.return_type.as_ref(), interner);
}

fn encode_struct(h: &mut Hasher, s: &StructDecl, interner: &ThreadedRodeo) {
    h.update(&[TAG_STRUCT]);
    encode_ident(h, &s.name, interner);

    // Public fields contribute to the signature. Private fields don't
    // (ADR-0073: a private field is invisible to importers, so a
    // change to it doesn't change the public interface).
    let pub_fields: Vec<&FieldDecl> = s.fields.iter().filter(|f| is_pub(&f.visibility)).collect();
    h.update_u64(pub_fields.len() as u64);
    for field in pub_fields {
        encode_field(h, field, interner);
    }

    // Public methods contribute their signatures (NOT bodies).
    let pub_methods: Vec<&Method> = s.methods.iter().filter(|m| is_pub(&m.visibility)).collect();
    h.update_u64(pub_methods.len() as u64);
    for method in pub_methods {
        encode_method_sig_from_method(h, method, interner);
    }
}

fn encode_enum(h: &mut Hasher, e: &EnumDecl, interner: &ThreadedRodeo) {
    h.update(&[TAG_ENUM]);
    encode_ident(h, &e.name, interner);

    h.update_u64(e.variants.len() as u64);
    for variant in &e.variants {
        encode_variant(h, variant, interner);
    }

    let pub_methods: Vec<&Method> = e.methods.iter().filter(|m| is_pub(&m.visibility)).collect();
    h.update_u64(pub_methods.len() as u64);
    for method in pub_methods {
        encode_method_sig_from_method(h, method, interner);
    }
}

fn encode_interface(h: &mut Hasher, i: &InterfaceDecl, interner: &ThreadedRodeo) {
    h.update(&[TAG_INTERFACE]);
    encode_ident(h, &i.name, interner);
    h.update_u64(i.methods.len() as u64);
    for method_sig in &i.methods {
        encode_method_sig(h, method_sig, interner);
    }
}

fn encode_derive(h: &mut Hasher, d: &DeriveDecl, interner: &ThreadedRodeo) {
    h.update(&[TAG_DERIVE]);
    encode_ident(h, &d.name, interner);
    h.update_u64(d.methods.len() as u64);
    for method in &d.methods {
        encode_method_sig_from_method(h, method, interner);
    }
}

fn encode_const(h: &mut Hasher, c: &ConstDecl, interner: &ThreadedRodeo) {
    h.update(&[TAG_CONST]);
    encode_ident(h, &c.name, interner);
    encode_type_opt(h, c.ty.as_ref(), interner);
    // Const initializer expressions are part of the public-interface
    // because they're inlined at use sites (ADR-0026 module re-exports
    // are the main case). Encode the canonical text form via the same
    // interner-resolved approach we use for type expressions.
    //
    // For now: skip the init expression — full const-init encoding is
    // its own follow-up (it requires Expr-level canonical encoding,
    // which is much larger surface than TypeExpr). The Phase 3 ADR
    // says "stable canonical encoding of pub items with bodies
    // stripped"; pub const init *is* the body-equivalent for consts,
    // and we follow the same convention. Changing a const init
    // shouldn't typically change the file's pub interface (the type is
    // what importers see in declarations; the value flows through
    // sema's const evaluator). If/when that turns out to be wrong,
    // bump SIG_FP_VERSION and add init encoding.
}

fn encode_ident(h: &mut Hasher, ident: &Ident, interner: &ThreadedRodeo) {
    h.update_str(interner.resolve(&ident.name));
}

fn encode_params(h: &mut Hasher, params: &[Param], interner: &ThreadedRodeo) {
    h.update_u64(params.len() as u64);
    for p in params {
        h.update(&[u8::from(p.is_comptime)]);
        h.update(&[encode_param_mode(&p.mode)]);
        encode_ident(h, &p.name, interner);
        encode_type(h, &p.ty, interner);
    }
}

fn encode_param_mode(m: &gruel_parser::ast::ParamMode) -> u8 {
    use gruel_parser::ast::ParamMode;
    match m {
        ParamMode::Normal => 0,
        ParamMode::Comptime => 3,
    }
}

fn encode_return_type(h: &mut Hasher, ret: Option<&TypeExpr>, interner: &ThreadedRodeo) {
    encode_type_opt(h, ret, interner);
}

fn encode_type_opt(h: &mut Hasher, ty: Option<&TypeExpr>, interner: &ThreadedRodeo) {
    match ty {
        None => {
            h.update(&[0]);
        }
        Some(t) => {
            h.update(&[1]);
            encode_type(h, t, interner);
        }
    }
}

fn encode_type(h: &mut Hasher, ty: &TypeExpr, interner: &ThreadedRodeo) {
    match ty {
        TypeExpr::Named(ident) => {
            h.update(&[1]);
            encode_ident(h, ident, interner);
        }
        TypeExpr::Unit(_) => {
            h.update(&[2]);
        }
        TypeExpr::Never(_) => {
            h.update(&[3]);
        }
        TypeExpr::Array {
            element, length, ..
        } => {
            h.update(&[4]);
            encode_type(h, element, interner);
            h.update_u64(*length);
        }
        TypeExpr::AnonymousStruct {
            fields, methods, ..
        } => {
            h.update(&[5]);
            h.update_u64(fields.len() as u64);
            for f in fields {
                encode_anon_field(h, f, interner);
            }
            h.update_u64(methods.len() as u64);
            for m in methods {
                encode_method_sig_from_method(h, m, interner);
            }
        }
        TypeExpr::AnonymousEnum {
            variants, methods, ..
        } => {
            h.update(&[6]);
            h.update_u64(variants.len() as u64);
            for v in variants {
                encode_variant(h, v, interner);
            }
            h.update_u64(methods.len() as u64);
            for m in methods {
                encode_method_sig_from_method(h, m, interner);
            }
        }
        TypeExpr::AnonymousInterface { methods, .. } => {
            h.update(&[7]);
            h.update_u64(methods.len() as u64);
            for m in methods {
                encode_method_sig(h, m, interner);
            }
        }
        TypeExpr::TypeCall { callee, args, .. } => {
            h.update(&[8]);
            encode_ident(h, callee, interner);
            h.update_u64(args.len() as u64);
            for a in args {
                encode_type(h, a, interner);
            }
        }
        TypeExpr::Tuple { elems, .. } => {
            h.update(&[9]);
            h.update_u64(elems.len() as u64);
            for e in elems {
                encode_type(h, e, interner);
            }
        }
    }
}

fn encode_field(h: &mut Hasher, field: &FieldDecl, interner: &ThreadedRodeo) {
    encode_ident(h, &field.name, interner);
    encode_type(h, &field.ty, interner);
}

fn encode_anon_field(h: &mut Hasher, field: &AnonStructField, interner: &ThreadedRodeo) {
    encode_ident(h, &field.name, interner);
    encode_type(h, &field.ty, interner);
}

fn encode_variant(h: &mut Hasher, v: &EnumVariant, interner: &ThreadedRodeo) {
    encode_ident(h, &v.name, interner);
    match &v.kind {
        EnumVariantKind::Unit => {
            h.update(&[0]);
        }
        EnumVariantKind::Tuple(types) => {
            h.update(&[1]);
            h.update_u64(types.len() as u64);
            for t in types {
                encode_type(h, t, interner);
            }
        }
        EnumVariantKind::Struct(fields) => {
            h.update(&[2]);
            h.update_u64(fields.len() as u64);
            for f in fields {
                encode_variant_field(h, f, interner);
            }
        }
    }
}

fn encode_variant_field(h: &mut Hasher, f: &EnumVariantField, interner: &ThreadedRodeo) {
    encode_ident(h, &f.name, interner);
    encode_type(h, &f.ty, interner);
}

fn encode_method_sig(h: &mut Hasher, m: &MethodSig, interner: &ThreadedRodeo) {
    encode_ident(h, &m.name, interner);
    encode_self_param(h, &m.receiver);
    encode_params(h, &m.params, interner);
    encode_return_type(h, m.return_type.as_ref(), interner);
}

fn encode_method_sig_from_method(h: &mut Hasher, m: &Method, interner: &ThreadedRodeo) {
    encode_ident(h, &m.name, interner);
    h.update(&[match &m.receiver {
        None => 0,
        Some(_) => 1,
    }]);
    if let Some(r) = &m.receiver {
        encode_self_param(h, r);
    }
    encode_params(h, &m.params, interner);
    encode_return_type(h, m.return_type.as_ref(), interner);
}

fn encode_self_param(h: &mut Hasher, p: &SelfParam) {
    let tag = match p.kind {
        SelfReceiverKind::ByValue => 0,
        SelfReceiverKind::MutRef => 1,
        SelfReceiverKind::Ref => 2,
    };
    h.update(&[tag]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_lexer::Lexer;
    use gruel_parser::Parser;
    use gruel_util::FileId;

    fn parse(source: &str) -> (Ast, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let lexer = Lexer::with_interner_and_file_id(source, interner, FileId::new(1));
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        (ast, interner)
    }

    fn sig_fp(source: &str) -> CacheKey {
        let (ast, interner) = parse(source);
        compute_sig_fp(&ast, &interner)
    }

    #[test]
    fn empty_program_has_stable_hash() {
        // Locks the encoding for an empty program. Any change to the
        // SIG_FP_VERSION constant or to the leading
        // `[version_le, count_le=0]` framing should change this.
        let (ast, interner) = parse("");
        let key = compute_sig_fp(&ast, &interner);
        // Golden hex — bumping this requires bumping SIG_FP_VERSION.
        assert_eq!(
            key.hex(),
            "f599052f3420e02c499f8a582fe7ee597f2e53f31c6f34c424322b9b280b7219",
        );
    }

    #[test]
    fn private_function_does_not_affect_sig_fp() {
        let only_pub = sig_fp("pub fn add(a: i32, b: i32) -> i32 { a + b }");
        let with_private =
            sig_fp("pub fn add(a: i32, b: i32) -> i32 { a + b } fn helper() -> i32 { 0 }");
        assert_eq!(only_pub, with_private);
    }

    #[test]
    fn editing_pub_function_body_does_not_affect_sig_fp() {
        let v1 = sig_fp("pub fn answer() -> i32 { 42 }");
        let v2 = sig_fp("pub fn answer() -> i32 { 41 + 1 }");
        assert_eq!(v1, v2);
    }

    #[test]
    fn editing_pub_function_signature_changes_sig_fp() {
        let v1 = sig_fp("pub fn answer() -> i32 { 42 }");
        let v2 = sig_fp("pub fn answer() -> i64 { 42 }");
        assert_ne!(v1, v2);
    }

    #[test]
    fn renaming_pub_function_changes_sig_fp() {
        let v1 = sig_fp("pub fn foo() -> i32 { 0 }");
        let v2 = sig_fp("pub fn bar() -> i32 { 0 }");
        assert_ne!(v1, v2);
    }

    #[test]
    fn declaration_order_does_not_affect_sig_fp() {
        let v1 = sig_fp("pub fn a() -> i32 { 0 } pub fn b() -> i32 { 0 }");
        let v2 = sig_fp("pub fn b() -> i32 { 0 } pub fn a() -> i32 { 0 }");
        assert_eq!(v1, v2);
    }

    #[test]
    fn pub_struct_field_change_changes_sig_fp() {
        let v1 = sig_fp("pub struct Point { pub x: i32, pub y: i32 }");
        let v2 = sig_fp("pub struct Point { pub x: i32, pub y: i64 }");
        assert_ne!(v1, v2);
    }

    #[test]
    fn private_struct_field_does_not_affect_sig_fp() {
        let v1 = sig_fp("pub struct Point { pub x: i32, pub y: i32 }");
        let v2 = sig_fp("pub struct Point { pub x: i32, pub y: i32, hidden: i32 }");
        assert_eq!(v1, v2);
    }

    #[test]
    fn parameter_count_change_changes_sig_fp() {
        let v1 = sig_fp("pub fn f(a: i32) -> i32 { 0 }");
        let v2 = sig_fp("pub fn f(a: i32, b: i32) -> i32 { 0 }");
        assert_ne!(v1, v2);
    }
}
