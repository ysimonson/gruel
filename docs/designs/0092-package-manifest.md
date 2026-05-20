---
id: 0092
title: Package Manifest (`gruel.json`)
status: proposal
tags: [tooling, lsp, manifest, packaging, dx]
feature-flag: package_manifest
created: 2026-05-19
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0092: Package Manifest (`gruel.json`)

## Status

Proposal

## Summary

Introduce a minimal `gruel.json` manifest file at the workspace root
that declares a Gruel package's `name`, `version` (semver), and
exactly one of `bin` or `lib` — a target object whose `root` field
points at the entry `.gruel` file. The Gruel LSP
reads it to determine the compilation unit (entry-point-driven, with
`@import` followed lazily by sema); the CLI (`gruel build`, `run`,
`check`, `doc`) reads it when invoked with no positional source
argument, using `root` as the entry file. In the absence of a manifest, the LSP
falls back to analyzing each opened Gruel file in isolation, fixing
the architectural issue documented in [ADR-0091](0091-language-server.md).
The manifest is deliberately tiny; this ADR does **not** introduce a
package manager, dependency resolution, or remote registries.

## Context

### What we have today

- ADR-0026 established a "file is a struct" module model: each
  `.gruel` file is an anonymous struct, and `@import("path.gruel")`
  is the sole cross-file lookup. Sema resolves imports lazily — given
  an entry file, it pulls in only the modules that are actually
  referenced.
- ADR-0091 shipped the LSP as a thin façade over `gruel-compiler`'s
  frontend. The LSP enumerates **every** `*.gruel` file under the
  workspace root (via `workspace::enumerate_gruel_files`) and feeds
  them to the frontend as one merged unit. This matches the
  pre-module ADR-0023 flat-multi-file CLI mode, not the ADR-0026
  module model.
- The compiler still has both modes. `parse_all_files_with_preview` +
  `merge_symbols` produces the flat namespace; passing a single entry
  file plus letting sema resolve `@import` produces the module-model
  view.

### What we want from a manifest

Tiny. A name, a kind, an entry file, a version. Enough for the LSP to
say "this is one compilation unit, here is its root, follow its
imports." Enough for the CLI to let a user type `gruel build` in the
package directory instead of `gruel build src/main.gruel`. **Nothing
more.** No dependencies section, no registry URL, no lockfile, no
build scripts, no features matrix. Anything beyond the handful of
fields above is out of scope; future ADRs will introduce dependency
resolution,
workspace inheritance, lockfiles, and friends as separate concerns.

### Why JSON

`package.json` (npm), `tsconfig.json`, `deno.json` — JSON manifests
have proven serviceable for these "tiny declarative metadata" cases.
The handful of fields here don't benefit from TOML's tables or
comments. JSON
parsing via `serde_json` is already in the workspace; no new
dependency is needed. ADR-0026 mentioned `gruel.toml` as a future
sketch, but that was speculative shape, not a binding decision —
we choose JSON deliberately here.

### Why now

