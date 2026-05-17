//! ADR-0089 Phase 5: intra-doc link rewriting.
//!
//! Rewrites bare reference-style links (`[Name]`, `[Name::method]`,
//! `[fn name]`, `[struct Name]`, `[enum Name]`, `[interface Name]`,
//! `[derive Name]`, `[const Name]`) in a doc body into ordinary
//! Markdown links pointing at the rendered page for the named item.
//!
//! Anything that doesn't resolve to a known item is left alone, exactly
//! as rustdoc does. This is a single pre-render pass — the doc bodies
//! themselves are immutable inputs from the AST.

use std::collections::HashMap;

/// A name-resolution table: identifier → (kind label, slug).
///
/// `kind` is one of `"fn" | "struct" | "enum" | "interface" | "derive"
/// | "const"`. The kind disambiguates `[fn foo]` from a hypothetical
/// `[struct foo]` (Gruel disallows shadowing across kinds today, but
/// the lookup table tolerates both spellings).
#[derive(Debug, Default, Clone)]
pub struct LinkTable {
    by_name: HashMap<String, (String, String)>,
}

impl LinkTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a single item; later entries with the same name overwrite
    /// earlier ones, matching the parser's "last-defined wins" semantics.
    pub fn insert(&mut self, name: &str, kind: &str, slug: &str) {
        self.by_name
            .insert(name.to_string(), (kind.to_string(), slug.to_string()));
    }

    fn lookup(&self, query: &str) -> Option<&(String, String)> {
        // Strip optional "kind " prefix (e.g. `fn foo` → `foo`).
        let trimmed = match query.split_once(' ') {
            Some((_kind, name)) if is_known_kind(_kind) => name.trim(),
            _ => query.trim(),
        };
        // Strip method suffix: `Name::method` → `Name` (we link to the
        // type page; per-method anchors are out of scope for MVP).
        let trimmed = match trimmed.split_once("::") {
            Some((parent, _method)) => parent,
            None => trimmed,
        };
        self.by_name.get(trimmed)
    }
}

fn is_known_kind(s: &str) -> bool {
    matches!(
        s,
        "fn" | "struct" | "enum" | "interface" | "derive" | "const" | "link_extern"
    )
}

/// Rewrite intra-doc references in a markdown body.
///
/// `extension` is the file extension to append to the slug — `".md"`
/// for the Markdown renderer, `".html"` for the HTML renderer.
pub fn rewrite(body: &str, table: &LinkTable, extension: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end_rel) = find_balanced_bracket(&bytes[i + 1..]) {
                let end = i + 1 + end_rel;
                let inner = &body[i + 1..end];
                // Reference-style links: skip if this is `[label][ref]`
                // or `[label](url)` (already a real link). We only
                // rewrite the bare `[Name]` shortcut form.
                let after = bytes.get(end + 1).copied();
                if after == Some(b'(') || after == Some(b'[') {
                    out.push_str(&body[i..=end]);
                    i = end + 1;
                    continue;
                }
                if let Some((_, slug)) = table.lookup(inner) {
                    out.push('[');
                    out.push_str(inner);
                    out.push_str("](");
                    out.push_str(slug);
                    out.push_str(extension);
                    out.push(')');
                    i = end + 1;
                    continue;
                }
            }
            out.push('[');
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Find the relative position of the closing `]` for a `[…]` whose
/// opening `[` was just consumed. Returns `None` if the brackets are
/// unbalanced or contain newlines (we never rewrite multi-line links).
fn find_balanced_bracket(rest: &[u8]) -> Option<usize> {
    let mut depth: i32 = 0;
    for (i, b) in rest.iter().enumerate() {
        match b {
            b'\n' => return None,
            b'[' => depth += 1,
            b']' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with(name: &str, kind: &str, slug: &str) -> LinkTable {
        let mut t = LinkTable::new();
        t.insert(name, kind, slug);
        t
    }

    #[test]
    fn rewrites_bare_name() {
        let t = table_with("foo", "fn", "fn.foo");
        let out = rewrite("see [foo] for details", &t, ".html");
        assert_eq!(out, "see [foo](fn.foo.html) for details");
    }

    #[test]
    fn rewrites_kind_prefix() {
        let t = table_with("foo", "fn", "fn.foo");
        let out = rewrite("call [fn foo]", &t, ".html");
        assert_eq!(out, "call [fn foo](fn.foo.html)");
    }

    #[test]
    fn rewrites_method_to_parent() {
        let t = table_with("Vec", "struct", "struct.Vec");
        let out = rewrite("see [Vec::push]", &t, ".html");
        assert_eq!(out, "see [Vec::push](struct.Vec.html)");
    }

    #[test]
    fn leaves_unknown_alone() {
        let t = LinkTable::new();
        let out = rewrite("see [bar] for details", &t, ".html");
        assert_eq!(out, "see [bar] for details");
    }

    #[test]
    fn leaves_explicit_links_alone() {
        // `[label](url)` and `[label][ref]` are real links — don't
        // touch them.
        let t = table_with("foo", "fn", "fn.foo");
        let body = "see [foo](other.html) and [foo][ref] and [foo] last";
        let out = rewrite(body, &t, ".html");
        assert_eq!(
            out,
            "see [foo](other.html) and [foo][ref] and [foo](fn.foo.html) last"
        );
    }

    #[test]
    fn extension_swaps_md_and_html() {
        let t = table_with("foo", "fn", "fn.foo");
        assert_eq!(rewrite("[foo]", &t, ".md"), "[foo](fn.foo.md)");
        assert_eq!(rewrite("[foo]", &t, ".html"), "[foo](fn.foo.html)");
    }

    #[test]
    fn no_rewrite_across_newlines() {
        let t = table_with("foo", "fn", "fn.foo");
        // `[foo]` only — but the opening `[` is followed by content
        // containing a newline before `]`. Treat it as not-a-link.
        let body = "weird [foo\n] nope";
        let out = rewrite(body, &t, ".html");
        assert_eq!(out, "weird [foo\n] nope");
    }
}
