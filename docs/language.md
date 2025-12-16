# Rue Language Reference

This document provides an overview of the Rue programming language. For the authoritative specification with test cases, see `crates/rue-spec/cases/`.

## Overview

Rue is a systems programming language aiming for:
- Memory safety without garbage collection
- Higher-level ergonomics than Rust/Zig
- Rust-like syntax (initially)

## Current Status

Rue is in early development. The implemented feature set is minimal:

| Feature | Status |
|---------|--------|
| Integer literals | ✓ Implemented |
| Functions | ✓ Basic (no parameters) |
| Line comments | ✓ Implemented |
| Arithmetic operators | Planned |
| Variables | Planned |
| Control flow | Planned |

## Specification Tests

The executable specification lives in `crates/rue-spec/cases/`:

| File | Section | Description |
|------|---------|-------------|
| `01-integers.toml` | 1.1 | Integer literals and exit codes |
| `02-comments.toml` | 1.2 | Line comments |
| `03-whitespace.toml` | 1.3 | Whitespace handling |
| `04-functions.toml` | 2.1 | Function declarations |
| `05-errors.toml` | 3.1 | Compilation errors |
| `06-ir-dumps.toml` | 4.1 | IR output golden tests |
| `07-error-golden.toml` | 5.1 | Error message golden tests |

Each `.toml` file contains test cases that define expected behavior:

```toml
[[case]]
name = "simple_return"
source = "fn main() -> i32 { 42 }"
exit_code = 42
```

Run specs with `./test.sh` or directly with `buck2 run //crates/rue-spec:rue-spec`.

## Quick Reference

### Minimal Program

```rue
fn main() -> i32 {
    0
}
```

### With Comments

```rue
// A comment
fn main() -> i32 {
    42  // inline comment
}
```

### Grammar (Current)

```ebnf
program     = { function } ;
function    = "fn" IDENT "(" ")" "->" type "{" expression "}" ;
type        = "i32" ;
expression  = INTEGER ;
```

## Planned Features

See `docs/design-decisions.md` (ADR-009) for language philosophy. Planned additions include:

- Arithmetic: `+`, `-`, `*`, `/`
- Variables: `let x = 42;`
- Control flow: `if`/`else`, `while`, `loop`
- Functions with parameters
- Structs and user-defined types
- Memory safety model (influenced by Hylo/Swift/Rust)
