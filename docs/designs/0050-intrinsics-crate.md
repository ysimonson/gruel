---
id: 0050
title: Centralized Intrinsics Registry
status: implemented
tags: [refactor, infrastructure, docs]
feature-flag: none
created: 2026-04-22
accepted: 2026-04-23
implemented: 2026-04-23
spec-sections: [4.13, 9.2]
superseded-by:
---

# ADR-0050: Centralized Intrinsics Registry

## Status

Implemented

## Summary

Introduce a `gruel-intrinsics` crate that holds a single declarative registry of every `@intrinsic` in the language (name, argument shape, return type, preview gate, unchecked requirement, runtime binding, and docstring). Each pipeline stage (RIR, Sema, Codegen) reads the registry instead of hard-coding intrinsic names, and the website's intrinsic reference page is generated from the same source of truth.

## Context

Intrinsics are currently defined by convention, spread across at least six places:

| Where | What it knows |
|---|---|
| `gruel-rir/src/astgen.rs:10` | Hard-coded `TYPE_INTRINSICS` list of names that take a type arg |
| `gruel-air/src/sema/known_symbols.rs` | Pre-interned `Spur` per intrinsic name, tested by name |
| `gruel-air/src/sema/analysis.rs:3353` | `analyze_intrinsic_impl` — a ~70-line if/else dispatch chain with per-intrinsic analyzers (~40 fns, most of analysis.rs lines 3438–9100) |
| `gruel-air/src/inference/generate.rs` | Special-cases `"cast"`, `"size_of"`, etc. during HM inference |
| `gruel-codegen-llvm/src/codegen.rs:2575` | `translate_intrinsic` — another big match arm by name string |
| `gruel-runtime/src/{random,parse,debug}.rs` | Runtime implementations keyed by symbol |
| `docs/spec/src/04-expressions/13-intrinsics.md` | Hand-maintained markdown table of every intrinsic |

Adding an intrinsic today means editing all six locations and hoping the spec table stays in sync. Renaming one means grepping for a string literal. There is no single place that answers "what intrinsics exist?" and nothing prevents the spec's table from drifting from the compiler.

Contrast this with built-in types (ADR-0020), which are declared once in `gruel-builtins/src/lib.rs` as `BuiltinTypeDef` entries and injected as synthetic structs. The same pattern can be applied to intrinsics.

## Decision

Create a new crate `gruel-intrinsics` containing:

1. A declarative `IntrinsicDef` data model describing every intrinsic.
2. A `const` slice `INTRINSICS: &[IntrinsicDef]` — the single source of truth.
3. Helper queries (`lookup_by_name`, `is_type_intrinsic`, `iter()`, `by_category()`) used by other crates.
4. A doc exporter that renders the registry to markdown for the website.

Each compiler stage consults the registry instead of carrying its own name list. Stages still own their *behavior* (semantic analysis, codegen), because behavior is genuinely per-intrinsic and hard to express declaratively — but they are dispatched via a stable `IntrinsicId` enum rather than string matching.

### `IntrinsicDef` shape

```rust
pub struct IntrinsicDef {
    pub id: IntrinsicId,              // enum variant, used for stable dispatch
    pub name: &'static str,           // "dbg", "size_of", ...
    pub kind: IntrinsicKind,          // Expr | Type | TypeOrIdent
    pub arity: Arity,                 // Exact(n) | Range(min, max) | Variadic
    pub args: &'static [ArgSpec],     // expected kinds (Expr, Type, ...)
    pub return_ty: ReturnSpec,        // Fixed(Type) | Inferred | InferredFromArg(n) | ...
    pub requires_unchecked: bool,     // true for ptr_* and syscall
    pub preview: Option<PreviewFeature>,
    pub runtime_fn: Option<&'static str>,  // "gruel_random_u32", etc.
    pub category: Category,           // Debug | Cast | Pointer | Platform | Comptime | IO | Random | Meta
    pub summary: &'static str,        // one-liner for the docs table
    pub description: &'static str,    // longer markdown for the detail page
    pub examples: &'static [&'static str], // code snippets
}
```

`IntrinsicId` is an enum with one variant per intrinsic (e.g., `Dbg`, `Cast`, `SizeOf`, `PtrRead`). Stages dispatch on the id, not the string name.

### Integration points after the refactor

