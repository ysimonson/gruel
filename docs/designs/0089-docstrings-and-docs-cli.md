---
id: 0089
title: Docstrings and a doc-dump CLI
status: proposal
tags: [docs, lexer, parser, ast, cli, tooling]
feature-flag: docs
created: 2026-05-16
accepted:
implemented:
spec-sections: ["2.2", "6.1"]
superseded-by:
---

# ADR-0089: Docstrings and a doc-dump CLI

## Status

Proposal

## Summary

Add Rust-style line doc comments (`///` only — no `//!` inner form)
to the lexer and surface them as a `Doc` field on every nameable AST
item. Module (file) docs are determined by position: the file's first
docstring block attaches to the module **iff** no item appears above
it *and* a blank line separates it from the next item below. Every
other docstring block must be immediately followed (no blank line)
by an item. Introduce a new compiler mode — `--doc` — that walks the
AST and writes either Markdown or HTML to a chosen output directory,
modeled on `cargo doc`. Markdown rendering is in-tree (free); HTML
rendering is done via [`pulldown-cmark`][pcm] (MIT, the
rustdoc/mdBook/Zola parser) wrapped in hand-written HTML templates.
No template engine or syntax highlighter is taken on as a dependency
in the MVP — both can be added later if they earn their weight.

[pcm]: https://github.com/pulldown-cmark/pulldown-cmark

## Context

Gruel has no docstring concept today. The lexer unconditionally skips
everything matching `//[^\n]*` (see `crates/gruel-lexer/src/logos_lexer.rs:282`),
which silently drops any `///` content. Spec section 2.2 codifies
this: "Comments are discarded during lexical analysis and do not affect
program semantics." There is no place to attach documentation, no
tooling to render it, and no spec hook anywhere in the language.

Several adjacent ADRs assume some form of human-facing description
exists or will exist:

- ADR-0050 (intrinsics crate) ships generated `intrinsics-reference.md`
  by reading hand-coded `summary`/`description`/`examples` strings out
  of `INTRINSICS`. That registry's doc strings already prove the rendering
  problem is small.
- ADR-0026 (module system) calls files struct-shaped modules; users
  will reasonably want to render docs at the module (file) level.
- ADR-0085/0086 (FFI blocks) attach human-relevant link-name and
  library context to extern fns that today has no structured home
  outside ADR text.

We want a documentation surface that is:

1. **Familiar.** Rust-style `///` / `//!` is what users will already
   reach for; matching that costs essentially nothing.
2. **Attached to the AST.** Downstream tooling — not just our doc dumper
   — can read it. Examples we explicitly want to enable later: LSP
   hover, error messages quoting the docstring on the failing call, the
   spec/website pulling stdlib docs from source.
3. **Renderable to both Markdown and HTML.** Markdown is the lingua
   franca our spec already uses (via Zola). HTML is what `cargo doc`
   produces and what most users expect from `--doc`.
4. **Cheap to build.** No giant rustdoc-style frontend rewrite. Most of
   that work — markdown → HTML, code highlighting, sidebars — is either
   ecosystem-provided or post-MVP polish.

### Research: existing crates

I evaluated whether to take a heavyweight dependency to skip the
implementation work. Findings:

| Crate | Verdict |
|-------|---------|
| **`pulldown-cmark`** (MIT) | Pull parser for CommonMark + GFM tables/footnotes/strikethrough/task lists. Used by rustdoc, mdBook, Zola, and docs.rs. Low transitive-dep footprint. Stable, actively maintained (0.13.3 in 2026). **Adopting for `markdown → HTML`.** |
| **`comrak`** (BSD-2) | Full AST, GFM-compat, used by crates.io/docs.rs/GitLab. Larger surface and heavier than what we need. Skipping. |
| **`syntect`** (MIT) | Sublime-grammar based code-block highlighter. Ships large grammar/theme data. Useful but not load-bearing for MVP — defer; the website already does highlighting on its end. |
| **`maud`** / **`askama`** / **`minijinja`** | HTML templating engines. Real value only when templates grow large. For a page-per-item dump, hand-written `write!()` is shorter than the template wiring. Skipping. |
| **`rustdoc-json`** / **`manners`** / **`cargo-docs-md`** | Tightly coupled to rustdoc's JSON shape and to `rustc`. Cannot be reused for a non-Rust language. Skipping. |
| **`rustdoc` itself** | Not a library. Tightly coupled to `rustc_*`. Cannot be reused. |

