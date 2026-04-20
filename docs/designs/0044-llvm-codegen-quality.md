---
id: 0044
title: LLVM Codegen Quality Improvements and Build Profiles
status: proposal
tags: [codegen, llvm, optimization, cli]
feature-flag:
created: 2026-04-20
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0044: LLVM Codegen Quality Improvements and Build Profiles

## Status

Proposal

## Summary

Improve the quality of LLVM IR emitted by `gruel-codegen-llvm` so that LLVM's existing optimization passes can do more with it, and add `--debug`/`--release` build profile flags to the CLI. The four codegen changes are: (1) mark panic helper functions `cold`, (2) add branch weight metadata to runtime check branches, (3) emit `llvm.lifetime.start`/`llvm.lifetime.end` intrinsics, and (4) annotate `inout` parameters with `noalias`. The CLI adds `--debug` (maps to `-O0`) and `--release` (maps to `-O3`), mutually exclusive with each other and with explicit `-O` flags.

## Context

Gruel currently passes the `default<OX>` pipeline string to LLVM's `PassBuilder` and does nothing else to help LLVM optimize. The IR we emit is correct but leaves optimization opportunities on the table:

1. **Panic functions lack `cold`**: `__gruel_overflow`, `__gruel_bounds_check`, `__gruel_div_by_zero`, and `__gruel_intcast_overflow` are declared with `noreturn` but not `cold`. Without `cold`, LLVM's block placement, inlining heuristics, and SimplifyCFG treat the panic path as equally likely as the happy path.

2. **No branch weights on runtime checks**: Every overflow check, bounds check, and division-by-zero check generates a conditional branch where the "panic" side is almost never taken, but LLVM has no metadata to know this. Branch probability defaults to 50/50 without `!prof` metadata or `llvm.expect`.

3. **No lifetime markers**: Every `alloca` created by `get_or_create_local` and `get_or_create_param_alloca` lives for the entire function. LLVM's stack coloring pass cannot reuse stack slots for non-overlapping lifetimes because there are no `llvm.lifetime.start`/`llvm.lifetime.end` intrinsics.

4. **No `noalias` on `inout` parameters**: The language spec (§6.1) explicitly forbids passing the same variable to multiple `inout` parameters, and the compiler enforces this (see `check_exclusivity` in `sema/analysis.rs`). The spec also forbids mixing `borrow` and `inout` on the same variable in a single call. This means `inout` pointers are guaranteed non-aliasing at every call site, but LLVM doesn't know this because we don't emit `noalias`.

5. **No `--debug`/`--release` shorthand**: Users must remember `-O0` vs `-O3`. Other compilers provide profile names as shorthand.

## Decision

### 1. `cold` attribute on panic functions

In `codegen.rs`, functions declared via `get_or_declare_noreturn_fn` will additionally receive the `cold` attribute:

```rust
f.add_attribute(
    AttributeLoc::Function,
    self.ctx.create_string_attribute("cold", ""),
);
```

This applies to all four panic helpers: `__gruel_overflow`, `__gruel_bounds_check`, `__gruel_div_by_zero`, `__gruel_intcast_overflow`.

### 2. Branch weight metadata on runtime checks

After each conditional branch in `build_checked_int_op`, `build_bounds_check`, and `build_div_zero_check` (and the narrowing-cast overflow check), emit `!prof` branch weight metadata. The weights should strongly favor the non-panic branch. We use `llvm.expect.i1` on the condition before branching, which is simpler than manually attaching `!prof` metadata through inkwell:

```rust
// Wrap the overflow flag with llvm.expect(overflow, false)
let expect_fn = self.module.get_intrinsic("llvm.expect", &[self.ctx.bool_type().into()]);
let expected = self.builder.build_call(expect_fn, &[overflow.into(), self.ctx.bool_type().const_zero().into()], "").unwrap()
    .try_as_basic_value().left().unwrap().into_int_value();
self.builder.build_conditional_branch(expected, overflow_bb, cont_bb);
```

If `inkwell` doesn't expose `get_intrinsic` cleanly for `llvm.expect`, we fall back to declaring it manually as `declare i1 @llvm.expect.i1(i1, i1)`.

**Alternative considered**: Attaching `!prof` metadata directly to branch instructions. This is more precise but inkwell's metadata API is limited and brittle across LLVM versions. `llvm.expect` achieves the same effect and is the approach Rust uses.

### 3. Lifetime markers

For each `alloca` created by `get_or_create_local`, emit `llvm.lifetime.start` immediately after the alloca, and `llvm.lifetime.end` at function return points (or scope exits, if we can determine them).

**Scope**: The initial implementation will use a conservative approach — `lifetime.start` at allocation, `lifetime.end` at every function return terminator. This is correct (lifetimes may be over-extended but never under-extended) and captures the main benefit: LLVM can at minimum distinguish function parameters from locals, and locals from each other when they're created at different points.

A more precise implementation tracking lexical scopes can come later if profiling shows stack size is a problem.

**Note**: `llvm.lifetime.start`/`llvm.lifetime.end` take a size parameter (`i64`) and a pointer. The size should be the `abi_size_of` the type, or `-1` if unknown.

### 4. `noalias` on `inout` parameters

When declaring functions in `declare_function`, iterate over parameters and add the `noalias` attribute to any parameter whose corresponding `param_modes` entry is `true` (inout):

```rust
for (llvm_idx, is_inout) in /* mapped param indices */ {
    if is_inout {
        fn_value.add_attribute(
            AttributeLoc::Param(llvm_idx),
            ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("noalias"), 0),
        );
    }
}
```

**Soundness argument**: The spec (§6.1) states:

