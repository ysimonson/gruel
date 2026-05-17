//! ADR-0089 Phase 4: HTML rendering via pulldown-cmark.
//!
//! The flow is: we already have Markdown for each page (from
//! `markdown::render_markdown`), so HTML rendering is "feed it through
//! pulldown-cmark and wrap the result in a minimal `<html>` skeleton".
//! GFM extensions (tables, footnotes, strikethrough, task lists) are
//! enabled — without these the output looks noticeably worse than what
//! users expect from `cargo doc`.

use pulldown_cmark::{html, Options, Parser};

use crate::links::LinkTable;
use crate::{DocFile, DocItem};

const STYLE: &str = include_str!("style.css");

/// Render a single item page as a standalone HTML document.
///
/// `siblings` is a list of `(slug, name)` pairs for the sidebar — pass an
/// empty slice for no sidebar. `index_link` controls the "back to file
/// index" link target at the top of the page.
pub fn render_html(
    item: &DocItem,
    file_stem: &str,
    siblings: &[(String, String)],
    index_link: &str,
) -> String {
    render_html_with(item, file_stem, siblings, index_link, &LinkTable::new())
}

/// ADR-0089 Phase 5: render a single item page, rewriting intra-doc
/// links against `table`. When called from the site driver, `table` is
/// usually `DocSite::link_table()`.
pub fn render_html_with(
    item: &DocItem,
    file_stem: &str,
    siblings: &[(String, String)],
    index_link: &str,
    table: &LinkTable,
) -> String {
    let markdown = crate::markdown::render_markdown_with(item, table);
    let body_html = markdown_to_html(&markdown);
    wrap(
        &format!("{} — {}", item.kind.label(), item.name),
        file_stem,
        siblings,
        index_link,
        &body_html,
    )
}

/// Render the per-file index page (`<file>/index.html`).
pub fn render_index_html(file: &DocFile) -> String {
    render_index_html_with(file, &LinkTable::new())
}

/// Render the per-file index page with intra-doc link rewriting.
pub fn render_index_html_with(file: &DocFile, table: &LinkTable) -> String {
    let markdown = crate::markdown::render_index_with(file, table);
    let body_html = markdown_to_html(&markdown);
    let siblings: Vec<(String, String)> = file
        .items
        .iter()
        .map(|i| (i.slug.clone(), format!("{} {}", i.kind.label(), i.name)))
        .collect();
    wrap(
        &file.stem,
        &file.stem,
        &siblings,
        "../index.html",
        &body_html,
    )
}

/// Render the top-level site index listing every file.
pub fn render_site_index_html(files: &[DocFile]) -> String {
    let mut body = String::from("<h1>Documentation</h1>\n<ul>\n");
    for f in files {
        body.push_str(&format!(
            "  <li><a href=\"{stem}/index.html\">{stem}</a></li>\n",
            stem = escape_html(&f.stem),
        ));
    }
    body.push_str("</ul>\n");
    wrap("Documentation", "", &[], "", &body)
}

fn markdown_to_html(md: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, options);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

fn wrap(
    title: &str,
    file_stem: &str,
    siblings: &[(String, String)],
    index_link: &str,
    body: &str,
) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("  <meta charset=\"utf-8\">\n");
    out.push_str(&format!("  <title>{}</title>\n", escape_html(title)));
    out.push_str("  <style>\n");
    out.push_str(STYLE);
    out.push_str("  </style>\n</head>\n<body>\n");
    out.push_str("<div class=\"layout\">\n");

    if !siblings.is_empty() {
        out.push_str("<nav class=\"sidebar\">\n");
        if !file_stem.is_empty() {
            out.push_str(&format!(
                "  <h2 class=\"sidebar-title\"><a href=\"{}\">{}</a></h2>\n",
                escape_html(index_link),
                escape_html(file_stem),
            ));
        }
        out.push_str("  <ul>\n");
        for (slug, name) in siblings {
            out.push_str(&format!(
                "    <li><a href=\"{slug}.html\">{name}</a></li>\n",
                slug = escape_html(slug),
                name = escape_html(name),
            ));
        }
        out.push_str("  </ul>\n</nav>\n");
    }

    out.push_str("<main class=\"content\">\n");
    out.push_str(body);
    out.push_str("</main>\n");
    out.push_str("</div>\n</body>\n</html>\n");
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocItem, ItemDetail, ItemKind};
    use gruel_parser::ast::Doc;
    use gruel_util::Span;

    fn make_doc(body: &str) -> Doc {
        Doc {
            body: body.to_string(),
            span: Span::default(),
        }
    }

    #[test]
    fn html_includes_title_and_body() {
        let item = DocItem {
            slug: "fn.foo".into(),
            name: "foo".into(),
            kind: ItemKind::Function,
            doc: Some(make_doc("Does **the** foo.")),
            detail: ItemDetail::default(),
        };
        let html = render_html(&item, "lib", &[], "../index.html");
        assert!(html.contains("<title>fn — foo</title>"));
        // pulldown-cmark turns `**the**` into `<strong>the</strong>`.
        assert!(html.contains("<strong>the</strong>"));
    }

    #[test]
    fn html_escapes_specials_in_title_attrs() {
        // Item name is HTML-escaped in the <title> tag and other
        // attribute-like positions so unusual identifiers don't break
        // the page chrome. (pulldown-cmark itself passes through inline
        // HTML in markdown bodies on purpose — that's its documented
        // behavior, and docstrings come from trusted source code.)
        let item = DocItem {
            slug: "fn.<weird>".into(),
            name: "<weird>".into(),
            kind: ItemKind::Function,
            doc: None,
            detail: ItemDetail::default(),
        };
        let html = render_html(
            &item,
            "lib",
            &[("fn.<weird>".into(), "fn <weird>".into())],
            "../index.html",
        );
        assert!(html.contains("&lt;weird&gt;"));
    }

    #[test]
    fn html_renders_table_from_gfm_extension() {
        let item = DocItem {
            slug: "fn.foo".into(),
            name: "foo".into(),
            kind: ItemKind::Function,
            doc: Some(make_doc("| a | b |\n|---|---|\n| 1 | 2 |")),
            detail: ItemDetail::default(),
        };
        let html = render_html(&item, "lib", &[], "../index.html");
        assert!(html.contains("<table>"));
    }
}