| Stage | Before | After |
|---|---|---|
| RIR astgen | hard-coded `TYPE_INTRINSICS` list | `INTRINSICS.lookup(name).map(|d| d.kind == Type)` |
| Sema known-symbols | 40+ fields pre-interned | generated from `INTRINSICS` at startup into a `HashMap<Spur, IntrinsicId>` |
| Sema analyze_intrinsic_impl | string/Spur if-else chain | `match lookup(name).id { ... }` — still one arm per intrinsic, but dispatching on a closed enum is checked by the compiler |
| Sema inference | string checks | id-based checks |
| Codegen translate_intrinsic | string match on name | match on `IntrinsicId` |
| Runtime | unchanged file layout | `runtime_fn` field in the registry names the extern symbol |

Behavior code (analyzers, codegen arms) stays in its existing crate. What moves into `gruel-intrinsics` is the *metadata* and the *identity* (the enum). This keeps the blast radius contained: no semantic logic is relocated.

### Documentation export

`gruel-intrinsics` exposes a function `render_reference_markdown() -> String` that produces the full intrinsics reference page from the registry. A small binary target (`gruel-intrinsics --dump-docs`) writes the output to a file under `docs/spec/src/04-expressions/13-intrinsics.md` (or a new dedicated page under `website/content/`). The website build script invokes it so docs can't drift.

The hand-maintained quick-reference table in the spec is replaced by the generated page. Spec paragraph IDs (e.g., `4.13:*`) stay in a small hand-edited wrapper section; the per-intrinsic table and detail sections are generated.

### What stays hand-written

- Per-intrinsic *behavior* (analyzer fn, codegen arm) — the existing fns in `analysis.rs` and `codegen.rs` are kept, they just dispatch on `IntrinsicId`.
- Runtime implementations in `gruel-runtime` — unchanged.
- Deep spec prose (examples, edge cases, rationale). The registry supplies the summary/signature; longer narrative lives in prose paragraphs that the generator splices in.

### Non-goals

- We are *not* trying to eliminate per-intrinsic Rust code (analyzers, codegen arms). Those handle real behavior that varies per intrinsic and is awkward to express in data.
- We are *not* changing intrinsic *semantics* or adding/removing any intrinsic in this ADR.
- We are *not* introducing a plugin system or runtime registration — the registry is `const` and closed.

## Implementation Phases

- [x] **Phase 1: Scaffold `gruel-intrinsics` crate**
  - New crate with `IntrinsicDef`, `IntrinsicId`, `IntrinsicKind`, `ArgSpec`, `ReturnSpec`, `Category`, `Arity` types.
  - `INTRINSICS: &[IntrinsicDef]` populated from the *existing* set of ~30 intrinsics, each entry capturing the data currently scattered across the compiler (names come from `known_symbols.rs`, unchecked flags from `analyze_intrinsic_impl`, type-intrinsic flags from `astgen.rs`, runtime fns from `gruel-runtime`).
  - Query helpers: `lookup_by_name`, `iter`, `by_category`.
  - Unit tests asserting (a) no duplicate names, (b) every `IntrinsicId` variant appears exactly once in the slice.
  - Crate compiles, no consumers yet.

- [x] **Phase 2: Wire RIR astgen to the registry**
  - Depend on `gruel-intrinsics` from `gruel-rir`.
  - Replace hard-coded `TYPE_INTRINSICS` in `astgen.rs:10` with a registry lookup.
  - Behavior must be byte-identical — confirm with spec-test suite.

- [x] **Phase 3: Wire Sema to the registry**
  - Replace `KnownSymbols` intrinsic fields with a `HashMap<Spur, IntrinsicId>` built at sema startup.
  - Rewrite `analyze_intrinsic_impl` (and the `analyze_type_intrinsic` side) to dispatch on `IntrinsicId` via a single exhaustive match. The per-intrinsic `analyze_*_intrinsic` fns stay put; only the dispatcher changes.
  - Replace string checks in `inference/generate.rs` with id-based checks.
  - Update `require_checked_for_intrinsic` calls to read `requires_unchecked` from the registry (eliminating the hard-coded list).
  - Preview-feature gating uses `def.preview` instead of ad-hoc calls.

- [x] **Phase 4: Wire codegen to the registry**
  - `translate_intrinsic` in `gruel-codegen-llvm` matches on `IntrinsicId`.
  - Runtime-fn name strings (`"gruel_random_u32"`, etc.) come from `def.runtime_fn`.

- [x] **Phase 5: Doc export**
  - Implement `render_reference_markdown()` producing the quick-reference table and per-intrinsic detail sections.
  - Add a `gruel-intrinsics` bin or `build.rs` hook that writes the generated page.
  - Replace the hand-maintained table in `docs/spec/src/04-expressions/13-intrinsics.md` with the generated content (preserving spec paragraph IDs in a handwritten header section).
  - Wire the exporter into `website/build.sh`.
  - Add a `make check` step that runs the exporter and fails if the committed doc differs from the generated output (prevents drift).

