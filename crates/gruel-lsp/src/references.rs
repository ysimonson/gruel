//! Find references (ADR-0091 Phase 5).
//!
//! Walk the merged AST and collect every identifier whose name matches a
//! given target name *and* whose enclosing scope is consistent with the
//! definition. Without sema-level symbol resolution we use a textual
//! match scoped to:
//!
//! - all references whose name matches a top-level item's defining name
//!   (functions, structs, enums, interfaces, derives, consts) — Phase 5
//!   conservatively returns every occurrence, since the same name at
//!   top level can only resolve to that item under the current
//!   "all symbols live in a flat namespace" rule (ADR-0023).
//! - parameter and local-let references — limited to the enclosing
//!   function body. The same name in a different function is a
//!   different binding.

use gruel_parser::ast::{
    AssignTarget, Ast, BlockExpr, Expr, Function, Ident, Item, MatchArm, Method, Pattern,
    Statement, TypeExpr,
};
use gruel_util::{FileId, Span};
use lasso::{Spur, ThreadedRodeo};

use crate::goto::definition_at;

/// Find every reference to the identifier under the cursor.
///
/// `include_declaration` mirrors the LSP `referencesParams.context`.
pub fn references_at(
    ast: &Ast,
    interner: &ThreadedRodeo,
    file_id: FileId,
    byte: u32,
    include_declaration: bool,
) -> Vec<Span> {
    let target = match find_ident_at(ast, file_id, byte) {
        Some(i) => i,
        None => return Vec::new(),
    };
    let def_span = definition_at(ast, interner, file_id, byte);

    // Decide scope: if def is a top-level item's name, scope = whole AST.
    // Otherwise (def lives inside a function body — a param or let), scope
    // = that function body.
    let scope = if let Some(def) = def_span {
        if is_top_level_item_name(ast, def) {
            Scope::Workspace
        } else if let Some(f) = enclosing_function(ast, def) {
            Scope::Function(f)
        } else {
            Scope::Workspace
        }
    } else {
        Scope::Workspace
    };

    let mut out = Vec::new();
    collect_references(ast, target.name, scope, &mut out);

    if !include_declaration {
        if let Some(def) = def_span {
            out.retain(|s| *s != def);
        }
    }
    out.sort_by_key(|s| (s.file_id.0, s.start, s.end));
    out.dedup();
    out
}

#[derive(Clone)]
enum Scope<'a> {
    Workspace,
    Function(&'a Function),
}

fn enclosing_function(ast: &Ast, def: Span) -> Option<&Function> {
    for item in &ast.items {
        if let Item::Function(f) = item {
            if def.file_id == f.span.file_id && def.start >= f.span.start && def.end <= f.span.end {
                return Some(f);
            }
        }
    }
    None
}

fn is_top_level_item_name(ast: &Ast, span: Span) -> bool {
    for item in &ast.items {
        let name_span = match item {
            Item::Function(f) => f.name.span,
            Item::Struct(s) => s.name.span,
            Item::Enum(e) => e.name.span,
            Item::Interface(i) => i.name.span,
            Item::Derive(d) => d.name.span,
            Item::Const(c) => c.name.span,
            _ => continue,
        };
        if name_span == span {
            return true;
        }
    }
    false
}

fn collect_references(ast: &Ast, name: Spur, scope: Scope, out: &mut Vec<Span>) {
    let mut walker = RefWalker { name, out };
    match scope {
        Scope::Workspace => {
            for item in &ast.items {
                walker.visit_item(item);
            }
        }
        Scope::Function(f) => {
            walker.visit_function_body(f);
        }
    }
}

struct RefWalker<'a> {
    name: Spur,
    out: &'a mut Vec<Span>,
}

impl<'a> RefWalker<'a> {
    fn consider(&mut self, ident: Ident) {
        if ident.name == self.name {
            self.out.push(ident.span);
        }
    }

