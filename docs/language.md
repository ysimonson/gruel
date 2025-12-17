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
| Boolean literals | ✓ Implemented |
| Functions | ✓ Basic (no parameters) |
| Line comments | ✓ Implemented |
| Arithmetic operators | ✓ Implemented |
| Comparison operators | ✓ Implemented |
| Variables | ✓ Implemented |
| If/else expressions | ✓ Implemented |
| Loops | Planned |

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
| `10-conditionals.toml` | 10.1 | If/else expressions and comparisons |

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

### Types

Rue currently supports two primitive types:

| Type | Description | Literals |
|------|-------------|----------|
| `i32` | 32-bit signed integer | `0`, `42`, `-17` |
| `bool` | Boolean | `true`, `false` |

Type annotations are optional when the type can be inferred:

```rue
fn main() -> i32 {
    let x = 42;        // inferred as i32
    let flag = true;   // inferred as bool
    let y: i32 = 10;   // explicit annotation
    x + y
}
```

### Operators

Operators by precedence (highest to lowest):
1. `-` (unary negation)
2. `*`, `/`, `%` (multiplicative)
3. `+`, `-` (additive)
4. `==`, `!=`, `<`, `>`, `<=`, `>=` (comparison)

All binary operators are left-associative: `10 - 3 - 2` equals `5` (not `9`).

Comparison operators return `bool`:

```rue
fn main() -> i32 {
    let a = 1 == 1;    // true
    let b = 2 < 3;     // true
    let c = 5 >= 5;    // true
    if a { 1 } else { 0 }
}
```

### Conditionals

If/else is an expression that returns a value:

```rue
fn main() -> i32 {
    let x = if true { 42 } else { 0 };
    x
}
```

Both branches must have the same type:

```rue
fn main() -> i32 {
    let n = 5;
    if n > 0 { 100 } else { 0 }
}
```

If/else can be nested:

```rue
fn main() -> i32 {
    let x = 5;
    if x < 3 { 1 }
    else { if x < 7 { 2 }
    else { 3 } }
}
```

The condition must be of type `bool`. Integer values are not implicitly converted to booleans:

```rue
fn main() -> i32 {
    if 1 { 42 } else { 0 }  // ERROR: expected bool, found i32
}
```

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
type           = "i32" | "bool" ;
expression     = comparison ;
comparison     = additive { ("==" | "!=" | "<" | ">" | "<=" | ">=") additive } ;
additive       = multiplicative { ("+" | "-") multiplicative } ;
multiplicative = unary { ("*" | "/" | "%") unary } ;
unary          = "-" unary | primary ;
primary        = INTEGER | BOOL | IDENT | "(" expression ")" | block_expr | if_expr ;
block_expr     = "{" block "}" ;
if_expr        = "if" expression "{" block "}" [ "else" "{" block "}" ] ;

BOOL           = "true" | "false" ;
```

## Planned Features

See `docs/design-decisions.md` (ADR-009) for language philosophy. Planned additions include:

- Loops: `while`, `loop`
- Functions with parameters
- Structs and user-defined types
- Memory safety model (influenced by Hylo/Swift/Rust)
