---
id: 0091
title: Language Server (LSP) for Gruel
status: proposal
tags: [tooling, ide, lsp, dx]
feature-flag: language_server
created: 2026-05-17
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0091: Language Server (LSP) for Gruel

## Status

Proposal

## Summary

Add a Language Server Protocol implementation for Gruel, shipped as a
`gruel lsp` subcommand built on `tower-lsp`. The server reuses the
existing compiler pipeline (`gruel-compiler`) and its diagnostic model,
plus the on-disk parse cache from ADR-0074, to deliver live diagnostics,
hover, goto-definition, find-references, completion, and code actions to
editors. Editor highlighting, folding, document-symbol outlines, and
local-scope queries are explicitly delegated to the tree-sitter grammar
from [ADR-0090](0090-tree-sitter-and-parser-differential.md) (already
exposed via `tree-sitter-gruel/queries/`); the LSP adds the
semantically-typed surface that tree-sitter alone cannot. The feature is
gated behind `--preview language_server` until it stabilises.

## Context

### What we have today

- A complete compiler frontend (`gruel-lexer` → `gruel-parser` → AstGen →
  `gruel-air::Sema`) with rich, structured diagnostics
  (`MultiFileFormatter`, `MultiFileJsonFormatter`, `JsonDiagnostic`,
  `JsonSpan`, `JsonSuggestion`). The JSON shape is already LSP-friendly:
  byte offsets, line/col, severity, suggestions, applicability.
- A `gruel check` subcommand that drives the same frontend without
  codegen ([`run_check` in `crates/gruel/src/main.rs:1389`]).
- A persistent parse cache keyed by `parse_key(build_fp, source_bytes)`
  (`gruel-compiler::parse_cache`) with hit/miss instrumentation. ADR-0074
  also caches sema (`air/`) and per-function bitcode, all keyed by
  content hash plus a `pub` signature fingerprint.
- A tree-sitter grammar in `tree-sitter-gruel/` with `highlights.scm`,
  `locals.scm`, `indents.scm`, `folds.scm`, exposed through a Rust
  binding (`tree-sitter-gruel/bindings/rust/lib.rs`). Modern editors
  (Zed, Helix, Neovim, Emacs) consume these directly without an LSP.
- A doc-rendering crate (`gruel-doc`) that walks an `Ast` and renders
  `///` doc-comments to Markdown / HTML (ADR-0089). The same renderer
  can produce LSP hover markdown.
- Span-aware ASTs: every nameable AST node carries a `Span` with
  `FileId`, byte offsets, and line/col conversion helpers (`LineIndex`
  in `gruel-util::span`). The AIR layer carries per-instruction `Type`
  information and `AnalyzedFunction` metadata.

### The problem

There is no editor integration that surfaces *semantic* information.
Without an LSP:

- Errors and warnings only appear after the user manually runs
  `gruel check` or `gruel build`.
- Type-of-expression-under-cursor, signature, and docstring information
  is invisible — even though the compiler computes all of it.
- Cross-file `@import` resolution and find-references require manual
  grepping.
- Suggestions attached to diagnostics (`Applicability::MachineApplicable`,
  e.g. "did you mean `i32`?") never reach the user as a fixit.

Tree-sitter (ADR-0090) gives editors syntactic affordances for free —
highlighting, folds, symbol outlines, locals-aware highlight, indent
rules — but it cannot type-check. The LSP fills exactly the
complementary slot: anything that requires `gruel-air::Sema`.

### Why now

- The compiler frontend is stable enough that the LSP wire shape will
  not churn under us. The work needed to wire LSP isn't experimental —
  it's plumbing.
- The parse cache (ADR-0074) was designed for CLI invocations but its
  content-addressed key works equally well for "open buffer at version
  N" — we get incremental warmth for free.
- The tree-sitter foundation just landed (ADR-0090). With both parsers
  in tree, the LSP can lean on the canonical chumsky parser for
  correctness and (later, optionally) on tree-sitter for
  while-you-type resilience.

### What this ADR explicitly does *not* cover

- **Syntactic editor features.** Highlighting, folding, document
  symbols, indent guides, and locals-aware identifier highlighting are
  the tree-sitter grammar's job (ADR-0090, `queries/`). The LSP does
  not duplicate them. (If/when a future editor without tree-sitter
  support needs document symbols via LSP, that becomes a small
  follow-up — but is not required for the MVP.)
- **Formatting.** No `gruel-fmt` exists yet; `textDocument/formatting`
  is deferred until there is a formatter to drive.
- **Refactorings other than rename.** Extract-function, inline, etc.
  are out of scope; they have to wait for the compiler to grow the
  primitives that make them safe.
- **Debug adapter (DAP).** Separate protocol, separate ADR.

## Decision

### High-level architecture

```
                  ┌──────────────────────────────┐
   editor ←─JSON-RPC─→  gruel lsp  (tower-lsp)   │
                  │   ┌────────────────────────┐ │
                  │   │ document store          │ │
                  │   │  (DashMap<Url,         │ │
                  │   │   DocState>)            │ │
                  │   └────────────┬────────────┘ │
                  │                ▼              │
                  │   ┌────────────────────────┐ │
                  │   │ analysis worker (tokio │ │
                  │   │ task per workspace) —  │ │
                  │   │ debounced compile      │ │
                  │   └────────────┬────────────┘ │
                  │                ▼              │
                  │   ┌────────────────────────┐ │
                  │   │ gruel-compiler::         │ │
                  │   │  compile_frontend_…    │ │
                  │   │ + gruel-cache          │ │
                  │   └────────────────────────┘ │
                  └──────────────────────────────┘
```

