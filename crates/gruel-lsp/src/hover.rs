//! Hover (ADR-0091 Phase 3).
//!
//! Given an LSP `Position`, find the smallest AST item whose span contains
//! the position and return a markdown hover with the item's signature
//! followed by its `///` docstring (rendered through `gruel_doc`).
//!
//! Phase 3 covers top-level items (`fn`, `struct`, `enum`, `interface`,
//! `derive`, `const`, fields, variants, methods, parameters, type
//! references). Hover for arbitrary expressions / locals lands in Phase 4
//! when sema's expr-type side-table is wired up.

use gruel_compiler::{Type, TypeInternPool};
use gruel_parser::ast::{
    Ast, ConstDecl, DeriveDecl, EnumDecl, EnumVariant, EnumVariantKind, FieldDecl, Function, Ident,
    InterfaceDecl, Item, LinkExternBlock, Method, MethodSig, Param, StructDecl, TypeExpr,
};
use gruel_util::Span;
use lasso::ThreadedRodeo;
use rustc_hash::FxHashMap;

/// Hover content rendered to Markdown (plus the AST node's span for the
/// returned LSP `Hover.range` field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverContent {
    pub markdown: String,
    pub span: Span,
}

/// Find an item under the cursor and produce hover content for it.
///
/// `file_id` constrains the search to one file: items from other files
/// (via the merged AST) are skipped because their spans live in another
/// `FileId` namespace.
pub fn hover_at(
    ast: &Ast,
    interner: &ThreadedRodeo,
    file_id: gruel_util::FileId,
    byte: u32,
) -> Option<HoverContent> {
    let target = SmallestSpanFinder::new(file_id, byte).find(ast)?;
    target.render(interner)
}

/// Like [`hover_at`], but also consults the AIR expression-type side
/// table. If the cursor is inside an expression whose type sema computed,
/// we return the type as fallback hover content (Phase 4).
pub fn hover_at_with_expr_types(
    ast: &Ast,
    interner: &ThreadedRodeo,
    expr_types: &FxHashMap<Span, Type>,
    type_pool: Option<&TypeInternPool>,
    file_id: gruel_util::FileId,
    byte: u32,
) -> Option<HoverContent> {
    if let Some(content) = hover_at(ast, interner, file_id, byte) {
        return Some(content);
    }
    // Fall back to the smallest span in `expr_types` covering `byte`.
    let mut best: Option<(Span, Type)> = None;
    for (span, ty) in expr_types {
        if span.file_id != file_id {
            continue;
        }
        if byte < span.start || byte >= span.end {
            continue;
        }
        if best.map_or(true, |(b, _)| span.end - span.start < b.end - b.start) {
            best = Some((*span, *ty));
        }
    }
    let (span, ty) = best?;
    let display = type_pool
        .map(|p| p.format_type_name(ty))
        .unwrap_or_else(|| format!("{:?}", ty));
    Some(HoverContent {
        markdown: format!("```gruel\n{}\n```", display),
        span,
    })
}

/// Visitor that finds the smallest AST node whose span contains a target
/// byte. Walks declaratively over a single AST; returns the most specific
/// match.
struct SmallestSpanFinder {
    file_id: gruel_util::FileId,
    byte: u32,
    best: Option<HoverTarget>,
    best_size: u32,
}

/// The kind of AST node the finder landed on. Hover rendering reads this.
#[derive(Debug, Clone)]
enum HoverTarget {
    Function(Function),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(InterfaceDecl),
    Derive(DeriveDecl),
    Const(ConstDecl),
    LinkExtern(LinkExternBlock),
    Field(FieldDecl),
    EnumVariant(EnumVariant),
    Method(Method),
    MethodSig(MethodSig),
    Param(Param),
    /// A type reference (e.g. `i32` in a parameter list). Rendered as
    /// `: T` where T is the type display string.
    TypeRef(TypeExpr),
    /// Identifier reference (Phase 4 will resolve to a definition; Phase 3
    /// just emits the bare identifier name).
    Identifier(Ident),
}

impl SmallestSpanFinder {
    fn new(file_id: gruel_util::FileId, byte: u32) -> Self {
        Self {
            file_id,
            byte,
            best: None,
            best_size: u32::MAX,
        }
    }

    fn find(mut self, ast: &Ast) -> Option<HoverTarget> {
        for item in &ast.items {
            self.visit_item(item);
        }
        self.best
    }

    fn span_matches(&self, span: Span) -> bool {
        span.file_id == self.file_id && self.byte >= span.start && self.byte < span.end
    }

