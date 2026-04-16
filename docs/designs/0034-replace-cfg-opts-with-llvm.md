---
id: 0034
title: Replace CFG-Level Optimization Passes With LLVM Equivalents
status: implemented
tags: [compiler, codegen, llvm, optimization]
feature-flag: none
created: 2026-04-16
accepted: 2026-04-16
implemented: 2026-04-16
spec-sections: []
superseded-by:
---

<!-- Optimizations are compiler internals; no preview feature flag is needed. -->

# ADR-0034: Replace CFG-Level Optimization Passes With LLVM Equivalents

## Status

Implemented

## Summary

Remove the CFG-level `constfold` and `dce` passes introduced in ADR-0012 and
instead drive LLVM's existing mid-end optimization pipeline via
`TargetMachine`'s optimization level and `Module::run_passes`. Keep the
`-O0..-O3` CLI surface unchanged; only the implementation of what "optimized"
means moves from our CFG to LLVM. `try_evaluate_const` in Sema is a comptime
language feature (ADR-0003 / ADR-0025 / ADR-0033), not an optimization, and is
explicitly out of scope.

## Context

After ADR-0033 replaced the custom x86-64 / AArch64 backends with an LLVM
backend, the compiler has two layers of optimization that overlap:

1. **Gruel CFG passes** at `crates/gruel-cfg/src/opt/`
   - `constfold.rs` (~700 lines): folds arithmetic, comparisons, bitwise, and
     shift operations on constant operands. Skips folds that would overflow at
     runtime so that the runtime panic is preserved.
   - `dce.rs` (~630 lines): marks side-effecting instructions live, propagates
     liveness to uses, drops dead instructions, and marks unreachable blocks
     with `Terminator::Unreachable`.
   - `mod.rs`: `OptLevel` enum and the `optimize()` entry point, run per
     function in parallel in `gruel-compiler/src/unit.rs:453` and
     `lib.rs:913,1039`.

2. **LLVM** at `crates/gruel-codegen-llvm/src/codegen.rs:129`
   - `TargetMachine` is always built with `OptimizationLevel::None`.
   - `Module::run_passes` is never called.
   - `-O1/-O2/-O3` therefore have no backend effect today.

LLVM's InstCombine, InstSimplify, ADCE/BDCE, GVN, SCCP, and SimplifyCFG all
subsume what our two CFG passes do — and go substantially further (strength
reduction, CSE, jump threading, loop simplification, SROA, inlining, etc.).
Checked arithmetic is already emitted as `llvm.sadd.with.overflow` and friends
(see `gruel-codegen-llvm/src/codegen.rs:787-840`), which LLVM folds correctly:
literal non-overflowing ops become constants, literal overflowing ops become
unconditional branches to `__gruel_overflow`. Our conservative CFG fold does
strictly less.

The result is ~1,300 lines of CFG optimization code that duplicates LLVM,
covers fewer cases, and (since `-O1+` is never passed to LLVM) provides the
only optimization the compiler ever applies. We should lean on LLVM.

### What is NOT an optimization pass

- `Sema::try_evaluate_const` in `gruel-air/src/sema/analysis.rs:3426` looks
  like constant folding but is part of language semantics — it evaluates
  `comptime` blocks, folds array sizes, resolves type values, etc. Per
  ADR-0033 this grows into a proper AIR interpreter. Out of scope here.
- Semantic-analysis micro-optimizations (e.g. `analyze_ops.rs:1221`) are
  local choices inside sema, not passes. Out of scope.

## Decision

### Pipeline after this change

```
AIR → CfgBuilder → CFG → LLVM IR → [LLVM passes] → object file → link → binary
                          ↑               ↑
                   unchanged        driven by -O level
```

CFG stays a straightforward lowering target. No CFG → CFG transforms.

### Changes by crate

#### `gruel-cfg`