- [x] **Phase 6: Cleanup**
  - Delete now-unused fields from `KnownSymbols`.
  - Delete `TYPE_INTRINSICS` const in `astgen.rs`.
  - Collapse any remaining string-keyed intrinsic maps.
  - Update `CLAUDE.md` "Modifying the Language" section to document the new "add an entry to `INTRINSICS`" workflow.

Each phase is independently committable and leaves the compiler in a green state.

## Consequences

### Positive

- **Single source of truth.** Name, arity, unchecked-ness, preview gate, runtime binding, and docs all live in one `IntrinsicDef`.
- **Adding an intrinsic is mechanical.** New entry in `INTRINSICS` + `IntrinsicId` variant; the compiler's exhaustive matches force you to implement analyzer + codegen, and docs regenerate automatically.
- **Renaming is safe.** One string edit; no scattered literals to miss.
- **Docs can't drift.** CI fails if the generated reference disagrees with the registry.
- **Follows the ADR-0020 pattern.** Same shape as `BuiltinTypeDef`, so the project has one consistent "declarative registry" idiom.

### Negative

- **Indirection cost.** Each intrinsic dispatch now goes through a registry lookup (one `HashMap::get` at sema) instead of direct `Spur` comparison. Expected impact: negligible — the lookup happens once per intrinsic call site, not per token.
- **Behavior still lives elsewhere.** The registry centralizes metadata but not the analyzer/codegen arms. Someone touching an intrinsic still edits multiple files; we've reduced duplication, not eliminated it.
- **Doc generator adds build complexity.** Another step in `website/build.sh`; another `make check` gate.

### Neutral

- `gruel-error`'s `PreviewFeature` enum is now referenced from `gruel-intrinsics`. A new crate-dependency edge but no cycle (`gruel-intrinsics -> gruel-error`, as with other crates).

## Open Questions

- **Where does the generated doc live?** Inline in `docs/spec/src/04-expressions/13-intrinsics.md` (replacing most of it) or as a separate website page linked from the spec? Prefer the former for discoverability, but the spec's `{{ rule(id=...) }}` shortcodes complicate generation. Will decide during Phase 5 based on how messy the splice ends up.
- **Should the registry carry spec paragraph IDs?** If so, generated docs can emit `rule(id=...)` markers automatically. Likely yes, but the mapping is loose (one intrinsic → many paragraphs) so it may be cleaner to keep rule IDs in the hand-written wrapper.
- **Runtime fn linking.** Today the runtime exposes symbols like `gruel_random_u32`; codegen builds call sites by name. Should `runtime_fn` also drive an automatic `extern` declaration emitter, or remain descriptive only? Descriptive in this ADR; an emitter is plausible future work.
- **What earns a place in the registry?** Addressed by [ADR-0087](0087-prelude-fns-for-libc-wrappers.md) — intrinsics carry **compiler magic**, not transport. A row earns its place if it does codegen-emitted lowering, compile-time type / kind dispatch, or compile-time evaluation; the libc-wrapper rows that don't (`@read_line`, `@parse_*`, `@random_*`, `@utf8_validate`, `@bytes_eq`, `@alloc`, `@free`, `@realloc`) moved to `prelude/runtime_wrappers.gruel`. ADR-0087 also tracks the rows that *should* leave but currently can't (`@panic` family, `@spawn` / `@thread_join`, `@cstr_to_vec`, `@dbg`), each with a documented prerequisite. The `gruel-intrinsics` crate-level docs carry the same rule.

## Future Work

- Apply the same pattern to built-in operators (currently scattered like intrinsics were) — would extend ADR-0020 to operators.
- Generate an editor-completion file (LSP snippets, JSON) from the registry.
- Expose the registry to comptime (`@type_info`-style reflection over intrinsics themselves) — out of scope here.

## References

- ADR-0020: Built-in types as synthetic structs (the analogous pattern this ADR follows)
- ADR-0027: Random intrinsics
- ADR-0028: Unsafe and raw pointers (source of most unchecked-requiring intrinsics)
- `crates/gruel-air/src/sema/known_symbols.rs` — current pre-interned name table
- `crates/gruel-air/src/sema/analysis.rs:3353` — current dispatch chain
- `crates/gruel-codegen-llvm/src/codegen.rs:2575` — current codegen dispatch