    fn consider(&mut self, span: Span, target: HoverTarget) {
        if !self.span_matches(span) {
            return;
        }
        let size = span.end.saturating_sub(span.start);
        if size <= self.best_size {
            self.best_size = size;
            self.best = Some(target);
        }
    }

    fn visit_item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => {
                self.consider(f.span, HoverTarget::Function(f.clone()));
                // Re-record the function on its name's span specifically so
                // hovering the name itself wins over (and is shorter than)
                // the larger body span — but renders the same content.
                self.consider(f.name.span, HoverTarget::Function(f.clone()));
                for p in &f.params {
                    self.visit_param(p);
                }
                if let Some(rt) = &f.return_type {
                    self.visit_type_expr(rt);
                }
            }
            Item::Struct(s) => {
                self.consider(s.span, HoverTarget::Struct(s.clone()));
                self.consider(s.name.span, HoverTarget::Struct(s.clone()));
                for field in &s.fields {
                    self.visit_field(field);
                }
                for m in &s.methods {
                    self.visit_method(m);
                }
            }
            Item::Enum(e) => {
                self.consider(e.span, HoverTarget::Enum(e.clone()));
                self.consider(e.name.span, HoverTarget::Enum(e.clone()));
                for v in &e.variants {
                    self.visit_variant(v);
                }
                for m in &e.methods {
                    self.visit_method(m);
                }
            }
            Item::Interface(i) => {
                self.consider(i.span, HoverTarget::Interface(i.clone()));
                self.consider(i.name.span, HoverTarget::Interface(i.clone()));
                for sig in &i.methods {
                    self.visit_method_sig(sig);
                }
            }
            Item::Derive(d) => {
                self.consider(d.span, HoverTarget::Derive(d.clone()));
                self.consider(d.name.span, HoverTarget::Derive(d.clone()));
                for m in &d.methods {
                    self.visit_method(m);
                }
            }
            Item::Const(c) => {
                self.consider(c.span, HoverTarget::Const(c.clone()));
                self.consider(c.name.span, HoverTarget::Const(c.clone()));
                if let Some(ty) = &c.ty {
                    self.visit_type_expr(ty);
                }
            }
            Item::LinkExtern(b) => {
                self.consider(b.span, HoverTarget::LinkExtern(b.clone()));
                for ext in &b.items {
                    for p in &ext.params {
                        self.visit_param(p);
                    }
                    if let Some(rt) = &ext.return_type {
                        self.visit_type_expr(rt);
                    }
                }
            }
            Item::Error(_) => {}
        }
    }

    fn visit_field(&mut self, field: &FieldDecl) {
        self.consider(field.span, HoverTarget::Field(field.clone()));
        self.consider(field.name.span, HoverTarget::Field(field.clone()));
        self.visit_type_expr(&field.ty);
    }

    fn visit_variant(&mut self, v: &EnumVariant) {
        self.consider(v.span, HoverTarget::EnumVariant(v.clone()));
        self.consider(v.name.span, HoverTarget::EnumVariant(v.clone()));
        match &v.kind {
            EnumVariantKind::Unit => {}
            EnumVariantKind::Tuple(tys) => {
                for ty in tys {
                    self.visit_type_expr(ty);
                }
            }
            EnumVariantKind::Struct(fields) => {
                for f in fields {
                    self.visit_type_expr(&f.ty);
                }
            }
        }
    }

    fn visit_method(&mut self, m: &Method) {
        self.consider(m.span, HoverTarget::Method(m.clone()));
        self.consider(m.name.span, HoverTarget::Method(m.clone()));
        for p in &m.params {
            self.visit_param(p);
        }
        if let Some(rt) = &m.return_type {
            self.visit_type_expr(rt);
        }
    }

    fn visit_method_sig(&mut self, m: &MethodSig) {
        self.consider(m.span, HoverTarget::MethodSig(m.clone()));
        self.consider(m.name.span, HoverTarget::MethodSig(m.clone()));
        for p in &m.params {
            self.visit_param(p);
        }
        if let Some(rt) = &m.return_type {
            self.visit_type_expr(rt);
        }
    }

    fn visit_param(&mut self, p: &Param) {
        self.consider(p.span, HoverTarget::Param(p.clone()));
        self.consider(p.name.span, HoverTarget::Param(p.clone()));
        self.visit_type_expr(&p.ty);
    }

    fn visit_type_expr(&mut self, ty: &TypeExpr) {
        let span = ty.span();
        self.consider(span, HoverTarget::TypeRef(ty.clone()));
        if let TypeExpr::Named(ident) = ty {
            self.consider(ident.span, HoverTarget::Identifier(*ident));
        }
        // Recurse into compound types.
        match ty {
            TypeExpr::Array { element, .. } => self.visit_type_expr(element),
            TypeExpr::Tuple { elems, .. } => {
                for e in elems {
                    self.visit_type_expr(e);
                }
            }
            TypeExpr::TypeCall { args, .. } => {
                for a in args {
                    self.visit_type_expr(a);
                }
            }
            _ => {}
        }
    }
}

