---
id: 0077
title: LLVM-backed target system
status: implemented
tags: [codegen, target, cross-compilation]
created: 2026-05-02
accepted: 2026-05-03
implemented: 2026-05-03
spec-sections: []
superseded-by:
---

# ADR-0077: LLVM-backed target system

## Status

Implemented (2026-05-03).

## Summary

Replace the three-variant `Target` enum in `gruel-target` with a struct backed by
`target_lexicon::Triple`, accepting any LLVM-supported triple from the command line.
Expand the Gruel-language `Arch` and `Os` enums from 2 variants each to cover the
full set of practically relevant LLVM targets. Remove dead code left over from the
custom ELF backend. Wire the compilation target into inkwell so `--target` actually
produces cross-compiled output.

## Context

`gruel-target` was written when Gruel had a custom x86-64 ELF backend. At that time,
enumerating three targets (x86-64 Linux, AArch64 Linux, AArch64 macOS) was sufficient
and the `Target` type carried ELF-specific metadata (`elf_machine`, `default_base_addr`,
`macos_min_version`) needed by the hand-written linker.

Since ADR-0024 the backend is LLVM (via inkwell). LLVM supports dozens of architectures
out of the box, but two bugs were never fixed during the migration:

1. **Cross-compilation is silently broken.** All three `TargetMachine` creation sites in
   `gruel-codegen-llvm/src/codegen.rs` call `TargetMachine::get_default_triple()` — the
   host machine's triple — instead of the triple passed via `--target`. Passing
   `--target aarch64-linux` on an x86-64 host produces x86-64 object files.

2. **`@target_arch()` and `@target_os()` reflect the host, not the compile target.**
   Both intrinsic handlers call `Target::host().arch()` / `Target::host().os()` instead
   of `self.options.target.arch()` / `self.options.target.os()`. This means conditional
   compilation based on `@target_arch()` cannot be used with `--target`.

Additional problems introduced by the old design:
- `elf_machine`, `default_base_addr`, `macos_min_version` are ELF-writer debris; nothing
  outside `gruel-target`'s own test suite calls them.
- `Arch` and `Os` in `gruel-target` have 2 variants each; the Gruel-language `Arch`
  and `Os` enums (in `gruel-builtins`) likewise have only 2 variants, meaning programs
  compiled to Windows or RISC-V have no way to write platform-conditional code.

## Decision

### 1. Replace `Target` with a `target_lexicon`-backed struct

Add `target-lexicon = "0.12"` to `gruel-target`. Replace the enum with:

```rust
pub struct Target {
    triple: target_lexicon::Triple,
}
```

`Target` is no longer `Copy` (because `target_lexicon::Triple` is not `Copy`). Call
sites that relied on implicit copy are updated to use `Clone` or take references.

`Target::from_str` accepts any string that `target_lexicon::Triple` can parse.

`Target::host()` uses `target_lexicon::HOST` (a compile-time constant provided by the
crate) rather than `#[cfg(...)]` ladders.

`Target::all()` returns the curated list of "blessed" targets — those that Gruel
explicitly tests and supports. These are the same three as today; adding a new blessed
target is a one-line change to the constant list.

The CLI `--target` flag still defaults to `Target::host()`, accepts any valid triple,
and emits a warning if the triple is not in the blessed list.

### 2. Derive `arch()` and `os()` from the lexicon

The compiler-internal `Arch` and `Os` types in `gruel-target` expand to cover
everything `target_lexicon` models. New variants are derived from the parsed triple
rather than hardcoded match arms. The full set Gruel needs today:

```
Arch: X86, X86_64, Arm, Aarch64, Riscv32, Riscv64, Wasm32, Wasm64
Os:   Linux, Macos, Windows, Freestanding, Wasi
```

The existing `is_elf()` and `is_macho()` helpers (used by the linker) are kept; they
are derived from `triple.binary_format`.

### 3. Remove dead custom-backend methods

Delete from `gruel-target`:
- `elf_machine()` — ELF machine constant; LLVM handles this internally
- `default_base_addr()` — load address; the LLVM linker script owns this
- `macos_min_version()` — Mach-O version encoding; the system linker handles it
- `page_size()`, `stack_alignment()`, `pointer_size()` — useful concepts, but currently
  called by nobody outside tests; delete now, re-add when actually needed by a pass

### 4. Wire the target triple into inkwell

Add `target: Target` to `CodegenInputs` (and `compile_bitcode_to_object`). All three
TargetMachine creation sites in `codegen.rs` replace:

```rust
let target_triple = TargetMachine::get_default_triple();
```

with:

```rust
let target_triple = TargetTriple::create(inputs.target.triple());
```

For cross-compilation, LLVM must be built with the required backend enabled. For
targets whose LLVM backend is not compiled into the host's LLVM installation, codegen
returns a clear error rather than silently falling back to the host.

`LlvmTarget::initialize_native` is replaced with `initialize_all` when the compile
target differs from the host, so all backends are available.