The server is one tokio runtime hosting a `tower-lsp` `LanguageServer`
trait impl. It owns:

1. A **document store** — `DashMap<lsp_types::Url, DocState>` where
   `DocState` carries the latest text, version, and a `LineIndex`. Text
   sync is `TextDocumentSyncKind::INCREMENTAL`; the store reconstitutes
   full text by applying patches.
2. An **analysis worker** — one tokio task that drains a coalescing
   channel of "files changed". Re-runs the frontend over the affected
   workspace (debounced ~150ms after the last keystroke), then turns
   the resulting `JsonDiagnostic` set into `PublishDiagnosticsParams`
   per file. The worker carries a **`CancellationToken`** through the
   compile call; if the next keystroke arrives mid-compile, the token
   is tripped and the in-flight pass is dropped at the next safe
   yield point (between files at parse, between functions at sema).
   A hard timeout (default 5s, configurable) bounds the worst case
   so a runaway comptime loop can't hang the worker.
3. A **semantic snapshot** — the most recent successful (or partially
   successful) compile state, held inside an `ArcSwap<Snapshot>`:
   merged `Ast`, `ThreadedRodeo`, `SemaOutput` (when sema completed),
   and a side-table mapping `Span` ranges to AST node refs for
   position queries. LSP request handlers `.load()` the snapshot
   atomically — no locks held across the request. **Stale-while-
   revalidate:** the snapshot is *only* replaced when a compile
   succeeds (or completes with errors that still produce a usable
   sema). Cancelled or timed-out compiles do not perturb the
   snapshot, so hover/goto/references keep working on the last good
   state while edits are in flight.

The LSP is a thin façade over `gruel-compiler`'s existing public API.
It does *not* fork or shadow compiler internals; if behaviour diverges,
that's a bug in the LSP and not a design choice.

### Crate layout

A new workspace member `crates/gruel-lsp` containing:

```
crates/gruel-lsp/
├── Cargo.toml          # deps: tower-lsp, tokio (rt-multi-thread, macros),
│                       #       serde, gruel-compiler, gruel-doc,
│                       #       gruel-cache, dashmap
├── src/
│   ├── lib.rs          # public `run_server()` entry point
│   ├── server.rs       # `Backend` struct + LanguageServer impl
│   ├── document.rs     # DocState, incremental text application
│   ├── analysis.rs     # debounced compile worker + result cache
│   ├── diagnostics.rs  # JsonDiagnostic → lsp_types::Diagnostic mapping
│   ├── hover.rs
│   ├── goto.rs
│   ├── references.rs
│   ├── completion.rs
│   ├── code_actions.rs
│   ├── position.rs     # LSP Position ↔ Span conversion (UTF-16 ↔ bytes)
│   └── workspace.rs    # workspace root discovery + file enumeration
└── tests/              # integration tests (spawn server, drive RPC)
```

The crate is a library so the binary can be a thin shim and so
integration tests can drive it through its public API without spawning
a subprocess. The binary entry lives in `crates/gruel/src/main.rs`
under a new `gruel lsp` subcommand.

### `gruel lsp` subcommand

```rust
#[derive(Args, Debug)]
struct LspArgs {
    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,

    /// Workspace root override (defaults to `initializeParams.rootUri`).
    #[arg(long, value_name = "PATH")]
    root: Option<String>,

    /// Cache directory for incremental compilation. Inherits the same
    /// resolution as `gruel build`/`run`/`check`.
    #[arg(long, value_name = "PATH", env = "GRUEL_CACHE_DIR")]
    cache_dir: Option<String>,
}
```

Inside `run_lsp`, the server requires `--preview language_server` (the
runtime check lives in the LSP entry point, since there's no `Sema`
call where a `require_preview()` shows up naturally for the LSP itself).
This keeps the feature off by default until stabilised.

### Position model

LSP uses UTF-16 code-unit offsets in `Position`; Gruel's `Span` uses
byte offsets. Conversion is encapsulated in `position.rs`:

- `byte_to_position(line_index, source, byte) -> lsp_types::Position`
- `position_to_byte(line_index, source, pos) -> u32`
- `span_to_range(line_index, source, span) -> lsp_types::Range`

`gruel-util`'s `LineIndex` provides O(log n) line lookup; UTF-16 column
math is per-line and runs O(line length) which is fine. The mapping is
recomputed lazily and cached on `DocState`.

`initialize` advertises `positionEncoding: ["utf-16"]` and (optionally)
`"utf-8"` if the client supports it (LSP 3.17). When the client picks
`utf-8` we can skip the UTF-16 conversion entirely.

### Comptime and responsiveness

This section is the load-bearing risk of the ADR and worth calling out
explicitly. Gruel's generics are comptime-shaped (see
`[[project_no_user_generics]]`): every `Vec(i32)` instantiation, every
`@derive(...)`, every type-level construction runs the comptime
interpreter during `Sema`. The LSP's "thin façade over sema" design
therefore couples editor responsiveness to comptime cost.

