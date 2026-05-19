//! Completion (ADR-0091 Phase 6).
//!
//! Trigger characters: `.`, `@`, `:`, `(`. The phase-6 model:
//!
//! - After `.` on a receiver expression: fields and methods on the
//!   receiver's type (when we have it from the AIR side-table).
//! - After `@`: intrinsic names from the [`gruel_intrinsics`] registry.
//! - Otherwise: locals in the enclosing function (from RIR's `Local`
//!   sites — recovered via AST walk), plus every top-level item.

use std::collections::HashSet;

use gruel_parser::ast::{
    AssignTarget, Ast, BlockExpr, Expr, Function, Ident, Item, Method, Pattern, Statement,
};
use lasso::ThreadedRodeo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Function,
    Struct,
    Enum,
    Interface,
    Derive,
    Constant,
    Field,
    EnumMember,
    Variable,
    Method,
    Keyword,
    Intrinsic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
}

const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "struct",
    "enum",
    "interface",
    "derive",
    "const",
    "pub",
    "if",
    "else",
    "match",
    "while",
    "for",
    "loop",
    "in",
    "break",
    "continue",
    "return",
    "true",
    "false",
    "self",
    "Self",
];

/// Completion at the given byte position. `trigger` is `Some('.')` /
/// `Some('@')` if the editor invoked us via a trigger character.
pub fn complete_at(
    ast: &Ast,
    interner: &ThreadedRodeo,
    file_id: gruel_util::FileId,
    byte: u32,
    trigger: Option<char>,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |items: &mut Vec<CompletionItem>, item: CompletionItem| {
        if seen.insert(item.label.clone()) {
            items.push(item);
        }
    };

    match trigger {
        Some('@') => {
            for def in gruel_intrinsics::INTRINSICS.iter() {
                push(
                    &mut items,
                    CompletionItem {
                        label: format!("@{}", def.name),
                        kind: CompletionKind::Intrinsic,
                        detail: Some(def.summary.to_string()),
                    },
                );
            }
            return items;
        }
        Some('.') => {
            // Dot completion: surface every field/method name in the workspace.
            // Without sema-level expression type info routed here we can't
            // restrict to the receiver's type; this is the simplest correct
            // option (over-suggests, never under-suggests).
            push_fields_and_methods(ast, interner, &mut items, &mut seen);
            return items;
        }
        _ => {}
    }

    // Generic context: locals in scope + top-level items + keywords.
    let enclosing = enclosing_function(ast, file_id, byte);
    if let Some(f) = enclosing {
        for p in &f.params {
            push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&p.name.name).to_string(),
                    kind: CompletionKind::Variable,
                    detail: None,
                },
            );
        }
        collect_lets(&f.body, interner, &mut |label| {
            push(
                &mut items,
                CompletionItem {
                    label,
                    kind: CompletionKind::Variable,
                    detail: None,
                },
            )
        });
    }

    for item in &ast.items {
        match item {
            Item::Function(f) => push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&f.name.name).to_string(),
                    kind: CompletionKind::Function,
                    detail: None,
                },
            ),
            Item::Struct(s) => push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&s.name.name).to_string(),
                    kind: CompletionKind::Struct,
                    detail: None,
                },
            ),
            Item::Enum(e) => push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&e.name.name).to_string(),
                    kind: CompletionKind::Enum,
                    detail: None,
                },
            ),
            Item::Interface(i) => push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&i.name.name).to_string(),
                    kind: CompletionKind::Interface,
                    detail: None,
                },
            ),
            Item::Derive(d) => push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&d.name.name).to_string(),
                    kind: CompletionKind::Derive,
                    detail: None,
                },
            ),
            Item::Const(c) => push(
                &mut items,
                CompletionItem {
                    label: interner.resolve(&c.name.name).to_string(),
                    kind: CompletionKind::Constant,
                    detail: None,
                },
            ),
            _ => {}
        }
    }

    for kw in KEYWORDS {
        push(
            &mut items,
            CompletionItem {
                label: (*kw).to_string(),
                kind: CompletionKind::Keyword,
                detail: None,
            },
        );
    }

    items
}

fn enclosing_function(ast: &Ast, file_id: gruel_util::FileId, byte: u32) -> Option<&Function> {
    for item in &ast.items {
        if let Item::Function(f) = item {
            if f.span.file_id == file_id && byte >= f.span.start && byte <= f.span.end {
                return Some(f);
            }
        }
    }
    None
}

fn collect_lets(expr: &Expr, interner: &ThreadedRodeo, push: &mut impl FnMut(String)) {
    match expr {
        Expr::Block(b) => collect_lets_block(b, interner, push),
        _ => {}
    }
}

