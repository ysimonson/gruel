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
| Arithmetic operators | ✓ Implemented |
| Variables | ✓ Implemented |
| Control flow | Planned |

## Specification Tests

The executable specification lives in `crates/rue-spec/cases/`:

| File | Section | Description |
|------|---------|-------------|
| `01-integers.toml` | 1.1 | Integer literals and exit codes |
| `02-comments.toml` | 1.3 | Line comments |
| `03-whitespace.toml` | 1.4 | Whitespace handling |
| `04-functions.toml` | 2.1 | Function declarations |
| `05-errors.toml` | 3.1 | Compilation errors |
| `06-ir-dumps.toml` | 4.1 | IR output golden tests |
| `07-error-golden.toml` | 5.1 | Error message golden tests |
| `08-arithmetic.toml` | 1.2 | Arithmetic operators |
| `09-variables.toml` | 2.2 | Local variables |

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

### Arithmetic

```rue
fn main() -> i32 {
    1 + 2 * 3  // = 7 (multiplication binds tighter)
}
```

```rue
fn main() -> i32 {
    (1 + 2) * 3  // = 9 (parentheses override)
}
```

```rue
fn main() -> i32 {
    -42  // unary negation
}
```

### Variables

```rue
fn main() -> i32 {
    let x = 40;
    let y = 2;
    x + y
}
```

Variables are immutable by default. Use `let mut` for mutable bindings:

```rue
fn main() -> i32 {
    let mut counter = 0;
    counter = counter + 1;
    counter
}
```

Type annotations are optional:

```rue
fn main() -> i32 {
    let x: i32 = 42;
    x
}
```

Shadowing is allowed:

```rue
fn main() -> i32 {
    let x = 10;
    let x = x + 5;  // shadows previous x
    x  // returns 15
}
```

Operators by precedence (highest to lowest):
1. `-` (unary negation)
2. `*`, `/`, `%` (multiplicative)
3. `+`, `-` (additive)

All binary operators are left-associative: `10 - 3 - 2` equals `5` (not `9`).

### Runtime Errors

Division by zero and integer overflow cause runtime errors:

```rue
fn main() -> i32 { 10 / 0 }           // runtime error: division by zero
fn main() -> i32 { 2147483647 + 1 }   // runtime error: integer overflow
```

### Grammar (Current)

```ebnf
program        = { function } ;
function       = "fn" IDENT "(" ")" "->" type "{" block "}" ;
block          = { statement } expression ;
statement      = let_stmt | assign_stmt ;
let_stmt       = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
assign_stmt    = IDENT "=" expression ";" ;
type           = "i32" ;
expression     = additive ;
additive       = multiplicative { ("+" | "-") multiplicative } ;
multiplicative = unary { ("*" | "/" | "%") unary } ;
unary          = "-" unary | primary ;
primary        = INTEGER | IDENT | "(" expression ")" ;
```

## Planned Features

See `docs/design-decisions.md` (ADR-009) for language philosophy. Planned additions include:

- Control flow: `if`/`else`, `while`, `loop`
- Functions with parameters
- Structs and user-defined types
- Memory safety model (influenced by Hylo/Swift/Rust)