impl HoverTarget {
    fn render(&self, interner: &ThreadedRodeo) -> Option<HoverContent> {
        match self {
            HoverTarget::Function(f) => Some(render_function(f, interner)),
            HoverTarget::Struct(s) => Some(render_struct(s, interner)),
            HoverTarget::Enum(e) => Some(render_enum(e, interner)),
            HoverTarget::Interface(i) => Some(render_interface(i, interner)),
            HoverTarget::Derive(d) => Some(render_derive(d, interner)),
            HoverTarget::Const(c) => Some(render_const(c, interner)),
            HoverTarget::LinkExtern(b) => Some(render_link_extern(b, interner)),
            HoverTarget::Field(f) => Some(render_field(f, interner)),
            HoverTarget::EnumVariant(v) => Some(render_variant(v, interner)),
            HoverTarget::Method(m) => Some(render_method(m, interner)),
            HoverTarget::MethodSig(m) => Some(render_method_sig(m, interner)),
            HoverTarget::Param(p) => Some(render_param(p, interner)),
            HoverTarget::TypeRef(ty) => Some(render_type_ref(ty, interner)),
            HoverTarget::Identifier(i) => Some(HoverContent {
                markdown: format!("`{}`", interner.resolve(&i.name)),
                span: i.span,
            }),
        }
    }
}

fn render_function(f: &Function, interner: &ThreadedRodeo) -> HoverContent {
    let mut sig = String::new();
    sig.push_str("fn ");
    sig.push_str(interner.resolve(&f.name.name));
    sig.push('(');
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            sig.push_str(", ");
        }
        sig.push_str(interner.resolve(&p.name.name));
        sig.push_str(": ");
        sig.push_str(&type_expr_display(&p.ty, interner));
    }
    sig.push(')');
    if let Some(rt) = &f.return_type {
        sig.push_str(" -> ");
        sig.push_str(&type_expr_display(rt, interner));
    }
    HoverContent {
        markdown: markdown_with_doc(&sig, f.doc.as_ref()),
        span: f.span,
    }
}

fn render_struct(s: &StructDecl, interner: &ThreadedRodeo) -> HoverContent {
    let sig = format!("struct {}", interner.resolve(&s.name.name));
    HoverContent {
        markdown: markdown_with_doc(&sig, s.doc.as_ref()),
        span: s.span,
    }
}

fn render_enum(e: &EnumDecl, interner: &ThreadedRodeo) -> HoverContent {
    let sig = format!("enum {}", interner.resolve(&e.name.name));
    HoverContent {
        markdown: markdown_with_doc(&sig, e.doc.as_ref()),
        span: e.span,
    }
}

fn render_interface(i: &InterfaceDecl, interner: &ThreadedRodeo) -> HoverContent {
    let sig = format!("interface {}", interner.resolve(&i.name.name));
    HoverContent {
        markdown: markdown_with_doc(&sig, i.doc.as_ref()),
        span: i.span,
    }
}

fn render_derive(d: &DeriveDecl, interner: &ThreadedRodeo) -> HoverContent {
    let sig = format!("derive {}", interner.resolve(&d.name.name));
    HoverContent {
        markdown: markdown_with_doc(&sig, d.doc.as_ref()),
        span: d.span,
    }
}

fn render_const(c: &ConstDecl, interner: &ThreadedRodeo) -> HoverContent {
    let mut sig = format!("const {}", interner.resolve(&c.name.name));
    if let Some(ty) = &c.ty {
        sig.push_str(": ");
        sig.push_str(&type_expr_display(ty, interner));
    }
    HoverContent {
        markdown: markdown_with_doc(&sig, c.doc.as_ref()),
        span: c.span,
    }
}

fn render_link_extern(b: &LinkExternBlock, interner: &ThreadedRodeo) -> HoverContent {
    let library = interner.resolve(&b.library.value);
    let sig = format!("link_extern(\"{}\")", library);
    HoverContent {
        markdown: markdown_with_doc(&sig, b.doc.as_ref()),
        span: b.span,
    }
}

