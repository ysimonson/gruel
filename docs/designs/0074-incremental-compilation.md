---
id: 0074
title: Incremental Compilation with File-Level Caching
status: proposal
tags: [architecture, compiler, performance, build-system]
feature-flag: incremental_compilation
created: 2026-05-02
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0074: Incremental Compilation with File-Level Caching

## Status

Proposal

## Summary

Add a persistent on-disk cache that lets the compiler skip the entire frontend (lex, parse, RIR, sema, comptime evaluation, generic monomorphization) and the per-function AIR→bitcode translation when inputs haven't changed since the last build. The cache is keyed by content hashes plus a stable "signature hash" of each file's `pub` exports, so editing a function body invalidates only that file's pipeline up to bitcode, while editing a `pub` signature invalidates downstream importers. The LLVM optimizer and back-end always run on the assembled whole-program module — they are not cached. The biggest user-visible speedup is on comptime-heavy code, where the cached frontend captures the dominant cost. Caching is opt-in via a `--preview incremental_compilation` flag during stabilization, and lives entirely on the local filesystem (no shared/CI cache in this ADR).

## Context

### What we have today

- **Module system (ADR-0026)** — files are structs, `@import` is the only cross-file dependency edge, `pub` defines a file's public interface, lazy semantic analysis only analyzes reachable declarations.
- **Multi-file compilation (ADR-0023)** — frontend already has per-file parsing and per-file RIR generation paths (`parse_all_files`, `validate_and_generate_rir_parallel`).
- **Comptime is a sema-level interpreter** (ADR-0033). Comptime evaluation, generic monomorphization, and inline-method registration all happen during sema, before codegen. By the time AIR exists, all comptime values are baked in and all generic specializations are concrete functions.
- **Whole-program LLVM codegen** — `gruel-codegen-llvm::generate` lowers every function into a single LLVM module (`gruel_module`) and emits one object file. LLVM's optimizer is free to inline across all functions in the module at `-O1+`. ThinLTO is **not** in use and is not planned for this ADR's scope (the inkwell crate doesn't expose ThinLTO, and the C++ shim required to produce ThinLTO-format bitcode is out of scope).
- **No persistent cache** — every `gruel` invocation reruns the full pipeline from source.

### The problem

Recompilation cost splits roughly into:

1. **Frontend** — lex, parse, RIR, sema, comptime evaluation, generic monomorphization. Reruns on every invocation. For comptime-heavy code (derives, generics, type-level computation) this dominates.
2. **LLVM optimization + back-end** — runs whole-program every invocation. For runtime-heavy code at `-O2+` this dominates.
3. **Link** — small, runs whole-program every invocation.

A one-line edit currently re-lexes, re-parses, re-typechecks, re-runs comptime evaluation, and re-LLVMs the entire program. The whole frontend is recoverable; (1) is what we cache. The LLVM-side work is structurally tied to the whole-program-module architecture and is not addressed here.

ADR-0026's "Future Work" section explicitly lists this as the natural follow-up to lazy analysis:

> Incremental compilation — Build on lazy analysis for file-level caching.

### Why now

- **Modules give us bounded dependencies.** Without `@import` edges, every file potentially saw every symbol; cache invalidation would have been "any change anywhere = rebuild everything." With modules, a file depends only on what it transitively imports.
- **Lazy sema gives us a per-declaration analysis model**, which extends naturally into a per-declaration cache key.
- **AIR is post-comptime and post-monomorphization.** Caching AIR caches the comptime interpreter's results — the most expensive thing a comptime-heavy program does. This is the highest-leverage cache layer and the reason the ADR is worth doing despite the LLVM ceiling.

### What we explicitly defer

- **Shared / cross-machine cache** (sccache-style). The cache lives in `target/gruel-cache/` and is keyed by local paths and content hashes; cross-machine reproducibility is a stretch goal for a future ADR.
- **Watch-mode daemon / persistent compiler process**. This ADR is about cold-invocation caching only.
- **IDE / language-server integration.** Out of scope.
- **Cache for `--emit` artifacts.** Caching is about the binary build path; debug emits always recompute.
- **Caching across preview-feature changes.** Toggling `--preview` flags invalidates the entire cache for that build (cheap to detect, hard to reason about partial overlaps).

## Decision

### Architecture

A new crate `gruel-cache` provides:

- A content-addressed on-disk store rooted at `target/gruel-cache/`.
- Stable fingerprint computation for files, signatures, and functions.
- A typed lookup/store API used by the compiler driver.

The compiler pipeline grows three cache-checkpoints:

```
Source ──▶ Lex ──▶ Parse ──▶ RIR ──▶ Sema ──▶ AIR→bitcode ──▶ LLVM opt ──▶ Link
                              │        │              │           │
                              ▼        ▼              ▼           │
                         [parse+rir] [air +     [per-fn bitcode]  │
                            cache  comptime           cache       │
                                   output buf]                    │
                                                            (always runs,
                                                             not cached)
```

At each checkpoint, the compiler asks the cache: "given this fingerprint, do you have the output?" — on hit, deserialize and skip the stage; on miss, run the stage and store the result. The LLVM optimizer, back-end, and linker run on every build regardless of cache state.

### Cache directory layout

```
target/gruel-cache/
├── version              # compiler version + cache schema version
├── manifest.json        # path → file_fingerprint mapping (last build)
├── parse/
│   └── <hash>.bin       # serialized AST + interner slice (per file)
├── air/
│   └── <hash>.bin       # serialized AnalyzedFunction + types (per file)
├── llvm-ir/
│   └── <hash>.bc        # per-function LLVM bitcode (pre-optimization)
└── tmp/                 # staging for atomic writes
```

All writes are atomic (write to `tmp/`, then `rename`). All filenames are content-hashes (BLAKE3, 32 bytes, hex-encoded), so concurrent invocations cannot corrupt each other.

### Fingerprints

Three layered fingerprints, each a BLAKE3 hash:

**1. File fingerprint** — pure function of file bytes:

```
file_fp(file) = blake3(file_contents)
```

**2. Signature fingerprint** — hash of a file's public interface only:

```
sig_fp(file) = blake3(canonical_encoding(
    pub items in file, with bodies stripped:
      - fn name + param types + return type + attributes
      - struct name + field names + field types + visibility
      - enum name + variants
      - pub const name + type
      - pub interfaces and their method signatures
))
```

The canonical encoding is a stable, type-resolved form computed *after* sema runs on that file. Critically, `sig_fp` does not depend on function bodies — editing inside a `pub fn` does not change the file's `sig_fp`.

**3. Compilation fingerprint** — what actually keys the cache:

```
build_fp = blake3(
    compiler_fp,                          // see "Compiler fingerprint" below
    target_triple,
    opt_level,
    enabled_preview_features (sorted),
)

parse_key(file)    = blake3(build_fp, file_fp(file))
air_key(file)      = blake3(build_fp, file_fp(file),
                            sorted([sig_fp(imp) for imp in transitive_imports(file)]))
bitcode_key(func)  = blake3(build_fp, air_hash(func))
```

### Compiler fingerprint

`compiler_fp` must change whenever the compiler's behavior could change — including local edits during compiler development, not just released versions. A bare `CARGO_PKG_VERSION` is wrong: it stays constant across local `cargo build` cycles, which would silently serve stale cache entries to anyone hacking on the compiler.

The fingerprint is a **hash of the running compiler binary**, memoized so we don't rehash 30MB on every invocation:

```
compiler_fp(self_path) =
    let key = (self_path, mtime(self_path), size(self_path))
    if let Some(hash) = read("~/.cache/gruel/binary-hash/{key}") { return hash }
    let hash = blake3(read_bytes(self_path))
    write_atomic("~/.cache/gruel/binary-hash/{key}", hash)
    hash
```

Plus, `build.rs` embeds `git_sha` and `dirty: bool` as compile-time constants, surfaced through `gruel --version` for diagnostics. These are *not* part of the cache key — the binary hash already covers everything they encode — but they make "why did my cache invalidate?" answerable for users.

This makes the cache robust to:

- **Releases** — different binary bytes → different `compiler_fp`.
- **Local `cargo build` of compiler changes** — different binary bytes → different `compiler_fp`. Critical for compiler developers.
- **Debug vs. release builds** of the compiler — different binary, different fingerprint.
- **Rebasing onto a new compiler commit** — different binary, different fingerprint.

The memoization keeps the runtime cost at one `stat` call in the hot path; the actual BLAKE3 only runs the first time after a `cargo build` of the compiler.

### Invalidation behavior (worked examples)

- **Edit a private function body in `utils.gruel`:**
  - `file_fp(utils)` changes → `parse_key(utils)` misses → re-parse, re-RIR, re-sema (and re-comptime, re-monomorphize) `utils`.
  - `sig_fp(utils)` unchanged → no importer's `air_key` is invalidated. They reuse cached AIR, including all comptime work.
  - The edited function's AIR hash changed → its `bitcode_key` misses → that one function's AIR is re-translated to LLVM bitcode. Other functions in `utils.gruel` still hit the bitcode cache.
  - Bitcode for all functions (mix of cached + freshly produced) is loaded into a single LLVM module; LLVM optimizer + object emission + link still run on the whole program. The savings are: all-of-frontend for files that didn't change (the comptime-heavy case wins big here), and AIR→bitcode for functions whose AIR didn't change.

- **Edit a `pub fn`'s signature in `utils.gruel`:**
  - `file_fp(utils)` and `sig_fp(utils)` both change.
  - `air_key` for every file that imports `utils` (transitively) misses. Importers re-run sema, comptime, and monomorphization.
  - Any function whose resulting AIR hash changed gets a new `bitcode_key` and is re-translated.

- **Edit the body of a generic function `fn identity<T>(x: T) -> T`:**
  - `file_fp` of the defining file changes → re-parse + re-sema that file.
  - Generic specializations are produced during sema (in `gruel-air/src/specialize.rs`). Re-running sema produces fresh AIR for all specializations, with new AIR hashes → new `bitcode_key`s for each → each specialization is re-translated. No special bookkeeping needed — the AIR cache layer captures the dependency automatically because AIR contains the monomorphized form.

- **Add a `// comment`:**
  - `file_fp` changes → parse cache misses, parse runs.
  - The resulting AST has no comments, so the AIR for each function is identical, so `air_key` and `bitcode_key` both hit. Net cost: re-parse one file. ~10ms (plus the unavoidable LLVM optimizer + back-end + link).

- **Bump compiler version (or `cargo build` the compiler with local changes):**
  - The compiler binary's bytes change → `compiler_fp` changes → `build_fp` changes → every key misses → full rebuild. Crucially, this works even when `CARGO_PKG_VERSION` is unchanged (the common case during compiler development).
  - The old cache directory is detected as stale on startup (via `version` file) and deleted.

- **Switch between two git branches that share most files:**
  - `git checkout` rewrites files on disk and updates their mtimes, but the *contents* of unchanged files are byte-identical.
  - All identical files have the same `file_fp` → parse and AIR caches hit. Only files that actually differ between branches incur work, plus any file whose `sig_fp` changed transitively invalidates its importers' AIR cache.
  - This is materially better than Cargo's mtime-based fingerprinting, which rebuilds crates whose source files `git checkout` re-touched even when contents are byte-identical. Switching back and forth between two branches becomes nearly free for the unchanged majority of the codebase.

### What gets serialized

- **Parse cache:** AST + per-file interner slice. Spurs are file-local; on load they get re-interned into the build-wide interner.
- **AIR cache:** `AnalyzedFunction` for each function in the file (including monomorphized specializations), plus the file's resolved type-pool entries, the signature fingerprint, and any comptime side-effect output (see "Comptime side-effects replay" below). Reusing these requires that interned `TypeId`s be remappable on load — see "Open Questions."
- **Bitcode cache:** per-function LLVM bitcode (pre-optimization). On hit, the bitcode is parsed and added to the build's LLVM module instead of being re-translated from AIR. On miss, the function is translated normally and the resulting bitcode is written to the cache.

We do **not** cache the LLVM optimizer output, the final object file, or the linked binary. The whole-program LLVM module is reassembled and reoptimized on every build. This is a deliberate scope decision tied to the current codegen architecture, not a temporary limitation.

### Comptime side-effects replay

Comptime evaluation can produce user-visible output: `@dbg` prints, `dbg_clog` lines, comptime-generated warnings. If we cache AIR and a cache hit skips the comptime interpreter, those outputs would silently disappear from the build's stderr — the build would still be correct but observably different from a cold build.

The fix: alongside cached AIR, store the comptime side-effect buffer for that file (the list of `@dbg` lines, warnings, and any other deterministic comptime output). On AIR cache hit, replay the buffered output to stderr in the same order it would have appeared during sema.

This makes cache-hit and cache-miss observably identical for the user. Cargo does the equivalent for build-script `cargo:warning=...` lines.

The buffer is small (kilobytes) and is part of the AIR cache entry, not a separate cache. It invalidates with the AIR — any change that would re-run comptime also re-captures the output.

### CLI surface

```bash
# Default: caching off until stabilized
gruel build src/*.gruel -o out

# Enable caching
gruel build --preview incremental_compilation src/*.gruel -o out

# After stabilization: enabled by default, opt-out flag
gruel build --no-cache src/*.gruel -o out

# Wipe cache
gruel cache clean

# Show cache stats (size, hit rate from last build)
gruel cache stats
```

The cache directory location follows the workspace root by default but is overridable:

```bash
gruel build --cache-dir /tmp/gruel-cache ...
GRUEL_CACHE_DIR=/tmp/gruel-cache gruel build ...
```

### Observability

- `--time-passes` reports cache hit rates per stage: `parse: 47/50 hits, air: 45/50 hits, bitcode: 198/210 hits`.
- `tracing` events from `gruel-cache` (one canonical wide event per stage with `cache_hit=true/false`, `key=...`, `bytes=...`).
- `gruel cache stats` reads the manifest from the last build.

### Concurrency and safety

- Multiple `gruel` invocations on the same cache directory are safe: cache writes are atomic renames, so a concurrent reader either sees the old file or the new file, never a partial one.
- A coarse lock file (`target/gruel-cache/.lock`, `flock`-based) serializes the manifest update at end-of-build. Stage caches don't need locking.
- The cache is **content-addressed and deterministic**. If two invocations compute the same key, they produce the same bytes (modulo serializer non-determinism, which the test plan exercises).

### Cache lifecycle

The cache has no automatic size or age limit. Like Cargo's `target/` directory, it grows as long as a workspace exists and is wiped when the user wants it wiped. Two commands manage it:

- `gruel cache clean` — delete the entire `target/gruel-cache/` directory.
- `gruel cache stats` — print size, entry count, and hit rate from the last build, so users can see when it's worth cleaning.

Files in the cache directory are never deleted by the compiler during a build. The only deletions are explicit (`gruel cache clean`) or driven by the version-mismatch check at startup (which wipes a cache from an incompatible compiler version).

**Rationale:** Time-based and size-based GC both involve picking arbitrary numbers and create surprising deletion behavior (a build "randomly" gets slower because GC ran and deleted artifacts the next build needed). Following Cargo's "no automatic GC" model is consistent with what users already expect from artifacts under `target/`, removes a knob, and avoids edge cases. Surveyed alternatives:

- **Cargo / npm / pip / Nix** — no automatic GC. Manual clean command. Has worked for a decade in Rust; users adapt.
- **Go build cache** — time-based, deletes entries unused for 5 days. Doesn't bound size. Picks a semantically meaningful number rather than an arbitrary byte count.
- **sccache / ccache** — size-cap with LRU. Picks an arbitrary byte limit (10GB, 5GB respectively). Justified for them because their cache is global across all projects on the machine.

The Cargo model fits Gruel best because the cache is per-workspace (so abandoned-project bloat dies with the workspace), and "predictable behavior under `target/`" is more valuable than "bounded disk usage." If real-world usage shows the cache growing problematically, a time-based eviction policy (Go's "unused for N days" model) can be added in a follow-up without breaking existing behavior.

### Correctness fallback

If the cache returns a value that fails to deserialize, or if any stage detects a cache mismatch (e.g. AIR's referenced types don't match what's in the current type pool), the stage logs a warning and recomputes. The cache is an optimization, never a source of truth.

### Benchmarking and validation

Without explicit benchmarks, three failure modes go undetected:

1. **The cache doesn't deliver the promised speedup.** Hot-cache builds stay slow because lookup overhead, deserialization, or `TypeId` remapping eats the savings.
2. **Cold-cache builds get slower** because the cache infrastructure adds overhead even when nothing hits (hashing every file, missing every key, writing all results).
3. **Cache hit rate silently drops.** A determinism regression sneaks in (e.g. `HashMap` iteration order leaking into a serialized blob), warm builds keep working but hit rate falls from 99% → 50%, and nobody notices because it's still "faster than uncached."

The existing perf-dashboard infrastructure (ADR-0019, ADR-0031, ADR-0043) is extended with a "cache" benchmark family covering, at minimum:

| Scenario | What it measures |
|----------|------------------|
| **Cold cache, caching disabled** | Baseline — what the compiler does today |
| **Cold cache, caching enabled** | Overhead of the cache infra when nothing hits (hashing files, missing every key, writing all results) |
| **Fully hot cache, zero source changes** | The "rebuild a clean tree" floor — validates lookup overhead and the LLVM-stage cost we always pay |
| **Warm cache, one function body changed in a leaf file (runtime-heavy program)** | The runtime-heavy dev-loop scenario. LLVM ceiling caps the speedup. |
| **Warm cache, one function body changed in a leaf file (comptime-heavy program)** | The comptime-heavy dev-loop scenario. Expected to show the ADR's headline win — cached AIR skips the comptime interpreter for unchanged files. |
| **Warm cache, one `pub` signature changed in a leaf file** | Partial AIR invalidation for direct importers, bitcode invalidation for users of that symbol. |
| **Warm cache, one `pub` signature changed in a widely-imported file** | Worst-case partial invalidation — quantifies how bad the "edit a popular helper" case actually is. |
| **Branch switch (swap to a branch with mostly-unchanged contents)** | The content-addressing-vs-Cargo win. Hit rate should be ~100% on identical files. |

The runtime-heavy and comptime-heavy variants are deliberately split: they test fundamentally different cost regimes, and a single representative project would hide which one we're actually winning on.

In addition to wall-clock time, each run reports **cache hit rate per stage** (parse / AIR / bitcode). Hit rate is the leading indicator for determinism regressions: it moves before wall-clock does.

These benchmarks are required to land *before* the feature can be considered for stabilization, and must run on the perf dashboard for several iterations so we have real data on hit rate stability and cold-cache overhead before changing any defaults.

## Implementation Phases

- [x] **Phase 1: Cache infrastructure** — Create `gruel-cache` crate. Implement `CacheStore` with atomic writes, BLAKE3 keying, version stamping. Implement `compiler_fp` (hash of own binary, memoized at `~/.cache/gruel/binary-hash/` keyed by `(path, mtime, size)`). Add `build.rs` embedding `git_sha` + `dirty` flag for diagnostics. Add `--preview incremental_compilation` and `--cache-dir` plumbing. No pipeline integration yet; tested in isolation.

- [x] **Phase 2: Parse caching** — Cache parser output (AST + interner snapshot) keyed by `parse_key`. Wired into `CompilationUnit::parse` via `parse_cache::parse_files_into`. End-to-end verified: cold build writes `target/gruel-cache/parse/<hash>.bin`; warm rebuild produces an identical binary from the cache hit. The `RemapSpurs` walker covers all AST types so cached Spurs are correctly substituted into the build's shared interner. **Per-file RIR caching deferred** to a follow-up commit because the RIR walker requires understanding the variant-dependent layout of `Rir.extra` (used for packed call args, directives, match arms, etc.); doing it correctly is its own focused implementation pass and Phase 4's AIR cache delivers a much larger speedup. **`--time-passes` cache-hit metrics also deferred** alongside the RIR walker.

- [x] **Phase 3: Signature fingerprinting** — `compute_sig_fp(ast, interner)` in `gruel-cache::signature` produces a stable BLAKE3 hash of a file's `pub` interface. Encoding is locked by `SIG_FP_VERSION = 1` and a golden empty-program test; behavioral tests verify that private items / body changes / declaration order do NOT affect the hash, while signature changes, renames, parameter counts, and pub field type changes do. **Deviation from ADR**: implementation hashes the AST, not post-sema AIR — Phase 4 hasn't landed to provide the sema output, and AST-based sig_fp is conservative (over-invalidates rare type-aliasing cases, never under-invalidates). Bumping to post-sema can happen by bumping `SIG_FP_VERSION`.

- [x] **Phase 4: AIR caching with comptime side-effects replay** — End-to-end via 5 sub-commits: 4a/4b serde derives throughout `gruel-air` (Type, InternedType, AnalyzedFunction, AirInst/AirInstData, AirPlace, AirPattern, StructDef/EnumDef/InterfaceDef, etc.); 4c custom Serialize/Deserialize for `TypeInternPool` (snapshots `Vec<TypeData>`, reconstructs structural-dedup HashMaps on load); 4d `CachedAirOutput` envelope in `gruel-cache::wire_air` with `InternerSnapshot` + functions + type pool + strings/bytes + interface defs/vtables + `comptime_dbg_output`; 4e wired into `CompilationUnit::analyze` (`open_air_cache` keyed by build_fp + concatenated source contents; on hit restores the build's interner from the cached snapshot, replays `@dbg` output, returns the cached `SemaOutput`; on miss runs sema and writes the cache). **Verified end-to-end**: cold build writes both `target/gruel-cache/parse/<h>.bin` and `target/gruel-cache/air/<h>.bin` (10.5 KB), warm build skips sema entirely and produces an identical binary. **Deviations from ADR**: cache is whole-program (not per-file) because sema currently runs on the merged AST — per-file granularity needs sema-side refactor; warnings are not cached (DiagnosticWrapper serde is its own follow-up); TypeId remap walker is not yet needed because whole-program cache restores the entire interner rather than merging.

- [x] **Phase 5: LLVM bitcode caching** — Two new gruel-codegen-llvm entry points (`generate_bitcode` returns pre-opt bitcode without running passes; `compile_bitcode_to_object` parses cached or fresh bitcode through optimizer + back-end). `CompilationUnit::compile_with_bitcode_cache` looks up bitcode under the same `air_key` (bitcode is a deterministic function of AIR). On hit: skip AIR→IR translation. On miss: generate bitcode + write to cache + emit object. The optimizer + back-end + linker still run on every build per ADR's no-codegen-output-caching constraint. **Verified end-to-end**: cold build writes parse + air + llvm-ir caches (3 entries, 26.1 KB total); warm build hits all three and produces identical binary. **Deviation from ADR**: cache is whole-program (one bitcode blob keyed by air_key) rather than per-function — per-function would require restructuring `build_module` to produce per-function modules and link them, its own focused codegen refactor.

- [x] **Phase 6: Observability and cache CLI** — `gruel --cache-stats` walks the cache directory and prints per-kind entry counts + human-readable byte sizes; `gruel --cache-clean` wipes and recreates the layout. Top-level flags rather than `gruel cache <sub>` subcommands; that refactor is deferred but functionally equivalent. `tracing` events from `gruel-cache` already in place from Phase 1. No automatic GC. **Cache hit-rate reporting in `--time-passes` deferred** alongside the RIR walker (ParseCacheStats threading through CompilationUnit's timing reporter is its own small change).

- [x] **Phase 7: Cold-vs-hot benchmark infrastructure** — `bench_cache.sh` runs three scenarios per program (cold-no-cache, cold-cache-on, warm-cache-on) with median-of-N timing and warmup-pass to remove first-invocation OS-cache bias. Two benchmark programs in `benchmarks/cache/` (runtime_heavy, comptime_heavy). Initial release-build measurements on a tiny test program: warm-cache speedup ~1x because the workload is dominated by LLVM optimizer + linker time which the cache by design does not skip. A larger comptime-heavy workload would show the headline win — adding more representative programs and integrating into `bench.sh`'s manifest-driven runner is straightforward follow-up. The feature remains preview-gated and off by default.

**Stabilization is intentionally deferred to a follow-up ADR.** Flipping the default to enabled is a separate, evidence-driven decision that should be made only after the dashboard has shown stable hit rates and acceptable cold-cache overhead over several iterations. That follow-up ADR will define the stabilization criteria, propose the rollout, and supersede this one once accepted.

## Consequences

### Positive

- **Big speedup on comptime-heavy code.** Caching AIR captures comptime evaluation, generic monomorphization, and inline-method registration — the dominant costs for code that uses derives, generics, or type-level computation. For projects where comptime is a meaningful share of build time, edit-one-function rebuilds reuse cached AIR for every unchanged file. This is the highest-leverage win in the ADR.
- **Modest speedup on runtime-heavy code.** Skipped frontend + skipped AIR→bitcode translation for unchanged functions. The LLVM optimizer + back-end + linker still run on every build, which puts a hard ceiling on speedup at high opt levels.
- **Lazy sema gets persistent memoization.** The per-declaration cache that today resets every invocation now spans builds.
- **Clean failure mode.** Cache miss = recompute. There is no path where a stale cache produces a wrong binary; the keys are content-derived.
- **Foundation for more.** Watch-mode, language-server, and shared/CI caches all build on the same fingerprint scheme.

### Negative

- **Hard ceiling at the LLVM stage.** The optimizer + back-end always run whole-program. For runtime-heavy code at `-O2+` this is the dominant cost and we don't reduce it. Users compiling such code will see modest speedups, not dramatic ones. This is a deliberate scope decision — see Future Work for the architectural change that would unlock more.
- **Real implementation complexity.** Three layers of fingerprints, serialization for AST/AIR/types, interner and `TypeId` remapping on load. The codepath that loads cached AIR is genuinely subtle.
- **Determinism becomes load-bearing.** Anything non-deterministic in the pipeline (HashMap iteration order leaking into output, ASLR-affected pointer hashing) silently kills cache hit rate. We'll need a determinism-check test in CI.
- **Bug surface.** A cache bug looks like "build succeeds but binary is wrong" — the worst kind. Mitigated by: content-addressed keys (hard to be wrong about identity), correctness fallback on any mismatch, and a fuzz target that compares cached vs. uncached output.

### Neutral

- **Cache directory must be in `.gitignore`.** Already covered by `target/`.
- **`--preview` flag toggles full rebuild.** Acceptable because preview state changes rarely.
- **First build after `cargo build` of the compiler is always a cold cache.** Acceptable; the value is in subsequent builds.

## Open Questions

- **`TypeId` remapping.** `TypeInternPool` IDs are pool-local. On loading cached AIR, we need to remap each cached `TypeId` to the corresponding ID in the current build's pool. Is the cleanest path (a) re-intern on load, walking the AIR and rewriting IDs, or (b) make the cached pool the source of truth for the file and merge into the build pool? Phase 4 will pick one; preference is (a) for simplicity.
- **Comptime side-effect ordering.** When multiple cached files are loaded in a different order than they were first compiled in, can replayed `@dbg` output appear in a different order than a cold build would have produced? Probably yes. Decide whether to (a) preserve cold-build ordering by serializing the replays globally, or (b) accept per-file order and document it. Lean toward (b) — comptime output is a debug aid, not a build-output guarantee.
- **What about generated synthetic items?** Drop glue, clone glue, vtables. These are derived from struct/enum signatures; their AIR is produced during sema and naturally captured by the AIR cache. Their bitcode follows from their AIR like any other function. Should be straightforward but worth verifying explicitly in Phase 5.
- **Determinism enforcement.** Should we add a CI job that builds twice and diffs the cache contents to catch determinism regressions early? Recommended; cheap insurance.

## Future Work

- **Stabilization ADR.** A separate, follow-up ADR will propose flipping the default to enabled and removing the `--preview` gate, once the cold-vs-hot benchmarks have run on the perf dashboard for several iterations and we have evidence of stable hit rates and acceptable cold-cache overhead. That ADR will define concrete thresholds (e.g. hot-cache hit rate, cold-cache overhead ceiling) and will supersede this one.
- **Shared / CI cache.** Stable cross-machine fingerprints (path-relative, no absolute paths in keys) and a remote cache backend (S3, HTTP). Probably needs `gruel.toml` first so workspace roots are well-defined.
- **Watch-mode daemon.** A long-lived `gruel watch` process that keeps the in-memory caches warm and only flushes to disk periodically.
- **Language-server integration.** A `gruel check` mode that uses the cache for editor responsiveness.
- **Time-based GC.** If real-world usage shows the cache growing problematically, add Go-style "delete entries unused for N days" eviction. Additive, non-breaking.
- **Codegen-stage caching beyond bitcode.** Would require an architectural change to how codegen partitions the program (per-function or per-file LLVM modules instead of one whole-program module), plus either accepting the cross-function inlining loss or adopting ThinLTO. ThinLTO via the inkwell stack is non-trivial — the producer side requires a C++ shim because llvm-sys doesn't expose `WriteThinBitcodeToFile` and the relevant pass names are not reachable through `LLVMRunPasses`. Confirmed via spike. This is a real architectural decision deferred indefinitely.

## References

- [ADR-0023: Multi-File Compilation](0023-multi-file-compilation.md) — established the per-file frontend
- [ADR-0026: Module System](0026-module-system.md) — gives us bounded dependency edges and lazy sema; explicitly lists this ADR as future work
- [ADR-0033: LLVM Backend and Comptime Interpreter](0033-llvm-backend-and-comptime-interpreter.md) — defines the whole-program LLVM module and the comptime interpreter whose results we cache via AIR
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md) — the registry pattern for per-item metadata, similar to what `sig_fp` will encode
- `crates/gruel-air/src/specialize.rs` — the generic-monomorphization pass whose output is captured automatically by the AIR cache
- [Mitchell Hashimoto: Zig Sema](https://mitchellh.com/zig/sema) — inspiration for the lazy-analysis model that this ADR extends
- [Rust incremental compilation RFC](https://rust-lang.github.io/rfcs/1298-incremental-compilation.html) — a deeper version of the same general idea