### 5. Expand `Arch` and `Os` in the Gruel language

Update `gruel-builtins` to expand `ARCH_ENUM` and `OS_ENUM`. New variants are
**appended** to preserve existing variant indices (existing programs remain correct):

```
Arch:
  0: X86_64      (existing)
  1: Aarch64     (existing)
  2: X86         (new)
  3: Arm         (new)
  4: Riscv32     (new)
  5: Riscv64     (new)
  6: Wasm32      (new)
  7: Wasm64      (new)

Os:
  0: Linux       (existing)
  1: Macos       (existing)
  2: Windows     (new)
  3: Freestanding (new)
  4: Wasi        (new)
```

`@target_arch()` and `@target_os()` in sema are updated to:
- Use `self.options.target.arch()` / `self.options.target.os()` (the compile target)
- Map each variant to its correct index via the expanded enums

## Implementation Phases

- [x] **Phase 1: Rewrite `gruel-target` with `target_lexicon`**
  - Add `target-lexicon` dependency to `gruel-target/Cargo.toml`
  - Replace `Target` enum with `Target(target_lexicon::Triple)` newtype struct
  - Implement `Target::host()` via `target_lexicon::HOST`
  - Implement `Target::all()` as a const-defined blessed list
  - Implement `Target::from_str` by delegating to `target_lexicon`
  - Expand compiler-internal `Arch` and `Os` enums (8 and 5 variants)
  - Implement `arch()` and `os()` via lexicon field mapping
  - Update `is_elf()` / `is_macho()` via `triple.binary_format`
  - Update call sites that relied on `Target: Copy` to use `Clone` / `&Target`
  - All existing tests must pass; add tests for new triples

- [x] **Phase 2: Remove dead custom-backend methods**
  - Delete `elf_machine`, `default_base_addr`, `macos_min_version`, `page_size`,
    `stack_alignment`, `pointer_size` from `Target`
  - Delete their tests
  - (Already accomplished as part of the Phase 1 rewrite — the new module
    omits these methods and their tests.)

- [x] **Phase 3: Wire target into inkwell**
  - Add `target: Target` field to `CodegenInputs` and `compile_bitcode_to_object`
  - Update `gruel-compiler` to populate the field from `options.target`
  - Replace `get_default_triple()` at all three sites in `codegen.rs`
  - Use `initialize_all` when cross-compiling (target arch ≠ host arch)
  - Add integration test: compile to a non-host target and verify the object file
    header matches the expected machine type (via `readelf`/`otool`)

- [x] **Phase 4: Expand Gruel-language `Arch`/`Os` and fix intrinsics**
  - Expand `ARCH_ENUM.variants` and `OS_ENUM.variants` in `gruel-builtins`
  - Update the variant-index match arms in `analyze_target_arch_intrinsic` /
    `analyze_target_os_intrinsic` to use `self.options.target` (not `Target::host()`)
  - Extend the variant-index match arms in the constant-fold path (lines 8718–8737
    in `sema/analysis.rs`)
  - Add spec tests covering the new variants and cross-compilation behavior

## Consequences

### Positive

- `--target` cross-compilation actually works end-to-end
- `@target_arch()` / `@target_os()` reflect the compile target, enabling correct
  conditional compilation in cross-compiled builds
- Arbitrary LLVM triples are accepted; users can target platforms Gruel doesn't
  officially bless
- The hardcoded ELF debris is gone; the codebase no longer pretends the custom
  backend exists
- Adding a new blessed target is a one-line change plus codegen/linker wiring

### Negative

- `Target` loses `Copy`; call sites need minor updates (`clone()` or `&Target`)
- Cross-compilation requires the target's LLVM backend to be compiled into the host's
  LLVM install — the same constraint as `rustc`

## Open Questions

- Should the warning for unblessed triples be a hard error or just informational?
  (Leaning: informational warning, since adventurous users should be able to experiment)
- Does `initialize_all` meaningfully increase link time against LLVM? If so, we may want
  to initialize only the specific backend via `Target::initialize_<arch>`.

## Future Work

- Windows cross-compilation additionally requires `lld` or `link.exe`; this ADR only
  covers the codegen step, not the Windows-specific linker flags
- CPU feature detection (`--target-cpu`, `-march`): currently hard-coded to `"generic"`.
  Worth exposing once we have users who need micro-architecture tuning.
- Tier classification for blessed targets (tier 1: tested in CI; tier 2: builds but not CI)

## References

- [`target-lexicon` crate](https://crates.io/crates/target-lexicon) — Bytecodealliance's
  LLVM triple parser used by Wasmtime and Cranelift
- Rust's `TargetTriple` in `rustc_target::spec` — the enum + JSON approach Rust uses
- Zig's `std.Target` — the structured (Cpu + Os + Abi) approach Zig uses
- `gruel-codegen-llvm/src/codegen.rs` lines 269, 303, 341 — the three broken sites
- `gruel-air/src/sema/analysis.rs` lines 11491, 11536 — the broken intrinsic handlers
