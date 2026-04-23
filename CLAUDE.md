# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Gruel is a systems programming language aiming for memory safety without garbage collection, with higher-level ergonomics than Rust/Zig. Currently in early development with Rust-like syntax.

## Build System

This project uses Cargo.

### Common Commands

```bash
# Build the compiler
cargo build -p gruel

# Build everything
cargo build --workspace --exclude gruel-runtime

# Run all tests (unit + spec)
make test

# Run unit tests only
cargo test --workspace --exclude gruel-runtime

# Run spec tests only
cargo run -p gruel-spec

# Run a specific crate's tests
cargo test -p gruel-lexer

# Filter spec tests by pattern
cargo run -p gruel-spec -- "1.1"  # Section 1.1
cargo run -p gruel-spec -- "zero" # Tests matching "zero"

# Compile and run a one-off program (preferred for quick tests)
# Write source with the Write tool to scratch/test.gruel, then:
cargo run -p gruel -- scratch/test.gruel scratch/test_out && ./scratch/test_out

# Compile and run a program (single file)
cargo run -p gruel -- source.gruel output
./output

# Compile multiple files into one program
cargo run -p gruel -- main.gruel utils.gruel math.gruel -o program
./program

# With shell glob expansion
cargo run -p gruel -- src/*.gruel -o program

# Note: -o is required when compiling multiple files
cargo run -p gruel -- a.gruel b.gruel          # Error!
cargo run -p gruel -- a.gruel b.gruel -o out   # OK

# Emit intermediate representations (can specify multiple stages)
cargo run -p gruel -- --emit tokens source.gruel  # Lexer tokens
cargo run -p gruel -- --emit ast source.gruel     # Abstract syntax tree
cargo run -p gruel -- --emit rir source.gruel     # Untyped IR
cargo run -p gruel -- --emit air source.gruel     # Typed IR
cargo run -p gruel -- --emit cfg source.gruel     # Control flow graph
cargo run -p gruel -- --emit asm source.gruel     # LLVM IR (.ll format)

# Chain multiple stages to see the full pipeline
cargo run -p gruel -- --emit tokens --emit ast --emit rir source.gruel
```

## Architecture

The compiler pipeline transforms source through successive IRs:

```mermaid
graph LR
    Source --> Lexer --> Parser --> AstGen --> Sema --> CfgBuilder --> LLVM --> Link
```

| Stage | Pass | IR Produced | `--emit` flag |
|-------|------|-------------|---------------|
| 1 | Lexer | tokens | `tokens` |
| 2 | Parser | AST | `ast` |
| 3 | AstGen | RIR (untyped) | `rir` |
| 4 | Sema | AIR (typed) | `air` |
| 5 | CfgBuilder | CFG | `cfg` |
| 6 | LLVM | object file | `asm` (LLVM IR) |
| 7 | Link | native binary | - |

### Crate Responsibilities

| Crate | Purpose |
|-------|---------|
| `gruel` | CLI binary |
| `gruel-compiler` | Pipeline orchestration |
| `gruel-lexer` | Tokenization |
| `gruel-parser` | AST construction |
| `gruel-rir` | Untyped IR (post-parse, pre-typing) |
| `gruel-cfg` | Control flow graph construction and optimization |
| `gruel-air` | Typed IR (after semantic analysis) |
| `gruel-codegen-llvm` | LLVM-based code generation (via inkwell) |
| `gruel-error` | Error types and diagnostics |
| `gruel-span` | Source location tracking |
| `gruel-target` | Target platform configuration |
| `gruel-spec` | Specification test runner |
| `gruel-ui-tests` | UI/diagnostics tests (warnings, error messages) |
| `gruel-fuzz` | Fuzz testing infrastructure |
| `gruel-runtime` | Runtime support |
| `gruel-builtins` | Built-in type definitions (String, future Vec, etc.) |

### Multi-File Compilation

Gruel supports compiling multiple source files into a single executable:

```bash
# All files share a flat global namespace (no modules yet)
gruel main.gruel utils.gruel lib.gruel -o program
```

