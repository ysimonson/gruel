---
id: 0093
title: `gruel fmt` source formatter
status: implemented
tags: [tooling, formatter, dx]
feature-flag:
created: 2026-05-21
accepted: 2026-05-26
implemented: 2026-05-26
spec-sections: []
superseded-by:
---

# ADR-0093: `gruel fmt` source formatter

## Status

Implemented

## Summary

Add a `gruel fmt` subcommand that rewrites Gruel source into a single
canonical style. It accepts an optional path (file or directory) and,
with no argument, formats every `.gruel` file under the
manifest-discovered workspace (ADR-0092). It takes no user-supplied
configuration. The style largely matches rustfmt — 4-space indentation,
K&R brace placement, trailing commas in multi-line lists, doc-comment
normalization — with one deliberate divergence: **no column limit, so
the formatter never wraps lines on its own**. A new `gruel-fmt` crate
parses source with the existing chumsky frontend, walks the AST to emit
canonical text, and weaves in `//` line comments and blank lines via a
side trivia scan over the raw bytes. The same library powers a new
`textDocument/formatting` handler in `gruel-lsp`, so format-on-save
works in every LSP-aware editor out of the box. The feature is gated
behind `--preview fmt` until it stabilises.

## Context

### What we have today

- A complete, span-preserving chumsky frontend
  (`gruel-lexer` → `gruel-parser`) that produces an `Ast` with `Span`
  on every nameable node ([`crates/gruel-parser/src/ast.rs:53`]).
- The lexer skips `//` and `////+` line comments and preserves only
  `///` doc comments as `LineDoc` tokens
  ([`crates/gruel-lexer/src/logos_lexer.rs:299`]). Gruel has no block
  comments.
- Doc comments are attached to items by the parser as `Option<Doc>` on
  every `Function`, `StructDecl`, `EnumDecl`, etc. (ADR-0089), so a
  pure AST walker already covers them.
- The package manifest (ADR-0092) gives a stable, automatic answer to
  "what is the workspace?" — `discover_upward(start)` from CWD.
- ADR-0091 (LSP) explicitly deferred `textDocument/formatting` waiting
  for a formatter to exist. This ADR fills that hole in Phase 7 as a
  thin wrapper over `gruel-fmt::format_source`, reusing the in-memory
  `DocState.text` and `LineMap` the LSP already maintains.

### The problem

There is no canonical formatting today. Hand-written Gruel drifts:
some files use 2-space indent, some 4; some put `pub` before `fn`,
some after `unchecked`; some use trailing commas in multi-line struct
literals, some don't. Code review and copy-paste both suffer. There
is no `cargo fmt` muscle-memory equivalent, no
`textDocument/formatting`, and no CI gate for style.

The cost of style debates is already real. Even bigger payoff: once a
canonical style exists, every editor with LSP gets format-on-save,
every PR can be gated on `gruel fmt --check`, and code-generators
(macros, future scaffolding tools) can emit slightly-ugly code and
trust the formatter to tidy it.

### Why now

- The frontend is stable. Adding a new AST node kind triggers an
  exhaustive `match` in the emitter, so the emitter cannot silently
  fall behind syntax churn.
- The manifest just stabilised (ADR-0092 Phase 5 / commit `6f14fe22`).
  Workspace discovery has a single canonical answer now; no
  ad-hoc rules to invent.
- The LSP shipped (ADR-0091) with a documented hole for formatting.
  Filling it now (Phase 7 below) unlocks format-on-save in
  Zed/Helix/Neovim/VS Code without further LSP design work — the
  document store, position encoding, and workspace lifecycle already
  exist; the handler is a dozen lines of glue.

### Why no column limit

rustfmt's hardest, slowest, most-bug-prone code path is its
column-limit-driven line breaker — the "should this argument list
wrap, and if so where" machinery. Skipping it cuts the v1 scope by
roughly an order of magnitude. The trade-offs:

- We lose automatic rewrapping of long lines. Authors must split lines
  themselves where they want them.
- We avoid every "the formatter mangled my carefully aligned
  expression" frustration that haunts rustfmt power users.
- We keep "preserve user choice" line-break behaviour (see Decision §3
  below) which is the rustfmt feature users most often *wish* applied
  uniformly anyway.