The LSP has stabilised (ADR-0091's preview gate is removed) and editor
users are hitting the flat-namespace bug as soon as they open a Gruel
workspace with more than one independent program. Without this fix,
the LSP is unusable on this very repository. A minimal manifest also
unblocks future package-manager work (ADR-0026 Future Work) by
locking down the shape of the per-package metadata file before
dependency resolution piles on.

### What this ADR explicitly does *not* cover

- **Dependency resolution.** No dependencies field, no URL or
  path deps, no registry, no lockfile. The compiler resolves
  `@import` as it does today (relative paths from the importing
  file, then prelude / std search paths).
- **Workspaces / monorepos.** Exactly one `gruel.json` per workspace
  root. A future ADR can add a workspace member list or multi-package
  discovery once we have a real reason to.
- **Build configuration.** No optimisation level, no target triple,
  no linker. The CLI keeps reading those from flags; the manifest is
  inert metadata.
- **Library artifact emission.** A manifest with a `lib` block is a
  *declaration of intent* (no `main` required, can't `run`) — it
  does not yet produce `.so` / `.a` artefacts. A future ADR can add
  that.
- **Multiple targets in one manifest.** This ADR allows exactly one
  of `bin` or `lib` per manifest. A future ADR can introduce
  multiple bins or a `[examples]` block; the per-target object
  shape leaves room for that without further schema churn.

## Decision

### Schema

```json
{
  "name": "my-package",
  "version": "0.1.0",
  "bin": { "root": "src/main.gruel" }
}
```

or

```json
{
  "name": "math",
  "version": "0.2.3",
  "lib": { "root": "src/_math.gruel" }
}
```

| Field      | Required             | Type                           | Notes                                                                                       |
|------------|----------------------|--------------------------------|---------------------------------------------------------------------------------------------|
| `name`     | yes                  | string                         | Display name; defaults to the binary output name (`-o`) when `gruel build` is run with no `-o`. No charset rules in this ADR beyond "non-empty"; future package-manager ADR can constrain. |
| `version`  | yes                  | string (semver per semver.org) | Validated against the `semver::Version` parser. Stored but otherwise inert in this ADR.     |
| `bin`      | exactly one of these | object (see below)             | Marks this package as a binary. The package's compilation unit starts at `bin.root`.        |
| `lib`      | exactly one of these | object (see below)             | Marks this package as a library. The package's compilation unit starts at `lib.root`.       |

**Target object** (`bin` / `lib`):

| Field   | Required | Type                   | Notes                                                                                       |
|---------|----------|------------------------|---------------------------------------------------------------------------------------------|
| `root`  | yes      | string (relative path) | Path to the entry `.gruel` file, **relative to the manifest's directory**. Must exist on disk at load time. |

The package's *kind* is encoded by which key is present, not by a
separate `type` field. This is a deliberate choice over the earlier
`type: "binary"` / `type: "library"` shape: the per-target object
gives future fields (artefact name, output dir, conditional preview
flags, etc.) a natural home without re-shaping the schema.

It is a **load error** to provide neither key, both keys, or any
unknown top-level key. Unknown fields *inside* the target object are
also rejected. We add fields as we need them, not in anticipation.

### Discovery

**LSP**: on `initialize`, the server looks for `gruel.json` at the
workspace root path (`workspaceFolders[0]` → `rootUri` →
`GRUEL_LSP_ROOT` → CWD, in the same order
`crates/gruel-lsp/src/server.rs` already uses). It does **not** walk
upward and does **not** scan subdirectories — exactly one manifest,
exactly at the root. If the file is missing, malformed, or fails
validation, the LSP logs a warning via `client.log_message` and falls
back to the isolation behaviour (see "LSP behaviour" below).

**CLI**: when `gruel build` / `run` / `check` / `doc` is invoked
with **no** positional source arguments, the CLI searches CWD,
then walks **up** through ancestor directories, taking the first
`gruel.json` it
finds. This is npm-style — familiar, and necessary because users
don't always run the CLI from the package root (e.g. `cd src && gruel
run`). The package's directory is the directory containing the
manifest; the `root` path is resolved against it.

When the **first positional** is given, the CLI classifies it by
shape and resolves accordingly — a path may point at a single Gruel
file, a manifest file, or a package directory:

| Shape of first positional                                  | Behaviour                                                                |
|------------------------------------------------------------|--------------------------------------------------------------------------|
| `*.gruel` file                                             | Existing single-file behaviour. Manifest is **not** consulted.           |
| File named `gruel.json`                                    | Load it as a manifest via `load_at`. No discovery walk.                  |
| Directory                                                  | Load `<dir>/gruel.json` via `load_at`. No discovery walk.                |
| Anything else (non-existent path, other extension, …)      | Error: `"unsupported source: expected *.gruel, gruel.json, or a directory containing one"`. |

`gruel build` / `run` accept any of the four (so a user can write
`gruel run ./my-package`, `gruel run ./my-package/gruel.json`, or
`gruel run ./src/main.gruel`). `gruel check` follows the same rules
for consistency. Additional positionals after the first remain valid
**only** for the `.gruel`-file shape — that's the existing
multi-file legacy CLI from ADR-0023, untouched. Manifest-shaped
positionals reject extra positionals with a clear error.

When the first positional resolves to a manifest (either file or
directory), the resulting compile is *exactly* the same as if the
manifest had been discovered upward from CWD — same `bin`/`lib`
rules, same `root` resolution, same default output name. Explicit
and implicit manifest paths converge on the same code path.

### `gruel-manifest` crate

A new workspace member `crates/gruel-manifest/`:

```
crates/gruel-manifest/
├── Cargo.toml             # deps: serde, serde_json, semver, thiserror
├── src/
│   └── lib.rs             # Manifest, PackageType, ManifestError,
│                          # discover(), load_at(), validate()
└── tests/
    └── parse.rs           # unit tests for happy path and validation
```

Public API (roughly):

```rust
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: semver::Version,
    pub target: PackageTarget,
    pub manifest_dir: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum PackageTarget {
    Binary(TargetSpec),
    Library(TargetSpec),
}

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub root: PathBuf,    // absolute, resolved against manifest dir
}

impl PackageTarget {
    pub fn root(&self) -> &Path { /* delegate */ }
    pub fn is_binary(&self) -> bool { /* match */ }
    pub fn is_library(&self) -> bool { /* match */ }
}

pub fn load_at(manifest_path: &Path) -> Result<Manifest, ManifestError>;
pub fn discover_upward(start: &Path) -> Option<PathBuf>;     // CLI
pub fn discover_at_root(root: &Path) -> Option<PathBuf>;     // LSP
```

On the deserialisation side, `bin` and `lib` are sibling
`Option<TargetSpec>` fields on an internal `RawManifest`. The loader
asserts exactly one is `Some`, then folds them into the
`PackageTarget` enum. We avoid `#[serde(tag = "type", ...)]` because
it requires the discriminator to be a sibling string, which is the
shape we just rejected.

`ManifestError` carries a structured error kind (`Io`, `Parse`,
`UnknownField`, `MissingField`, `MissingTarget` (neither `bin` nor
`lib`), `ConflictingTargets` (both `bin` and `lib`), `BadVersion`,
`BadRoot`, `RootNotFound`) and a `Display` impl that produces a
user-facing message; the CLI prints it directly and the LSP routes
it through `client.log_message`. We do **not** route manifest errors
through the `gruel-error` diagnostic infrastructure — they aren't
compilation diagnostics, they're configuration errors, and they want
different formatting.

Manifest loading is deliberately cheap (single file, JSON parse), so
we re-read it on every CLI invocation and on each LSP compile. No
caching layer in this ADR.

### LSP behaviour

The `analyze` worker in `crates/gruel-lsp/src/analysis.rs` currently
takes a `Vec<WorkspaceFile>` and merges them. We change the input
shape and the file gathering:

1. **On `initialize`** the server tries
   `gruel_manifest::discover_at_root(workspace_root)`. The
   `Backend` grows `manifest: Arc<ArcSwap<Option<Manifest>>>` so the
   loaded value is read atomically by every compile.

2. **On every compile**, the analysis worker picks one of two modes:

   - **Manifested mode** (`manifest.is_some()`): the entry is the
     manifest's `root`. The worker hands the compiler **only** that
     one file as its `SourceFile`. Sema's existing lazy `@import`
     resolver walks the import graph from there, pulling in files
     from disk (or from the open-buffer overlay) as it goes. The
     overlay layer ensures `@import("foo.gruel")` reads the user's
     in-flight buffer when foo is open, not the on-disk text.
   - **Isolation mode** (`manifest.is_none()`): each open document
     is analyzed independently. The worker iterates over every open
     buffer and runs sema once per buffer, treating that buffer as
     the entry. Diagnostics are union'd before publishing. Files
     that aren't open get no diagnostics published against them.

3. **Snapshot**: in both modes the snapshot still holds a single
   merged `Ast` so hover / goto / references / completion / inlay
   hints keep working. In isolation mode the merge is naïve — we
   concatenate per-buffer ASTs into one container — and accept that
   cross-file goto won't resolve, because there is no cross-file
   relationship to resolve. In manifested mode the snapshot's AST
   is exactly what sema produced from the entry-point walk, which is
   the *correct* view of the program.

4. **File overlay for `@import`**: sema's import resolver reads from
   disk today. We introduce a small "open-buffer overlay" hook on
   `gruel-compiler` so the LSP can substitute its current
   in-memory text for any file that has been `did_open`'d. The hook
   is a `Fn(&Path) -> Option<String>` injected via a new options
   field; CLI callers leave it `None` and behaviour is unchanged.
   This change is *additive* — no existing call site needs to
   thread anything through.

5. **`workspace/didChangeWatchedFiles`** receives change
   notifications for `gruel.json`. On any event the LSP reloads
   the manifest and re-queues a compile. This is the only place we
   actively watch the manifest; otherwise it's read once at
   `initialize` and we trust the file watch to keep it fresh.

#### What this fixes (concrete)

Open this repository in an editor today and `crates/gruel-spec/cases/`
and `scratch/` and `examples/` all collapse into one giant
"duplicate `fn main`" mountain. After this ADR:

- If the repo has no `gruel.json`, **isolation mode** kicks in.
  Every spec case in `scratch/test.gruel` is analyzed on its own
  exactly the way the spec-corpus differential already does it. Zero
  false duplicates.
- If the repo grows a `gruel.json` pointing at a specific entry
  file (e.g. `crates/gruel/src/main.rs` analogue inside a Gruel
  workspace), **manifested mode** analyzes that one program and
  whatever it imports. Other files in the workspace are out of
  scope and don't produce diagnostics until they're reached via
  `@import` or opened in the editor.

This is the behaviour an editor user actually wants.

### CLI behaviour

For each of `gruel build` / `run` / `check` / `doc`, the resolver
runs a single classification step on the first positional (if any)
and funnels into one of three branches:

1. **No positional**:
   - Call `discover_upward(cwd)`.
   - If `None`, error: `"no source file specified and no gruel.json
     found in current or any ancestor directory"`. (Same exit code as
     today's missing-source error.)
   - If `Some(path)`, fall through to the manifest branch below.

2. **First positional is a `.gruel` file**:
   - Existing single-file (or multi-file, when more positionals
     follow) behaviour. Manifest machinery is bypassed entirely.

3. **First positional is a file named `gruel.json` or a directory**:
   - Reject any further positionals: `"manifest-based invocation
     accepts only one positional; got <N>"`.
   - Determine the manifest path: the positional itself if it's a
     file, or `<dir>/gruel.json` if it's a directory. The directory
     case checks for existence and errors with `"no gruel.json
     found at <dir>"` if missing.
   - `load_at(manifest_path)`. On error, print the manifest error
     and exit non-zero.

4. **Anything else** (non-existent path, other extension):
   - Error: `"unsupported source: expected *.gruel, gruel.json, or
     a directory containing one"`.

Once a manifest is in hand (from branch 1 or 3):

- For `build` / `run`: require `PackageTarget::Binary`. If
  `PackageTarget::Library`, error: `"package 'foo' is a library
  (no 'bin' in gruel.json); cannot build or run. Use 'gruel check'
  or pass an explicit source file."`. When valid, the implicit
  entry is the target's `root` and the implicit output name (when
  `-o` is omitted on `build`) is `manifest.name`.
- For `check`: accept either target kind. Entry is the target's
  `root`.
- For `doc`: accept either target kind (libraries are arguably the
  *primary* doc target). Entry is the target's `root`; the rendered
  site goes to `--output-dir` (default `target/doc/`) exactly as
  today. `gruel-doc` runs the parser only (no sema, no main
  requirement), so this works identically for `bin` and `lib`
  packages. Following `@import` transitively to render every
  imported file is **not** part of this ADR — the manifest hands
  `run_doc` exactly one source path (the target's `root`) and the
  existing renderer takes it from there. Walking the import graph
  to produce a multi-file doc site is a `gruel-doc` enhancement
  that can land independently.

Existing `BuildArgs.output` handling is unchanged for the
`.gruel`-file branch. The manifest's `name` only fills in a default
when `-o` is omitted on the manifest branch. (Old "2 positionals =
source + output" shorthand still works exactly as before on the
`.gruel`-file branch.)

The CLI does **not** add a `--manifest` flag in this ADR — the
overloaded positional covers explicit-path use cases. Future ADRs
can add `--manifest /path/to/gruel.json` or a `--package <name>`
selector when the workspace story grows beyond one package per
directory tree.

### Preview gating

```rust
// crates/gruel-util/src/error.rs
pub enum PreviewFeature {
    TestInfra,
    PackageManifest,  // NEW
}
```

- `name()` returns `"package_manifest"`.
- `adr()` returns `"ADR-0092"`.

Gate placement is **explicit at the discovery sites**, not buried in
sema (the manifest never reaches sema):

- LSP: `crates/gruel-lsp/src/server.rs::Backend::initialize` calls
  `discover_at_root` only when the preview features include
  `PackageManifest`; otherwise it skips manifest discovery entirely
  and runs **the new isolation mode regardless**. Reasoning: the
  isolation-mode fix is the most user-visible bug repair this ADR
  carries; we want it on by default even before the manifest itself
  stabilises.

  In other words: only the manifest-driven *behaviour* is gated.
  Isolation-mode replaces the current flat-merge enumeration
  unconditionally on the LSP path. We document this carve-out in
  the preview gate's `description` so future-us doesn't get
  surprised.
- CLI: `resolve_build`, `resolve_run`, `resolve_check` only consult
  `discover_upward` when `--preview package_manifest` is set. Without
  the flag, the existing "no source file specified" error is
  preserved exactly.

When the manifest feature stabilises (Phase 5 below), we delete the
gate entirely.

### Validation rules

1. `name`: non-empty string. No charset rules yet; future
   package-manager ADR can constrain (URL safety, length, reserved
   names).
2. `type`: exactly `"library"` or `"binary"`. Case-sensitive; we
   don't accept `"Library"` or `"BIN"`. Strict-on-case keeps the
   schema teachable.
3. `root`: a relative path (we reject absolute paths so manifests
   stay portable across machines). Resolved against the manifest's
   directory; the resulting path must exist as a regular file and
   have a `.gruel` extension. We reject paths containing `..` *only*
   if they escape the manifest's directory (i.e. the canonicalised
   root is not under `manifest_dir`). Same-directory siblings reach
   via `./other.gruel`.
4. `version`: parsed via `semver::Version::parse`. We accept
   prerelease and build metadata (`0.1.0-alpha.1+build.7`); the
   field is otherwise inert.
5. **Unknown fields are rejected** via
   `#[serde(deny_unknown_fields)]`. Typo'd fields surface as a clear
   parse error.

### Examples

```json
{
  "name": "hello",
  "version": "0.1.0",
  "bin": { "root": "src/main.gruel" }
}
```
`gruel run` in the package directory compiles and runs `src/main.gruel`.
Equivalent invocations from elsewhere:

```sh
gruel run ./path/to/hello                  # directory containing gruel.json
gruel run ./path/to/hello/gruel.json       # the manifest file directly
```

The LSP analyzes `src/main.gruel` + every file it `@import`s
(transitively).

```json
{
  "name": "math",
  "version": "0.2.3-rc.1",
  "lib": { "root": "src/_math.gruel" }
}
```
`gruel build` errors (library packages can't build). `gruel check`
validates `src/_math.gruel` and its imports. LSP analyzes the same
set.

```jsonc
// Invalid: both bin and lib present
{
  "name": "x",
  "version": "0.1.0",
  "bin": { "root": "main.gruel" },
  "lib": { "root": "lib.gruel" }
}
```
Loader rejects with `ConflictingTargets: only one of 'bin' or 'lib' may
be present`.

```jsonc
// Invalid: unknown field
{
  "name": "x",
  "version": "0.1.0",
  "bin": { "root": "main.gruel" },
  "license": "MIT"
}
```
Loader rejects with `unknown field 'license'`. Encourages additive ADRs
rather than ambient drift.

## Implementation Phases

- [x] **Phase 1: Manifest crate**
  - New `crates/gruel-manifest` workspace member. Adds `serde`,
    `serde_json`, `semver`, `thiserror` deps (first time `semver`
    enters the tree; small, focused crate).
  - `Manifest`, `PackageTarget`, `TargetSpec`, `ManifestError`
    types.
  - `load_at`, `discover_upward`, `discover_at_root` functions.
  - Validation: existence, extension, escape check, semver parse,
    `deny_unknown_fields`.
  - Unit tests for happy path and each validation error.

- [x] **Phase 2: Preview gate + CLI integration**
  - Add `PreviewFeature::PackageManifest` (snake_case `package_manifest`)
    to `gruel-util::error`; update `name()`, `adr()`, `all()`, and the
    `FromStr` impl.
  - `crates/gruel/src/main.rs`: in `resolve_build` / `resolve_run` /
    `resolve_check` / `resolve_doc`, classify the first positional
    via the four-branch rule in "CLI behaviour":
    - empty → `discover_upward(cwd)`;
    - `*.gruel` → existing single-file path;
    - `gruel.json` file → `load_at(file)`;
    - directory → `load_at(dir.join("gruel.json"))`;
    - other → error.
    Reject extra positionals on the manifest branches. Translate to
    `BuildOpts` / `RunOpts` / `CheckOpts` / `DocOpts` with the
    target's `root` and (for build) `name` as the default output.
    All four manifest entry points (CWD discovery, explicit file,
    explicit dir, ancestor discovery) share one `apply_manifest()`
    helper so they cannot drift apart.
  - `PackageTarget::Library` rejected by build/run with the
    user-facing message defined above; `check` and `doc` accept
    both target kinds.
  - CLI integration tests covering: implicit (CWD) discovery,
    `gruel build <dir>`, `gruel build <dir>/gruel.json`,
    `gruel build src/main.gruel` (legacy path still works), explicit
    library-target rejection on build/run, the "extra positional on
    manifest branch" error, and `gruel doc` against both a `bin`
    and a `lib` package (asserts the rendered output mentions
    `manifest.name` in the page title).

- [x] **Phase 3: Open-buffer overlay in `gruel-compiler`**
  - Add an `import_overlay: Option<Arc<dyn Fn(&Path) -> Option<String>
    + Send + Sync>>` option (name TBD; could be a small trait if
    `dyn Fn` ergonomics get ugly) on the public compile entry points
    consumed by the LSP. Sema's `@import` resolver checks the overlay
    before falling back to `fs::read_to_string`.
  - Default `None` preserves CLI behaviour exactly.
  - Test: a synthetic overlay that substitutes one file's text; assert
    the substituted text reaches sema.

- [x] **Phase 4: LSP integration**
  - `crates/gruel-lsp/src/workspace.rs`: add
    `discover_manifest(root)` that wraps `gruel_manifest`.
  - `Backend::initialize`: discover, store in `manifest:
    Arc<ArcSwap<Option<Manifest>>>`. Log on missing/invalid.
  - `Backend::workspace_did_change_watched_files` (or
    `did_change_watched_files`): re-discover on `gruel.json` events,
    re-queue compile.
  - `crates/gruel-lsp/src/analysis.rs::analyze`: split into two paths
    based on `manifest` presence — `analyze_from_entry` for the
    manifested case, `analyze_isolated` for the no-manifest case.
    Both build the same `Snapshot` shape so request handlers don't
    need to know which mode produced the snapshot.
  - The current `gather_workspace_files` is replaced (or renamed) by
    a function that, in isolation mode, returns only open buffers
    (no on-disk enumeration); in manifested mode, returns just the
    entry file plus an overlay closure capturing open buffers.
  - Integration tests:
    - **manifested**: workspace with `gruel.json` + `src/main.gruel`
      that `@import`s `src/util.gruel`. Open `main.gruel`, expect
      diagnostics from both. Introduce a duplicate `fn main` in
      `scratch/test.gruel` (NOT imported) — assert no diagnostic.
    - **isolation**: workspace without `gruel.json`, two files each
      defining `fn main`. Open both. Assert each gets its own
      diagnostics (or none) and no cross-file duplicate-definition
      error.
    - **watch**: open a workspace without a manifest, write
      `gruel.json` to disk, send `workspace/didChangeWatchedFiles`,
      assert the LSP switches to manifested mode and produces
      different diagnostics.
  - Update the spec-corpus diagnostic differential
    (`crates/gruel-lsp/tests/spec_corpus_diagnostic_differential.rs`):
    it currently runs each spec case in isolation through the LSP
    backend. With this change, isolation mode is the LSP's
    default-when-no-manifest, so the differential continues to hold;
    we keep the test as-is but add a fixture variant that exercises
    manifested mode against a tiny entry-file scenario, just to
    confirm both modes agree on diagnostics for the case where they
    *should* produce the same result.

- [ ] **Phase 5: Documentation + stabilisation**
  - `crates/gruel-lsp/README.md`: document discovery, watched-file
    behaviour, isolation fallback.
  - `crates/gruel/src/main.rs --help` / clap docstrings: mention the
    implicit-entry path for each subcommand.
  - Update the ADR-0091 "Known architectural issue" callout in
    CLAUDE.md to point at this ADR as the fix.
  - Remove the `PackageManifest` preview gate. Update this ADR's
    status to `implemented` and its frontmatter `implemented:` date.
  - Note in `crates/gruel-lsp` that isolation mode is unconditional
    (no preview gate); document it as the LSP's no-manifest default.

## Consequences

### Positive

- **Fixes the LSP's "open the whole repo and watch it explode" bug.**
  Isolation mode alone, even without a manifest, makes the LSP usable
  on workspaces with multiple independent programs (scratch dirs,
  spec corpora, examples).
- **One-line ergonomic win for CLI users.** `cd my-package && gruel
  run` is friendlier than `gruel run src/main.gruel`. Same gain for
  `build` and `check`.
- **Locks the manifest shape early.** Future ADRs can add fields
  additively without renegotiating what a package *is*. The four
  required fields cover everything we need today.
- **`deny_unknown_fields` keeps the surface honest.** If a field
  shows up in the wild that the spec doesn't sanction, the loader
  rejects it loudly. We don't accumulate folklore.
- **Aligns the LSP with ADR-0026's module model.** Entry-point-driven
  analysis with lazy `@import` resolution is what sema is built to
  do; the LSP just stops feeding it the wrong inputs.

### Negative

- **Library packages don't yet build artefacts.** A `lib` block is
  a *declaration* (no main, can't build/run) until a future ADR
  adds artefact emission. Users get a clear error message in the
  meantime, but the asymmetry is real.
- **Isolation-mode compile cost on huge workspaces.** If a user has
  100 open buffers and no manifest, the LSP runs sema 100 times per
  debounce. In practice editors keep open-buffer counts small, but
  pathological cases exist. We can layer a per-file sema cache
  (ADR-0074 already caches per-file sema) and a "only re-analyze
  the buffer that just changed" optimisation if measured pain
  warrants. Out of scope here.

### Neutral

- **No relationship to the parse cache.** Manifest discovery doesn't
  participate in ADR-0074's content-addressed cache; it's a single
  small file read on demand.
- **Editor support is per-LSP-client.** Clients that don't send
  `workspace/didChangeWatchedFiles` won't auto-refresh on manifest
  edits; they'll pick up changes on the next compile. Acceptable.
- **The schema is JSON, not TOML.** ADR-0026 sketched `gruel.toml`
  in passing. We override that sketch deliberately and document the
  reasoning above.

## Open Questions

1. **Should the LSP isolation-mode fix be split into its own ADR?**
   The bug repair is independently valuable; we could land it
   immediately and follow up with the manifest separately. The
   counter-argument: isolation mode is the natural complement to
   manifested mode, and shipping them together avoids a churn in the
   `analyze` worker's shape twice in a row. Tentatively keep them
   together; flag in code review.

2. **`name` charset constraints.** This ADR allows any non-empty
   string. A future package-manager ADR will need URL-safe / filesystem-safe
   constraints (probably `^[a-z][a-z0-9_-]*$` or similar). Deferring
   because the constraint is meaningless without publication infrastructure.

3. **What about a `tests/` or `examples/` convention?** Some
   languages let a manifest declare auxiliary entry points (Cargo's
   `[[bin]]`, `[[example]]`). This ADR ships exactly one root. A
   future ADR can add multiple entry points; for now, users with
   multiple programs split them into multiple workspaces (one
   package per directory).

4. **`lib` targets and `check`.** Phase 2 says `check` accepts
   either `bin` or `lib` targets. Open: does sema require a `main`
   for a library entry? If yes, we either need a sema-level
   `--no-main` flag or the library entry has to be a file that
   doesn't reference `main` anywhere. Verify during Phase 2
   implementation; if sema rejects library entries today, file a
   small fix as part of the phase or document the constraint.

5. **Should `discover_upward` cap at a marker file (e.g. `.git`) the
   way Cargo caps at the workspace root?** Without a cap,
   `discover_upward` keeps walking to `/`. In practice that's fine
   (a stray `gruel.json` at `$HOME` would be surprising), but a
   cap might prevent embarrassment. Defer; if real pain emerges,
   add it with a follow-up.

## Future Work

- **Dependency resolution.** A dependencies block, a registry
  protocol, content-addressed packages, a lockfile. Separate ADR(s).
- **Workspaces / monorepos.** Multiple packages per repo, member
  inheritance, workspace-level dependency resolution. Separate ADR.
- **Library artefact emission.** `gruel build` on a library produces
  a `.so` / `.a` / `.gruelib` (TBD). Separate ADR.
- **Multiple entry points.** `[[bin]]` / `[[example]]` / `[[test]]`
  style. Separate ADR; would also touch the test infrastructure.
- **Manifest charset constraints.** Naming rules for the registry
  era.
- **`gruel emit` / `gruel cache` manifest awareness.** Mechanical
  extensions once the manifest is the default entry-point source
  for `build` / `run` / `check` / `doc`.
- **`--manifest` and `--package` CLI flags.** Useful once workspaces
  contain more than one package.

## References

- [ADR-0005: Preview Features](0005-preview-features.md) — gating model
- [ADR-0023: Multi-file Compilation](0023-multi-file-compilation.md) —
  the flat-multi-file mode being phased out for editor analysis
- [ADR-0026: Module System](0026-module-system.md) — "file is a
  struct" + lazy `@import` resolution; the model the LSP should
  align with
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md) — pattern
  for "tiny declarative metadata in a focused crate"
- [ADR-0074: Incremental Compilation](0074-incremental-compilation.md)
  — per-file sema cache that isolation mode can amortise on
- [ADR-0091: Language Server](0091-language-server.md) — the LSP
  this ADR fixes; "Multi-file / workspace" section called out the
  manifest as future work
- [npm `package.json` reference](https://docs.npmjs.com/cli/configuring-npm/package-json/)
  — schema heritage and naming convention
- [`semver` crate](https://docs.rs/semver) — version parsing