**Key semantics:**
- All functions, structs, and enums are globally visible across files
- Duplicate definitions (same name in multiple files) cause a compile error
- `main()` must exist in exactly one file
- Files are parsed in parallel, then merged for semantic analysis

**Current limitations (will be addressed by the module system):**
- No visibility control (`pub`/private)
- No namespacing - all symbols share global scope
- No `mod` or `use` syntax
- Must list all files explicitly on command line

### Key Design Decisions

- **LLVM backend**: Code generation uses LLVM via the `inkwell` crate (`gruel-codegen-llvm`), providing cross-platform support and production-quality optimization
- **Index-based references**: Instructions stored in vectors, referenced by u32 indices (cache-friendly, no lifetimes)
- **System linker**: Links via `cc` or a user-specified system linker (no custom ELF writer)
- **Built-in types as synthetic structs**: Types like `String` are defined in `gruel-builtins` and injected as synthetic structs, not as hardcoded `Type` enum variants (see [ADR-0020](docs/designs/0020-builtin-types-as-structs.md))

### Built-in Types Architecture

Built-in types (currently just `String`, future `Vec<T>`, etc.) are implemented as "synthetic structs" — the compiler injects them before processing user code. This architecture:

- **Eliminates special-casing**: Built-in types flow through the same code paths as user-defined structs
- **Centralizes metadata**: All type information (fields, methods, operators) lives in `gruel-builtins`
- **Scales to new types**: Adding `Vec<T>` or `HashMap<K,V>` becomes "add an entry to `BUILTIN_TYPES`"

**Key components:**

| Component | Location | Purpose |
|-----------|----------|---------|
| Type definitions | `gruel-builtins/src/lib.rs` | `BuiltinTypeDef` constants describing fields, methods, operators |
| Injection point | `gruel-air/src/sema.rs` | `inject_builtin_types()` creates synthetic `StructDef` entries |
| Runtime functions | `gruel-runtime/src/lib.rs` | Actual implementations (e.g., `String__len`, `__gruel_drop_String`) |

**Adding a new built-in type:**

1. Define a `BuiltinTypeDef` in `gruel-builtins/src/lib.rs`
2. Add it to the `BUILTIN_TYPES` slice
3. Implement runtime functions in `gruel-runtime`

See the module documentation in `gruel-builtins` for a detailed example with hypothetical `Vec` type.

## Testing

### Development Workflow

The test suite has three layers optimized for different stages of development:

| Test Type | Command | Speed | When to Use |
|-----------|---------|-------|-------------|
| Unit tests | `make quick-test` | ~2-5s | During active development |
| Full suite | `make test` | ~30-60s | Before committing |
| Targeted spec | `cargo run -p gruel-spec -- "pattern"` | Varies | Testing specific features |

**Recommended workflow:**

```bash
# During development - fast feedback loop
make quick-test                # Unit tests only

# Before committing - full verification
make test                      # Unit + spec + UI + traceability

# Debugging specific areas
cargo run -p gruel-spec -- "arithmetic"  # Specific spec tests
cargo test -p gruel-codegen              # Specific crate
```

### Choosing the Right Test Type

| If you're... | Use... | Why |
|--------------|--------|-----|
| Iterating on a fix | `make quick-test` | Fast feedback, catches most issues |
| Adding a language feature | Spec tests | Required for traceability |
| Improving diagnostics | UI tests | Not spec-mandated behavior |
| About to commit | `make test` | Ensures nothing is broken |

**Rule of thumb:**
- **Unit tests** catch logic errors quickly during development
- **Spec tests** verify language semantics and maintain spec traceability
- **UI tests** verify compiler quality-of-life features (warnings, error messages)

### Unit Tests
Add to relevant crate's source file with `#[cfg(test)]` modules.

The `gruel-compiler` crate includes integration unit tests that test the full pipeline without execution. Use `compile_to_air()` and `compile_to_cfg()` helpers to test compilation without spawning processes.

### UI Tests

