# Rue Language Specification

This document describes the Rue programming language. Rue is a minimal, statically-typed language that compiles to native x86-64 ELF executables.

## 1. Expressions

### 1.1 Integer Literals

An integer literal is a sequence of decimal digits representing a signed 64-bit integer.

**Syntax:**
```ebnf
integer = digit+ ;
digit   = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

**Semantics:**
- Integer literals evaluate to their numeric value
- Range: 0 to 2^63-1 (negative literals not yet supported)
- When returned from `main`, the value is truncated to 8 bits for the process exit code (i.e., `value & 0xFF`)

**Examples:**
```rue
fn main() -> i32 { 0 }    // exits with code 0
fn main() -> i32 { 42 }   // exits with code 42
fn main() -> i32 { 256 }  // exits with code 0 (256 & 0xFF = 0)
```

**See also:** [Test cases](cases/01-integers.toml)

## 2. Declarations

### 2.1 Functions

Functions are declared with the `fn` keyword.

**Syntax:**
```ebnf
function = "fn" identifier "(" ")" [ "->" type ] "{" expression "}" ;
identifier = letter ( letter | digit | "_" )* ;
type = "i32" ;
```

**Semantics:**
- The `main` function is the entry point of the program
- Functions must have a return type annotation (currently only `i32` is supported)
- The function body is a single expression whose value is returned

**Examples:**
```rue
fn main() -> i32 { 42 }
```