    fn visit_item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => self.visit_function_full(f),
            Item::Struct(s) => {
                self.consider(s.name);
                for field in &s.fields {
                    self.consider(field.name);
                    self.visit_type(&field.ty);
                }
                for m in &s.methods {
                    self.visit_method_full(m);
                }
            }
            Item::Enum(e) => {
                self.consider(e.name);
                for v in &e.variants {
                    self.consider(v.name);
                }
                for m in &e.methods {
                    self.visit_method_full(m);
                }
            }
            Item::Interface(i) => {
                self.consider(i.name);
                for sig in &i.methods {
                    self.consider(sig.name);
                    for p in &sig.params {
                        self.consider(p.name);
                        self.visit_type(&p.ty);
                    }
                    if let Some(rt) = &sig.return_type {
                        self.visit_type(rt);
                    }
                }
            }
            Item::Derive(d) => {
                self.consider(d.name);
                for m in &d.methods {
                    self.visit_method_full(m);
                }
            }
            Item::Const(c) => {
                self.consider(c.name);
                if let Some(ty) = &c.ty {
                    self.visit_type(ty);
                }
                self.visit_expr(&c.init);
            }
            Item::LinkExtern(b) => {
                for ext in &b.items {
                    self.consider(ext.name);
                    for p in &ext.params {
                        self.consider(p.name);
                        self.visit_type(&p.ty);
                    }
                    if let Some(rt) = &ext.return_type {
                        self.visit_type(rt);
                    }
                }
            }
            Item::Error(_) => {}
        }
    }

    fn visit_function_full(&mut self, f: &Function) {
        self.consider(f.name);
        for p in &f.params {
            self.consider(p.name);
            self.visit_type(&p.ty);
        }
        if let Some(rt) = &f.return_type {
            self.visit_type(rt);
        }
        self.visit_expr(&f.body);
    }

    fn visit_method_full(&mut self, m: &Method) {
        self.consider(m.name);
        for p in &m.params {
            self.consider(p.name);
            self.visit_type(&p.ty);
        }
        if let Some(rt) = &m.return_type {
            self.visit_type(rt);
        }
        self.visit_expr(&m.body);
    }

    fn visit_function_body(&mut self, f: &Function) {
        for p in &f.params {
            self.consider(p.name);
        }
        self.visit_expr(&f.body);
    }

    fn visit_type(&mut self, ty: &TypeExpr) {
        match ty {
            TypeExpr::Named(ident) => self.consider(*ident),
            TypeExpr::TypeCall { callee, args, .. } => {
                self.consider(*callee);
                for a in args {
                    self.visit_type(a);
                }
            }
            TypeExpr::Array { element, .. } => self.visit_type(element),
            TypeExpr::Tuple { elems, .. } => {
                for e in elems {
                    self.visit_type(e);
                }
            }
            _ => {}
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => self.consider(*ident),
            Expr::Block(b) => self.visit_block(b),
            Expr::Call(c) => {
                self.consider(c.name);
                for arg in &c.args {
                    self.visit_expr(&arg.expr);
                }
            }
            Expr::MethodCall(m) => {
                self.visit_expr(&m.receiver);
                self.consider(m.method);
                for arg in &m.args {
                    self.visit_expr(&arg.expr);
                }
            }
            Expr::Field(f) => {
                self.visit_expr(&f.base);
                self.consider(f.field);
            }
            Expr::Binary(b) => {
                self.visit_expr(&b.left);
                self.visit_expr(&b.right);
            }
            Expr::Unary(u) => self.visit_expr(&u.operand),
            Expr::Paren(p) => self.visit_expr(&p.inner),
            Expr::If(i) => {
                self.visit_expr(&i.cond);
                self.visit_block(&i.then_block);
                if let Some(b) = &i.else_block {
                    self.visit_block(b);
                }
            }
            Expr::While(w) => {
                self.visit_expr(&w.cond);
                self.visit_block(&w.body);
            }
            Expr::For(f) => {
                self.consider(f.binding);
                self.visit_expr(&f.iterable);
                self.visit_block(&f.body);
            }
            Expr::Loop(l) => self.visit_block(&l.body),
            Expr::Match(m) => {
                self.visit_expr(&m.scrutinee);
                for arm in &m.arms {
                    self.visit_match_arm(arm);
                }
            }
            Expr::Return(r) => {
                if let Some(e) = &r.value {
                    self.visit_expr(e);
                }
            }
            Expr::Tuple(t) => {
                for e in &t.elems {
                    self.visit_expr(e);
                }
            }
            Expr::Index(i) => {
                self.visit_expr(&i.base);
                self.visit_expr(&i.index);
            }
            Expr::TupleIndex(t) => self.visit_expr(&t.base),
            Expr::StructLit(s) => {
                if let Some(b) = &s.base {
                    self.visit_expr(b);
                }
                self.consider(s.name);
                for fi in &s.fields {
                    self.consider(fi.name);
                    self.visit_expr(&fi.value);
                }
            }
            Expr::Path(p) => {
                if let Some(b) = &p.base {
                    self.visit_expr(b);
                }
                self.consider(p.type_name);
                self.consider(p.variant);
            }
            Expr::ArrayLit(a) => {
                for e in &a.elements {
                    self.visit_expr(e);
                }
            }
            Expr::IntrinsicCall(c) => {
                self.consider(c.name);
                for a in &c.args {
                    if let gruel_parser::ast::IntrinsicArg::Expr(e) = a {
                        self.visit_expr(e);
                    }
                }
            }
            _ => {}
        }
    }

    fn visit_block(&mut self, b: &BlockExpr) {
        for stmt in &b.statements {
            self.visit_statement(stmt);
        }
        self.visit_expr(&b.expr);
    }

    fn visit_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Let(l) => {
                if let Pattern::Ident { name, .. } = &l.pattern {
                    self.consider(*name);
                }
                if let Some(ty) = &l.ty {
                    self.visit_type(ty);
                }
                self.visit_expr(&l.init);
            }
            Statement::Assign(a) => {
                match &a.target {
                    AssignTarget::Var(i) => self.consider(*i),
                    AssignTarget::Field(f) => {
                        self.visit_expr(&f.base);
                        self.consider(f.field);
                    }
                    AssignTarget::Index(i) => {
                        self.visit_expr(&i.base);
                        self.visit_expr(&i.index);
                    }
                }
                self.visit_expr(&a.value);
            }
            Statement::Expr(e) => self.visit_expr(e),
        }
    }

    fn visit_match_arm(&mut self, arm: &MatchArm) {
        self.visit_expr(&arm.body);
    }
}

