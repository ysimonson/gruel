---
id: 0033
title: LLVM Backend and Comptime Interpreter
status: proposal
tags: [compiler, codegen, comptime, llvm]
feature-flag: llvm-backend
created: 2026-04-13
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0033: LLVM Backend and Comptime Interpreter

## Status

Proposal

## Summary

Replace the custom x86-64 and AArch64 machine code backends (~28,000 lines) with an LLVM backend via the `inkwell` crate, gaining broad platform support and production-quality optimization for free. To keep comptime fully functional and extensible through this transition — and beyond — replace the current expression-only constant folder (`try_evaluate_const_in_rir`) with a proper AIR-level interpreter that has a call stack, mutable local state, and a comptime heap. Comptime remains entirely a semantic-analysis concern; LLVM only ever sees fully-resolved, monomorphized CFGs.

## Context

### The codegen problem

The compiler currently maintains two parallel backends:

| File | Lines |
|------|-------|
| `gruel-codegen/src/x86_64/cfg_lower.rs` | 4,328 |
| `gruel-codegen/src/x86_64/emit.rs` | 4,344 |
| `gruel-codegen/src/aarch64/cfg_lower.rs` | 4,424 |
| `gruel-codegen/src/aarch64/emit.rs` | 3,422 |

Every new instruction type, calling convention detail, or codegen fix must be applied to both backends. Adding a third target (RISC-V, WASM) would require ~8,000 more lines. `gruel-linker` is another ~1,500 lines of custom ELF writing that duplicates functionality available in every system toolchain.

### The comptime interpreter problem

ADR-0025 implemented comptime through `try_evaluate_const_in_rir()` in `sema/analysis.rs`. This is a recursive descent function over RIR that returns `Option<ConstValue>` — effectively a constant folder. It cannot:

- Maintain mutable variable state across operations (no activation frames)
- Execute loops that modify variables (each `while` iteration needs fresh state)
- Call functions and return their result into a caller's local variables
- Allocate composite comptime values (structs, arrays)

These limitations will become blockers as comptime grows: comptime allocations, comptime reflection (`@typeInfo`), comptime strings, and comptime I/O all require a real execution model. The interpreter should be made correct and complete before the backend changes, so that comptime continues to work correctly throughout the LLVM transition.

### Why LLVM over alternatives

Cranelift was considered but LLVM was chosen because:
- Broader platform coverage (LLVM supports more targets than Cranelift, including 32-bit targets and exotic embedded architectures)
- More mature optimization pipeline
- Better ecosystem (debug info, sanitizers, profiling)

The `inkwell` crate provides safe Rust bindings to LLVM's C API and is the standard approach for LLVM-backed Rust compilers.

## Decision

### Pipeline after this change

```
Source → Lexer → Parser → AstGen → Sema+Interp → AIR → CFG → LLVM IR → .o → link → binary
                                          ↑
                               comptime interpreter
                               (replaces try_evaluate_const_in_rir)
```

Comptime is invisible to LLVM. By the time the CFG is built, all comptime blocks are evaluated, all generic functions are monomorphized, and all type values are resolved to concrete types. LLVM receives plain, concrete CFGs.

### Phase 1: AIR Interpreter

The current `try_evaluate_const_in_rir()` evaluates RIR expressions. A proper interpreter needs to evaluate AIR _statements_ — it needs state.

#### Execution model

```rust
// Rough structure of the new interpreter
struct ComptimeInterpreter<'a> {
    sema_ctx: &'a SemaCtx,        // Access to type tables, function defs
    call_stack: Vec<Frame>,        // Active call frames
    heap: ComptimeHeap,            // Comptime-only allocations
    step_budget: u32,              // Prevents infinite loops (configurable)
}

struct Frame {
    locals: IndexMap<LocalId, ConstValue>,  // Mutable local variable storage
    return_value: Option<ConstValue>,
}
```

#### ConstValue extensions

The existing `ConstValue` enum needs two new variants to support composite comptime values:

```rust
pub enum ConstValue {
    Integer(i64),
    Bool(bool),
    Type(TypeId),
    Unit,
    Struct(StructId, Vec<ConstValue>),  // comptime struct instances
    Array(Vec<ConstValue>),             // comptime array instances
}
```

#### What the interpreter handles

- All arithmetic, comparison, logical, and bitwise operations (already handled by try_evaluate_const — direct port)
- Mutable `let` bindings and assignments within a comptime frame
- `if`/`else` with comptime conditions
- `while` and `loop` with mutable loop variables (step budget enforced)
- `break` and `continue`
- Function calls: analyze the callee to AIR on demand, push a new frame, execute, pop
- Struct construction and field access
- Array construction and indexing
- `return` propagates through the call stack