> A single function call MUST NOT pass the same variable to multiple inout parameters.

> A single function call MUST NOT pass the same variable to both a borrow parameter and an inout parameter.

The compiler enforces both rules in `check_exclusivity`. This means at every call site, each `inout` pointer is guaranteed unique — exactly the semantics of `noalias`.

**Borrow parameters**: `borrow` parameters are read-only, shared references. Multiple borrows of the same variable are allowed, so `noalias` would be unsound for borrow params. However, we could add `readonly` to borrow params. This is deferred — the CFG's `param_modes: Vec<bool>` currently only distinguishes inout vs. not-inout, so we'd need to widen it to a proper enum (or a second bitvec) to know which non-inout params are borrow. That's a separate change.

### 5. `--debug` and `--release` CLI flags

Add two new flags to the CLI:

- `--debug` — equivalent to `-O0`
- `--release` — equivalent to `-O3`

**Mutual exclusivity rules** (all of these are compile errors):
- `--debug --release` — cannot use both profiles
- `--debug -O2` — cannot mix profile with explicit opt level
- `--release -O1` — cannot mix profile with explicit opt level

When neither `--debug`, `--release`, nor `-O<N>` is specified, the default remains `-O0` (same as today).

The flags are parsed in `parse_args_from` in `main.rs`. No changes to `CompileOptions` or `OptLevel` are needed — the flags simply set `opt_level` the same way `-O0` and `-O3` do, with an additional check for conflicts.

Help text update:

```
  --debug              Build without optimizations (equivalent to -O0)
  --release            Build with full optimizations (equivalent to -O3)
  -O<level>            Set optimization level (default: -O0)
                       Levels: -O0, -O1, -O2, -O3
                       Cannot be used with --debug or --release
```

## Implementation Phases

- [ ] **Phase 1: `cold` attribute on panic functions** — Add `cold` alongside `noreturn` in `get_or_declare_noreturn_fn`. Verify with `--emit asm` that the attribute appears. Add a golden test.
- [ ] **Phase 2: Branch weight metadata via `llvm.expect`** — Add `llvm.expect.i1` calls in `build_checked_int_op`, `build_bounds_check`, `build_div_zero_check`, and the intcast overflow checks. Verify with `--emit asm`.
- [ ] **Phase 3: Lifetime markers** — Emit `llvm.lifetime.start` after each local alloca in `get_or_create_local`, and `llvm.lifetime.end` at return terminators. Requires tracking which locals were allocated so we can end their lifetimes. Verify with `--emit asm` at `-O0`.
- [ ] **Phase 4: `noalias` on `inout` parameters** — Add the attribute in `declare_function` for inout params. Add a spec test or golden test showing the attribute. Verify that an optimized build of a two-inout-param function generates better code (e.g., no redundant reload after a store through one param).
- [ ] **Phase 5: `--debug`/`--release` CLI flags** — Add flag parsing with mutual exclusivity checks. Add unit tests for all valid and invalid combinations. Update help text.

## Consequences

### Positive

- Better optimized code at `-O1` and above with no changes to the LLVM pass pipeline
- `cold` and branch weights improve code layout and inlining decisions for code with many runtime checks (which is all Gruel code — every `+`, `-`, `*`, array index, and narrowing cast)
- Lifetime markers reduce stack frame size for functions with many locals
- `noalias` enables LLVM to reorder loads/stores across `inout` params, a significant win for functions like `swap(inout a, inout b)`
- `--debug`/`--release` are familiar to Rust/Cargo users and easier to remember than `-O` flags

### Negative

- `llvm.expect` calls add IR bulk at `-O0` (though they're no-ops semantically). We could gate them behind `-O1+` if this matters for debug compile time, but the overhead should be negligible.
- Lifetime markers add complexity to the codegen — we need to track which locals have been allocated to emit `lifetime.end` at returns.
- `noalias` correctness depends on the compiler's `check_exclusivity` being sound. If that check has bugs, `noalias` could cause miscompilations. This is the same class of risk Rust takes with `&mut` → `noalias`.

## Resolved Questions

- Should `llvm.expect` calls and lifetime markers be emitted at `-O0`, or only at `-O1+`? Emitting at all levels is simpler and keeps `--emit asm` output consistent, but adds noise to debug IR. The `cold` attribute and `noalias` are harmless at `-O0` since they're just metadata. Yes, emit at all levels.
- Should we also add `nounwind` to Gruel functions that can't unwind (which is all of them, since Gruel doesn't have exceptions)? This is a separate optimization but closely related. Yes

## Future Work

- Add `readonly` to `borrow` parameters (requires widening `Cfg::param_modes` from `Vec<bool>` to distinguish inout/borrow/normal).
- Pre-LLVM overflow check elimination in the CFG builder (proving checks redundant from value ranges before LLVM sees them).
- Custom LLVM pass pipeline if profiling reveals `default<OX>` is suboptimal for Gruel's IR patterns.

## References

- [LLVM LangRef: Branch Weight Metadata](https://llvm.org/docs/BranchWeightMetadata.html)
- [LLVM LangRef: `llvm.expect`](https://llvm.org/docs/LangRef.html#llvm-expect-intrinsic)
- [LLVM LangRef: `llvm.lifetime.start` / `llvm.lifetime.end`](https://llvm.org/docs/LangRef.html#llvm-lifetime-start-intrinsic)
- [LLVM LangRef: `noalias` parameter attribute](https://llvm.org/docs/LangRef.html#parameter-attributes)
- Spec §6.1 — Function parameters, inout exclusivity rules
- ADR-0034 — Removal of CFG-level optimization passes
