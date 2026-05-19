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