#### What is a compile error if attempted in comptime

- System calls or intrinsics with side effects
- Calling external (`extern`) functions
- Raw pointer dereferences
- Operations that would overflow (compile error, not silent wrap)

#### Integration with Sema

The interpreter replaces `try_evaluate_const_in_rir()`. The Sema pass calls it when it encounters:
- `comptime { ... }` blocks
- `const NAME: T = expr` declarations
- Arguments to `comptime` parameters at call sites

Because the interpreter works on AIR (typed), comptime-called functions must be analyzed to AIR before they are executed. Sema already processes declarations before uses; for comptime function calls, the callee is analyzed on-demand if not already done.

The interpreter returns `Result<ConstValue, CompileError>` rather than `Option<ConstValue>` — a failure is always a compile error in a comptime context, not a silent deferral.

### Phase 2: LLVM Backend

#### New crate: `gruel-codegen-llvm`

Dependencies: `inkwell` (with the appropriate LLVM version feature flag).

Input: `CfgModule` (the same type the existing backends consume).  
Output: an in-memory object file buffer, passed to the system linker.

The existing `gruel-codegen` crate's public interface (`generate(cfg, options) -> ObjectFile`) is preserved; `gruel-codegen-llvm` implements the same interface.

#### CFG → LLVM IR translation

CFG basic blocks map 1:1 to LLVM basic blocks. The translation is straightforward because CFG already makes control flow explicit:

| CFG concept | LLVM IR |
|---|---|
| `CfgInstData::Const` | `ConstantInt` / `ConstantStruct` |
| `CfgInstData::Add/Sub/Mul/...` | `BuildAdd` / `BuildSub` / `BuildMul` / ... |
| `CfgInstData::ICmp` | `BuildICmp` |
| `CfgInstData::Load` | `BuildLoad` |
| `CfgInstData::Store` | `BuildStore` |
| `CfgInstData::Call` | `BuildCall` |
| `CfgInstData::GEP` (struct/array field) | `BuildGEP` |
| `CfgTerminator::Branch` | `BuildCondBr` |
| `CfgTerminator::Jump` | `BuildBr` |
| `CfgTerminator::Return` | `BuildRet` |

Type mapping:

