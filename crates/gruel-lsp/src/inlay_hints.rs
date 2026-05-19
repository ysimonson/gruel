//! Inlay hints (ADR-0091 Phase 6).
//!
//! - After each `let` binding without an explicit type annotation, show
//!   the inferred type (`: i32`).
//! - After each unnamed call argument, show the parameter name (`x: 42`).

use gruel_compiler::{Type, TypeInternPool};
use gruel_parser::ast::{
    Ast, BlockExpr, Expr, Function, Item, Pattern, Statement,
};
use gruel_util::{FileId, Span};
use lasso::{Spur, ThreadedRodeo};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlayKind {
    Type,
    Parameter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    pub label: String,
    pub kind: InlayKind,
    /// Position (as byte offset) where the hint should render.
    pub byte: u32,
    /// FileId the byte is within.
    pub file_id: FileId,
}

/// Produce all inlay hints for a file. `expr_types` is the side-table
/// populated by Phase 4.
pub fn inlay_hints(
    ast: &Ast,
    interner: &ThreadedRodeo,
    expr_types: &FxHashMap<Span, Type>,
    type_pool: Option<&TypeInternPool>,
    file_id: FileId,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    for item in &ast.items {
        match item {
            Item::Function(f) => {
                if f.span.file_id == file_id {
                    visit_expr(&f.body, interner, expr_types, type_pool, file_id, Some(ast), &mut hints);
                }
            }
            Item::Struct(s) => {
                for m in &s.methods {
                    visit_expr(&m.body, interner, expr_types, type_pool, file_id, Some(ast), &mut hints);
                }
            }
            Item::Enum(e) => {
                for m in &e.methods {
                    visit_expr(&m.body, interner, expr_types, type_pool, file_id, Some(ast), &mut hints);
                }
            }
            Item::Derive(d) => {
                for m in &d.methods {
                    visit_expr(&m.body, interner, expr_types, type_pool, file_id, Some(ast), &mut hints);
                }
            }
            _ => {}
        }
    }
    hints
}

fn visit_expr(
    expr: &Expr,
    interner: &ThreadedRodeo,
    expr_types: &FxHashMap<Span, Type>,
    type_pool: Option<&TypeInternPool>,
    file_id: FileId,
    ast: Option<&Ast>,
    out: &mut Vec<InlayHint>,
) {
    match expr {
        Expr::Block(b) => {
            visit_block(b, interner, expr_types, type_pool, file_id, ast, out)
        }
        Expr::Call(c) => {
            // Look up the callee function for argument hints.
            if let Some(ast) = ast {
                if let Some(callee) = find_function(ast, c.name.name) {
                    for (i, arg) in c.args.iter().enumerate() {
                        if let Some(p) = callee.params.get(i) {
                            // Only suggest when the arg is a bare literal
                            // (i.e. it's not already an identifier matching
                            // the param name).
                            if matches!(arg.expr, Expr::Ident(id) if id.name == p.name.name) {
                                continue;
                            }
                            out.push(InlayHint {
                                label: format!("{}:", interner.resolve(&p.name.name)),
                                kind: InlayKind::Parameter,
                                byte: arg.span.start,
                                file_id: arg.span.file_id,
                            });
                        }
                    }
                }
            }
            for arg in &c.args {
                visit_expr(&arg.expr, interner, expr_types, type_pool, file_id, ast, out);
            }
        }
        Expr::If(i) => {
            visit_expr(&i.cond, interner, expr_types, type_pool, file_id, ast, out);
            visit_block(&i.then_block, interner, expr_types, type_pool, file_id, ast, out);
            if let Some(b) = &i.else_block {
                visit_block(b, interner, expr_types, type_pool, file_id, ast, out);
            }
        }
        Expr::While(w) => {
            visit_expr(&w.cond, interner, expr_types, type_pool, file_id, ast, out);
            visit_block(&w.body, interner, expr_types, type_pool, file_id, ast, out);
        }
        Expr::For(f) => {
            visit_expr(&f.iterable, interner, expr_types, type_pool, file_id, ast, out);
            visit_block(&f.body, interner, expr_types, type_pool, file_id, ast, out);
        }
        Expr::Loop(l) => visit_block(&l.body, interner, expr_types, type_pool, file_id, ast, out),
        Expr::Match(m) => {
            visit_expr(&m.scrutinee, interner, expr_types, type_pool, file_id, ast, out);
            for arm in &m.arms {
                visit_expr(&arm.body, interner, expr_types, type_pool, file_id, ast, out);
            }
        }
        Expr::Binary(b) => {
            visit_expr(&b.left, interner, expr_types, type_pool, file_id, ast, out);
            visit_expr(&b.right, interner, expr_types, type_pool, file_id, ast, out);
        }
        Expr::Unary(u) => visit_expr(&u.operand, interner, expr_types, type_pool, file_id, ast, out),
        Expr::Paren(p) => visit_expr(&p.inner, interner, expr_types, type_pool, file_id, ast, out),
        Expr::Return(r) => {
            if let Some(e) = &r.value {
                visit_expr(e, interner, expr_types, type_pool, file_id, ast, out);
            }
        }
        Expr::Tuple(t) => {
            for e in &t.elems {
                visit_expr(e, interner, expr_types, type_pool, file_id, ast, out);
            }
        }
        Expr::Index(i) => {
            visit_expr(&i.base, interner, expr_types, type_pool, file_id, ast, out);
            visit_expr(&i.index, interner, expr_types, type_pool, file_id, ast, out);
        }
        Expr::MethodCall(m) => {
            visit_expr(&m.receiver, interner, expr_types, type_pool, file_id, ast, out);
            for arg in &m.args {
                visit_expr(&arg.expr, interner, expr_types, type_pool, file_id, ast, out);
            }
        }
        _ => {}
    }
}

