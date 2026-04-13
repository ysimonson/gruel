# Development Guide

This document covers how to build, test, and contribute to the Gruel compiler.

## Prerequisites

### Required

- **dotslash** - For bootstrapping Buck2. Install via `brew install dotslash` (macOS) or see [dotslash docs](https://dotslash-cli.com/).

### Platform-Specific

The Rust toolchain is downloaded automatically by Buck2 (hermetic build). However, running Gruel programs has platform requirements:

| Platform | Requirements |
|----------|--------------|
| Linux x86_64 | None (fully hermetic) |
| macOS ARM64 | Xcode Command Line Tools (`xcode-select --install`) |
| macOS x86_64 | Xcode Command Line Tools (`xcode-select --install`) |

**Why macOS needs Xcode**: The Gruel compiler uses the system `clang` to link executables on macOS. On Linux, Gruel uses its own internal ELF linker.

### Optional (for IDE support)

- **Rust toolchain via rustup** - For IDE features (rust-analyzer, etc.). The `rust-toolchain.toml` in the repo root ensures the right version.

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
│   ├── gruel-runtime/    # The runtime
│   ├── gruel-span/      # Source locations
│   └── gruel-spec/      # Specification test runner
├── docs/              # Documentation
├── examples/          # Example .gruel programs
├── third-party/       # Vendored dependencies
└── toolchains/        # Buck2 toolchain definitions
```

## Building

### Build Everything

```bash
./buck2 build //...
```

### Build the Compiler

```bash
./buck2 build //crates/gruel:gruel
```

The binary is output to `buck-out/v2/gen/root/crates/gruel/__gruel__/gruel`.

### Build a Specific Crate

```bash
./buck2 build //crates/gruel-lexer:gruel-lexer
```

## Testing

### Run All Tests

```bash
./test.sh
```

This runs:
1. Unit tests for all crates (`buck2 test`)
2. Specification tests (`buck2 run //crates/gruel-spec:gruel-spec`)

### Run Unit Tests Only

```bash
./buck2 test //...
```

### Run Spec Tests Only

```bash
./buck2 run //crates/gruel-spec:gruel-spec
```

### Run a Specific Test

```bash
./buck2 test //crates/gruel-lexer:gruel-lexer-test
```

### Filter Spec Tests

```bash
./buck2 run //crates/gruel-spec:gruel-spec -- "1.1"  # Run section 1.1 tests
./buck2 run //crates/gruel-spec:gruel-spec -- "zero" # Run tests matching "zero"
```

## Using the Compiler

### Compile a Program

```bash
./buck2 run //crates/gruel:gruel -- source.gruel output
./output
echo $?  # Check exit code
```

### Dump Intermediate Representations

```bash
# Dump RIR (untyped IR)
./buck2 run //crates/gruel:gruel -- --dump-rir source.gruel

# Dump AIR (typed IR)
./buck2 run //crates/gruel:gruel -- --dump-air source.gruel

# Dump MIR (machine IR before register allocation)
./buck2 run //crates/gruel:gruel -- --dump-mir source.gruel
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

Ensure the crate has a `rust_test` target in its `BUCK` file.

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

This project uses [Jujutsu (jj)](https://github.com/martinvonz/jj) for version control.

### Common Commands

```bash
jj status          # Show working copy changes
jj diff            # Show diff
jj commit -m "msg" # Create a commit
jj log             # Show history
```

### Creating a Commit

```bash
jj commit -m "Add feature X"
```

## Code Style

- Follow standard Rust formatting (`rustfmt`)
- Keep functions small and focused
- Prefer explicit types in public APIs
- Add doc comments to public items

## Debugging

### Print IR at Each Stage

Use the `--dump-*` flags to see the IR at each compilation stage:

```bash
echo 'fn main() -> i32 { 42 }' > /tmp/test.gruel
./buck2 run //crates/gruel:gruel -- --dump-rir /tmp/test.gruel
./buck2 run //crates/gruel:gruel -- --dump-air /tmp/test.gruel
./buck2 run //crates/gruel:gruel -- --dump-mir /tmp/test.gruel
```

### Disassemble Output

```bash
./buck2 run //crates/gruel:gruel -- /tmp/test.gruel /tmp/test
objdump -d /tmp/test
```

### Run Under GDB

```bash
./buck2 run //crates/gruel:gruel -- /tmp/test.gruel /tmp/test
gdb /tmp/test
```

## Common Tasks

### Add a New Crate

1. Create directory `crates/gruel-newcrate/`
2. Add `BUCK` file with `rust_library` and `rust_test` targets
3. Add to dependencies in consuming crates' `BUCK` files

### Add a Third-Party Dependency

Dependencies are vendored in `third-party/`. See existing setup for patterns.

### Modify the Grammar

1. Update `gruel-lexer` if new tokens needed
2. Update `gruel-parser` for new syntax
3. Update `gruel-rir` for new IR instructions
4. Update `gruel-air` for typed versions
5. Update `gruel-codegen` for code generation
6. Add spec tests for new behavior