**Net:** the only piece worth pulling in is `pulldown-cmark`. Everything
else — collecting doc text in the lexer, attaching to AST, walking the
AST to lay out pages — is gruel-shaped work that no third-party crate
abstracts. This matches how all language doc generators (rustdoc, Zig's
`zig build-obj -femit-docs`, Hare's `haredoc`, OCaml's `odoc`) are built:
generic markdown library at the bottom, language-specific everything
else on top.

## Decision

### Doc-comment syntax (lexical)

One new token shape, line-based:

| Source | Token |
|--------|-------|
| `/// text` | `LineDoc(text)` |

There is **no** `//!` inner-doc form. Module-level docs are determined
by context, not by a separate token (see "Attachment rule" below).

A run of contiguous `///` lines (no blank or non-doc line between them)
forms a single docstring **block**. A blank source line — including a
line that is only whitespace — terminates the run. `//` line comments
remain unchanged and are still discarded; `////` (four-or-more slashes)
remains a plain comment, matching Rust.

Block doc comments (`/** … */`) are **explicitly out of scope** for
this ADR — gruel has no `/* … */` block comments today, and adding
both forms together is a separable concern.

### Attachment rule

The general rule: **every doc block must be immediately followed
(no blank line between) by an item — that block is that item's doc.**

The single exception is the file's first doc block when *no item
appears above it*:

- **Module-doc qualifier.** A doc block qualifies as the *module
  candidate* iff it is the textually first doc block in the file and
  no item precedes it in the file. (Other doc blocks, plain comments,
  and blank lines do not disqualify it — only an item does.)
- If a qualifying block is **separated by a blank line** from the next
  item, it attaches to the module and is stored on `Ast.module_doc`.
- If a qualifying block is **glued** (no blank line) to the next item,
  it attaches to that item instead — the module gets no doc.
- A doc block that does **not** qualify as the module candidate and is
  not immediately followed by an item is a parse error.

Examples (using `↵` to mark a blank line):

```
/// Module docs.
↵
fn main() {}
```
→ no item above; blank line below ⇒ `Ast.module_doc` = "Module docs."

```
/// Docs for main.
fn main() {}
```
→ no item above; glued below ⇒ `main.doc` = "Docs for main."

```
/// Module docs.
↵
/// Docs for main.
fn main() {}
```
→ first block: no item above, blank line below ⇒ `Ast.module_doc`.
   Second block: glued to `main` ⇒ `main.doc`.

```
fn helper() {}
↵
/// Docs for main.
fn main() {}
```
→ first doc block has an item (`helper`) above it, so it does **not**
  qualify as the module candidate. It's glued to `main` ⇒ `main.doc`.
  Module gets no doc.

```
fn helper() {}
↵
/// Stray.
↵
fn main() {}
```
→ doc block has an item above (not the module candidate) and is not
  glued to the next item ⇒ parse error.

```
/// Stray.
↵
fn helper() {}
↵
/// Docs for main.
fn main() {}
```
→ first block: no item above, blank line below ⇒ `Ast.module_doc`
  = "Stray." Second block: item above (disqualifies module), glued to
  `main` ⇒ `main.doc`.

This trades the symmetry of `///` vs `//!` for one fewer token kind
and one fewer thing to remember. The visual cue (blank line ⇒ module
doc) is the same cue authors already use to separate file-header
prose from the first item.

The text captured by the lexer is the raw content **after** the marker,
with a single leading space removed if present (matching Rust):

```
/// Hello, world.
///
///     code block
```

→ docstring body:

```
Hello, world.

    code block
```

### AST attachment

A new lightweight type on every nameable AST node:

```rust
// in gruel-parser/src/ast.rs

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Doc {
    /// Raw Markdown body (lines joined with '\n'), already with the
    /// `///` marker and the one shared leading space stripped.
    pub body: String,
    /// Span covering the doc-comment block in source.
    pub span: Span,
}
```

`Option<Doc>` is added as the first field of every item-shaped struct
that has a name in source:

- `Function`, `Method`, `MethodSig`, `ExternFn`
- `StructDecl`, `FieldDecl`, `AnonStructField`
- `EnumDecl`, `EnumVariant`, `EnumVariantField`
- `InterfaceDecl`, `DeriveDecl`
- `ConstDecl`, `LinkExternBlock`

The file-level `Ast` gains `module_doc: Option<Doc>` for the
module-level docstring (see "Attachment rule" above).

Anonymous types (`AnonymousStruct`, `AnonymousEnum`,
`AnonymousInterface`) do **not** carry docs in MVP — they have no
addressable name to expose. A `///` block immediately preceding them
inside a comptime-type body is a parse error to keep the attachment
rule uniform.

