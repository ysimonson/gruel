# Rue Roadmap

This document outlines planned work for the Rue compiler. Items are roughly ordered by priority within each section, but this is not a commitment to any particular timeline.

## Current Status

Rue can compile a minimal program:

```rue
fn main() -> i32 {
    42
}
```

The compiler produces a working Linux x86-64 ELF executable.

## Near-Term: Basic Language Features

### Arithmetic Operators
- [ ] Binary operators: `+`, `-`, `*`, `/`, `%`
- [ ] Operator precedence
- [ ] Parentheses for grouping

```rue
fn main() -> i32 {
    1 + 2 * 3  // = 7
}
```

### Local Variables
- [ ] `let` bindings (immutable)
- [ ] `let mut` bindings (mutable)
- [ ] Type inference
- [ ] Explicit type annotations

```rue
fn main() -> i32 {
    let x = 40;
    let y = 2;
    x + y
}
```

### More Types
- [ ] Integer types: `i8`, `i16`, `i64`, `u8`, `u16`, `u32`, `u64`
- [ ] Boolean: `bool`, `true`, `false`
- [ ] Comparison operators: `==`, `!=`, `<`, `>`, `<=`, `>=`
- [ ] Logical operators: `&&`, `||`, `!`

### Control Flow
- [ ] `if`/`else` expressions
- [ ] `while` loops
- [ ] `loop` (infinite loop with `break`)

```rue
fn main() -> i32 {
    let x = 10;
    if x > 5 {
        1
    } else {
        0
    }
}
```

### Functions with Parameters
- [ ] Function parameters
- [ ] Multiple functions
- [ ] Function calls

```rue
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(40, 2)
}
```

## Medium-Term: Type System

### Structs
- [ ] Struct definitions
- [ ] Field access
- [ ] Struct literals

```rue
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    p.x + p.y
}
```

### Enums
- [ ] Simple enums
- [ ] Enums with data
- [ ] Pattern matching

## Long-Term: Production Readiness

### Tooling
- [ ] Language server (LSP)
- [ ] Syntax highlighting definitions
- [ ] Formatter
- [ ] Package manager

### Compiler Infrastructure
- [ ] Parallel compilation
- [ ] Incremental compilation (maybe)
- [ ] Debug info (DWARF)
- [ ] Optimization passes

### Additional Targets
- [ ] ARM64 / AArch64
- [ ] WebAssembly
- [ ] macOS support
- [ ] Windows support

### Standard Library
- [ ] Core types
- [ ] I/O
- [ ] Collections
- [ ] String handling

## Non-Goals (For Now)

Things we're explicitly not pursuing yet:

- **Garbage collection** - Memory safety through other means
- **Exceptions** - Use Result types instead
- **Inheritance** - Composition over inheritance
- **Macros** - Focus on the core language first
- **Async/await** - Later, if at all

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md) for how to help.

Pick something from the near-term list and open an issue to discuss!