- Delete `src/opt/constfold.rs` and `src/opt/dce.rs`.
- Keep `src/opt/mod.rs` but strip it down to just the `OptLevel` enum +
  parsing + `Display` (still used by the CLI and tests). Consider renaming
  the module to `opt_level.rs` since there are no longer any passes — but
  keep `pub use` paths stable (`gruel_cfg::OptLevel`,
  `gruel_cfg::opt::OptLevel`) to avoid churning every call site in one PR.
- Delete the `optimize()` function.
- Delete the bit-set helper used only by DCE.

#### `gruel-compiler`

- Remove the `gruel_cfg::opt::optimize(&mut cfg, opt_level)` calls in
  `src/unit.rs:453`, `src/lib.rs:913`, and `src/lib.rs:1039`.
- Thread `OptLevel` into `compile_backend` / `generate_llvm_objects_and_link`
  so the LLVM backend sees it.

#### `gruel-codegen-llvm`

- Extend the public API:
  ```rust
  pub fn generate(
      functions: &[&Cfg],
      type_pool: &TypeInternPool,
      strings: &[String],
      interner: &ThreadedRodeo,
      opt_level: OptLevel,
  ) -> CompileResult<Vec<u8>>;
  ```
- Map `OptLevel` → `inkwell::OptimizationLevel` when building the
  `TargetMachine` (affects LLVM's own codegen pipeline):
  | Gruel    | LLVM                          |
  |----------|-------------------------------|
  | `O0`     | `OptimizationLevel::None`     |
  | `O1`     | `OptimizationLevel::Less`     |
  | `O2`     | `OptimizationLevel::Default`  |
  | `O3`     | `OptimizationLevel::Aggressive` |
- For `O1..O3`, run the LLVM mid-end pipeline on the module before emission
  via `module.run_passes("default<O1|O2|O3>", &target_machine, opts)`. For
  `O0`, skip `run_passes` entirely so debug-style codegen is preserved.
- `generate_ir` (used by `--emit asm`) takes the same `OptLevel` and runs
  passes the same way so users can inspect optimized IR.

#### CLI (`gruel`)

- No user-visible change. `-O0..-O3` already parse into `OptLevel` and are
  already forwarded into `CompileOptions::opt_level`. They now take effect
  in the LLVM backend instead of at CFG.

### Tests

- Spec tests are runtime-behavior tests (exit codes, stdout). They continue
  to pass unchanged. `opt_level = N` in spec cases continues to work.
- Golden IR tests in `crates/gruel-spec/cases/golden/ir-dumps.toml` test
  tokens / AST / RIR / AIR — none of those change. There are no `expected_cfg`
  cases in that file. `crates/gruel-spec/cases/types/destructors.toml` has
  six `expected_cfg` cases, but they run at default `-O0` and we're removing
  the `-O1+` CFG rewrites, so the pre-opt CFG they check stays identical.
- Unit tests in `constfold.rs` and `dce.rs` are deleted with their files.
- The compilation-unit integration test in `unit.rs` — add one end-to-end
  test at `-O2` to confirm the backend wiring works and produces a binary
  that exits with the expected code.

### What we lose, briefly

- `--emit cfg` output is no longer affected by `-O` level. That's fine:
  users who want to see optimized code use `--emit asm` (LLVM IR).
- Per-function CFG optimization in parallel is removed. LLVM's pipeline is
  single-threaded per module. In practice the cost is negligible for our
  current program sizes and is not the bottleneck.

### Rollback

If LLVM's pipeline turns out to be surprising (e.g. an optimization strips
a side effect we actually rely on, or `run_passes` panics on some pattern
we emit), the rollback is a one-line revert: reinstate `optimize()` and
re-add the call. The CFG passes themselves will stay in git history. No
schema or file-format changes are made.

## Implementation Phases