| Gruel type | LLVM type |
|---|---|
| `i8/i16/i32/i64` | `i8/i16/i32/i64` |
| `bool` | `i1` |
| `()` (unit) | `void` |
| `[T; N]` | `[N x T]` |
| struct | `{ field_types... }` (packed layout matching Gruel's ABI) |
| pointer | `ptr` (opaque pointer, LLVM ≥ 15) |

#### Linking

Replace `gruel-linker` with a system linker invocation. After LLVM emits a `.o` file:

```rust
// Simplified — actual implementation handles more flags
Command::new("cc")
    .arg("-o").arg(output_path)
    .arg(object_file_path)
    .arg(runtime_lib_path)
    .status()?;
```

This removes the entire `gruel-linker` crate and all ELF writing code.

#### `--emit` flag changes

| Flag | Old behavior | New behavior |
|---|---|---|
| `--emit tokens/ast/rir/air/cfg` | Unchanged | Unchanged |
| `--emit mir` | Machine IR with virtual registers | Removed (no MIR in LLVM path) |
| `--emit asm` | Annotated pseudo-assembly | LLVM textual IR (`*.ll`) |

If native assembly output is needed, LLVM's `emit_to_file` with `FileType::AssemblyFile` can produce it. This can be added as `--emit native-asm` if desired.

#### Running both backends simultaneously during transition

During Phase 2, both backends coexist. A compiler flag (`--codegen=llvm|native`, defaulting to `native`) selects which to use. The spec test suite runs against both to verify parity before the old backend is removed.

### Phase 3: Remove Custom Backends

Once the LLVM backend passes all spec, UI, and integration tests:

1. Delete `gruel-codegen/src/x86_64/`
2. Delete `gruel-codegen/src/aarch64/`
3. Delete `gruel-linker` crate
4. Remove MIR types: `X86Mir`, `Aarch64Mir`, `VReg`, `PhysReg`, and associated infrastructure
5. Remove shared codegen infrastructure that only served the old backends: `liveness.rs`, `regalloc.rs`, `stack_frame.rs`, `vreg.rs` from `gruel-codegen`
6. Rename or repurpose `gruel-codegen` → `gruel-codegen-llvm` (or just move the LLVM crate's contents in)
7. Remove the `--codegen` flag; LLVM is the only path
8. Update `gruel-compiler/src/lib.rs` to remove the old backend dispatch

## Implementation Phases

- [x] **Phase 1a: Interpreter core** — `ComptimeInterpreter` struct, `Frame`, `ComptimeHeap`; execute arithmetic, comparisons, `if`/`else`, mutable bindings. All existing comptime spec tests must still pass.
- [x] **Phase 1b: Interpreter loops and control flow** — `while`, `loop`, `break`, `continue`, `return`; add step budget; tests covering loop-based comptime computation.
- [x] **Phase 1c: Interpreter function calls** — push/pop frames, on-demand callee analysis, call stack depth limit; tests for comptime functions calling other comptime functions.
- [x] **Phase 1d: Interpreter composite values** — `ConstValue::Struct`, `ConstValue::Array`, struct construction, field access, array indexing; tests for comptime struct/array manipulation.
- [x] **Phase 1e: Wire in and delete old evaluator** — replace all call sites of `try_evaluate_const_in_rir()` with the new interpreter; return `CompileError` instead of `None` in comptime contexts; delete the old function; full test suite green.
- [x] **Phase 2a: LLVM backend scaffolding** — add `gruel-codegen-llvm` crate, `inkwell` dependency, LLVM context/module/builder setup, type mapping, function declaration stubs.
- [x] **Phase 2b: Basic block and arithmetic translation** — translate CFG blocks, all arithmetic/comparison/logical instructions, `BuildRet`; smoke tests with simple functions.
- [x] **Phase 2c: Control flow translation** — `BuildCondBr`, `BuildBr`, loops; all spec tests that don't involve structs/arrays must pass.
- [x] **Phase 2d: Struct and array support** — `BuildGEP`, struct layout, array indexing, calling convention for struct return values.
- [x] **Phase 2e: Replace linker** — emit `.o` via LLVM, invoke system linker; full spec suite green via LLVM path. (gruel-linker removal deferred to Phase 3.)
- [x] **Phase 2f: Feature parity verification** — run full spec + UI + integration suite against both `--codegen=native` and `--codegen=llvm`; fix any divergences. (1369/1369 tests pass on both backends.)
- [ ] **Phase 3: Remove custom backends** — delete x86_64, aarch64 source trees, `gruel-linker` crate, all MIR infrastructure; remove `--codegen` flag; full suite green.

## Consequences

### Positive

- ~28,000 lines of architecture-specific codegen deleted
- ~1,500 lines of custom ELF linker deleted
- Free platform support: any LLVM target works without writing a new backend
- LLVM's optimization passes improve generated code quality
- Comptime interpreter is now extensible: future work (comptime alloc, reflection, I/O) has a real execution model to build on
- Debug info (DWARF) becomes straightforward via LLVM's DIBuilder

### Negative

- LLVM is a large system dependency (~1 GB installed); the compiler's own build time increases significantly
- `inkwell` pins to specific LLVM major versions; LLVM upgrades require inkwell coordination
- Generated code is less directly inspectable (can't add a `//comment` to our own emit.rs)
- `--emit asm` semantics change from custom pseudo-assembly to LLVM IR
- System `cc`/`clang` must be present to link; cross-compilation requires a cross linker

### Neutral

- The `gruel-codegen` crate's public interface is unchanged; `gruel-compiler` needs only a new codegen dispatch
- `--emit mir` disappears (no MIR in the LLVM path); this is a debug tool, not a spec-mandated flag

## Open Questions

1. **LLVM version**: Which major version to target? LLVM 18 or 19 are current stable candidates. Later is better for opaque pointer support (removes some complexity).
2. **`inkwell` vs direct `llvm-sys`**: `inkwell` is safer and more ergonomic; `llvm-sys` gives more control. Start with `inkwell`.
3. **LLVM build strategy**: System LLVM (`brew install llvm` / `apt install llvm-dev`) vs bundled (`llvm-sys` with static linking). System is faster to compile; bundled is more portable. A Cargo feature can support both.
4. **`gruel-linker` fate**: Delete entirely, or keep behind a feature flag for potential no-std/embedded use cases where a system linker isn't available?
5. **Interpreter crate placement**: New `gruel-interp` crate (clean separation) vs extending `gruel-air/src/sema/` (fewer dependency edges). Leaning toward `gruel-air/src/sema/interp.rs` to avoid a new crate for code that is tightly coupled to the type system.

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md) — the comptime model this builds on
- [ADR-0003: Constant Expression Evaluation](0003-constant-evaluation.md) — superseded by Phase 1
- [inkwell](https://github.com/TheDan64/inkwell) — safe LLVM bindings for Rust
- [rustc_codegen_llvm](https://github.com/rust-lang/rust/tree/master/compiler/rustc_codegen_llvm) — reference for CFG → LLVM IR translation patterns
