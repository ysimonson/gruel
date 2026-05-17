//! ADR-0089: gruel's documentation surface.
//!
//! Inputs: a merged `Ast` + the `ThreadedRodeo` used to intern its
//! identifiers. Outputs: a `DocSite` — one file per input file, each
//! containing rendered per-item pages in either Markdown or HTML.

use gruel_parser::ast::{Ast, Doc, EnumDecl, Item, LinkExternBlock, StructDecl};
use lasso::ThreadedRodeo;

pub mod markdown;
pub use markdown::render_markdown;

pub mod html;
pub use html::render_html;

pub mod links;
pub use links::LinkTable;

/// A single file's rendered documentation.
#[derive(Debug, Clone)]
pub struct DocFile {
    /// Source-file stem used as the directory name in the output
    /// (e.g. `"math"` for `std/math.gruel`).
    pub stem: String,
    /// Optional module-level docstring.
    pub module_doc: Option<Doc>,
    /// One entry per top-level item in declaration order.
    pub items: Vec<DocItem>,
}

/// One renderable item with the URL stem we use to address it.
#[derive(Debug, Clone)]
pub struct DocItem {
    /// File-name base for this item, e.g. `"fn.foo"`, `"struct.Bar"`.
    pub slug: String,
    /// Display name (`"foo"`, `"Bar"`, etc.).
    pub name: String,
    /// What kind of item this is (purely cosmetic in the rendered page).
    pub kind: ItemKind,
    /// The item's own docstring, if any.
    pub doc: Option<Doc>,
    /// Type-specific extra info rendered into the page body.
    pub detail: ItemDetail,
}

/// Top-level item kind (used in the rendered headers).
#[derive(Debug, Clone, Copy)]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Interface,
    Derive,
    Const,
    LinkExtern,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Function => "fn",
            ItemKind::Struct => "struct",
            ItemKind::Enum => "enum",
            ItemKind::Interface => "interface",
            ItemKind::Derive => "derive",
            ItemKind::Const => "const",
            ItemKind::LinkExtern => "link_extern",
        }
    }
}

/// Type-specific information used to render a richer page than just
/// `# name + docstring`. Anything not modelled here falls back to
/// the bare `name + doc` rendering.
#[derive(Debug, Clone, Default)]
pub struct ItemDetail {
    /// Public fields (struct/enum struct-variants).
    pub fields: Vec<NamedDoc>,
    /// Enum variants.
    pub variants: Vec<NamedDoc>,
    /// Methods on a struct/enum/derive.
    pub methods: Vec<NamedDoc>,
    /// Extern fn declarations inside a `link_extern { }` block.
    pub extern_fns: Vec<NamedDoc>,
}

/// A name + (optional) doc pair, used for nested items.
#[derive(Debug, Clone)]
pub struct NamedDoc {
    pub name: String,
    pub doc: Option<Doc>,
}

/// A renderable site composed of one entry per source file.
#[derive(Debug, Clone, Default)]
pub struct DocSite {
    pub files: Vec<DocFile>,
}

impl DocSite {
    /// Build a `DocSite` from a single `Ast` + interner pair.
    ///
    /// `stem` is the file's display name (typically the source path's
    /// stem). Anonymous types and items lacking docs still appear in
    /// the output — the renderer just shows their header.
    pub fn from_ast(stem: impl Into<String>, ast: &Ast, interner: &ThreadedRodeo) -> DocFile {
        let mut items = Vec::new();
        for item in &ast.items {
            if let Some(doc_item) = item_to_doc_item(item, interner) {
                items.push(doc_item);
            }
        }
        DocFile {
            stem: stem.into(),
            module_doc: ast.module_doc.clone(),
            items,
        }
    }

    /// Add a `DocFile` produced by `from_ast` to this site.
    pub fn push(&mut self, file: DocFile) {
        self.files.push(file);
    }

    /// ADR-0089 Phase 5: build a `LinkTable` covering every top-level
    /// item in the site, with each slug prefixed by the containing file's
    /// stem. Right for the site-level index page where every link is one
    /// directory away.
    pub fn link_table(&self) -> LinkTable {
        let mut table = LinkTable::new();
        for file in &self.files {
            for item in &file.items {
                let slug_with_dir = format!("{}/{}", file.stem, item.slug);
                table.insert(&item.name, item.kind.label(), &slug_with_dir);
            }
        }
        table
    }
}

