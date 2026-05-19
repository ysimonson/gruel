//! Goto-definition (ADR-0091 Phase 4).
//!
//! Given a cursor position, find the identifier under it and walk the AST
//! to locate the *defining* occurrence (function, struct, enum, const,
//! field, variant, method). Phase 4 covers definitions visible in the
//! merged AST: type names referenced in type-position resolve to their
//! struct/enum/interface/derive declaration; identifiers in expression
//! position resolve when they name a top-level function, constant, or
//! local binding within the enclosing function.
//!
//! Field accesses across `@import` boundaries and cross-file imports are
//! deferred to later phases (they need sema-level symbol resolution).

use gruel_parser::ast::{
    AssignTarget, Ast, BlockExpr, Expr, Function, Ident, Item, MatchArm, Method, Pattern,
    Statement, TypeExpr,
};
use gruel_util::{FileId, Span};
use lasso::ThreadedRodeo;

/// Find the defining span for the identifier under the cursor, if any.
pub fn definition_at(
    ast: &Ast,
    interner: &ThreadedRodeo,
    file_id: FileId,
    byte: u32,
) -> Option<Span> {
    let target = find_ident_at(ast, file_id, byte)?;
    resolve_definition(ast, interner, target)
}

/// Walk the AST and find the identifier whose span contains `byte`.
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
        let span = ident.span;
        if span.file_id != self.file_id {
            return;
        }
        if self.byte < span.start || self.byte >= span.end {
            return;
        }
        let size = span.end.saturating_sub(span.start);
        if size <= self.best_size {
            self.best_size = size;
            self.result = Some(ident);
        }
    }

    fn visit_item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => {
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
            Item::Struct(s) => {
                self.consider(s.name);
                for field in &s.fields {
                    self.consider(field.name);
                    self.visit_type(&field.ty);
                }
                for m in &s.methods {
                    self.visit_method(m);
                }
            }
            Item::Enum(e) => {
                self.consider(e.name);
                for v in &e.variants {
                    self.consider(v.name);
                }
                for m in &e.methods {
                    self.visit_method(m);
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
                    self.visit_method(m);
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

    fn visit_method(&mut self, m: &Method) {
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
            Expr::TupleIndex(t) => self.visit_expr(&t.base),
            Expr::Index(i) => {
                self.visit_expr(&i.base);
                self.visit_expr(&i.index);
            }
            Expr::StructLit(s) => {
                if let Some(base) = &s.base {
                    self.visit_expr(base);
                }
                self.consider(s.name);
                for fi in &s.fields {
                    self.consider(fi.name);
                    self.visit_expr(&fi.value);
                }
            }
            Expr::EnumStructLit(_) => {}
            Expr::Path(p) => {
                if let Some(b) = &p.base {
                    self.visit_expr(b);
                }
                self.consider(p.type_name);
                self.consider(p.variant);
            }
            Expr::AssocFnCall(_) => {}
            Expr::IntrinsicCall(c) => {
                self.consider(c.name);
                for a in &c.args {
                    if let gruel_parser::ast::IntrinsicArg::Expr(e) = a {
                        self.visit_expr(e);
                    }
                }
            }
            Expr::ArrayLit(a) => {
                for e in &a.elements {
                    self.visit_expr(e);
                }
            }
            Expr::Range(r) => {
                if let Some(e) = &r.lo {
                    self.visit_expr(e);
                }
                if let Some(e) = &r.hi {
                    self.visit_expr(e);
                }
            }
            Expr::AnonFn(f) => {
                for p in &f.params {
                    self.consider(p.name);
                    self.visit_type(&p.ty);
                }
                self.visit_block(&f.body);
            }
            Expr::Comptime(_) => {}
            Expr::ComptimeUnrollFor(_) => {}
            Expr::Checked(_) => {}
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
                self.consider_pattern(&l.pattern);
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

    fn consider_pattern(&mut self, p: &Pattern) {
        if let Pattern::Ident { name, .. } = p {
            self.consider(*name);
        }
    }

    fn visit_match_arm(&mut self, arm: &MatchArm) {
        self.visit_expr(&arm.body);
    }
}

/// Resolve the identifier to its defining span by scanning top-level
/// items and the bodies that introduced it.
fn resolve_definition(ast: &Ast, _interner: &ThreadedRodeo, target: Ident) -> Option<Span> {
    // Top-level matches.
    for item in &ast.items {
        let span = match item {
            Item::Function(f) if f.name.name == target.name => Some(f.name.span),
            Item::Struct(s) if s.name.name == target.name => Some(s.name.span),
            Item::Enum(e) if e.name.name == target.name => Some(e.name.span),
            Item::Interface(i) if i.name.name == target.name => Some(i.name.span),
            Item::Derive(d) if d.name.name == target.name => Some(d.name.span),
            Item::Const(c) if c.name.name == target.name => Some(c.name.span),
            _ => None,
        };
        if let Some(s) = span {
            return Some(s);
        }
    }
    // Locals and params live inside a specific function body.
    if let Some(span) = resolve_in_function_bodies(ast, target) {
        return Some(span);
    }
    None
}

fn resolve_in_function_bodies(ast: &Ast, target: Ident) -> Option<Span> {
    for item in &ast.items {
        match item {
            Item::Function(f) => {
                if let Some(s) = resolve_in_function(f, target) {
                    return Some(s);
                }
            }
            Item::Struct(s) => {
                for m in &s.methods {
                    if let Some(s) = resolve_in_method(m, target) {
                        return Some(s);
                    }
                }
            }
            Item::Enum(e) => {
                for m in &e.methods {
                    if let Some(s) = resolve_in_method(m, target) {
                        return Some(s);
                    }
                }
            }
            Item::Derive(d) => {
                for m in &d.methods {
                    if let Some(s) = resolve_in_method(m, target) {
                        return Some(s);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn resolve_in_function(f: &Function, target: Ident) -> Option<Span> {
    if !expr_contains_target(&f.body, target) {
        return None;
    }
    for p in &f.params {
        if p.name.name == target.name {
            return Some(p.name.span);
        }
    }
    find_local_def_in_expr(&f.body, target)
}

fn resolve_in_method(m: &Method, target: Ident) -> Option<Span> {
    if !expr_contains_target(&m.body, target) {
        return None;
    }
    for p in &m.params {
        if p.name.name == target.name {
            return Some(p.name.span);
        }
    }
    find_local_def_in_expr(&m.body, target)
}

#[allow(dead_code)]
fn block_contains(b: &BlockExpr, target: Ident) -> bool {
    target.span.start >= b.span.start && target.span.end <= b.span.end
}

fn expr_contains_target(e: &Expr, target: Ident) -> bool {
    let span = match e {
        Expr::Block(b) => b.span,
        _ => return false,
    };
    target.span.start >= span.start && target.span.end <= span.end
}

fn find_local_def_in_expr(expr: &Expr, target: Ident) -> Option<Span> {
    let mut found: Option<Span> = None;
    walk_expr_for_let(expr, target, &mut found);
    found
}

fn walk_block_for_let(block: &BlockExpr, target: Ident, found: &mut Option<Span>) {
    for stmt in &block.statements {
        match stmt {
            Statement::Let(l) => {
                if let Pattern::Ident { name, .. } = &l.pattern {
                    if name.name == target.name && name.span.start <= target.span.start {
                        *found = Some(name.span);
                    }
                }
                walk_expr_for_let(&l.init, target, found);
            }
            Statement::Assign(a) => {
                walk_expr_for_let(&a.value, target, found);
            }
            Statement::Expr(e) => {
                walk_expr_for_let(e, target, found);
            }
        }
    }
    walk_expr_for_let(&block.expr, target, found);
}

fn walk_expr_for_let(expr: &Expr, target: Ident, found: &mut Option<Span>) {
    match expr {
        Expr::Block(b) => walk_block_for_let(b, target, found),
        Expr::If(i) => {
            walk_block_for_let(&i.then_block, target, found);
            if let Some(b) = &i.else_block {
                walk_block_for_let(b, target, found);
            }
        }
        Expr::While(w) => walk_block_for_let(&w.body, target, found),
        Expr::For(f) => {
            if f.binding.name == target.name && f.binding.span.start <= target.span.start {
                *found = Some(f.binding.span);
            }
            walk_block_for_let(&f.body, target, found);
        }
        Expr::Loop(l) => walk_block_for_let(&l.body, target, found),
        Expr::Match(m) => {
            for arm in &m.arms {
                walk_expr_for_let(&arm.body, target, found);
            }
        }
        _ => {}
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
    fn goto_function_reference() {
        let src = "fn foo() -> i32 { 0 }\nfn main() -> i32 { foo() }";
        let (ast, interner) = parse(src);
        let foo_call = src.rfind("foo").unwrap() as u32;
        let def = definition_at(&ast, &interner, FileId::new(1), foo_call + 1).unwrap();
        assert_eq!(def.start, src.find("foo").unwrap() as u32);
    }

    #[test]
    fn goto_struct_reference_in_type_position() {
        let src = "struct Point { x: i32 }\nfn make() -> Point { Point { x: 0 } }";
        let (ast, interner) = parse(src);
        let pos = src.find("-> Point").unwrap() as u32 + 3;
        let def = definition_at(&ast, &interner, FileId::new(1), pos).unwrap();
        assert_eq!(def.start, src.find("Point").unwrap() as u32);
    }

    #[test]
    fn goto_local_variable() {
        let src = "fn main() -> i32 { let x = 42; x }";
        let (ast, interner) = parse(src);
        let pos = src.rfind('x').unwrap() as u32;
        let def = definition_at(&ast, &interner, FileId::new(1), pos).unwrap();
        // `let x` → x position
        assert_eq!(def.start, src.find("x =").unwrap() as u32);
    }

    #[test]
    fn goto_function_param() {
        let src = "fn id(x: i32) -> i32 { x }";
        let (ast, interner) = parse(src);
        let pos = src.rfind('x').unwrap() as u32;
        let def = definition_at(&ast, &interner, FileId::new(1), pos).unwrap();
        assert_eq!(def.start, src.find("x:").unwrap() as u32);
    }
}
