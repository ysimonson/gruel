# Development Guide

This document covers how to build, test, and contribute to the Gruel compiler.

## Prerequisites

- **Rust toolchain** - Install via [rustup](https://rustup.rs/). The `rust-toolchain.toml` in the repo root ensures the right version.

### Platform-Specific

| Platform | Requirements |
|----------|--------------|
| Linux x86_64 | None |
| macOS ARM64 | Xcode Command Line Tools (`xcode-select --install`) |
| macOS x86_64 | Xcode Command Line Tools (`xcode-select --install`) |

**Why macOS needs Xcode**: The Gruel compiler uses the system `clang` to link executables on macOS. On Linux, Gruel uses its own internal ELF linker.

## Repository Structure

```
gruel/
├── crates/
│   ├── gruel/           # CLI binary
│   ├── gruel-air/       # Typed IR + semantic analysis
│   ├── gruel-codegen/   # Machine code generation
│   ├── gruel-compiler/  # Pipeline orchestration
│   ├── gruel-error/     # Error types
│   ├── gruel-lexer/     # Tokenizer
│   ├── gruel-linker/    # Object file creation and linking
│   ├── gruel-parser/    # AST construction
│   ├── gruel-rir/       # Untyped IR
│   ├── gruel-runtime/   # The runtime
│   ├── gruel-span/      # Source locations
│   └── gruel-spec/      # Specification test runner
├── docs/              # Documentation
└── examples/          # Example .gruel programs
```

## Building

### Build Everything

```bash
cargo build --workspace --exclude gruel-runtime
```

### Build the Compiler

```bash
cargo build -p gruel
```

The binary is output to `target/debug/gruel` (or `target/release/gruel` with `--release`).

### Build a Specific Crate

```bash
cargo build -p gruel-lexer
```

## Testing

### Run All Tests

```bash
./test.sh
```

This runs:
1. Unit tests for all crates (`cargo test --workspace`)
2. Specification tests (`cargo run -p gruel-spec`)
3. Spec traceability check
4. UI tests (`cargo run -p gruel-ui-tests`)

### Run Unit Tests Only

```bash
./quick-test.sh
# or
cargo test --workspace --exclude gruel-runtime
```

### Run Spec Tests Only

```bash
cargo run -p gruel-spec
```

### Run a Specific Crate's Tests

```bash
cargo test -p gruel-lexer
```

### Filter Spec Tests

```bash
cargo run -p gruel-spec -- "1.1"    # Run section 1.1 tests
cargo run -p gruel-spec -- "zero"   # Run tests matching "zero"
```

## Using the Compiler

### Compile a Program

```bash
cargo run -p gruel -- source.gruel output
./output
echo $?  # Check exit code
```

### Dump Intermediate Representations

```bash
cargo run -p gruel -- --emit rir source.gruel
cargo run -p gruel -- --emit air source.gruel
cargo run -p gruel -- --emit mir source.gruel
```

## Adding Tests

### Unit Tests

Add tests to the relevant crate's source file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // ...
    }
}
```

### Specification Tests

Add test cases to `.toml` files in `crates/gruel-spec/cases/`:

```toml
[[case]]
name = "my_test"
source = "fn main() -> i32 { 42 }"
exit_code = 42
```

For compile-fail tests:

```toml
[[case]]
name = "my_error_test"
source = "fn main() { }"
compile_fail = true
error_contains = "expected '->'"
```

For golden tests (exact output matching):

```toml
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

## Version Control

This project uses git.

### Creating a Commit

```bash
git add -p
git commit -m "Add feature X"
```

## Code Style

- Follow standard Rust formatting (`cargo fmt`)
- Keep functions small and focused
- Prefer explicit types in public APIs
- Add doc comments to public items

## Debugging

### Print IR at Each Stage

Use the `--emit` flags to see the IR at each compilation stage:

```bash
echo 'fn main() -> i32 { 42 }' > /tmp/test.gruel
cargo run -p gruel -- --emit rir /tmp/test.gruel
cargo run -p gruel -- --emit air /tmp/test.gruel
cargo run -p gruel -- --emit mir /tmp/test.gruel
```

### Disassemble Output

```bash
cargo run -p gruel -- /tmp/test.gruel /tmp/test
objdump -d /tmp/test
```

### Run Under GDB

```bash
cargo run -p gruel -- /tmp/test.gruel /tmp/test
gdb /tmp/test
```

## Common Tasks

### Add a New Crate

1. Create directory `crates/gruel-newcrate/`
2. Add `Cargo.toml` with the new crate's metadata and dependencies
3. Add the crate to the workspace `members` list in the root `Cargo.toml`
4. Add it as a dependency in consuming crates' `Cargo.toml` files

### Add a Third-Party Dependency

1. Add the dependency to `[workspace.dependencies]` in the root `Cargo.toml`
2. Reference it with `dep.workspace = true` in the crate's `Cargo.toml`

### Modify the Grammar

1. Update `gruel-lexer` if new tokens needed
2. Update `gruel-parser` for new syntax
3. Update `gruel-rir` for new IR instructions
4. Update `gruel-air` for typed versions
5. Update `gruel-codegen` for code generation
6. Add spec tests for new behavior
