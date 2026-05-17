//! ADR-0089 Phase 3: Markdown rendering.

use std::fmt::Write;

use crate::{DocFile, DocItem, ItemKind, NamedDoc};

/// Render the per-file index page (`<file>/index.md`) listing every
/// item with a one-line lead pulled from the first line of its docs.
pub fn render_index(file: &DocFile) -> String {
    let mut out = String::new();
    write!(out, "# {}\n\n", file.stem).unwrap();
    if let Some(module_doc) = &file.module_doc {
        out.push_str(&module_doc.body);
        out.push_str("\n\n");
    }
    if file.items.is_empty() {
        return out;
    }
    out.push_str("## Items\n\n");
    for item in &file.items {
        write!(out, "- `{} {}`", item.kind.label(), item.name).unwrap();
        if let Some(summary) = first_line(&item.doc) {
            write!(out, " — {}", summary).unwrap();
        }
        out.push('\n');
    }
    out
}

/// Render a single item's Markdown page (`<slug>.md`).
pub fn render_markdown(item: &DocItem) -> String {
    let mut out = String::new();
    write!(out, "# `{} {}`\n\n", item.kind.label(), item.name).unwrap();
    if let Some(doc) = &item.doc {
        out.push_str(&doc.body);
        out.push_str("\n\n");
    }
    render_sections(&mut out, item);
    out
}

fn render_sections(out: &mut String, item: &DocItem) {
    render_named_section(out, "Fields", &item.detail.fields);
    render_named_section(out, "Variants", &item.detail.variants);
    render_named_section(out, "Methods", &item.detail.methods);
    if !item.detail.extern_fns.is_empty() {
        match item.kind {
            ItemKind::LinkExtern => {
                render_named_section(out, "Extern functions", &item.detail.extern_fns)
            }
            _ => render_named_section(out, "Functions", &item.detail.extern_fns),
        }
    }
}

fn render_named_section(out: &mut String, heading: &str, items: &[NamedDoc]) {
    if items.is_empty() {
        return;
    }
    write!(out, "## {}\n\n", heading).unwrap();
    for it in items {
        write!(out, "- `{}`", it.name).unwrap();
        if let Some(summary) = first_line(&it.doc) {
            write!(out, " — {}", summary).unwrap();
        }
        out.push('\n');
    }
    out.push('\n');
}

fn first_line(doc: &Option<gruel_parser::ast::Doc>) -> Option<&str> {
    doc.as_ref().and_then(|d| d.body.lines().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_parser::ast::Doc;
    use gruel_util::Span;

    fn make_doc(body: &str) -> Doc {
        Doc {
            body: body.to_string(),
            span: Span::default(),
        }
    }

    #[test]
    fn render_fn_page() {
        let item = DocItem {
            slug: "fn.foo".into(),
            name: "foo".into(),
            kind: ItemKind::Function,
            doc: Some(make_doc("Does the foo.\n\nMore details.")),
            detail: Default::default(),
        };
        let md = render_markdown(&item);
        assert!(md.contains("# `fn foo`"));
        assert!(md.contains("Does the foo."));
        assert!(md.contains("More details."));
    }

    #[test]
    fn render_index_page() {
        let file = DocFile {
            stem: "math".into(),
            module_doc: Some(make_doc("Module-level docs.")),
            items: vec![
                DocItem {
                    slug: "fn.add".into(),
                    name: "add".into(),
                    kind: ItemKind::Function,
                    doc: Some(make_doc("Adds two ints.")),
                    detail: Default::default(),
                },
                DocItem {
                    slug: "fn.sub".into(),
                    name: "sub".into(),
                    kind: ItemKind::Function,
                    doc: None,
                    detail: Default::default(),
                },
            ],
        };
        let md = render_index(&file);
        assert!(md.contains("# math"));
        assert!(md.contains("Module-level docs."));
        assert!(md.contains("- `fn add` — Adds two ints."));
        assert!(md.contains("- `fn sub`"));
    }

    #[test]
    fn struct_page_lists_fields() {
        let item = DocItem {
            slug: "struct.Point".into(),
            name: "Point".into(),
            kind: ItemKind::Struct,
            doc: Some(make_doc("A 2D point.")),
            detail: crate::ItemDetail {
                fields: vec![
                    NamedDoc {
                        name: "x".into(),
                        doc: Some(make_doc("x coordinate")),
                    },
                    NamedDoc {
                        name: "y".into(),
                        doc: None,
                    },
                ],
                ..Default::default()
            },
        };
        let md = render_markdown(&item);
        assert!(md.contains("## Fields"));
        assert!(md.contains("- `x` — x coordinate"));
        assert!(md.contains("- `y`\n"));
    }
}