**How ZLS (Zig Language Server) avoids this.** Zig has the same
problem and made the opposite choice. ZLS reimplements its own
analyzer (`zls.Analyser`) rather than running the compiler's sema. The
analyzer does best-effort, lazy comptime evaluation: simple constant
folding and shallow generic instantiation work; deep comptime, full
type-level computation, and anything depending on whole-program state
bail and surface "unknown" in hover. The explicit trade is **"always
responsive, sometimes incomplete"** over "always correct, sometimes
hung." Cancellation is everywhere — typing again invalidates the
in-flight analysis.

**Rust-analyzer also reimplements** name resolution, type inference,
and trait solving rather than reusing rustc's. The reason there is
different (rustc's queries aren't shaped for incremental low-latency
re-use), but the conclusion lands at the same place: when latency
matters, the LSP can't share the canonical analyzer.

**Why this ADR still chooses the thin-façade approach.**

1. The headline Phase-1 feature is diagnostics, which need full sema
   to be correct. There is no shortcut: a partial analyzer that omits
   comptime omits errors that fire in comptime. We would rather ship
   slightly-laggy correct diagnostics than instant wrong diagnostics.
2. Phases 3–4 (hover, goto, signature help) need types. A
   non-comptime analyzer can serve some hovers ("this is an
   `Identifier` defined at line N") but not the most valuable ones
   ("this expression has inferred type `Vec(i32)` because `T` was
   bound to `i32` at comptime").
3. Duplicating the analyzer in `gruel-lsp` is a large, ongoing tax —
   every compiler change to inference, comptime, or generics has to
   be mirrored in the LSP's shadow analyzer. Code-base of one
   maintainer (today) cannot afford that.
4. The current design has a credible escape valve: if comptime cost
   becomes prohibitive, we can layer a lightweight pre-sema analyzer
   *behind* the same façade later. The hard reverse — pulling
   compiler-frontend dependency out of the LSP — is not necessary up
   front.

**Mitigations baked into Phase 1 (see below).** Even with the
thin-façade design, we keep responsiveness defensible by:

- **Cancellation tokens** carried through the compile call. If the
  next keystroke arrives before the in-flight compile finishes, sema
  is cancelled at the next safe point (after each function, between
  passes). The interrupted result is discarded.
- **Stale-while-revalidate.** The most recent successful compile is
  the "current" snapshot for hover/goto/references. While a new
  compile is in flight, the previous snapshot continues to serve
  queries; only diagnostics are gated on the new result.
- **Per-request timeouts.** A safety net: any single
  hover/goto/completion that doesn't have a fresh snapshot within
  ~200ms returns the previous snapshot's answer rather than waiting.
- **Comptime budget.** Sema already has a comptime step limit
  (`MAX_COMPTIME_STEPS`-ish); the LSP path can lower it further when
  invoked under `with_lsp_sidetables(true)` so a runaway comptime
  loop bounds the LSP's latency rather than the build's correctness.

**Escalation path (if Phase-3 hover latency is bad on real
codebases).** A future ADR (call it ADR-0091-followup or its own
number) introduces a `gruel-lsp-analyzer` module that does
ZLS-style partial analysis: name resolution + structural type lookup
without comptime. Hover then uses sema results when available, falls
back to the lightweight analyzer when sema hasn't finished. We do not
commit to this in Phase 1; we commit only to the architecture *not
foreclosing* it.

This section deliberately documents the trade-off so future
contributors don't accidentally undo it without thinking, and so a
later pivot to ZLS-style analysis is a deliberate response to
measured pain rather than a vague "we always knew."

### Concurrency

Three concurrency boundaries to think about; each is treated
separately.

**1. Cross-process: `gruel lsp` ↔ `gruel build`/`run`/`check`.**

The on-disk cache (`gruel-cache::CacheStore`) is already
multi-process-safe by construction:

- Writes go to `tmp/<pid>-<counter>-<hash>.tmp` and `fs::rename` into
  place. `rename` is atomic on POSIX and on Windows when source and
  destination are on the same volume.
- Reads use `fs::File::open` on the final path. A reader either sees
  the previous fully-written blob or the new one — never a torn
  intermediate.
- Entries are content-addressed by `(build_fingerprint, source_hash)`.
  Two processes racing to compute the same key compute identical
  bytes; whichever rename wins is the visible result and both are
  semantically equivalent. (See `gruel-cache::CacheStore::put` doc.)
- The `version` file is consulted only on `CacheStore::open`. If a
  schema mismatch wipes the directory while the LSP is mid-compile,
  the LSP still has its in-memory state and the next compile simply
  remisses on every file. Acceptable.

What this means in practice: the user can run `gruel build src/*.gruel`
in a terminal while the LSP is open in their editor, and neither
corrupts the other's results. The LSP's in-memory `Snapshot` is
unaffected by external builds (it's purely RAM); on the next compile,
it benefits from any cache entries the external build populated.

**2. In-process: tower-lsp's request handlers.**

`tower-lsp` invokes each request handler on the tokio runtime; multiple
requests can be in flight concurrently. We design for this:

- The `Backend` struct holds `Arc<DashMap<Url, DocState>>` (per-doc
  state), `Arc<ArcSwap<Snapshot>>` (latest sema result), and a
  channel sender for the analysis worker. All cheaply cloneable, no
  contended locks held across `await` points.
- Read handlers (`hover`, `definition`, `references`, `completion`)
  take an immutable snapshot via `ArcSwap::load()` and operate on
  borrowed data. No handler blocks another reader.
- Write handlers (`didOpen`, `didChange`, `didClose`) mutate the
  per-doc `DashMap` entry — one writer per Url at a time, but
  different Urls progress independently.
- The analysis worker is the *only* writer to `ArcSwap<Snapshot>`. It
  builds the new snapshot off-thread, then `.store()`s it atomically.
  No reader sees a half-built snapshot.

**3. Analysis worker ↔ user keystrokes.**

Discussed in "Comptime and responsiveness." The worker carries a
`CancellationToken` checked at parse boundaries and sema-pass
boundaries; the next `didChange` trips the token and the in-flight
compile is dropped. The previous snapshot stays visible until a fresh
compile lands.

**What we explicitly don't do.**

- **No advisory file lock** on the cache directory. Atomic-rename is
  sufficient; a lock would serialize unrelated CLI invocations.
- **No coordination between LSP and CLI processes** beyond the
  shared cache. If a user runs `gruel build` from a script while
  editing, both processes do their own work and benefit from cache
  hits where their keys overlap. A 2× sema cost on the overlap is
  acceptable; coordinating would require IPC we don't want to build.
- **No request-level mutex on `Backend`.** Everything goes through
  `Arc` + atomic swap so a slow handler doesn't block fast ones.

### Document lifecycle

- `initialize` — record `workspaceFolders`, set up the analysis worker,
  walk the root for `*.gruel` files (using `walkdir`, respecting
  `.gitignore` via `ignore` crate). Cache them as "closed but known".
- `textDocument/didOpen` — store text + version, mark as "open", queue
  a compile.
- `textDocument/didChange` — apply incremental edits in place, bump
  version, queue a compile (debounced).
- `textDocument/didSave` — queue a compile (even if no changes — picks
  up changes to peer files saved externally).
- `textDocument/didClose` — mark as "closed but known"; do not drop
  state (closing a file shouldn't make its diagnostics vanish if it's
  still part of the workspace).
- `workspace/didChangeWatchedFiles` — for files on disk we don't have
  open buffers for, re-read and re-queue.

The compile path is identical to what `gruel check` runs today: lex →
parse → merge symbols → RIR → sema. The LSP never invokes codegen.

### Diagnostics

The single most valuable feature. Implementation:

1. After each successful or partial compile, walk both `CompileErrors`
   and `state.warnings` through `MultiFileJsonFormatter` to get
   `JsonDiagnostic` values.
2. Group by file via `JsonSpan.file` (which is the path supplied to
   `SourceFile`).
3. Convert each `JsonDiagnostic` to `lsp_types::Diagnostic`:
   - `severity` ← `Error` | `Warning`
   - `code` ← `JsonDiagnostic.code` (e.g. `"E0206"`)
   - `range` ← primary `JsonSpan` via `span_to_range`
   - `message` ← concatenation of `message`, `notes`, `helps` (notes
     and helps as `relatedInformation` is also valid; LSP 3.17 supports
     both — we prefer `relatedInformation` for cleanliness)
   - `relatedInformation` ← secondary spans + their labels
   - `data` ← serialized `JsonSuggestion[]` (used later by code actions)
4. Publish via `client.publish_diagnostics(uri, diagnostics, version)`.
5. **Always clear** diagnostics for files that no longer have any —
   otherwise stale red squiggles linger.

For workspace-wide diagnostics (e.g. duplicate symbol detected in
`merge_symbols`), both files referenced by the error get the diagnostic
published against them; the spans in `relatedInformation` cross-link
the two locations.

The compile pipeline must run on the *current buffer contents*, not the
on-disk file. The LSP synthesises in-memory `SourceFile<'_>` entries
that mix open-buffer text with on-disk fallback for files we have not
been notified about.

### Hover

For `textDocument/hover` at `Position p`:

1. Convert `p` to a byte offset `pos`.
2. Find the smallest AST node whose span contains `pos`. We add a
   small visitor in `gruel-lsp::position::SmallestSpanFinder` rather
   than carrying a new AST trait in `gruel-parser` — the LSP is the
   only known consumer.
3. Resolve what the node *is* via the latest `SemaOutput` snapshot:
   - **Identifier reference** → resolve to its definition via the
     symbol tables already built by sema (function, struct, enum
     variant, local). For locals, we recover from RIR's `Local`
     instructions whose span covers `pos`.
   - **Type position** → render `Type` via the existing
     `TypeInternPool::display(ty)`.
   - **Expression** → render its inferred type from
     `AnalyzedFunction.air[expr_id].ty` if such a side-table exists,
     and a backing side-table is added in Phase 4 (see below).
4. If the resolved item has a doc comment (ADR-0089's `Doc` field on
   the AST node), feed it to `gruel-doc::render::hover_markdown(doc,
   item_signature)` to get the markdown content.
5. Return `Hover { contents: MarkupContent { kind: Markdown, value },
   range: Some(node_span_as_range) }`.

**Side-table needed:** today, `AnalyzedFunction` holds the AIR, but
there is no public `expr_id → Type` lookup that downstream tools can
use without re-walking. Phase 4 adds a minimal `expr_types:
HashMap<Span, Type>` (or equivalent) on `SemaOutput`, populated as a
no-cost byproduct of analysis. The cost is one `insert` per typed
expression during sema; the win is that hover becomes O(log n) instead
of "re-do sema in the LSP".

### Goto definition / find references

`textDocument/definition`:

1. Same span-to-AST-node lookup as hover.
2. If the node is an `Ident` used as a reference, walk through sema's
   symbol resolution to find the *defining* `Ident`'s span, which is
   the LSP's goto target.
3. For `@import("foo.gruel")` calls, resolve to the imported file's
   `Span::point_in_file(file_id, 0)`.

`textDocument/references`:

1. Same lookup; identify the defining `Ident`.
2. Walk the merged AST, collecting every `Ident` that sema resolved to
   this definition. We add an `ident_refs: HashMap<Spur,
   Vec<Span>>`-shaped side-table on `SemaOutput` (Phase 5), so the LSP
   doesn't replicate name resolution.

The side-tables are deliberately optional and lazily produced: a flag
on `SemaOutput` (`with_lsp_sidetables: bool`) controls whether sema
records them. The flag is off in normal compile paths so we don't pay
the cost in `gruel build`/`run`/`check`.

### Completion

Phase 4 ships a minimal completion model:

- **Trigger characters:** `.`, `@`, `:`, `(` (and the implicit identifier
  start).
- **Strategy:** find the AST context at the cursor. If we are inside an
  expression position and the previous token is a `.`, suggest fields
  and methods of the receiver's type (from sema). Otherwise list locals
  in scope (recovered from the enclosing function's RIR) plus all
  top-level items reachable from the current file (via the import
  graph and the symbol table sema built).
- **Item kinds:** `Function`, `Struct`, `Enum`, `EnumMember`, `Field`,
  `Variable`, `Keyword`, `Constant`, `Method`.
- **Docs:** for items that have a `Doc`, include
  `documentation: MarkupContent` (markdown) by re-running the same
  `gruel-doc` helper as hover.
- **Signature help** (`textDocument/signatureHelp`) is bundled with
  completion in Phase 4: when the cursor is after `(` or `,` inside a
  call, return the callee's signature.

What we explicitly punt on for the MVP: trait/interface method
suggestion across `Ref(I)`/`MutRef(I)` dispatch (needs more thinking
about fat-pointer lookup tables), `@import` path autocomplete (filesystem
walk + cache invalidation strategy), and snippet support for keywords
beyond a hand-crafted shortlist.

### Code actions

`textDocument/codeAction`:

- Iterate the diagnostics overlapping the requested `Range`.
- For each diagnostic, deserialise the `data.suggestions` (the
  `JsonSuggestion[]` we stashed in Phase 2).
- Emit `CodeAction { kind: QuickFix, edit: WorkspaceEdit { changes:
  {uri: [TextEdit{range, new_text}]}}, diagnostics: [diag] }`.
- `Applicability::MachineApplicable` actions are advertised with
  `isPreferred: true`; `MaybeIncorrect` and below are still offered but
  without preference.

This is essentially free given the rich diagnostic model already in
place. Every existing suggestion in the compiler immediately becomes a
fix in the editor.

### Inlay hints (stretch)

Phase 6 ships `textDocument/inlayHint`:

- After each `let` binding without an explicit type annotation, show
  the inferred type from sema (`: i32`).
- After unnamed call arguments, show the parameter name (`x: 42` →
  `: x` next to `42`).

Both reuse the `expr_types` and parameter-name tables already in
`AnalyzedFunction` and `SemaOutput`.

### What about tree-sitter inside the LSP?

The user-facing answer: editors that already use tree-sitter (Zed,
Helix, Neovim, Emacs+treesit) should keep using
`tree-sitter-gruel/queries/` directly for highlights/folds/locals —
that's faster and editor-native. The LSP does not duplicate any of
those features.

Internally, the LSP may optionally use the tree-sitter parser as an
acceleration:

- For **"what node is at this position"** during heavy keystroke
  bursts, a tree-sitter parse is incremental and produces a tree even
  when the chumsky parser would error out. Hover/goto can still
  respond on best-effort syntactic shape while the canonical sema
  compile is debounced.
- For **completion trigger context** (is the cursor after a `.`?), the
  tree-sitter CST gives a faster, more reliable answer than rerunning
  the canonical parser on a broken buffer.

These uses are **opportunistic** and not required for any phase below.
Phase 7 explores them; if they prove unnecessary, the tree-sitter
binding is simply not pulled in.

### Multi-file / workspace

The MVP (Phase 1) operates on the file the editor opened. Phase 5
extends this:

- On `initialize`, enumerate `*.gruel` under the workspace root,
  excluding `.git`, `target`, and gitignored files (`ignore` crate).
- The compile path always treats the workspace as a single
  `CompilationUnit` — same as `gruel build a.gruel b.gruel c.gruel`.
  Module resolution (`@import`) works because the file paths are
  passed through.
- Diagnostics from any file get published against that file's URI.
- `workspace/symbol` walks the merged AST and emits a
  `SymbolInformation` per top-level item.

No project file (`gruel.toml`, etc.) is required at this stage; the
workspace = "every `*.gruel` under root". Future package-manager work
(ADR-0026 "Future Work") will introduce a manifest, at which point the
LSP picks it up via a small `workspace.rs` change.

### Performance budget

Target: hover and goto under **150ms** on a 10k-LOC workspace after
warm cache. Diagnostics latency target: **500ms** from last keystroke
to red-squiggle update on the same workspace.

Mitigations available if these are missed:

- Parse cache already keys on content hash, so unchanged files are
  free (`parse_all_files_cached`).
- ADR-0074's sema cache short-circuits the most expensive pass for
  files whose `pub` signatures and bodies are unchanged.
- We can introduce a small in-memory LRU on top of the on-disk cache
  to avoid the disk hop for hot files.
- Cancellation + hard timeout (see Comptime section) bound the worst
  case. A user typing fast never waits on a stale compile because
  the in-flight pass is dropped, not awaited.
- Stale-while-revalidate keeps hover/goto live during a compile, so
  the *perceived* responsiveness is decoupled from compile latency
  for everything except diagnostics.

If we still cannot make it, that's a profiling-driven follow-up — the
architecture above does not require any specific optimisation to be
*correct*, only fast. The ZLS-style lightweight-analyzer escape
hatch (see Comptime section) is the last resort.

### CI / testing

- Unit tests inside `gruel-lsp::*` for: position conversion (round-trip
  with UTF-16 surrogate pairs), incremental text application, diagnostic
  → LSP mapping, span containment for hover lookup.
- Integration tests that spawn the in-process server (no subprocess) and
  drive a sequence of LSP messages, asserting on the responses. Pattern:
  one test per feature (`diagnostics_basic`, `hover_function`,
  `goto_definition_struct_field`, …). These live in
  `crates/gruel-lsp/tests/`.
- A small "smoke" integration test in `make test` that calls
  `gruel lsp --preview language_server` with a scripted message pipe.

### Drift-prevention differential

The chief long-term risk for the LSP is silent divergence from the
canonical compiler — the editor stops surfacing errors the CLI shows,
or shows errors the CLI doesn't. Most of the architectural decisions
above (thin façade, no shadow analyzer, side-tables populated *during*
sema rather than re-derived in the LSP) are motivated by this, but
architecture is a precondition, not a test. We add a **spec-corpus
diagnostic differential** in
`crates/gruel-lsp/tests/spec_corpus_diagnostic_differential.rs`,
mirroring the tree-sitter parser differential from ADR-0090:

1. For every spec test in `crates/gruel-spec/cases/` and UI test in
   `crates/gruel-ui-tests/cases/`, take the source and any required
   preview features.
2. Compile via the `gruel check` code path and collect
   `JsonDiagnostic[]` (already what `MultiFileJsonFormatter` emits).
3. Drive the same source through the in-process LSP backend
   (`Backend::did_open` + wait on the analysis worker), then collect
   every diagnostic the worker published to the client.
4. Normalize both sets to
   `(file, line, col, len, severity, code, primary_message)`, sort,
   and assert equality. Any diagnostic the CLI reports that the LSP
   doesn't (or vice versa) is a build failure.

The test is wired into `make test`. It catches:

- Refactors of `compile_frontend_with_options_*` whose LSP-side
  caller doesn't keep up.
- New diagnostic codes that the LSP mapping accidentally drops.
- Side-table population bugs that mask diagnostics on the LSP path.
- Preview-gate divergence (CLI-side check vs. LSP-entry-point check).
- `with_lsp_sidetables(true)` paths that perturb sema's diagnostic
  output (they must be additive only — same errors, plus side data).

What it deliberately does **not** test:

- LSP-only behaviour (cancellation, stale-while-revalidate, hover
  contents, code action presentation). Those have their own targeted
  integration tests.
- Notes/helps/suggestions content — the primary diagnostic
  `(code, range, message)` is the contract; relatedInformation and
  code-action `data` are normalized away because the LSP shape
  intentionally restructures them.
- Diagnostics emitted only under comptime timeout, which may differ
  if the LSP and CLI run with different step budgets — the
  differential pins both sides to the same budget.

If a future ADR introduces a ZLS-style lightweight analyzer (Open
Question 6), the differential's scope expands: every diagnostic the
lightweight analyzer produces must either match sema's exactly or
carry an explicit "approximate" marker that the differential
allowlists.

### Preview gating

```rust
// crates/gruel-util/src/error.rs
pub enum PreviewFeature {
    TestInfra,
    LanguageServer,   // NEW
}
```

- `name()` returns `"language_server"`.
- `adr()` returns `"ADR-0091"`.

The gate check lives in the LSP entry point
(`crates/gruel-lsp/src/lib.rs::run_server`): if the feature isn't
enabled, `run_server` returns an error before starting the message
pump.

## Implementation Phases

- [x] **Phase 1: Scaffolding + diagnostics**
  - Add `gruel-lsp` crate with `tower-lsp`, `tokio`, `dashmap`,
    `arc-swap`, `tokio-util` (for `CancellationToken`) deps.
  - Add `gruel lsp` subcommand in `crates/gruel/src/main.rs`.
  - Add `PreviewFeature::LanguageServer` and gate the entry point.
  - Implement `initialize`, `initialized`, `shutdown`,
    `textDocument/didOpen|didChange|didClose|didSave`.
  - Document store with incremental text sync and `LineIndex`.
  - Position ↔ byte conversion with UTF-16 + UTF-8 paths.
  - Debounced analysis worker calling
    `compile_frontend_with_options_full_target` over the current
    workspace files. Worker carries a `CancellationToken`, checks it
    between files and between sema passes, and bounds itself with a
    hard timeout (default 5s).
  - `ArcSwap<Snapshot>` for stale-while-revalidate; only successful
    or partially-successful compiles replace the snapshot.
  - `JsonDiagnostic` → `lsp_types::Diagnostic` mapping in
    `diagnostics.rs`; `client.publish_diagnostics` after every compile.
  - Integration tests: open a file with an error, assert diagnostic;
    fix it, assert clear; introduce a warning, assert publication.
  - Cancellation test: trigger a slow compile (large file with heavy
    comptime), send another `didChange` before it finishes, assert
    the first compile was cancelled and only the second's
    diagnostics get published.
  - Cross-process test: spawn `gruel build` against a tempdir cache
    while the in-process LSP backend is also using it; assert both
    produce identical results and the cache survives.

- [x] **Phase 2: Code actions for diagnostic suggestions**
  - Carry `JsonSuggestion[]` on the LSP `Diagnostic.data` field.
  - Implement `textDocument/codeAction` to convert suggestions into
    `CodeAction { kind: QuickFix, edit: WorkspaceEdit }`.
  - `Applicability::MachineApplicable` → `isPreferred: true`.
  - Integration test: trigger a known-suggestion diagnostic, accept the
    fix, re-compile, assert no diagnostic.

- [x] **Phase 3: Hover (signatures + docstrings, no expression types
      yet)**
  - Add `SmallestSpanFinder` AST walker in `gruel-lsp`.
  - For top-level items (`fn`, `struct`, `enum`, `interface`, `const`),
    return signature + `gruel-doc`-rendered markdown for the `Doc`
    field.
  - For type references in source, return the resolved type's display
    string.
  - Tests for each kind of item.

- [x] **Phase 4: Expression types, hover for locals, goto-definition,
      signature help**
  - Add `expr_types: HashMap<Span, Type>` side-table on `SemaOutput`,
    populated via a `with_lsp_sidetables(true)` builder bit so normal
    compile paths still skip the cost. (Implementation note:
    populated post-analysis by walking `AnalyzedFunction.air` —
    `AirInst` already carries both `span` and `ty`, so no Sema
    instrumentation is needed. The cost lives on the LSP path only.)
  - Hover for arbitrary expressions: read the side-table.
  - `textDocument/definition`: span → ident → defining span.
  - `textDocument/signatureHelp`: when cursor is inside a call,
    return the callee's parameter list with the active parameter index.
  - Integration tests across these four features.

- [ ] **Phase 5: Find references + workspace symbols + multi-file
      diagnostics**
  - Add `ident_refs: HashMap<DefId, Vec<Span>>` side-table on
    `SemaOutput`, gated by the same `with_lsp_sidetables` flag.
  - `textDocument/references` returns all refs (optionally including
    the definition per the LSP `includeDeclaration` flag).
  - On `initialize`, walk the workspace; build the merged compilation
    unit; publish diagnostics against every file with any.
  - `workspace/symbol` from top-level items.
  - Integration tests with two-file workspaces.

- [ ] **Phase 6: Completion + inlay hints**
  - Completion (Phase 4 scope) wired up.
  - Inlay hints for inferred-type `let` bindings and unnamed call args.
  - Integration tests for trigger-character completion, member access
    completion, and inlay hint rendering.

- [ ] **Phase 7: Polish + editor integration docs (defer tree-sitter
      acceleration and lightweight-analyzer unless profiling demands
      them)**
  - VS Code extension stub (a minimal `package.json` + `extension.ts`
    that launches `gruel lsp`).
  - Editor configuration recipes in `crates/gruel-lsp/README.md` for
    Helix, Neovim (built-in LSP), Zed, and Emacs (eglot).
  - `gruel lsp` self-doc: `--help` mentions the preview gate and the
    fact that highlights/folds are tree-sitter, not LSP.
  - If profiling shows responsiveness gaps under load, *then* wire the
    tree-sitter parser in as the "fast path" for syntactic queries
    (otherwise skip).
  - Stabilisation decision: when all six previous phases ship and tests
    are green, remove the `LanguageServer` preview gate.

## Consequences

### Positive

- **Live diagnostics in the editor.** The single biggest DX win in this
  ADR; every existing diagnostic, including all suggestions, becomes
  visible inline.
- **Quick fixes for free.** Because the compiler already carries
  `JsonSuggestion` with `Applicability`, the LSP exposes them as code
  actions without any compiler-side change.
- **No duplicate parsers.** The LSP uses the same chumsky parser the
  compiler uses; what the LSP says about your code is what `gruel
  build` will say about it.
- **Tree-sitter complements the LSP cleanly.** Editors get
  highlights/folds/symbols from tree-sitter (already shipped) and
  semantic info from the LSP. Neither needs to know about the other.
- **Side-tables are opt-in.** Sema only pays for hover/goto lookup
  structures when an LSP-shaped consumer asks for them; CLI builds
  remain unaffected.
- **Preview gating gives us room to iterate.** No commitment to wire
  shape, side-table layout, or completion strategy until stabilisation.

### Negative

- **New crate, new dependency footprint.** `tower-lsp`, `tokio`,
  `dashmap`, `lsp-types`, plus their transitive trees. Mitigated by
  keeping `gruel-lsp` out of the `gruel-compiler` build graph — only
  `crates/gruel` (the CLI binary) depends on it.
- **A second consumer of compiler internals.** Compile-frontend
  refactors now have two callers (CLI + LSP) instead of one. The risk
  is small because the LSP only uses public functions; if those
  signatures change, callers move in lockstep.
- **Side-tables grow `SemaOutput`.** The two new optional
  `HashMap`-shaped fields add memory cost when enabled. With the
  builder-bit flag, normal builds don't pay it.
- **UTF-16 conversion overhead.** Every LSP `Position` mapping does
  per-line UTF-16 math. Negligible in practice (lines are short) but
  measurable on pathological input. The `utf-8` position encoding
  negotiation removes it for capable clients.
- **Compile latency dictates perceived responsiveness.** If sema is
  slow on a 50k-LOC workspace, the LSP can't be faster than sema. The
  parse cache and per-file sema cache (ADR-0074) make this tractable
  but not free.

### Neutral

- The LSP server is a subcommand, not a separate binary. Single-binary
  install; one less artefact to publish. Should we ever want
  `gruel-lsp` as its own crate-name (for VSCode marketplace or similar
  reasons), it's a small split.
- `tower-lsp` is the conventional choice but not the only one
  (`lsp-server` / `async-lsp`). If `tower-lsp` proves unmaintained,
  swapping is a Phase-7-or-later concern, not a blocker.

## Open Questions

1. **`utf-8` position encoding default.** LSP 3.17 lets the server
   prefer UTF-8 if the client supports it; VS Code is the holdout that
   only supports UTF-16. Decision deferred to Phase 1.

2. **Workspace boundary for "open file" with no folder.** If the editor
   opens a single `.gruel` file with no `workspaceFolders`, we run in
   "single-file mode" — compile only that file, no cross-file
   resolution. Acceptable for the MVP; user-facing behaviour is "open
   the folder to get cross-file features."

3. **Cache directory for the LSP.** Should the LSP write to
   `target/gruel-cache/` (same as `gruel build`) or to a separate path
   (e.g. `target/gruel-lsp-cache/`) to avoid eviction races? Phase 1
   uses the build cache; revisit if benchmarks show eviction
   contention.

4. **`ident_refs` side-table shape.** Spans-keyed-by-`DefId` vs
   `Spur`-keyed are both workable; the right shape depends on how sema
   currently models definitions across `@import` boundaries. Phase 5
   pins it down.

5. **Cancellation granularity.** Phase 1 cancels between files and
   between sema passes. If real comptime evaluations turn out to run
   for multiple seconds *within a single function*, we'll need
   finer-grained cancellation inside the comptime interpreter. That
   would touch `gruel-air::sema::comptime`; deferred until we
   measure the need.

6. **Should hover latency under 200ms even cold-start be a hard
   requirement?** If yes, Phase 8 likely needs the ZLS-style
   lightweight analyzer (separate ADR). If no, we accept "first hover
   may take a beat; subsequent are fast." Decision deferred to
   profiling after Phase 4.

## Future Work

- **Refactorings beyond rename.** Extract function, extract variable,
  inline — these need compiler primitives (AST mutators, span
  preservation across rewrites) that aren't yet in place.
- **Formatter integration.** When a `gruel-fmt` exists, wire
  `textDocument/formatting` and `textDocument/rangeFormatting`.
- **DAP server.** Debugging support is a separate protocol and a
  separate ADR.
- **Per-cargo-style project model.** When the package manager lands,
  drive the workspace boundary from a manifest rather than directory
  walking.
- **Tree-sitter acceleration.** Use the in-tree grammar for syntactic
  position queries and completion-context detection if Phase 7 profiling
  shows it's worth the integration cost.
- **Split `gruel-lsp` into its own crate-name.** Once stable, possibly
  publish a `gruel-language-server` crate so editors can declare a
  conventional binary name.

## References

- [ADR-0005: Preview Features](0005-preview-features.md) — gating model
- [ADR-0023: Multi-file Compilation](0023-multi-file-compilation.md) —
  the compilation unit the LSP drives
- [ADR-0026: Module System](0026-module-system.md) — `@import`
  resolution shape
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md) — registry the
  LSP reads for hover on `@name(...)` calls
- [ADR-0074: Incremental Compilation](0074-incremental-compilation.md) —
  the cache the LSP rides on
- [ADR-0089: Doc Comments and `gruel doc`](0089-docstrings-and-docs-cli.md)
  — `Doc` field walked by hover and completion
- [ADR-0090: Tree-sitter and Parser Differential](0090-tree-sitter-and-parser-differential.md)
  — the grammar editors use for highlights/folds; deliberately
  complementary to this ADR
- [Language Server Protocol Specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)
- [`tower-lsp`](https://github.com/ebkalderon/tower-lsp)
- [rust-analyzer architecture notes](https://rust-analyzer.github.io/book/contributing/architecture.html)
  — overall shape this ADR borrows from; analyzer-duplication
  approach we deliberately don't take in Phase 1
- [ZLS (Zig Language Server)](https://github.com/zigtools/zls) — the
  closest analogue given Gruel's comptime, and the counter-example
  that motivates the "Comptime and responsiveness" section and the
  Phase-8 escape hatch