fn visit_block(
    b: &BlockExpr,
    interner: &ThreadedRodeo,
    expr_types: &FxHashMap<Span, Type>,
    type_pool: Option<&TypeInternPool>,
    file_id: FileId,
    ast: Option<&Ast>,
    out: &mut Vec<InlayHint>,
) {
    for stmt in &b.statements {
        if let Statement::Let(l) = stmt {
            if l.ty.is_none() {
                if let Pattern::Ident { name, .. } = &l.pattern {
                    if name.span.file_id == file_id {
                        if let Some(ty) = lookup_init_type(&l.init, expr_types) {
                            let label = if let Some(pool) = type_pool {
                                format!(": {}", pool.format_type_name(ty))
                            } else {
                                format!(": {:?}", ty)
                            };
                            out.push(InlayHint {
                                label,
                                kind: InlayKind::Type,
                                byte: name.span.end,
                                file_id: name.span.file_id,
                            });
                        }
                    }
                }
            }
            visit_expr(&l.init, interner, expr_types, type_pool, file_id, ast, out);
        } else if let Statement::Expr(e) = stmt {
            visit_expr(e, interner, expr_types, type_pool, file_id, ast, out);
        } else if let Statement::Assign(a) = stmt {
            visit_expr(&a.value, interner, expr_types, type_pool, file_id, ast, out);
        }
    }
    visit_expr(&b.expr, interner, expr_types, type_pool, file_id, ast, out);
}

fn lookup_init_type(init: &Expr, expr_types: &FxHashMap<Span, Type>) -> Option<Type> {
    let span = expr_span(init)?;
    // Try the exact init span first.
    if let Some(ty) = expr_types.get(&span) {
        return Some(*ty);
    }
    // Otherwise: any expr_types entry that exactly equals the init span.
    None
}

fn expr_span(e: &Expr) -> Option<Span> {
    match e {
        Expr::Int(l) => Some(l.span),
        Expr::Float(l) => Some(l.span),
        Expr::String(l) => Some(l.span),
        Expr::Char(l) => Some(l.span),
        Expr::Bool(l) => Some(l.span),
        Expr::Unit(l) => Some(l.span),
        Expr::Ident(i) => Some(i.span),
        Expr::Binary(b) => Some(b.span),
        Expr::Unary(u) => Some(u.span),
        Expr::Paren(p) => Some(p.span),
        Expr::Block(b) => Some(b.span),
        Expr::If(i) => Some(i.span),
        Expr::Match(m) => Some(m.span),
        Expr::While(w) => Some(w.span),
        Expr::For(f) => Some(f.span),
        Expr::Loop(l) => Some(l.span),
        Expr::Call(c) => Some(c.span),
        Expr::MethodCall(m) => Some(m.span),
        Expr::Field(f) => Some(f.span),
        Expr::Tuple(t) => Some(t.span),
        Expr::Index(i) => Some(i.span),
        Expr::Return(r) => Some(r.span),
        _ => None,
    }
}

fn find_function(ast: &Ast, name: Spur) -> Option<Function> {
    for item in &ast.items {
        if let Item::Function(f) = item {
            if f.name.name == name {
                return Some(f.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_compiler::PreviewFeatures;
    use gruel_target::Target;

    fn snap_for(source: &str) -> std::sync::Arc<crate::analysis::Snapshot> {
        use crate::analysis::{WorkspaceFile, analyze};
        use std::path::PathBuf;
        let files = vec![WorkspaceFile {
            path: PathBuf::from("main.gruel"),
            text: source.to_string(),
            file_id: FileId::new(1),
        }];
        let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
        std::sync::Arc::new(res.snapshot.unwrap())
    }

    #[test]
    fn inlay_for_untyped_let() {
        let src = "fn main() -> i32 { let answer = 42; answer }";
        let snap = snap_for(src);
        let hints = inlay_hints(
            &snap.ast,
            &snap.interner,
            &snap.expr_types,
            snap.type_pool.as_deref(),
            FileId::new(1),
        );
        let type_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == InlayKind::Type)
            .collect();
        assert!(!type_hints.is_empty(), "expected at least one type hint");
        assert!(type_hints.iter().any(|h| h.label.contains("i32")));
    }

    #[test]
    fn parameter_hint_for_call() {
        let src = "fn add(x: i32, y: i32) -> i32 { x + y }\nfn main() -> i32 { add(1, 2) }";
        let snap = snap_for(src);
        let hints = inlay_hints(
            &snap.ast,
            &snap.interner,
            &snap.expr_types,
            snap.type_pool.as_deref(),
            FileId::new(1),
        );
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == InlayKind::Parameter)
            .collect();
        assert!(
            param_hints.iter().any(|h| h.label == "x:"),
            "expected `x:` hint, got: {:?}",
            hints
        );
    }
}
