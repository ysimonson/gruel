//! Workspace symbols (ADR-0091 Phase 5).
//!
//! Walks the merged AST and emits a `SymbolInformation`-shaped entry per
//! top-level item. Filtered by substring match against the LSP query.

use gruel_parser::ast::{Ast, Ident, Item};
use gruel_util::Span;
use lasso::ThreadedRodeo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Interface,
    Derive,
    Constant,
    Field,
    EnumMember,
    Method,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
    /// Optional container name (e.g. struct/enum that owns a method).
    pub container: Option<String>,
}

/// Collect every top-level (and nested method/field/variant) symbol
/// matching `query` (substring match, case-insensitive).
pub fn workspace_symbols(
    ast: &Ast,
    interner: &ThreadedRodeo,
    query: &str,
) -> Vec<WorkspaceSymbol> {
    let query_lower = query.to_lowercase();
    let mut out = Vec::new();
    for item in &ast.items {
        emit_item(item, interner, &query_lower, &mut out);
    }
    out
}

fn emit_item(
    item: &Item,
    interner: &ThreadedRodeo,
    query: &str,
    out: &mut Vec<WorkspaceSymbol>,
) {
    match item {
        Item::Function(f) => {
            push_if_match(&f.name, SymbolKind::Function, None, interner, query, out);
        }
        Item::Struct(s) => {
            push_if_match(&s.name, SymbolKind::Struct, None, interner, query, out);
            let container = interner.resolve(&s.name.name).to_string();
            for field in &s.fields {
                push_if_match(
                    &field.name,
                    SymbolKind::Field,
                    Some(container.clone()),
                    interner,
                    query,
                    out,
                );
            }
            for m in &s.methods {
                push_if_match(
                    &m.name,
                    SymbolKind::Method,
                    Some(container.clone()),
                    interner,
                    query,
                    out,
                );
            }
        }
        Item::Enum(e) => {
            push_if_match(&e.name, SymbolKind::Enum, None, interner, query, out);
            let container = interner.resolve(&e.name.name).to_string();
            for v in &e.variants {
                push_if_match(
                    &v.name,
                    SymbolKind::EnumMember,
                    Some(container.clone()),
                    interner,
                    query,
                    out,
                );
            }
            for m in &e.methods {
                push_if_match(
                    &m.name,
                    SymbolKind::Method,
                    Some(container.clone()),
                    interner,
                    query,
                    out,
                );
            }
        }
        Item::Interface(i) => {
            push_if_match(&i.name, SymbolKind::Interface, None, interner, query, out);
            let container = interner.resolve(&i.name.name).to_string();
            for sig in &i.methods {
                push_if_match(
                    &sig.name,
                    SymbolKind::Method,
                    Some(container.clone()),
                    interner,
                    query,
                    out,
                );
            }
        }
        Item::Derive(d) => {
            push_if_match(&d.name, SymbolKind::Derive, None, interner, query, out);
            let container = interner.resolve(&d.name.name).to_string();
            for m in &d.methods {
                push_if_match(
                    &m.name,
                    SymbolKind::Method,
                    Some(container.clone()),
                    interner,
                    query,
                    out,
                );
            }
        }
        Item::Const(c) => {
            push_if_match(&c.name, SymbolKind::Constant, None, interner, query, out);
        }
        Item::LinkExtern(b) => {
            for ext in &b.items {
                push_if_match(&ext.name, SymbolKind::Function, None, interner, query, out);
            }
        }
        Item::Error(_) => {}
    }
}

fn push_if_match(
    ident: &Ident,
    kind: SymbolKind,
    container: Option<String>,
    interner: &ThreadedRodeo,
    query: &str,
    out: &mut Vec<WorkspaceSymbol>,
) {
    let name = interner.resolve(&ident.name);
    if !query.is_empty() && !name.to_lowercase().contains(query) {
        return;
    }
    out.push(WorkspaceSymbol {
        name: name.to_string(),
        kind,
        span: ident.span,
        container,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_compiler::{FileId, PreviewFeatures, SourceFile, merge_symbols, parse_all_files_with_preview};

    fn parse(source: &str) -> (Ast, ThreadedRodeo) {
        let sources = vec![SourceFile::new("main.gruel", source, FileId::new(1))];
        let parsed = parse_all_files_with_preview(&sources, &PreviewFeatures::default()).unwrap();
        let merged = merge_symbols(parsed).unwrap();
        (merged.ast, merged.interner)
    }

    #[test]
    fn all_top_level_items() {
        let src = "fn foo() -> i32 { 0 }\nstruct Bar { x: i32 }\nconst N: i32 = 1;";
        let (ast, interner) = parse(src);
        let syms = workspace_symbols(&ast, &interner, "");
        let names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"N"));
        assert!(names.contains(&"x"));
    }

    #[test]
    fn filter_by_substring() {
        let src = "fn foo() -> i32 { 0 }\nstruct Bar { x: i32 }";
        let (ast, interner) = parse(src);
        let syms = workspace_symbols(&ast, &interner, "bar");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Bar");
    }
}