fn render_field(f: &FieldDecl, interner: &ThreadedRodeo) -> HoverContent {
    let sig = format!(
        "{}: {}",
        interner.resolve(&f.name.name),
        type_expr_display(&f.ty, interner)
    );
    HoverContent {
        markdown: markdown_with_doc(&sig, f.doc.as_ref()),
        span: f.span,
    }
}

fn render_variant(v: &EnumVariant, interner: &ThreadedRodeo) -> HoverContent {
    let mut sig = interner.resolve(&v.name.name).to_string();
    match &v.kind {
        EnumVariantKind::Unit => {}
        EnumVariantKind::Tuple(tys) => {
            sig.push('(');
            for (i, ty) in tys.iter().enumerate() {
                if i > 0 {
                    sig.push_str(", ");
                }
                sig.push_str(&type_expr_display(ty, interner));
            }
            sig.push(')');
        }
        EnumVariantKind::Struct(fields) => {
            sig.push_str(" { ");
            for (i, f) in fields.iter().enumerate() {
                if i > 0 {
                    sig.push_str(", ");
                }
                sig.push_str(interner.resolve(&f.name.name));
                sig.push_str(": ");
                sig.push_str(&type_expr_display(&f.ty, interner));
            }
            sig.push_str(" }");
        }
    }
    HoverContent {
        markdown: markdown_with_doc(&sig, v.doc.as_ref()),
        span: v.span,
    }
}

fn render_method(m: &Method, interner: &ThreadedRodeo) -> HoverContent {
    let mut sig = format!("fn {}(", interner.resolve(&m.name.name));
    if let Some(_recv) = &m.receiver {
        sig.push_str("self");
        if !m.params.is_empty() {
            sig.push_str(", ");
        }
    }
    for (i, p) in m.params.iter().enumerate() {
        if i > 0 {
            sig.push_str(", ");
        }
        sig.push_str(interner.resolve(&p.name.name));
        sig.push_str(": ");
        sig.push_str(&type_expr_display(&p.ty, interner));
    }
    sig.push(')');
    if let Some(rt) = &m.return_type {
        sig.push_str(" -> ");
        sig.push_str(&type_expr_display(rt, interner));
    }
    HoverContent {
        markdown: markdown_with_doc(&sig, m.doc.as_ref()),
        span: m.span,
    }
}

fn render_method_sig(m: &MethodSig, interner: &ThreadedRodeo) -> HoverContent {
    let mut sig = format!("fn {}(self", interner.resolve(&m.name.name));
    for p in &m.params {
        sig.push_str(", ");
        sig.push_str(interner.resolve(&p.name.name));
        sig.push_str(": ");
        sig.push_str(&type_expr_display(&p.ty, interner));
    }
    sig.push(')');
    if let Some(rt) = &m.return_type {
        sig.push_str(" -> ");
        sig.push_str(&type_expr_display(rt, interner));
    }
    sig.push(';');
    HoverContent {
        markdown: markdown_with_doc(&sig, m.doc.as_ref()),
        span: m.span,
    }
}

fn render_param(p: &Param, interner: &ThreadedRodeo) -> HoverContent {
    let sig = format!(
        "{}: {}",
        interner.resolve(&p.name.name),
        type_expr_display(&p.ty, interner)
    );
    HoverContent {
        markdown: markdown_with_doc(&sig, None),
        span: p.span,
    }
}

fn render_type_ref(ty: &TypeExpr, interner: &ThreadedRodeo) -> HoverContent {
    HoverContent {
        markdown: format!("```gruel\n{}\n```", type_expr_display(ty, interner)),
        span: ty.span(),
    }
}

fn markdown_with_doc(sig: &str, doc: Option<&gruel_parser::ast::Doc>) -> String {
    let mut out = String::from("```gruel\n");
    out.push_str(sig);
    out.push_str("\n```");
    if let Some(d) = doc {
        out.push_str("\n\n");
        out.push_str(d.body.trim());
    }
    out
}