fn find_ident_at(ast: &Ast, file_id: FileId, byte: u32) -> Option<Ident> {
    let mut finder = IdentFinder::new(file_id, byte);
    for item in &ast.items {
        finder.visit_item(item);
    }
    finder.result
}

struct IdentFinder {
    file_id: FileId,
    byte: u32,
    result: Option<Ident>,
    best_size: u32,
}

impl IdentFinder {
    fn new(file_id: FileId, byte: u32) -> Self {
        Self {
            file_id,
            byte,
            result: None,
            best_size: u32::MAX,
        }
    }
    fn consider(&mut self, ident: Ident) {
        if ident.span.file_id != self.file_id {
            return;
        }
        if self.byte < ident.span.start || self.byte >= ident.span.end {
            return;
        }
        let size = ident.span.end.saturating_sub(ident.span.start);
        if size <= self.best_size {
            self.best_size = size;
            self.result = Some(ident);
        }
    }

    fn visit_item(&mut self, item: &Item) {
        let mut walker = RefWalker {
            name: lasso::Spur::default(),
            out: &mut Vec::new(),
        };
        // We don't actually filter by name here — just collect every Ident
        // and pick the smallest containing the byte.
        let _ = &mut walker;
        // Simpler: re-implement a passthrough walker that calls our consider().
        self.walk_item(item);
    }

