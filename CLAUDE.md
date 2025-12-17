# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rue is a systems programming language aiming for memory safety without garbage collection, with higher-level ergonomics than Rust/Zig. Currently in early development with Rust-like syntax.

## Build System

This project uses Buck2 (via `./buck2` wrapper script), not Cargo.

You will need to source `~/.profile` before running any commands.

### Common Commands

```bash
# Build the compiler
./buck2 build //crates/rue:rue

# Build everything
./buck2 build //...

# Run all tests (unit + spec)
./test.sh

# Run unit tests only
./buck2 test //...

# Run spec tests only
./buck2 run //crates/rue-spec:rue-spec

# Run a specific crate's tests
./buck2 test //crates/rue-lexer:rue-lexer-test

# Filter spec tests by pattern
./buck2 run //crates/rue-spec:rue-spec -- "1.1"  # Section 1.1
./buck2 run //crates/rue-spec:rue-spec -- "zero" # Tests matching "zero"

# Compile and run a program
./buck2 run //crates/rue:rue -- source.rue output
./output

# Dump intermediate representations
./buck2 run //crates/rue:rue -- --dump-rir source.rue  # Untyped IR
./buck2 run //crates/rue:rue -- --dump-air source.rue  # Typed IR
./buck2 run //crates/rue:rue -- --dump-mir source.rue  # Machine IR
```

## Architecture

The compiler pipeline transforms source through successive IRs:

```
Source → Lexer → Parser → AstGen → Sema → Lower → RegAlloc → Emit → Link
         tokens   AST      RIR      AIR    X86Mir          bytes   ELF
```

### Crate Responsibilities

| Crate | Purpose |
|-------|---------|
| `rue` | CLI binary |
| `rue-compiler` | Pipeline orchestration |
| `rue-lexer` | Tokenization |
| `rue-parser` | AST construction |
| `rue-rir` | Untyped IR (post-parse, pre-typing) |
| `rue-air` | Typed IR (after semantic analysis) |
| `rue-codegen` | x86-64 machine code generation |
| `rue-linker` | ELF object file creation and linking |
| `rue-error` | Error types |
| `rue-span` | Source location tracking |
| `rue-intern` | String interning |
| `rue-spec` | Specification test runner |
| `rue-runtime` | Runtime support |

### Key Design Decisions

- **Architecture-specific MIR**: Each target gets its own machine IR (currently X86Mir), following Zig's approach
- **Index-based references**: Instructions stored in vectors, referenced by u32 indices (cache-friendly, no lifetimes)
- **Direct code emission**: No LLVM dependency; machine code emitted directly
- **Minimal ELF**: Static executables with direct syscalls (Linux x86-64 only)

## Testing

### Unit Tests
Add to relevant crate's source file with `#[cfg(test)]` modules. Ensure crate has `rust_test` target in its `BUCK` file.

### Specification Tests
Add test cases to `.toml` files in `crates/rue-spec/cases/`:

```toml
# Run-pass test
[[case]]
name = "my_test"
source = "fn main() -> i32 { 42 }"
exit_code = 42

# Compile-fail test
[[case]]
name = "my_error_test"
source = "fn main() { }"
compile_fail = true
error_contains = "expected '->'"

# Golden test (exact IR output)
[[case]]
name = "my_golden_test"
source = "fn main() -> i32 { 42 }"
expected_air = """
function main:
air (return_type: i32) {
    %0 : i32 = const 42
    %1 : i32 = ret %0
}
"""
```

## Modifying the Grammar

1. Update `rue-lexer` if new tokens needed
2. Update `rue-parser` for new syntax
3. Update `rue-rir` for new IR instructions
4. Update `rue-air` for typed versions
5. Update `rue-codegen` for code generation
6. Add spec tests in `crates/rue-spec/cases/`

## Version Control

Uses Jujutsu (jj): `jj status`, `jj diff`, `jj commit -m "msg"`, `jj log`

## Code Style

- Standard Rust formatting (rustfmt)
- Rust edition 2024