- [x] **Phase 1: Wire OptLevel into the LLVM backend**
  - Extend `gruel_codegen_llvm::generate` / `generate_ir` signatures with
    `opt_level: OptLevel`.
  - Plumb `opt_level` through `compile_backend` and
    `generate_llvm_objects_and_link` in `gruel-compiler`.
  - Build the `TargetMachine` with the mapped `OptimizationLevel`.
  - For `-O1+`, call `module.run_passes("default<OX>", ...)` with a
    `PassBuilderOptions` instance.
  - Update unit tests in `gruel-compiler` that instantiate these functions.
  - Verify: `cargo run -p gruel -- --emit asm -O2 <example>` now shows
    optimized LLVM IR; `-O0` shows the unoptimized IR we get today.

- [x] **Phase 2: Delete the CFG-level passes**
  - Remove `gruel_cfg::opt::optimize` calls from
    `gruel-compiler/src/unit.rs` and `gruel-compiler/src/lib.rs` (two
    sites in `lib.rs`).
  - Delete `crates/gruel-cfg/src/opt/constfold.rs` and
    `crates/gruel-cfg/src/opt/dce.rs`.
  - Shrink `crates/gruel-cfg/src/opt/mod.rs` to just the `OptLevel` enum,
    its parser, `Display`, and tests.
  - `cargo test --workspace --exclude gruel-runtime` and `make test` both
    pass.

- [x] **Phase 3: Update ADR-0012 and docs**
  - Mark ADR-0012 as superseded by this ADR (set `superseded-by: 0034` and
    change status).
  - Update any CLAUDE.md / docs references to the CFG opt pipeline.
  - Move this ADR to `status: implemented` with the implemented date.

## Consequences

### Positive

- Removes ~1,300 lines of duplicated optimization code.
- `-O1/-O2/-O3` flags become meaningful: users get real optimization
  including InstCombine, GVN, SCCP, inlining, SROA, SimplifyCFG, and so on.
- Smaller surface area means fewer places where an optimization bug could
  silently change program behavior.
- LLVM's pipeline is the correct layer for target-independent mid-end
  optimization — this is exactly what it was designed for.

### Negative

- `-O0` LLVM IR is still relatively verbose because we don't run any
  cleanup at all. (Today CFG-level DCE trims it somewhat.) Users inspecting
  `--emit asm` at `-O0` will see every store/load. Acceptable: `-O0` is
  supposed to be debuggable.
- We lose the ability to dump an "optimized CFG" via `--emit cfg`. Not a
  feature anyone relies on, but worth calling out.
- Adds a small dependency on LLVM pass-pipeline semantics (e.g. the
  `default<OX>` pipeline name).

### Neutral

- No language semantic changes. All spec tests continue to pass.
- `OptLevel` enum, CLI flags, and test-runner `opt_level` field are
  preserved.

## Open Questions

- Should `generate_ir` (used by `--emit asm`) always run the full opt
  pipeline at the requested level, or should there be a way to get
  post-build / pre-opt IR for debugging? Proposal: always run passes at the
  requested level, since `-O0` skips `run_passes` entirely, which already
  gives a "pre-opt" mode.

## Future Work

- LTO: `module.run_passes("lto<O3>", ...)` over a merged module if / when
  multi-object linking is replaced with a single LLVM module (already the
  case today, so this is essentially free later).
- Expose a `-C passes=...` escape hatch like rustc's, for experimentation.
- PGO / instrumentation support once a profiling story exists.

## References

- [ADR-0012: Compiler Optimization Passes](0012-optimization-passes.md) —
  superseded by this decision.
- [ADR-0033: LLVM Backend and Comptime Interpreter](0033-llvm-backend-and-comptime-interpreter.md)
  — the backend migration that made the CFG passes redundant.
- [LLVM New Pass Manager](https://llvm.org/docs/NewPassManager.html) —
  `default<OX>` pipeline definitions used by `run_passes`.
- `inkwell::module::Module::run_passes` (inkwell 0.9).