UI tests verify compiler behavior that is **not** part of the language specification, such as:
- Warning messages (unused variables, unreachable code)
- Diagnostic quality and formatting
- Compiler flags and options
- Error message wording

#### UI Test Directory Structure

UI tests are in `crates/gruel-ui-tests/cases/`:

```
cases/
├── warnings/         # Warning detection tests
│   ├── unused.toml   # Unused variable/function warnings
│   └── unreachable.toml  # Unreachable code warnings
└── diagnostics/      # Error message quality tests (future)
```

#### UI Test Format

```toml
[section]
id = "warnings.unused"
name = "Unused Variable Warnings"
description = "Tests for detection of unused variables."

[[case]]
name = "unused_variable_warning"
source = """
fn main() -> i32 {
    let x = 42;
    0
}
"""
exit_code = 0
warning_contains = ["unused variable", "'x'"]
expected_warning_count = 1

[[case]]
name = "no_warnings_expected"
source = """
fn main() -> i32 {
    let x = 42;
    x
}
"""
exit_code = 42
no_warnings = true
```

#### Running UI Tests

```bash
# Run all UI tests
cargo run -p gruel-ui-tests

# Filter by pattern
cargo run -p gruel-ui-tests -- "unused"
```

#### When to Add UI Tests vs Spec Tests

- **Spec tests** (`crates/gruel-spec/cases/`): Language semantics defined in the specification. These tests have `spec = [...]` references linking to spec paragraphs.
- **UI tests** (`crates/gruel-ui-tests/cases/`): Compiler quality-of-life features not in the spec (warnings, diagnostics, CLI behavior).

### Specification Tests

The specification test system provides traceability between the language specification and tests.

#### Test Directory Structure

Tests are organized in `crates/gruel-spec/cases/` by language feature:

```
cases/
├── lexical/          # Tokens, comments, whitespace
├── types/            # Integer, boolean, unit, never types
├── expressions/      # Literals, operators, control flow
├── statements/       # Let, assignment, expression statements
├── items/            # Functions, structs
├── arrays/           # Fixed-size arrays
├── runtime/          # Intrinsics, runtime behavior
├── golden/           # IR dump tests
└── errors/           # Compile-time error tests
```

#### Test Format

```toml
[section]
id = "expressions.arithmetic"
spec_chapter = "4.2"           # Links to spec chapter
name = "Arithmetic Operators"

# Run-pass test with spec traceability
[[case]]
name = "addition_basic"
spec = ["4.2:1", "4.2:2"]      # Spec paragraphs this test covers
source = "fn main() -> i32 { 1 + 2 }"
exit_code = 3

# Compile-fail test
[[case]]
name = "type_mismatch"
spec = ["4.2:5"]
source = "fn main() -> i32 { 1 + true }"
compile_fail = true
error_contains = "type mismatch"

# Golden test (exact IR output)
[[case]]
name = "simple_add_air"
spec = ["4.2:1"]
source = "fn main() -> i32 { 42 }"
expected_air = """
function main:
air (return_type: i32) {
    %0 : i32 = const 42
    %1 : i32 = ret %0
}
"""

# Preview feature test (allowed to fail)
[[case]]
name = "some_preview_feature"
spec = ["X.Y:Z"]
preview = "test_infra"           # Requires --preview test_infra
source = "..."
exit_code = 0

# Preview feature test (must pass)
[[case]]
name = "some_preview_feature_basic"
spec = ["X.Y:Z"]
preview = "test_infra"
preview_should_pass = true       # Fails CI if this test fails
source = "..."
exit_code = 0
```

#### Preview Feature Tests

Tests for preview features use two fields:
- `preview = "feature_name"` - Marks the test as requiring a preview feature. The test runs with `--preview feature_name` and is allowed to fail (shows as "ignored" in output).
- `preview_should_pass = true` - When combined with `preview`, makes the test required to pass. Use this for portions of preview features that are already implemented.

**Workflow for preview features:**
1. Initially, add tests with just `preview = "feature_name"` (allowed to fail)
2. As you implement parts of the feature, add `preview_should_pass = true` to tests that should now pass
3. When stabilizing the feature, remove both `preview` and `preview_should_pass` fields