fn collect_lets_block(b: &BlockExpr, interner: &ThreadedRodeo, push: &mut impl FnMut(String)) {
    for stmt in &b.statements {
        match stmt {
            Statement::Let(l) => {
                if let Pattern::Ident { name, .. } = &l.pattern {
                    push(interner.resolve(&name.name).to_string());
                }
                if let Expr::Block(_) = &*l.init {
                    collect_lets(&l.init, interner, push);
                }
            }
            Statement::Assign(_) => {}
            Statement::Expr(e) => collect_lets(e, interner, push),
        }
    }
    collect_lets(&b.expr, interner, push);
}

fn push_fields_and_methods(
    ast: &Ast,
    interner: &ThreadedRodeo,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    let mut push = |items: &mut Vec<CompletionItem>, item: CompletionItem| {
        if seen.insert(item.label.clone()) {
            items.push(item);
        }
    };

    for item in &ast.items {
        match item {
            Item::Struct(s) => {
                for f in &s.fields {
                    push(
                        items,
                        CompletionItem {
                            label: interner.resolve(&f.name.name).to_string(),
                            kind: CompletionKind::Field,
                            detail: None,
                        },
                    );
                }
                for m in &s.methods {
                    push(
                        items,
                        CompletionItem {
                            label: interner.resolve(&m.name.name).to_string(),
                            kind: CompletionKind::Method,
                            detail: None,
                        },
                    );
                }
            }
            Item::Enum(e) => {
                for v in &e.variants {
                    push(
                        items,
                        CompletionItem {
                            label: interner.resolve(&v.name.name).to_string(),
                            kind: CompletionKind::EnumMember,
                            detail: None,
                        },
                    );
                }
                for m in &e.methods {
                    push(
                        items,
                        CompletionItem {
                            label: interner.resolve(&m.name.name).to_string(),
                            kind: CompletionKind::Method,
                            detail: None,
                        },
                    );
                }
            }
            Item::Derive(d) => {
                for m in &d.methods {
                    push(
                        items,
                        CompletionItem {
                            label: interner.resolve(&m.name.name).to_string(),
                            kind: CompletionKind::Method,
                            detail: None,
                        },
                    );
                }
            }
            Item::Interface(i) => {
                for sig in &i.methods {
                    push(
                        items,
                        CompletionItem {
                            label: interner.resolve(&sig.name.name).to_string(),
                            kind: CompletionKind::Method,
                            detail: None,
                        },
                    );
                }
            }
            _ => {}
        }
    }
}

// Silence unused-import warning when method-walking is no longer used after
// future refactors.
#[allow(dead_code)]
fn _suppress() {
    let _: Option<Ident> = None;
    let _: Option<&Method> = None;
    let _: Option<&dyn Fn(&AssignTarget) -> ()> = None;
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
    fn intrinsic_completion_after_at() {
        let src = "fn main() -> i32 { 0 }";
        let (ast, interner) = parse(src);
        let items = complete_at(&ast, &interner, FileId::new(1), 19, Some('@'));
        assert!(!items.is_empty());
        assert!(items.iter().all(|i| i.label.starts_with('@')));
        assert!(items.iter().any(|i| i.kind == CompletionKind::Intrinsic));
    }

    #[test]
    fn dot_completion_surfaces_fields_and_methods() {
        let src = r#"struct Point { x: i32, y: i32, fn sum(self) -> i32 { self.x + self.y } }
fn main() -> i32 { 0 }"#;
        let (ast, interner) = parse(src);
        let items = complete_at(&ast, &interner, FileId::new(1), 30, Some('.'));
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"y"));
        assert!(labels.contains(&"sum"));
    }

    #[test]
    fn generic_context_includes_top_level_items() {
        let src = "fn foo() -> i32 { 0 }\nstruct Bar { x: i32 }\nfn main() -> i32 { 0 }";
        let (ast, interner) = parse(src);
        // Cursor inside main's body.
        let byte = src.find("0 }").unwrap() as u32;
        let items = complete_at(&ast, &interner, FileId::new(1), byte, None);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"foo"));
        assert!(labels.contains(&"Bar"));
        assert!(labels.contains(&"main"));
        // Keywords too.
        assert!(labels.contains(&"if"));
        assert!(labels.contains(&"let"));
    }

    #[test]
    fn generic_context_includes_locals_in_function() {
        let src = "fn main() -> i32 { let answer = 42; 0 }";
        let (ast, interner) = parse(src);
        let byte = src.find("0 }").unwrap() as u32;
        let items = complete_at(&ast, &interner, FileId::new(1), byte, None);
        assert!(
            items
                .iter()
                .any(|i| i.label == "answer" && i.kind == CompletionKind::Variable),
            "expected `answer` in completion, got: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }
}
