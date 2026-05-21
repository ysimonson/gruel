# gruel-lsp

Language Server Protocol implementation for Gruel (ADR-0091). Runs as
`gruel lsp` over stdio.

## What this server provides

- **Diagnostics** (live errors and warnings, including code actions for
  `MachineApplicable` suggestions).
- **Hover** (signatures + `///` docstrings; falls back to inferred
  expression types from the AIR side-table).
- **Goto-definition** (top-level items, function params, let-bindings in
  the enclosing function).
- **Find references** (top-level items workspace-wide; locals scoped to
  the enclosing function body).
- **Workspace symbols** (`workspace/symbol`).
- **Completion** (`.`, `@`, `:`, `(` trigger characters; locals + top-level
  items + intrinsics + keywords).
- **Inlay hints** (inferred types after untyped `let`; parameter names
  before unnamed call arguments).
- **Signature help** inside calls.

## What this server does *not* provide

Editor highlights, folds, document outlines, indent rules, and
locals-aware identifier highlighting all live in the in-tree
[tree-sitter grammar](../../tree-sitter-gruel/) and are consumed
directly by editors that support tree-sitter — the LSP does not
duplicate them.

Formatting (no `gruel-fmt` yet), refactorings beyond rename, and the
debug adapter protocol are deferred.

## Editor configuration

### Neovim (built-in LSP)

```lua
-- ~/.config/nvim/init.lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = "gruel",
  callback = function()
    vim.lsp.start({
      name = "gruel-lsp",
      cmd = { "gruel", "lsp" },
      root_dir = vim.fs.dirname(vim.fs.find({ ".git" }, { upward = true })[1]),
    })
  end,
})
vim.filetype.add({ extension = { gruel = "gruel" } })
```

### Helix

`~/.config/helix/languages.toml`:

```toml
[[language]]
name = "gruel"
scope = "source.gruel"
file-types = ["gruel"]
roots = [".git"]
language-servers = ["gruel-lsp"]

[language-server.gruel-lsp]
command = "gruel"
args = ["lsp"]
```

### Zed

`~/.config/zed/settings.json`:

```jsonc
{
  "languages": {
    "Gruel": {
      "language_servers": ["gruel-lsp"]
    }
  },
  "lsp": {
    "gruel-lsp": {
      "binary": { "path": "gruel", "arguments": ["lsp"] }
    }
  }
}
```

### Emacs (eglot)

```elisp
(add-to-list 'auto-mode-alist '("\\.gruel\\'" . prog-mode))
(add-to-list 'eglot-server-programs '(gruel-mode . ("gruel" "lsp")))
```

### VS Code

A minimal extension stub lives at [`editors/vscode/`](./editors/vscode/).
Build it with `cd editors/vscode && npm install && npm run build`, then
load it via `code --extensionDevelopmentPath=$(pwd)`.

## Compilation unit (manifest vs isolation mode)

The LSP picks the compilation unit per analysis pass based on whether a
`gruel.json` manifest exists at the workspace root (ADR-0092):

- **Manifested mode** — `gruel.json` is present at the workspace root
  and successfully loaded. The LSP analyzes one compilation unit: the
  manifest's `target.root` entry file plus every file transitively
  reachable through `@import("...")`. Sibling `.gruel` files that
  aren't reached through the import graph produce no diagnostics — they
  are simply not part of this program. Open-buffer text overrides the
  on-disk file for any file in the closure, so in-flight edits feed the
  resolver.
- **Isolation mode** — no manifest, or the manifest fails to load.
  Each open editor buffer is its own compilation root: the LSP parses
  it, walks its imports, and runs sema on that closure independently.
  Two unrelated `fn main()` files in the same workspace don't get
  merged. This is the default behaviour with no manifest required; the
  isolation fix itself is *not* preview-gated.

### When the manifest changes

The LSP subscribes to `workspace/didChangeWatchedFiles`. Any event
whose path ends in `gruel.json` triggers a reload of the manifest and
a fresh analysis pass. Editors that don't proactively send watched-file
events will pick up manifest changes on the next compile.

### Manifest schema

The supported keys are:

- `name` — non-empty string (used as the default output binary name
  for `gruel build`).
- `version` — semver string (`semver::Version` rules).
- exactly one of `bin` / `lib` — an object with a `root` field naming
  the entry `.gruel` file (relative path, must be a real file under
  the manifest's directory).

Unknown top-level keys and unknown keys inside `bin` / `lib` are
rejected — by design, so the schema accretes via explicit ADRs rather
than ambient drift.

Example:

```json
{
  "name": "hello",
  "version": "0.1.0",
  "bin": { "root": "src/main.gruel" }
}
```

## Architecture

The server is a thin façade over `gruel-compiler`'s public API. A tokio
runtime hosts:

1. A `DashMap<Url, DocState>` document store with incremental text sync.
2. A debounced analysis worker that re-runs the frontend (lex → parse →
   merge → sema) on every keystroke burst, carrying a `CancellationToken`
   and a hard 5-second timeout.
3. An `ArcSwap<Snapshot>` that holds the most recent successful AST +
   interner + per-instruction `(span, type)` map. Read handlers
   atomically load it without blocking writers.

The same compile call as `gruel check` runs in the worker — so the LSP
and the CLI cannot drift on diagnostics. See ADR-0091 for the full
design discussion.