The `preview` field must match a valid `PreviewFeature` variant name. The test runner validates all preview feature names on startup and will fail with a clear error if an unknown feature name is used.

#### Spec Paragraph References

The `spec` field links tests to specification paragraphs using the format `{chapter}.{section}:{paragraph}`:
- `3.1:1` - Chapter 3, Section 1, Paragraph 1
- `4.2:5` - Chapter 4, Section 2, Paragraph 5

### Language Specification

The formal language specification is in `docs/spec/src/`. It is integrated into the website via Zola.

#### Building the Spec

The spec is built as part of the website:

```bash
./website/build.sh
# Output in website/public/spec/
```

#### Spec Structure

```
docs/spec/src/
├── _index.md               # Spec root (Zola section)
├── 01-introduction.md      # Conformance, definitions
├── 02-lexical-structure/   # Tokens, comments, keywords
├── 03-types/               # Type system
├── 04-expressions/         # All expression forms
├── 05-statements/          # Statement forms
├── 06-items/               # Functions, structs
├── 07-arrays/              # Array types
├── 08-runtime-behavior/    # Overflow, bounds checking
└── appendices/             # Grammar, UB summary
```

#### Spec Paragraph Format

Each paragraph has an ID using the Zola shortcode format `{{ rule(id="X.Y:Z", cat="category") }}`:

```markdown
{{ rule(id="3.1:1", cat="normative") }}
A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.

{{ rule(id="3.1:2", cat="normative") }}
Signed integer arithmetic that overflows causes a runtime panic.

{{ rule(id="3.1:3", cat="example") }}
```gruel
let x: i32 = 42;
```
```

The format is `{{ rule(id="X.Y:Z") }}` or `{{ rule(id="X.Y:Z", cat="category") }}` where:
- `X.Y` is the chapter and section (e.g., `3.1` for Chapter 3, Section 1)
- `Z` is the paragraph number within that section
- The colon (`:`) separates the structural location from the paragraph number
- `cat` is optional (defaults to `informative` if omitted)

**Paragraph categories:**
- `normative` - General normative rule (requires test coverage)
- `legality-rule` - Compile-time requirements (normative)
- `dynamic-semantics` - Runtime behavior (normative)
- `syntax` - Grammar rules (normative)
- `undefined-behavior` - UB conditions (normative)
- `example` - Code examples (informative)
- `informative` - Explanatory text (informative, default)

#### Traceability Report

Generate a report showing test coverage of spec paragraphs:

```bash
# Summary report
cargo run -p gruel-spec -- --traceability

# Detailed matrix (shows all paragraphs and their covering tests)
cargo run -p gruel-spec -- --traceability --detailed
```

The traceability check is run as part of `make test` and fails if:
- Any spec paragraph has no covering test (coverage < 100%)
- Any test references a non-existent spec paragraph ID

### Fuzz Testing

Fuzz testing uses [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer). Fuzz targets live in `fuzz/fuzz_targets/`. Requires nightly Rust.

#### Available Fuzz Targets

```bash
# List targets
cargo +nightly fuzz list

# Targets:
# - lexer:               Tokenization (raw bytes)
# - parser:              Lexing + parsing (raw bytes)
# - compiler:            Full frontend (raw bytes)
# - emitter:             x86-64 instruction encoding (raw bytes)
# - emitter_sequence:    Instruction sequences with labels/jumps (raw bytes)
# - structured_compiler: Valid Gruel programs (arbitrary crate)
# - structured_invalid:  Semantically invalid programs (arbitrary crate)
# - structured_emitter:  Structured x86-64 MIR sequences (arbitrary crate)
```

#### Running Fuzz Tests

```bash
# Run a target indefinitely (Ctrl+C to stop)
cargo +nightly fuzz run lexer

# Run for a specific duration (300 seconds)
cargo +nightly fuzz run parser -- -max_total_time=300

# Run all targets for 5 minutes each
for target in lexer parser compiler emitter emitter_sequence structured_compiler structured_invalid structured_emitter; do
    cargo +nightly fuzz run $target -- -max_total_time=300
done
```