This matches what users actually asked for ("largely matches rustfmt,
but does not impose a column limit"). If we later want a column limit
we can add `--max-width` as a follow-up ADR; nothing about the v1
architecture forecloses that.

### What this ADR explicitly does *not* cover

- **Column limit / automatic line wrapping.** Deferred to a follow-up
  ADR if and when demand appears.
- **Configuration.** Single canonical style; no `rustfmt.toml`
  equivalent. New options would need their own ADR.
- **Reordering.** The formatter does not reorder items, struct
  fields, match arms, imports, etc. It only rewrites whitespace,
  punctuation, and indentation.
- **Reformatting invalid source.** Parse failure → diagnostic →
  the file is skipped; other files in the run still get formatted.
- **`textDocument/rangeFormatting` and `textDocument/onTypeFormatting`
  in the LSP.** Document-level `textDocument/formatting` *is* in
  scope (Phase 7). Range formatting needs a range-to-AST-subtree
  mapper (the emitter is structured per-node, but figuring out which
  node a user-selected range corresponds to is non-trivial). On-type
  formatting needs paren/brace-aware re-indent heuristics that are
  awkward to make idempotent against a half-typed buffer. Both
  deferred until users ask.
- **`make fmt` / CI gate.** Trivial follow-up; called out under
  Future Work.

## Decision

### High-level shape

```
                       ┌─────────────────────────────┐
                       │       gruel-fmt (lib)       │
                       │                             │
   source bytes ──▶  ┌─┴────────────────────────────┐│
                     │  lexer (existing)            ││
                     │  parser  ──▶  Ast            ││
                     └─┬────────────────────────────┘│
                       │                             │
   source bytes ──▶  ┌─┴─ trivia_scan() ──▶ TriviaTable │
                       │                             │
                       │  Emitter(Ast, TriviaTable)  │
                       │   ──▶ canonical String      │
                       └─────────────────────────────┘
```

- New crate: `gruel-fmt`, owning all formatting logic.
- Public API: `format_source(&str) -> Result<String, FmtError>`
  (single-file). Callers (CLI, LSP) layer file IO and diffing on top.
- Pipeline: parse, scan trivia from raw bytes, walk AST, emit. The
  trivia table is a list of `(byte_offset, TriviaKind)` entries with
  `TriviaKind::LineComment(text)` and `TriviaKind::BlankLines(n)`.

### Style rules

**Whitespace and indentation.**

- Indentation: 4 spaces, no tabs.
- One space after `,`, `:`, `;` (never before).
- One space around binary operators (`+`, `*`, `==`, `&&`, `..`, …).
- No space inside `(`, `[`, `{` at the open, or before `)`, `]`, `}`.
- No trailing whitespace.
- Exactly one trailing newline at EOF.

**Brace and keyword placement (K&R).**

- `fn foo(…) -> T {` — opening brace on the same line.
- `if cond {` / `else {` / `else if cond {` — all on one line.
- `match expr {` — same.
- `struct Foo {` / `enum Foo {` / `interface Foo {` — same.

**Multi-line lists.**

When a list (call args, struct fields, struct literal, enum variants,
match arms, parameter list, derive list, intrinsic args, `link_extern`
block) is emitted across multiple lines, each entry sits on its own
line and the list ends with a trailing comma:

```gruel
let p = Point {
    x: 1,
    y: 2,
};
```

On a single line, no trailing comma:

```gruel
let p = Point { x: 1, y: 2 };
```

The decision to break across lines is **preserve-user-choice** (see
"Line-break policy" below).

**Blank lines.**

- At most one consecutive blank line anywhere.
- Exactly one blank line between top-level items (collapsed from any
  larger run).
- No blank line at the start of a block; at most one before a closing
  brace.

**Doc comments and directives.**

- `///` blocks emit verbatim, one per line, attached to the following
  item. Leading-space normalization matches the lexer's existing
  "strip up to one leading space" rule.
- Module-doc blocks (the leading doc separated from the first item by
  a blank line) emit at the top of the file.
- Directives (`@allow(unused)`, `@derive(Eq)`, `@mark(copy)`) each get
  their own line, immediately before the item, after doc comments.

**Item-internal ordering on items.**

- `pub` precedes `unchecked` precedes `fn` / `const`.
- `comptime` precedes the parameter name.
- These are syntactic — the parser already encodes the order — the
  formatter just emits in canonical sequence.

### Line-break policy (the "preserve user choice" rule)

For every comma list, the emitter chooses single-line vs multi-line
by looking at the original source spanned by the list's outer
delimiters (`(…)`, `{…}`, `[…]`):

- If the original contained at least one newline between the outermost
  delimiters, emit multi-line (one entry per line, trailing comma).
- Otherwise emit single-line (no trailing comma).

This means a single `\n` between `Foo {` and the first field is the
signal that says "I want this multi-line", and conversely a one-liner
stays a one-liner. The check is a byte scan, not a structural one;
nested lists are evaluated independently.

Edge cases:

- An empty list (`Foo {}`, `foo()`) is always single-line.
- A list with a single element follows the same rule; a sole element
  written across lines stays multi-line with trailing comma.
- For `match` expressions, each arm is always on its own line
  (rustfmt-equivalent); this is a hard rule, not user-choice.
- For function bodies (`BlockExpr`), the block always emits on multiple
  lines (a function body collapsed to one line is a readability loss
  we never want).

### Comment weaving

Line comments do not appear in the AST. The emitter weaves them in by
consulting the `TriviaTable`:

1. `trivia_scan(src)` walks the bytes once, recording every `//…\n`
   slice (excluding `///…\n` which is already a parsed doc) and every
   run of blank lines. Each entry has a `(start, end)` byte range and
   payload.
2. The emitter maintains a `cursor: usize` over the trivia table. At
   every emission point that crosses a span boundary, it drains all
   trivia whose end ≤ the next span start.
3. Drained trivia render as either:
   - A blank line (at most one, after collapsing).
   - A `// …` line at the current indentation, *or* a same-line
     trailing comment if the trivia's start is on the same line as
     the previous emission (line-number derived from the source).

Hard cases this v1 explicitly handles:

- Leading file comment: emitted at the top, before module doc.
- Comment between two items: emitted between them, separated by the
  standard one blank line above and below if the source had any.
- Comment inside a block, before a statement: emitted at the
  statement's indentation level.
- Trailing same-line comment after a `let` or expression statement:
  emitted on the same line.

Hard cases this v1 does **not** try to be clever about:

- Comments inside a deeply nested expression (e.g., between two
  binary operands) emit as their own line at the enclosing
  statement's indentation. Authors who care about precise placement
  can use a temporary `let`.
- Comments inside `match` arm patterns (rare) emit as a leading
  comment for the arm.

We exercise these via the idempotence test (Phase 6); any input where
weaving is lossy must either be made lossless or documented as a
known-deficiency snapshot test.

### CLI surface

```text
gruel fmt [PATH]               Format in place
gruel fmt [PATH] --check       Print a unified diff per file; exit 1 if any would change
gruel fmt [PATH] --emit stdout Write to stdout (forces a single file or `-`)
gruel fmt -                    Read source from stdin, write to stdout
```

`PATH` resolution:

- **Omitted:** discover `gruel.json` upward from CWD (ADR-0092
  `discover_upward`). The workspace is the manifest's directory.
  Format every `.gruel` file under it recursively (sorted by path for
  deterministic output ordering). If no manifest is found, error and
  exit non-zero with a message pointing the user at `gruel.json` or
  passing an explicit path.
- **File `*.gruel`:** format that file.
- **Directory:** format every `.gruel` file under it recursively.
- **`-`:** stdin → stdout. Implies `--emit stdout`.

The recursive walk skips:

- `target/` (build output)
- `.git/`
- any directory beginning with `.`

These are not configurable. The set matches what rustfmt does
implicitly via `cargo fmt`.

Errors:

- Parse failure on any file: emit a diagnostic via the standard
  `MultiFileFormatter`, **skip that file**, continue with the rest,
  and exit non-zero at the end.
- IO failure (read or write): emit and skip the same way.
- `--check` exits 1 if any file would have changed *or* failed to
  parse.

### Preview gating

Add `PreviewFeature::Fmt` to `crates/gruel-util/src/error.rs`. The
`gruel fmt` CLI entry point errors without `--preview fmt`. The
library is unconditionally available (the gate lives at the CLI layer
so the LSP and tests can call it without invoking the gate).

### LSP integration

The same library that drives `gruel fmt` also powers a new
`textDocument/formatting` handler in `gruel-lsp`, closing the gap
ADR-0091 left open.

- **Capability.** `Backend::initialize` adds
  `document_formatting_provider: Some(OneOf::Left(true))` to
  `ServerCapabilities`. Since the engine is unconditionally
  available (see "Preview gating" above), the LSP advertises the
  provider unconditionally — clients don't need a `--preview`-style
  opt-in to get format-on-save.
- **Handler.** `Backend::formatting` (new `async fn`, alongside
  `hover`/`references`/`inlay_hint` in
  `crates/gruel-lsp/src/server.rs`) looks up the requested URI in
  the document store, calls `gruel_fmt::format_source(&doc.text)`,
  and returns the result as `Option<Vec<TextEdit>>`.
- **Edit shape — minimal diff, not full-document replace.** Replacing
  the whole document on every save loses cursor position, fold state,
  and undo granularity in some clients. Instead, the handler diffs
  original vs. formatted using the same `similar` crate the CLI's
  `--check` mode uses (one dependency, one algorithm) and emits one
  `TextEdit` per change hunk. Ranges are computed against the
  document's existing `LineMap` via the same `byte_to_position` path
  hover and goto already use
  (`crates/gruel-lsp/src/position.rs`), so UTF-8 vs. UTF-16 client
  encoding negotiation is handled by code that is already proven. If
  the file is already formatted, the handler returns
  `Some(vec![])` so the editor records a clean save with no edits.
- **Parse failure.** If `format_source` errors (parse failure on the
  buffer's current text), the handler returns `Ok(None)`. The editor
  leaves the buffer untouched, and the user sees no failure
  notification — diagnostics from the existing analysis pipeline
  already explain what's wrong. Crucially, format-on-save on a
  half-typed file does *not* clobber it.
- **Document source.** The handler reads from the in-memory
  `DocState.text`, never from disk. This matches how every other
  LSP request sees the buffer and avoids racing the editor.
- **No manifest dependency.** Formatting a single buffer needs no
  workspace discovery. Both isolation mode and manifested mode
  (ADR-0091 Phase 8 / ADR-0092) use the same handler — the manifest
  only matters when the *CLI* needs a workspace root.
- **No diagnostic differential impact.** The formatter does not run
  sema and does not emit diagnostics, so the
  `spec_corpus_diagnostic_differential` test (ADR-0091) needs no
  update.

### Idempotence invariant

`format_source(format_source(x)) == format_source(x)` is a tested
invariant. Phase 6 runs every spec/UI test case through the formatter
twice and asserts equality. Additionally,
`parse(format_source(x)) == parse(x)` (modulo span info) — the
formatter must never change semantics.

## Implementation Phases

- [x] **Phase 1: Scaffolding + smallest formatter**
  - Create `crates/gruel-fmt` with `Cargo.toml` and `src/lib.rs`.
    Deps: `gruel-parser`, `gruel-lexer`, `gruel-util`, `lasso`.
  - `pub fn format_source(src: &str) -> Result<String, FmtError>`.
  - `Printer` struct that owns the output buffer, current indent
    level, and helpers (`write_str`, `newline`, `indent`, `dedent`).
  - Handle the smallest case: a file containing a single
    `fn main() -> i32 { 0 }`. Emit canonical form.
  - Snapshot test infra under `crates/gruel-fmt/tests/snapshots/`.

- [x] **Phase 2: Expressions and statements**
  - Exhaustive `match` over `Expr` and `Statement`. Compiler enforces
    completeness so any new variant adds a TODO arm via
    `#[deny(non_exhaustive_omitted_patterns)]` (or matching default
    arm panic that the test harness catches).
  - Operator precedence-aware parens: emit a paren iff the AST has a
    `Paren` wrapper *or* the natural emission would re-parse
    differently. Default to "AST-shape-preserving" — every `Paren`
    becomes literal `(…)`, every non-`Paren` does not.
  - Blocks (`BlockExpr`): always multi-line; final expression has no
    trailing `;`.

- [x] **Phase 3: All top-level items**
  - `Function`, `StructDecl`, `EnumDecl`, `InterfaceDecl`,
    `DeriveDecl`, `ConstDecl`, `LinkExternBlock`.
  - Doc comments (`///`) and directives in canonical order.
  - Visibility, `unchecked`, `comptime` modifiers.
  - Parameter list, return type, body.
  - Snapshot tests cover one example per item kind.

- [x] **Phase 4: Trivia weaving**
  - `trivia_scan(src) -> TriviaTable` over raw bytes. Handles `//`
    line comments (any slash run except `///` exactly), and blank-line
    runs. Returns a sorted vector of `(start, end, kind)`.
  - `Printer` extension: `drain_trivia_before(byte_offset)` — emits
    any pending trivia at the right indentation, deciding inline vs
    own-line by comparing source line numbers (`LineIndex` from
    `gruel-util::span`).
  - Blank-line collapsing: at most one consecutive blank in output.
  - Tests for every weaving case listed under "Comment weaving"
    above.

- [x] **Phase 5: CLI subcommand**
  - `gruel fmt` clap subcommand in `crates/gruel/src/main.rs` with
    `BUILD/RUN/CHECK`-style `FmtArgs` and resolved `FmtOpts`.
  - Manifest discovery for the no-arg case (reuse `discover_upward`).
  - Directory walking (`walkdir` is already a dev-dep; promote to
    runtime if needed).
  - `--check` with unified diff (use `similar` crate; verify it isn't
    already in the workspace tree first).
  - `--emit stdout` and `-` (stdin → stdout).
  - Wire `PreviewFeature::Fmt` gate at the CLI entry.

- [x] **Phase 6: Idempotence and corpus tests**
  - Test in `gruel-fmt` that loads every `.gruel` source from
    `crates/gruel-spec/cases/` and `crates/gruel-ui-tests/cases/`
    (the test TOMLs already inline source), runs `format_source`
    twice, and asserts equality.
  - Differential test: `parse(format_source(x))` produces an
    `Ast`-equivalent (modulo spans) to `parse(x)` for the same
    corpus.
  - Wire into `make test` as a new target line under the existing
    test orchestration (similar to the tree-sitter differential
    test from ADR-0090).

- [x] **Phase 7: LSP integration**
  - Add `gruel-fmt` to `crates/gruel-lsp/Cargo.toml` dependencies.
  - Implement `async fn formatting(&self, params:
    DocumentFormattingParams) -> jsonrpc::Result<Option<Vec<TextEdit>>>`
    on `Backend` in `crates/gruel-lsp/src/server.rs`, sitting
    alongside `hover` / `references` / `inlay_hint`. Steps:
    1. Look up the document by URI in `self.documents`; return
       `Ok(None)` if absent.
    2. Call `gruel_fmt::format_source(&doc.text)`; on `Err`, return
       `Ok(None)` and log at `debug` level (diagnostics already
       cover the cause).
    3. If the formatted text equals the original, return
       `Ok(Some(vec![]))`.
    4. Diff original vs. formatted with `similar` (same crate as
       `--check`); convert each hunk to a `TextEdit` whose `range`
       is computed via `byte_to_position` using `doc.line_map` and
       the negotiated `PositionEncoding`.
  - Add `document_formatting_provider: Some(OneOf::Left(true))` to
    the `ServerCapabilities` returned from `Backend::initialize`.
  - Tests under `crates/gruel-lsp/tests/`:
    - `formatting_basic.rs`: open a buffer with messy whitespace,
      request formatting, apply the returned `TextEdit`s, assert
      the resulting text equals `format_source(original)`.
    - `formatting_unchanged.rs`: an already-formatted buffer
      returns `Some(vec![])`.
    - `formatting_parse_error.rs`: a buffer that doesn't parse
      returns `Ok(None)`; the editor-side text is unchanged.
    - UTF-16 client encoding test: a buffer with multi-byte
      characters formats to the expected edits when the negotiated
      encoding is UTF-16.
  - Cross-link from ADR-0091's "Future Work → Formatter integration"
    bullet to this phase (single-line update).

- [x] **Phase 8: Stabilisation**
  - Remove `PreviewFeature::Fmt` and its CLI gate.
  - Add `make fmt` (runs `gruel fmt`) and `make fmt-check` (runs
    `gruel fmt --check`) to `Makefile`.
  - Document the formatter and its conventions in `CLAUDE.md` under
    a new "Formatting" section, including the LSP capability so
    editor users know format-on-save is available.
  - Update ADR-0091 (LSP) and ADR-0090 (tree-sitter): in ADR-0091,
    move the "Formatter integration" Future Work bullet to a
    "Delivered by ADR-0093" line under References; in ADR-0090, add
    a short note that the same chumsky AST drives both the parser
    differential and the formatter.

## Consequences

### Positive

- A single canonical style. Code reviews stop bikeshedding whitespace.
- `gruel fmt --check` becomes a one-line CI gate.
- LSP format-on-save becomes a thin wrapper — every editor benefits.
- Code generators (macros, scaffolding tools) can emit ugly code and
  trust the formatter.
- New AST nodes that the emitter doesn't handle fail loudly at compile
  time (exhaustive matches), so the formatter cannot silently lag the
  language.
- No column limit means no rewrap-mangling complaints — users
  control line breaks, the formatter just tidies them.

### Negative

- No automatic line wrapping. Long single-line expressions stay long
  unless the author breaks them. Mitigation: users opt in to
  multi-line by inserting a single `\n` between delimiters.
- Comment weaving for deeply-nested-in-expression comments is
  approximate. Mitigation: documented; users can hoist to a `let`.
- A new crate to maintain. Mitigation: small, AST-only, no LLVM
  dependencies; builds fast.
- The "preserve user choice" rule means the formatter's output
  depends on input line breaks (not just AST shape). Mitigation: this
  is intentional — it's the rule that makes "no column limit" usable.
  Idempotence is still tested (fmt(fmt(x)) == fmt(x)).

## Open Questions

- Are there cases where `parse(format_source(x))` is not
  AST-equivalent to `parse(x)` that we should fail on, vs. accept?
  Likely candidates: parenthesisation around chains of equal-precedence
  operators where `Paren` is dropped or added. **Tentative:** the
  emitter preserves every literal `Paren` node and never adds new
  ones; idempotence will catch drift.
- Should the formatter touch trailing newlines inside string literals?
  **No** — string literal bodies are emitted verbatim.
- What encoding do we assume? **UTF-8.** Files that aren't valid
  UTF-8 fail to read and are skipped with an error, matching how the
  rest of the compiler handles source.
- Do we ship a `// rustfmt::skip`-style escape hatch? **Not in v1.**
  No column limit makes it largely unnecessary; revisit if a real
  user need appears.

## Future Work

- **Column limit (`--max-width`).** If users ask. The architecture
  doesn't preclude it — the emitter would gain a layout pass that
  consults max-width when emitting comma lists.
- **LSP `textDocument/rangeFormatting` and `onTypeFormatting`.**
  Document-level formatting ships in Phase 7. Range formatting needs
  a range-to-AST-subtree mapper (the emitter is already structured
  per-node, so the engine side is small; the hard part is figuring
  out which node a user-selected range corresponds to, especially
  when the range bisects an expression). On-type formatting needs
  heuristics that survive a half-typed buffer without churning the
  cursor. Defer both until users ask.
- **`make fmt-check` CI gate.** A GitHub Actions job that runs
  `gruel fmt --check` and fails the PR on drift. Trivial follow-up
  after stabilisation.
- **Import-graph-aware mode.** A `--imports-only` flag that formats
  only files reachable from the manifest entry. Niche; defer until
  asked.
- **Tree-sitter-driven fmt.** If/when chumsky's error recovery hits
  ceilings, we could format off tree-sitter's CST (ADR-0090) for
  resilience on broken input. Same `Emitter` interface, different
  front end.

## References

- ADR-0089 — Doc comments and `gruel doc`. `///` blocks are already
  on the AST; the emitter consumes them directly.
- ADR-0090 — Tree-sitter and parser differential. Same chumsky AST
  used here; tree-sitter could later serve as a fallback front end.
- ADR-0091 — Language Server. Documented hole for
  `textDocument/formatting`; this ADR fills it in Phase 7 as a thin
  wrapper over `format_source`, sharing the document store,
  `LineMap`, and position encoding the LSP already maintains.
- ADR-0092 — Package manifest. Used for workspace discovery in the
  no-arg case.
- ADR-0005 — Preview features. Gating mechanism reused.
- rustfmt's
  [Style Guide](https://doc.rust-lang.org/nightly/style-guide/) —
  reference for most rules.
