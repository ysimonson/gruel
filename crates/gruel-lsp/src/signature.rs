//! Signature help (ADR-0091 Phase 4).
//!
//! When the cursor sits after `(` or `,` inside a call, return the
//! callee's parameter list with the active parameter index.

use gruel_parser::ast::{Ast, BlockExpr, CallArg, Expr, Function, Item, Statement, TypeExpr};
use gruel_util::FileId;
use lasso::{Spur, ThreadedRodeo};

/// Result returned to the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelpResult {
    /// One signature label, e.g. `"fn foo(x: i32, y: bool) -> i32"`.
    pub label: String,
    /// Parameter labels — `(start, end)` byte offsets into `label`.
    pub parameters: Vec<(u32, u32)>,
    /// Active parameter index (0-based).
    pub active_parameter: usize,
}

/// Find the enclosing call at byte `byte` and produce signature help.
pub fn signature_help(
    ast: &Ast,
    interner: &ThreadedRodeo,
    file_id: FileId,
    byte: u32,
) -> Option<SignatureHelpResult> {
    let info = find_enclosing_call(ast, file_id, byte)?;
    let target_fn = find_function(ast, info.callee)?;
    Some(build_signature(target_fn, interner, info.active_parameter))
}

#[derive(Clone, Debug)]
struct CallInfo {
    callee: Spur,
    active_parameter: usize,
}

fn active_param(args: &[CallArg], byte: u32) -> usize {
    let mut idx = 0usize;
    for arg in args {
        if byte <= arg.span.end {
            return idx;
        }
        idx += 1;
    }
    idx
}

fn find_enclosing_call(ast: &Ast, file_id: FileId, byte: u32) -> Option<CallInfo> {
    let mut best: Option<(u32, CallInfo)> = None;
    for item in &ast.items {
        match item {
            Item::Function(f) => visit_function(f, file_id, byte, &mut best),
            Item::Struct(s) => {
                for m in &s.methods {
                    visit_expr(&m.body, file_id, byte, &mut best);
                }
            }
            Item::Enum(e) => {
                for m in &e.methods {
                    visit_expr(&m.body, file_id, byte, &mut best);
                }
            }
            Item::Derive(d) => {
                for m in &d.methods {
                    visit_expr(&m.body, file_id, byte, &mut best);
                }
            }
            Item::Const(c) => visit_expr(&c.init, file_id, byte, &mut best),
            _ => {}
        }
    }
    best.map(|(_, c)| c)
}

fn visit_function(f: &Function, file_id: FileId, byte: u32, best: &mut Option<(u32, CallInfo)>) {
    visit_expr(&f.body, file_id, byte, best);
}

fn visit_block(b: &BlockExpr, file_id: FileId, byte: u32, best: &mut Option<(u32, CallInfo)>) {
    for stmt in &b.statements {
        match stmt {
            Statement::Let(l) => visit_expr(&l.init, file_id, byte, best),
            Statement::Assign(a) => visit_expr(&a.value, file_id, byte, best),
            Statement::Expr(e) => visit_expr(e, file_id, byte, best),
        }
    }
    visit_expr(&b.expr, file_id, byte, best);
}