Corpus files are stored in `fuzz/corpus/<target>/` and are persisted across runs.

#### CI Integration

Fuzzing runs automatically in CI via `.github/workflows/fuzz.yml`. Each target runs for 5 minutes daily. Any crashes trigger a non-zero exit code and create an issue with the `fuzz-crash` label.

#### When a Crash is Found

Crash inputs are saved to `fuzz/artifacts/<target>/`:

```bash
# Reproduce the crash
cargo +nightly fuzz run lexer fuzz/artifacts/lexer/crash-*

# Or compile the crashing input directly
cargo run -p gruel -- fuzz/artifacts/compiler/crash-*.txt output
```

## Modifying the Language

When adding or changing language features, follow this checklist.

### Preview Features (Gating New Features)

**IMPORTANT**: New language features MUST be gated behind preview flags until complete. See [ADR-0005](docs/designs/0005-preview-features.md) for the full design.

#### When to Use Preview Features

Use preview gating when:
- Adding new syntax (keywords, operators, constructs)
- Adding new type system features
- Any feature that spans multiple implementation phases

#### How to Gate a Feature

1. **Add to PreviewFeature enum** in `gruel-error/src/lib.rs`:
   ```rust
   pub enum PreviewFeature {
       YourNewFeature,  // Add your feature here
   }
   ```
   Also update `name()`, `adr()`, `all()`, and `FromStr` impl.

2. **Add the gate check in Sema** (`gruel-air/src/sema.rs`):
   ```rust
   // At the point where the feature is used:
   self.require_preview(PreviewFeature::YourNewFeature, "your feature description", span)?;
   ```

   **This is the critical step that actually gates the feature!** Without this call, users can use the feature without `--preview`.

3. **Add spec tests with `preview` field**:
   ```toml
   [[case]]
   name = "your_feature_basic"
   spec = ["X.Y:Z"]
   preview = "your_new_feature"  # Matches PreviewFeature::name()
   source = """..."""
   exit_code = 42
   ```

4. **Test that the gate works**:
   - Without `--preview your_new_feature`: Should get "requires preview feature" error
   - With `--preview your_new_feature`: Should compile/run

#### Stabilizing a Feature

When all tests pass and the feature is complete:

1. Remove `preview = "..."` from spec tests
2. Remove the `require_preview()` call from Sema
3. Remove the variant from `PreviewFeature` enum
4. Update the ADR status to "Implemented"

### Implementation Steps

1. **Update the specification** (`docs/spec/src/`)
   - Add/modify spec paragraphs with proper IDs (e.g., `r[4.2:3#normative]`)
   - Include normative rules, dynamic semantics, and examples
   - Update the grammar appendix if syntax changes

2. **Update `gruel-lexer`** if new tokens needed

3. **Update `gruel-parser`** for new syntax

4. **Update `gruel-rir`** for new IR instructions

5. **Update `gruel-air`** for typed versions
   - **If this is a new feature**: Add the `require_preview()` gate (see above)

6. **Update `gruel-codegen`** for code generation

7. **Add spec tests** in `crates/gruel-spec/cases/`
   - Include `spec = ["X.Y:Z"]` references to link to spec paragraphs
   - Cover all normative paragraphs (traceability check enforces 100% coverage)
   - **If this is a preview feature**: Include `preview = "feature_name"` field

8. **Add UI tests** in `crates/gruel-ui-tests/cases/` if the feature includes:
   - New warnings or lints
   - Changes to error message formatting
   - New compiler flags or options

9. **Run `make test`** to verify all tests pass and traceability is maintained

### Adding a new intrinsic

Intrinsics are declared once in the `gruel-intrinsics` registry (ADR-0050) and then wired through sema/codegen via the closed `IntrinsicId` enum. To add one:

1. In `crates/gruel-intrinsics/src/lib.rs`:
   - Add a variant to `IntrinsicId`.
   - Append an `IntrinsicDef` entry to `INTRINSICS` (name, kind, category, arity, preview gate, runtime symbol if any, summary/description/examples for the generated docs).