impl DocFile {
    /// ADR-0089 Phase 5: build a `LinkTable` for cross-references from
    /// inside this file. Items in the same file are siblings, so their
    /// slugs are unprefixed.
    pub fn link_table(&self) -> LinkTable {
        let mut table = LinkTable::new();
        for item in &self.items {
            table.insert(&item.name, item.kind.label(), &item.slug);
        }
        table
    }
}

fn item_to_doc_item(item: &Item, interner: &ThreadedRodeo) -> Option<DocItem> {
    match item {
        Item::Function(f) => Some(DocItem {
            slug: format!("fn.{}", interner.resolve(&f.name.name)),
            name: interner.resolve(&f.name.name).to_string(),
            kind: ItemKind::Function,
            doc: f.doc.clone(),
            detail: ItemDetail::default(),
        }),
        Item::Struct(s) => Some(struct_doc_item(s, interner)),
        Item::Enum(e) => Some(enum_doc_item(e, interner)),
        Item::Interface(i) => Some(DocItem {
            slug: format!("interface.{}", interner.resolve(&i.name.name)),
            name: interner.resolve(&i.name.name).to_string(),
            kind: ItemKind::Interface,
            doc: i.doc.clone(),
            detail: ItemDetail {
                methods: i
                    .methods
                    .iter()
                    .map(|m| NamedDoc {
                        name: interner.resolve(&m.name.name).to_string(),
                        doc: m.doc.clone(),
                    })
                    .collect(),
                ..ItemDetail::default()
            },
        }),
        Item::Derive(d) => Some(DocItem {
            slug: format!("derive.{}", interner.resolve(&d.name.name)),
            name: interner.resolve(&d.name.name).to_string(),
            kind: ItemKind::Derive,
            doc: d.doc.clone(),
            detail: ItemDetail {
                methods: d
                    .methods
                    .iter()
                    .map(|m| NamedDoc {
                        name: interner.resolve(&m.name.name).to_string(),
                        doc: m.doc.clone(),
                    })
                    .collect(),
                ..ItemDetail::default()
            },
        }),
        Item::Const(c) => Some(DocItem {
            slug: format!("const.{}", interner.resolve(&c.name.name)),
            name: interner.resolve(&c.name.name).to_string(),
            kind: ItemKind::Const,
            doc: c.doc.clone(),
            detail: ItemDetail::default(),
        }),
        Item::LinkExtern(b) => Some(link_extern_doc_item(b, interner)),
        Item::Error(_) => None,
    }
}

fn struct_doc_item(s: &StructDecl, interner: &ThreadedRodeo) -> DocItem {
    DocItem {
        slug: format!("struct.{}", interner.resolve(&s.name.name)),
        name: interner.resolve(&s.name.name).to_string(),
        kind: ItemKind::Struct,
        doc: s.doc.clone(),
        detail: ItemDetail {
            fields: s
                .fields
                .iter()
                .map(|f| NamedDoc {
                    name: interner.resolve(&f.name.name).to_string(),
                    doc: f.doc.clone(),
                })
                .collect(),
            methods: s
                .methods
                .iter()
                .map(|m| NamedDoc {
                    name: interner.resolve(&m.name.name).to_string(),
                    doc: m.doc.clone(),
                })
                .collect(),
            ..ItemDetail::default()
        },
    }
}

fn enum_doc_item(e: &EnumDecl, interner: &ThreadedRodeo) -> DocItem {
    DocItem {
        slug: format!("enum.{}", interner.resolve(&e.name.name)),
        name: interner.resolve(&e.name.name).to_string(),
        kind: ItemKind::Enum,
        doc: e.doc.clone(),
        detail: ItemDetail {
            variants: e
                .variants
                .iter()
                .map(|v| NamedDoc {
                    name: interner.resolve(&v.name.name).to_string(),
                    doc: v.doc.clone(),
                })
                .collect(),
            methods: e
                .methods
                .iter()
                .map(|m| NamedDoc {
                    name: interner.resolve(&m.name.name).to_string(),
                    doc: m.doc.clone(),
                })
                .collect(),
            ..ItemDetail::default()
        },
    }
}

fn link_extern_doc_item(b: &LinkExternBlock, interner: &ThreadedRodeo) -> DocItem {
    let name = interner.resolve(&b.library.value).to_string();
    DocItem {
        slug: format!("link_extern.{}", &name),
        name,
        kind: ItemKind::LinkExtern,
        doc: b.doc.clone(),
        detail: ItemDetail {
            extern_fns: b
                .items
                .iter()
                .map(|f| NamedDoc {
                    name: interner.resolve(&f.name.name).to_string(),
                    doc: f.doc.clone(),
                })
                .collect(),
            ..ItemDetail::default()
        },
    }
}