    fn walk_item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => {
                self.consider(f.name);
                for p in &f.params {
                    self.consider(p.name);
                    self.walk_type(&p.ty);
                }
                if let Some(rt) = &f.return_type {
                    self.walk_type(rt);
                }
                self.walk_expr(&f.body);
            }
            Item::Struct(s) => {
                self.consider(s.name);
                for field in &s.fields {
                    self.consider(field.name);
                    self.walk_type(&field.ty);
                }
                for m in &s.methods {
                    self.walk_method(m);
                }
            }
            Item::Enum(e) => {
                self.consider(e.name);
                for v in &e.variants {
                    self.consider(v.name);
                }
                for m in &e.methods {
                    self.walk_method(m);
                }
            }
            Item::Interface(i) => {
                self.consider(i.name);
                for sig in &i.methods {
                    self.consider(sig.name);
                    for p in &sig.params {
                        self.consider(p.name);
                        self.walk_type(&p.ty);
                    }
                    if let Some(rt) = &sig.return_type {
                        self.walk_type(rt);
                    }
                }
            }
            Item::Derive(d) => {
                self.consider(d.name);
                for m in &d.methods {
                    self.walk_method(m);
                }
            }
            Item::Const(c) => {
                self.consider(c.name);
                if let Some(ty) = &c.ty {
                    self.walk_type(ty);
                }
                self.walk_expr(&c.init);
            }
            Item::LinkExtern(b) => {
                for ext in &b.items {
                    self.consider(ext.name);
                    for p in &ext.params {
                        self.consider(p.name);
                        self.walk_type(&p.ty);
                    }
                    if let Some(rt) = &ext.return_type {
                        self.walk_type(rt);
                    }
                }
            }
            Item::Error(_) => {}
        }
    }

    fn walk_method(&mut self, m: &Method) {
        self.consider(m.name);
        for p in &m.params {
            self.consider(p.name);
            self.walk_type(&p.ty);
        }
        if let Some(rt) = &m.return_type {
            self.walk_type(rt);
        }
        self.walk_expr(&m.body);
    }

    fn walk_type(&mut self, ty: &TypeExpr) {
        match ty {
            TypeExpr::Named(ident) => self.consider(*ident),
            TypeExpr::TypeCall { callee, args, .. } => {
                self.consider(*callee);
                for a in args {
                    self.walk_type(a);
                }
            }
            TypeExpr::Array { element, .. } => self.walk_type(element),
            TypeExpr::Tuple { elems, .. } => {
                for e in elems {
                    self.walk_type(e);
                }
            }
            _ => {}
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => self.consider(*ident),
            Expr::Block(b) => {
                for stmt in &b.statements {
                    self.walk_statement(stmt);
                }
                self.walk_expr(&b.expr);
            }
            Expr::Call(c) => {
                self.consider(c.name);
                for arg in &c.args {
                    self.walk_expr(&arg.expr);
                }
            }
            Expr::MethodCall(m) => {
                self.walk_expr(&m.receiver);
                self.consider(m.method);
                for arg in &m.args {
                    self.walk_expr(&arg.expr);
                }
            }
            Expr::Field(f) => {
                self.walk_expr(&f.base);
                self.consider(f.field);
            }
            Expr::Binary(b) => {
                self.walk_expr(&b.left);
                self.walk_expr(&b.right);
            }
            Expr::Unary(u) => self.walk_expr(&u.operand),
            Expr::Paren(p) => self.walk_expr(&p.inner),
            Expr::If(i) => {
                self.walk_expr(&i.cond);
                self.walk_expr(&Expr::Block(i.then_block.clone()));
                if let Some(b) = &i.else_block {
                    self.walk_expr(&Expr::Block(b.clone()));
                }
            }
            Expr::While(w) => {
                self.walk_expr(&w.cond);
                self.walk_expr(&Expr::Block(w.body.clone()));
            }
            Expr::For(f) => {
                self.consider(f.binding);
                self.walk_expr(&f.iterable);
                self.walk_expr(&Expr::Block(f.body.clone()));
            }
            Expr::Match(m) => {
                self.walk_expr(&m.scrutinee);
                for arm in &m.arms {
                    self.walk_expr(&arm.body);
                }
            }
            Expr::Return(r) => {
                if let Some(e) = &r.value {
                    self.walk_expr(e);
                }
            }
            Expr::Tuple(t) => {
                for e in &t.elems {
                    self.walk_expr(e);
                }
            }
            Expr::Index(i) => {
                self.walk_expr(&i.base);
                self.walk_expr(&i.index);
            }
            Expr::TupleIndex(t) => self.walk_expr(&t.base),
            Expr::StructLit(s) => {
                if let Some(b) = &s.base {
                    self.walk_expr(b);
                }
                self.consider(s.name);
                for fi in &s.fields {
                    self.consider(fi.name);
                    self.walk_expr(&fi.value);
                }
            }
            Expr::Path(p) => {
                if let Some(b) = &p.base {
                    self.walk_expr(b);
                }
                self.consider(p.type_name);
                self.consider(p.variant);
            }
            _ => {}
        }
    }

    fn walk_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Let(l) => {
                if let Pattern::Ident { name, .. } = &l.pattern {
                    self.consider(*name);
                }
                if let Some(ty) = &l.ty {
                    self.walk_type(ty);
                }
                self.walk_expr(&l.init);
            }
            Statement::Assign(a) => {
                match &a.target {
                    AssignTarget::Var(i) => self.consider(*i),
                    AssignTarget::Field(f) => {
                        self.walk_expr(&f.base);
                        self.consider(f.field);
                    }
                    AssignTarget::Index(i) => {
                        self.walk_expr(&i.base);
                        self.walk_expr(&i.index);
                    }
                }
                self.walk_expr(&a.value);
            }
            Statement::Expr(e) => self.walk_expr(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_compiler::{
        PreviewFeatures, SourceFile, merge_symbols, parse_all_files_with_preview,
    };

    fn parse(source: &str) -> (Ast, ThreadedRodeo) {
        let sources = vec![SourceFile::new("main.gruel", source, FileId::new(1))];
        let parsed = parse_all_files_with_preview(&sources, &PreviewFeatures::default()).unwrap();
        let merged = merge_symbols(parsed).unwrap();
        (merged.ast, merged.interner)
    }

    #[test]
    fn references_to_function_includes_call_sites() {
        let src = "fn foo() -> i32 { 0 }\nfn main() -> i32 { foo() + foo() }";
        let (ast, interner) = parse(src);
        let byte = src.find("foo").unwrap() as u32;
        let refs = references_at(&ast, &interner, FileId::new(1), byte, true);
        // 1 def + 2 call sites = 3 references
        assert!(refs.len() >= 3, "got: {:?}", refs);
    }

    #[test]
    fn references_to_local_limited_to_scope() {
        let src = "fn main() -> i32 { let x = 1; x + x }\nfn other() -> i32 { let x = 2; x }";
        let (ast, interner) = parse(src);
        let byte = src.find("let x").unwrap() as u32 + 4;
        let refs = references_at(&ast, &interner, FileId::new(1), byte, true);
        // 1 binding `x` + 2 references in main, NOT the `x` in other()
        assert_eq!(refs.len(), 3, "got: {:?}", refs);
    }

    #[test]
    fn references_excludes_declaration_when_requested() {
        let src = "fn foo() -> i32 { 0 }\nfn main() -> i32 { foo() }";
        let (ast, interner) = parse(src);
        let byte = src.find("foo").unwrap() as u32;
        let refs = references_at(&ast, &interner, FileId::new(1), byte, false);
        // Without declaration: 1 call site
        assert_eq!(refs.len(), 1);
    }
}