2. Implement the behavior:
   - Sema: add a `match` arm for the new `IntrinsicId` in `analyze_intrinsic_impl` (and `analyze_type_intrinsic` / inference as needed) — the compiler's exhaustive matches will force this.
   - Codegen: add the corresponding arm in `translate_intrinsic` in `gruel-codegen-llvm`.
3. If the intrinsic has a runtime component, implement the extern in `gruel-runtime` using the symbol name given by `runtime_fn`.
4. Run `make gen-intrinsic-docs` to regenerate `docs/generated/intrinsics-reference.md`, commit it, and then `make check` (which runs `make check-intrinsic-docs`) will fail if anyone lets the registry and the doc drift apart.

## Version Control

This project uses git.

### Commit Messages

When committing, use `git commit -m "message"` or for multi-line messages:
```bash
git add -p  # Stage relevant changes
git commit -m "Short summary

Longer description here.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

## Code Style

- Standard Rust formatting (rustfmt)
- Rust edition 2024

## Logging Guidelines

Gruel uses the `tracing` crate for structured logging, following the **"wide events"** philosophy from [loggingsucks.com](https://loggingsucks.com/). This means:

1. **Canonical log lines** - One rich, structured log per operation containing all debugging context
2. **Structured format** - Key-value pairs instead of plain strings for queryability
3. **High-cardinality data** - Include contextual data like function names, counts, sizes

### Using the Logging

```bash
# Normal compilation (no logging by default)
gruel source.gruel output

# Show timing per pass
gruel --time-passes source.gruel output

# Enable debug logging
gruel --log-level=debug source.gruel output
RUST_LOG=debug gruel source.gruel output

# JSON format for tooling integration
gruel --log-format=json --log-level=debug source.gruel output

# Filter to specific module
RUST_LOG=gruel_compiler::sema=trace gruel source.gruel output
```

### Adding Instrumentation

Each compilation pass should have a tracing span wrapping the work:

```rust
use tracing::{info_span, info};

pub fn my_pass(input: &Input) -> Result<Output> {
    // Create a span for the pass - includes timing automatically
    let _span = info_span!("my_pass").entered();

    // Do the work...
    let result = process(input)?;

    // Log completion with useful metrics
    info!(
        item_count = result.items.len(),
        "pass complete"
    );

    Ok(result)
}
```

### Logging Levels

| Level | Use for | Example |
|-------|---------|---------|
| `error` | Compilation failures, internal compiler errors | ICE, unrecoverable errors |
| `warn` | Suspicious patterns (surfaced via diagnostics) | Deprecated feature usage |
| `info` | Per-pass completion with summary metrics | "lexing complete", token counts |
| `debug` | Decision points, intermediate state | "resolving symbol X to Y" |
| `trace` | Detailed internal state, individual instructions | Instruction-by-instruction output |

### Good vs Bad Examples

**Good: Wide event with context**
```rust
let _span = info_span!(
    "codegen",
    arch = "x86_64",
    function_count = functions.len()
).entered();

// ... do code generation ...

info!(
    code_bytes = total_bytes,
    "code generation complete"
);
```

**Bad: Scattered debug statements**
```rust
println!("Starting codegen...");
for func in functions {
    println!("Generating function: {:?}", func.name);
    // ...
}
println!("Done!");
```

**Good: Structured key-value data**
```rust
info!(
    token_count = tokens.len(),
    source_bytes = source.len(),
    "lexing complete"
);
```

**Bad: String interpolation**
```rust
println!("Lexed {} tokens from {} bytes", tokens.len(), source.len());
```

### Key Principles

1. **Spans for timing**: Wrap passes in `info_span!()` - this enables `--time-passes`
2. **Events for outcomes**: Use `info!()` after completing work with metrics
3. **Context in spans**: Include high-level context (file, function count) in span fields
4. **Metrics in events**: Include computed metrics (instruction counts, sizes) in events
5. **Zero-cost when off**: Tracing has no overhead when no subscriber is active