The lexer emits `LineDoc` tokens with line-resolved spans. The parser
inspects line gaps between consecutive doc tokens (and between the
final doc token and the following item) to decide block boundaries
and module-doc-vs-item-doc per the rule above. Sema and downstream IRs
ignore docs entirely — they pass through unchanged across the
serde-cached `Ast` round-trip (cache invariance check already exists
per ADR-0088 follow-up).

### CLI surface: `--doc`

A new mode flag on `gruel`, paralleling `--cache-stats` / `--cache-clean`
(early-return modes that don't compile/link):

```bash
# Dump markdown docs for one or more source roots
gruel --doc=markdown src/lib.gruel

# Dump HTML docs into target/doc/
gruel --doc=html src/lib.gruel

# Custom output dir
gruel --doc=html --doc-output-dir build/docs src/lib.gruel
```

Concretely, in `crates/gruel/src/main.rs`:

```rust
/// Documentation output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DocFormat { Markdown, Html }

#[arg(long, value_name = "FORMAT", conflicts_with_all = ["emit", "cache_stats", "cache_clean"])]
doc: Option<DocFormat>,

#[arg(long, value_name = "DIR", default_value = "target/doc")]
doc_output_dir: String,
```

When `--doc` is set the driver:

1. Reads + lexes + parses every source file. (Sema is **not** run — we
   want the docs of code that may not yet type-check, and we want
   `--doc` to be fast.)
2. Walks the merged AST and produces one output file per top-level
   item, organized by source file:
   ```
   target/doc/
     index.{md,html}                         ← top-level module index
     <source-basename>/
       index.{md,html}                       ← module_doc + item list for that file
       fn.<name>.{md,html}
       struct.<name>.{md,html}               ← includes nested methods/fields
       enum.<name>.{md,html}                 ← includes variants/methods
       interface.<name>.{md,html}            ← includes method sigs
       derive.<name>.{md,html}
       const.<name>.{md,html}
       link_extern.<library>.{md,html}       ← includes extern fns
   ```
3. Markdown output is just the rendered text. HTML output wraps the
   rendered body in a minimal, opinionated `<html>` skeleton with a
   single embedded CSS file and a sidebar listing siblings. Default
   `<pre><code class="language-gruel">` markup; no syntax highlighting
   in MVP (users plug Prism/highlight.js in or rely on the website's
   pipeline).

### Cargo dependency

```toml
# crates/gruel-doc/Cargo.toml (new crate)
pulldown-cmark = { version = "0.13", default-features = false, features = ["html"] }
```

Default-features off keeps the dependency surface small (no `getopts`
binary harness, no GFM-by-default — we opt in to the extensions we
actually use). Workspace-pin the version in the root `Cargo.toml`.

### New crate: `gruel-doc`

Sits between `gruel-parser` and `gruel` (the binary). Inputs: the
merged `Ast` + `ThreadedRodeo`. Outputs: a `DocSite` value the binary
either writes as Markdown (no dep on pulldown-cmark required) or as
HTML (uses pulldown-cmark to render each doc body).

Splitting this into its own crate keeps the markdown/HTML dependency
off the hot-path compile pipeline; nothing in `gruel-compiler` or any
IR crate depends on `gruel-doc`.

### Preview gating

All of the above ships under a single preview feature `docs`:

- The lexer always recognizes `///` / `//!`. (Recognizing them isn't a
  semantic change — they used to be discarded.)
- Surfacing them onto the AST is gated: if the feature is off, the
  parser drops the doc tokens just like the old skip rule did. This
  keeps the AST stable for downstream stages on day one.
- `--doc` is rejected without `--preview docs`.

Once stabilization happens (Phase 7), the gate is removed and the AST
carries `Option<Doc>` unconditionally.

### Spec updates

- `02-lexical-structure/02-comments.md` (existing): rewrite §2.2:2 to
  scope "discarded" to non-doc comments. Add new normative paragraphs:
  - §2.2:5 — `///` introduces a doc-comment line.
  - §2.2:6 — A run of consecutive `///` lines, terminated by any blank
    line or non-doc token, forms a *doc block*.
  - §2.2:7 — A doc block qualifies as the *module candidate* iff it
    is the textually first doc block in the file and no item appears
    above it. A qualifying block separated by at least one blank line
    from the next item attaches to the module; a qualifying block
    glued to the next item attaches to that item.
  - §2.2:8 — Every doc block that is not the qualifying module
    candidate must be immediately followed by an item; otherwise it
    is a parse error.
  - One `example` paragraph and code sample.
- `06-items/`: add a new informative paragraph at the top of the items
  chapter noting that every item may carry a leading doc comment, and
  that the doc text is not interpreted by the compiler (Markdown
  rendering is a tooling concern).
- Each item type's spec page references the doc-attachment rule by
  paragraph id.
- No change to the grammar appendix beyond defining `doc_line` and
  `doc_block` non-terminals and threading them into `item` / the
  file-level production.

## Implementation Phases

- [x] **Phase 1: Lexer recognizes `///`.** Replace the single
      `#[logos(skip r"//[^\n]*")]` rule with explicit cases that:
      - Match `////…` (four or more slashes) and skip — plain line
        comment, matching Rust.
      - Match `///` followed by optional body → `LineDoc(Spur)`,
        interning the post-`///` text (with at most one leading space
        stripped).
      - Match `// …` and skip (as today).
      Add lexer unit tests for the boundary cases (`///`, `///x`,
      `/// x`, `////`, `///!`).

- [x] **Phase 2: AST changes behind `docs` preview gate.** Add the
      `Doc` type, thread `Option<Doc>` onto every named item struct,
      add `Ast::module_doc`. When `docs` preview is **off** the parser
      eats `LineDoc` tokens and discards them; when on, it accumulates
      contiguous `LineDoc` tokens into a `Doc` block and applies the
      attachment rule: a block qualifies as the module candidate only
      if it is the file's first doc block *and* no item appears above
      it; a qualifying block separated from the next item by a blank
      line becomes `module_doc`; any other block must be glued to a
      following item or it's a parse error. Block boundaries and
      blank-line detection both come from comparing line numbers in
      the token spans — no whitespace-token wrangling required. Add
      parser tests and a `gruel-spec` snapshot test for the AST shape.

- [x] **Phase 3: `gruel-doc` crate + Markdown output.** New crate.
      Define `DocSite` (`files: Vec<DocFile>`, each with rendered
      item-level pages). Implement Markdown rendering using only
      `std::fmt` (no external deps yet). Wire the CLI flag `--doc=markdown`
      and `--doc-output-dir`. Snapshot-test the output for a small
      sample tree in `examples/`.

- [x] **Phase 4: HTML output via pulldown-cmark.** Add the dependency.
      Render each item page by piping the user's doc body through
      `pulldown_cmark::Parser` → `pulldown_cmark::html::push_html`
      with the GFM extensions enabled (tables, footnotes, strikethrough,
      task lists). Wrap in a minimal `<html>` template with sibling
      sidebar and embedded CSS. UI tests covering: empty docs, code
      blocks, table, internal link, `<` escaping.

- [x] **Phase 5: Anchors, links, and cross-references.** Generate
      stable `id` attributes for every item, and rewrite intra-doc
      links of the form `[Name]` / `[Name::method]` / `[fn name]` to
      anchor URLs. No new dependency. Behaviour modelled on rustdoc's
      intra-doc-links; we accept partial coverage (must resolve in
      current file or via `@import` chain — anything else stays plain
      text).

- [x] **Phase 6: Stdlib + prelude land under `learn/references/` on
      the website.** Run `gruel --doc=markdown` over `std/` and
      `prelude/`, write the output to `docs/generated/stdlib/` and
      `docs/generated/prelude/`, and have `website/build.sh` copy
      those trees into `website/content/learn/references/stdlib/` and
      `website/content/learn/references/prelude/` next to the existing
      `intrinsics.md` and `builtins.md` (same pattern as
      `INTRINSICS_DST` in `website/build.sh:38`). Add a `make
      gen-stdlib-docs` target plus a `check-stdlib-docs` step in
      `make check` so the committed generated output cannot drift from
      source — mirroring `gen-intrinsic-docs` /
      `check-intrinsic-docs`. Update
      `website/content/learn/references/_index.md` to link the two new
      pages. Add docstrings to enough of `std/math.gruel` and the
      `prelude/*.gruel` files to make the rendered output meaningful;
      everything else can land empty and be filled in incrementally.
      Folding the hand-rolled `INTRINSICS` doc strings into this same
      flow is out of scope for this phase — track separately if/when
      we want the registry to flow through `gruel-doc` instead of its
      current bespoke generator.

- [ ] **Phase 7: Stabilization.** Remove the `docs` preview gate, add a
      reference page in the spec, update CONTRIBUTING/CLAUDE.md, and
      mark this ADR `status: stable`.

## Consequences

### Positive
- Users get a familiar Rust-flavoured way to write docs from day one of
  the feature landing.
- The AST gains a fact (docstring text) that tooling — LSP, IDE hover,
  better error messages — can mine without re-lexing.
- The intrinsic doc surface (ADR-0050) and stdlib docs can move from
  hand-maintained ADR appendices into source.
- Builds against `pulldown-cmark` are already a known quantity in the
  ecosystem; no surprise here.

### Negative
- A few hundred lines of pure mechanical change across the AST struct
  definitions. Serde-derived caching means version bumps need a careful
  `gruel-cache` round-trip test, which we already have hooks for
  (ADR-0088 follow-up).
- One new crate (`gruel-doc`) plus one new external dep
  (`pulldown-cmark`) — small but non-zero.

### Risks
- **Markdown ambiguity.** Mixed users will be surprised at what
  CommonMark does or doesn't do. We accept this and document the
  extension set we enable.
- **AST growth.** Adding `Option<Doc>` to every item is ~16 bytes per
  item (one heap pointer + length + Span). For a 10k-item program
  that's <200KB and worth it.
- **Doctests.** Rust runs `///` code blocks as tests. We **do not** in
  MVP — that requires a sandboxed compile-and-run harness and is a
  large feature in its own right. Future Work below.

## Open Questions

- Should the Markdown output be a single concatenated `<source>.md`
  file per source file, or a tree of per-item files? The ADR proposes
  per-item-file to mirror HTML output, but a `--doc-mode=single` knob
  could trivially be added later.
- ~~Where should we land the doc rendering for the standard library?~~
  Resolved: Markdown handoff into `website/content/learn/references/`,
  matching the existing `intrinsics.md` / `builtins.md` flow.
- Should `--doc` also produce a JSON dump (paralleling `rustdoc-json`)?
  Strong candidate for "follow-up ADR if external tooling asks for it."

## Future Work

- **Doctests.** Run fenced ` ```gruel ` code blocks as compile-and-run
  tests, like `cargo test --doc`. Needs sandbox plumbing + a clean test
  harness API; warrants its own ADR.
- **Syntax highlighting.** Either pull in `syntect` with a tiny gruel
  grammar, or call out to the website's Giallo/Zola highlighter on the
  HTML pipeline.
- **Block-form doc comments (`/** … */`).** Requires introducing
  regular `/* … */` block comments first; track separately.
- **Search index** (e.g., a side-loaded `search-index.js` matching
  rustdoc's UX).
- **Manpage / roff output.** `roff-rs` exists if we ever decide stdlib
  manpages are worth it.

## References

- ADR-0026 (Module System) — where items live and what `pub` means.
- ADR-0050 (Intrinsics crate) — current hand-rolled doc surface to
  fold in.
- ADR-0083 (`@mark` directive) — surface to extend with `@doc(hidden)`
  later.
- ADR-0085/0086 (FFI blocks) — additional doc attachment points.
- ADR-0088 (`@mark(unchecked)` on FFI/methods) + follow-up — sets the
  AST round-trip / cache-stability precedent we follow here.
- [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) — chosen markdown library.
- [Rust Reference: Doc Comments](https://doc.rust-lang.org/reference/comments.html#doc-comments) — syntax we mirror.
- [Zig `zig build-obj -femit-docs`](https://ziglang.org/documentation/master/#Doc-Comments) — precedent for a language-specific in-tree doc generator.