/// Pretty-print a `TypeExpr` resolving named idents through the interner.
fn type_expr_display(ty: &TypeExpr, interner: &ThreadedRodeo) -> String {
    match ty {
        TypeExpr::Named(ident) => interner.resolve(&ident.name).to_string(),
        TypeExpr::Unit(_) => "()".to_string(),
        TypeExpr::Never(_) => "!".to_string(),
        TypeExpr::Array {
            element, length, ..
        } => {
            format!("[{}; {}]", type_expr_display(element, interner), length)
        }
        TypeExpr::AnonymousStruct { .. } => "struct { … }".to_string(),
        TypeExpr::AnonymousEnum { .. } => "enum { … }".to_string(),
        TypeExpr::AnonymousInterface { .. } => "interface { … }".to_string(),
        TypeExpr::TypeCall { callee, args, .. } => {
            let mut s = interner.resolve(&callee.name).to_string();
            s.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&type_expr_display(a, interner));
            }
            s.push(')');
            s
        }
        TypeExpr::Tuple { elems, .. } => {
            let mut s = String::from("(");
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&type_expr_display(e, interner));
            }
            if elems.len() == 1 {
                s.push(',');
            }
            s.push(')');
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_compiler::{
        FileId, PreviewFeatures, SourceFile, merge_symbols, parse_all_files_with_preview,
    };

    fn parse(source: &str) -> (Ast, ThreadedRodeo) {
        let sources = vec![SourceFile::new("main.gruel", source, FileId::new(1))];
        let parsed = parse_all_files_with_preview(&sources, &PreviewFeatures::default()).unwrap();
        let merged = merge_symbols(parsed).unwrap();
        (merged.ast, merged.interner)
    }

    #[test]
    fn hover_function_name() {
        let src = "fn main() -> i32 { 0 }";
        let (ast, interner) = parse(src);
        // Cursor on the `m` of `main` (byte 3).
        let h = hover_at(&ast, &interner, FileId::new(1), 3).unwrap();
        assert!(h.markdown.contains("fn main"), "got: {}", h.markdown);
        assert!(h.markdown.contains("-> i32"));
    }

    #[test]
    fn hover_function_with_doc() {
        let src = "/// Does the thing.\nfn main() -> i32 { 0 }";
        let (ast, interner) = parse(src);
        // The function name starts after "/// Does the thing.\nfn ".
        let byte = src.find("main").unwrap() as u32;
        let h = hover_at(&ast, &interner, FileId::new(1), byte).unwrap();
        assert!(h.markdown.contains("Does the thing"), "got: {}", h.markdown);
    }

    #[test]
    fn hover_struct_name() {
        let src = "struct Point { x: i32, y: i32 }";
        let (ast, interner) = parse(src);
        let byte = src.find("Point").unwrap() as u32 + 1;
        let h = hover_at(&ast, &interner, FileId::new(1), byte).unwrap();
        assert!(h.markdown.contains("struct Point"));
    }

    #[test]
    fn hover_struct_field() {
        let src = "struct Point { x: i32, y: i32 }";
        let (ast, interner) = parse(src);
        let byte = src.find(": i32").unwrap() as u32 - 1; // on 'x'
        let h = hover_at(&ast, &interner, FileId::new(1), byte).unwrap();
        assert!(
            h.markdown.contains("x: i32") || h.markdown.contains("x"),
            "got: {}",
            h.markdown
        );
    }

    #[test]
    fn hover_type_reference() {
        let src = "fn id(x: i32) -> i32 { x }";
        let (ast, interner) = parse(src);
        // Position on first 'i32' (the parameter type).
        let byte = src.find("i32").unwrap() as u32;
        let h = hover_at(&ast, &interner, FileId::new(1), byte).unwrap();
        assert!(h.markdown.contains("i32"), "got: {}", h.markdown);
    }

    #[test]
    fn hover_enum_with_variants() {
        let src = "enum Color { Red, Green, Blue }";
        let (ast, interner) = parse(src);
        let byte = src.find("Red").unwrap() as u32;
        let h = hover_at(&ast, &interner, FileId::new(1), byte).unwrap();
        assert!(h.markdown.contains("Red"), "got: {}", h.markdown);
    }

    #[test]
    fn hover_const_with_doc() {
        let src = "/// The answer.\nconst N: i32 = 42;";
        let (ast, interner) = parse(src);
        let byte = src.find('N').unwrap() as u32;
        let h = hover_at(&ast, &interner, FileId::new(1), byte).unwrap();
        assert!(h.markdown.contains("const N"));
        assert!(h.markdown.contains("The answer"));
    }

    #[test]
    fn hover_outside_returns_none() {
        let src = "fn main() -> i32 { 0 }";
        let (ast, interner) = parse(src);
        let byte = src.len() as u32 + 100;
        assert!(hover_at(&ast, &interner, FileId::new(1), byte).is_none());
    }
}