fn visit_expr(e: &Expr, file_id: FileId, byte: u32, best: &mut Option<(u32, CallInfo)>) {
    match e {
        Expr::Call(c) => {
            if c.span.file_id == file_id && byte >= c.span.start && byte <= c.span.end {
                let size = c.span.end.saturating_sub(c.span.start);
                if best.as_ref().map_or(true, |(b, _)| size <= *b) {
                    *best = Some((
                        size,
                        CallInfo {
                            callee: c.name.name,
                            active_parameter: active_param(&c.args, byte),
                        },
                    ));
                }
            }
            for arg in &c.args {
                visit_expr(&arg.expr, file_id, byte, best);
            }
        }
        Expr::MethodCall(m) => {
            if m.span.file_id == file_id && byte >= m.span.start && byte <= m.span.end {
                let size = m.span.end.saturating_sub(m.span.start);
                if best.as_ref().map_or(true, |(b, _)| size <= *b) {
                    *best = Some((
                        size,
                        CallInfo {
                            callee: m.method.name,
                            active_parameter: active_param(&m.args, byte),
                        },
                    ));
                }
            }
            visit_expr(&m.receiver, file_id, byte, best);
            for arg in &m.args {
                visit_expr(&arg.expr, file_id, byte, best);
            }
        }
        Expr::Block(b) => visit_block(b, file_id, byte, best),
        Expr::If(i) => {
            visit_expr(&i.cond, file_id, byte, best);
            visit_block(&i.then_block, file_id, byte, best);
            if let Some(b) = &i.else_block {
                visit_block(b, file_id, byte, best);
            }
        }
        Expr::While(w) => {
            visit_expr(&w.cond, file_id, byte, best);
            visit_block(&w.body, file_id, byte, best);
        }
        Expr::For(f) => {
            visit_expr(&f.iterable, file_id, byte, best);
            visit_block(&f.body, file_id, byte, best);
        }
        Expr::Loop(l) => visit_block(&l.body, file_id, byte, best),
        Expr::Match(m) => {
            visit_expr(&m.scrutinee, file_id, byte, best);
            for arm in &m.arms {
                visit_expr(&arm.body, file_id, byte, best);
            }
        }
        Expr::Binary(b) => {
            visit_expr(&b.left, file_id, byte, best);
            visit_expr(&b.right, file_id, byte, best);
        }
        Expr::Unary(u) => visit_expr(&u.operand, file_id, byte, best),
        Expr::Paren(p) => visit_expr(&p.inner, file_id, byte, best),
        Expr::Field(fa) => visit_expr(&fa.base, file_id, byte, best),
        Expr::Return(r) => {
            if let Some(e) = &r.value {
                visit_expr(e, file_id, byte, best);
            }
        }
        Expr::Tuple(t) => {
            for e in &t.elems {
                visit_expr(e, file_id, byte, best);
            }
        }
        Expr::ArrayLit(a) => {
            for e in &a.elements {
                visit_expr(e, file_id, byte, best);
            }
        }
        Expr::Index(i) => {
            visit_expr(&i.base, file_id, byte, best);
            visit_expr(&i.index, file_id, byte, best);
        }
        _ => {}
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

fn build_signature(f: Function, interner: &ThreadedRodeo, active: usize) -> SignatureHelpResult {
    let mut label = String::from("fn ");
    label.push_str(interner.resolve(&f.name.name));
    label.push('(');
    let mut parameters = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let start = label.len() as u32;
        label.push_str(interner.resolve(&p.name.name));
        label.push_str(": ");
        label.push_str(&type_expr_display(&p.ty, interner));
        let end = label.len() as u32;
        parameters.push((start, end));
    }
    label.push(')');
    if let Some(rt) = &f.return_type {
        label.push_str(" -> ");
        label.push_str(&type_expr_display(rt, interner));
    }
    let clamped_active = if f.params.is_empty() {
        0
    } else {
        active.min(f.params.len() - 1)
    };
    SignatureHelpResult {
        label,
        parameters,
        active_parameter: clamped_active,
    }
}

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
        _ => "_".to_string(),
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
    fn signature_help_for_call() {
        let src = "fn add(x: i32, y: i32) -> i32 { x + y }\nfn main() -> i32 { add(1, 2) }";
        let (ast, interner) = parse(src);
        let pos = src.find("add(1").unwrap() as u32 + 4;
        let sig = signature_help(&ast, &interner, FileId::new(1), pos).unwrap();
        assert!(sig.label.starts_with("fn add(x: i32, y: i32)"));
        assert_eq!(sig.active_parameter, 0);
        assert_eq!(sig.parameters.len(), 2);
    }

    #[test]
    fn signature_help_active_param_advances_after_comma() {
        let src = "fn add(x: i32, y: i32) -> i32 { x + y }\nfn main() -> i32 { add(1, 2) }";
        let (ast, interner) = parse(src);
        let pos = src.find("add(1, 2)").unwrap() as u32 + 7;
        let sig = signature_help(&ast, &interner, FileId::new(1), pos).unwrap();
        assert_eq!(sig.active_parameter, 1);
    }
}
