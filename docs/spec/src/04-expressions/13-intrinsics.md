+++
title = "Intrinsic Expressions"
weight = 13
template = "spec/page.html"
+++

# Intrinsic Expressions

{{ rule(id="4.13:1", cat="normative") }}

An intrinsic expression is a [builtin](@/02-lexical-structure/05-builtins.md) that appears in expression position and produces a value.

{{ rule(id="4.13:2", cat="normative") }}

```ebnf
intrinsic = "@" IDENT "(" [ intrinsic_arg { "," intrinsic_arg } ] ")" ;
intrinsic_arg = expression | type ;
```

{{ rule(id="4.13:2a", cat="normative") }}

Intrinsics **MAY** accept expressions, types, or a combination of both as arguments, depending on the specific intrinsic.

{{ rule(id="4.13:3", cat="normative") }}

Each intrinsic has a fixed signature specifying the number and types of arguments it accepts.

{{ rule(id="4.13:4", cat="legality-rule") }}

It is a compile-time error to call an intrinsic with the wrong number of arguments.

{{ rule(id="4.13:5", cat="legality-rule") }}

It is a compile-time error to use an unknown intrinsic name.

## Quick Reference

{{ rule(id="4.13:5a", cat="informative") }}

The following table provides a quick reference to all available intrinsics:

| Intrinsic | Purpose | Arguments | Return Type |
|-----------|---------|-----------|-------------|
| `@dbg` | Print debug output | 0+ expressions (phase-dependent types) | `()` |
| `@size_of` | Get type size in bytes | 1 type | `usize` |
| `@align_of` | Get type alignment in bytes | 1 type | `usize` |
| `@cast` | Convert between numeric types | 1 expression (numeric) | inferred numeric type |
| `@read_line` | Read line from stdin | none | `String` |
| `@parse_i32` | Parse string to i32 | 1 expression (`String`) | `i32` |
| `@parse_i64` | Parse string to i64 | 1 expression (`String`) | `i64` |
| `@parse_u32` | Parse string to u32 | 1 expression (`String`) | `u32` |
| `@parse_u64` | Parse string to u64 | 1 expression (`String`) | `u64` |
| `@random_u32` | Generate random u32 | none | `u32` |
| `@random_u64` | Generate random u64 | none | `u64` |
| `@target_arch` | Get target architecture | none | `Arch` |
| `@target_os` | Get target OS | none | `Os` |
| `@range` | Construct integer range | 1-3 expressions (integers) | `Range(T)` |
| `@import` | Import module | 1 expression (string literal or `comptime_str`) | module type |
| `@embed_file` | Embed a file's bytes at compile time | 1 expression (string literal) | `Slice(u8)` |

## `@dbg`

{{ rule(id="4.13:6", cat="normative") }}

The `@dbg` intrinsic prints its arguments for debugging purposes. Its output destination depends on the phase in which it executes: calls evaluated at runtime print to standard output, while calls evaluated inside a [comptime context](@/04-expressions/14-comptime.md) print to standard error during compilation.

{{ rule(id="4.13:7", cat="normative") }}

`@dbg` accepts zero or more arguments. The accepted argument types depend on the evaluation phase:

- At runtime: each argument **MUST** be of integer, boolean, or `String` type.
- At compile time: each argument **MUST** be a compile-time evaluable value of integer, boolean, unit, or `comptime_str` type.

{{ rule(id="4.13:8", cat="normative") }}

`@dbg` formats each argument as a human-readable string, joins the results with single ASCII space characters, and emits the resulting line followed by a newline. A call with zero arguments emits an empty line.

{{ rule(id="4.13:9", cat="normative") }}

At runtime, the formatted line is written to standard output. Integer values are formatted as signed or unsigned decimal according to their declared type, boolean values as `true` or `false`, and `String` values as their UTF-8 contents.

{{ rule(id="4.13:9a", cat="normative") }}

When `@dbg` is evaluated during compile-time interpretation, the compiler immediately writes the formatted line to standard error, prefixed with the literal string `comptime dbg: `. Compile-time evaluation always formats integers as signed decimal, booleans as `true` or `false`, `comptime_str` values as their contents, and `()` as `()`.

{{ rule(id="4.13:9b", cat="normative") }}

Each `@dbg` call whose arguments are evaluated at compile time also produces a post-compilation warning ("debug statement present"). The warning is attached to the call site and is emitted once per call.

{{ rule(id="4.13:9c", cat="normative") }}

The compiler collects the formatted messages from compile-time `@dbg` calls in a buffer on the compilation result, whether or not the compiler also prints them. A compiler-driver flag (`--capture-comptime-dbg`) suppresses the on-the-fly stderr print while leaving the buffer intact; this flag is intended for tools that consume the buffer directly.

{{ rule(id="4.13:9d", cat="normative") }}

`@dbg` observes its arguments without consuming them. When an argument is a place expression of an affine type (e.g. `String`), the binding remains usable after the call.

{{ rule(id="4.13:10", cat="normative") }}

The return type of `@dbg` is `()`.

{{ rule(id="4.13:10a", cat="example") }}

```gruel
fn main() -> i32 {
    @dbg(42);                 // prints: 42
    @dbg(true);               // prints: true
    @dbg("hello");            // prints: hello
    @dbg("n =", 42);          // prints: n = 42 (variadic)
    @dbg();                   // prints an empty line
    0
}
```

{{ rule(id="4.13:11", cat="example") }}

`@dbg` is useful for inspecting values during development:

```gruel
fn factorial(n: i32) -> i32 {
    @dbg(n);  // trace each call
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
```

{{ rule(id="4.13:11a", cat="example") }}

Inside a comptime block, `@dbg` is a compile-time debugging tool. The output appears on the compiler's standard error and the build emits a warning for each call:

```gruel
fn compute(comptime n: i32) -> i32 {
    comptime { @dbg("computing with n =", n); }
    n * 2
}

fn main() -> i32 {
    compute(21)
    // compiler output: comptime dbg: computing with n = 21
    // compiler warning: debug statement present — remove before release
}
```

## `@size_of`

{{ rule(id="4.13:12", cat="normative") }}

The `@size_of` intrinsic returns the size of a type in bytes.

{{ rule(id="4.13:13", cat="normative") }}

`@size_of` accepts exactly one argument, which **MUST** be a type.

{{ rule(id="4.13:14", cat="normative") }}

The return type of `@size_of` is `usize`.

{{ rule(id="4.13:15", cat="normative") }}

The value returned by `@size_of` is determined at compile time.

{{ rule(id="4.13:16") }}

```gruel
fn main() -> i32 {
    let n: usize = @size_of(i32);   // 8 (one 8-byte slot)
    @cast(n)
}
```

{{ rule(id="4.13:17") }}

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let n: usize = @size_of(Point); // 16 (two 8-byte slots)
    @cast(n)
}
```

## `@align_of`

{{ rule(id="4.13:18", cat="normative") }}

The `@align_of` intrinsic returns the alignment of a type in bytes.

{{ rule(id="4.13:19", cat="normative") }}

`@align_of` accepts exactly one argument, which **MUST** be a type.

{{ rule(id="4.13:20", cat="normative") }}

The return type of `@align_of` is `usize`.

{{ rule(id="4.13:21", cat="normative") }}

The value returned by `@align_of` is determined at compile time.

{{ rule(id="4.13:22", cat="normative") }}

All types in Gruel currently have 8-byte alignment.

{{ rule(id="4.13:23") }}

```gruel
fn main() -> i32 {
    let a: usize = @align_of(i32);  // 8
    @cast(a)
}
```

## `@ownership`

{{ rule(id="4.13:108", cat="normative") }}

The `@ownership` intrinsic classifies the ownership posture of a type
(see ADR-0008, ADR-0059). The classification is computed from conformance
to the compiler-recognized `Copy` interface (§3.8) plus the `linear`
keyword.

{{ rule(id="4.13:109", cat="normative") }}

`@ownership` accepts exactly one argument, which **MUST** be a type.

{{ rule(id="4.13:110", cat="normative") }}

The return type of `@ownership` is the built-in enum `Ownership`, which has three variants:

| Variant | Meaning |
|---------|---------|
| `Ownership::Copy` | Values may be implicitly duplicated by bitwise copy. |
| `Ownership::Affine` | Values may be used at most once and are implicitly dropped if not consumed. This is the default for user-defined structs. |
| `Ownership::Linear` | Values must be explicitly consumed; implicit drop is a compile-time error. |

{{ rule(id="4.13:111", cat="normative") }}

The variants are mutually exclusive: every type has exactly one ownership
posture. The classification is:

- `Linear` if `T` carries the `linear` keyword.
- `Copy` if `T` is a primitive (integers, floats, `bool`, `char`, `()`),
  a pointer or reference, a struct or enum declared `copy`, or a tuple
  whose every element is Copy. Anonymous `enum { … }` literals (used by
  the prelude's `Option(T)` / `Result(T, E)`) infer Copy structurally
  the same way tuples do.
- `Affine` otherwise. Arrays and `Vec` are perpetually non-Copy
  regardless of their element type.

{{ rule(id="4.13:112", cat="normative") }}

The value returned by `@ownership` is determined at compile time.

{{ rule(id="4.13:113") }}

```gruel
fn main() -> i32 {
    match @ownership(i32) {
        Ownership::Copy => 1,
        Ownership::Affine => 2,
        Ownership::Linear => 3,
    }  // 1
}
```

{{ rule(id="4.13:114") }}

```gruel
struct Point { x: i32, y: i32 }  // Affine by default

fn main() -> i32 {
    match @ownership(Point) {
        Ownership::Copy => 1,
        Ownership::Affine => 2,
        Ownership::Linear => 3,
    }  // 2
}
```

## `@implements`

{{ rule(id="4.13:115", cat="normative") }}

The `@implements` intrinsic reports whether a type structurally implements
an interface (see §6 and ADR-0056).

{{ rule(id="4.13:116", cat="normative") }}

`@implements` accepts exactly two arguments. The first **MUST** be a type;
the second **MUST** name an interface.

{{ rule(id="4.13:117", cat="normative") }}

The return type of `@implements` is `bool`.

{{ rule(id="4.13:118", cat="normative") }}

`@implements(T, I)` evaluates to `true` if every method requirement of
interface `I` is satisfied by a method of type `T` whose receiver mode,
parameter types, and return type all match the requirement (with `Self`
substituted by `T`); otherwise it evaluates to `false`. For the
compiler-recognized interface `Drop`, conformance is determined by the
language's ownership rules rather than user-declared methods (see §3.8
and ADR-0059): `@implements(T, Drop)` is `true` iff `T` is non-`linear`
and not Copy. ADR-0080 retired `Copy` from the interface set —
`@implements(T, Copy)` falls through the existing "unknown interface"
diagnostic; query Copy posture via `@ownership(T) == Ownership::Copy`
instead.

{{ rule(id="4.13:119", cat="legality-rule") }}

It is a compile-time error if the second argument does not name an
interface, or if either argument cannot be resolved.

{{ rule(id="4.13:120", cat="normative") }}

The value returned by `@implements` is determined at compile time.

{{ rule(id="4.13:121") }}

```gruel
interface Greeter {
    fn greet(self);
}

struct Friendly {
    name: String,
    fn greet(self) {}
}

fn main() -> i32 {
    if @implements(Friendly, Greeter) { 1 } else { 0 }  // 1
}
```

{{ rule(id="4.13:122") }}

```gruel
fn main() -> i32 {
    if @implements(i32, Copy) { 1 } else { 0 }  // 1
}
```

## `@cast`

{{ rule(id="4.13:95", cat="normative") }}

The `@cast` intrinsic converts a numeric value from one numeric type to another, covering both integer-to-integer conversions and conversions involving floating-point types.

{{ rule(id="4.13:96", cat="normative") }}

`@cast` accepts exactly one argument, which **MUST** be a numeric type (any integer type or any floating-point type).

{{ rule(id="4.13:97", cat="normative") }}

The target type of the conversion is inferred from the context where `@cast` is used.

{{ rule(id="4.13:98", cat="legality-rule") }}

It is a compile-time error if the target type cannot be inferred or is not a numeric type.

{{ rule(id="4.13:99", cat="dynamic-semantics") }}

For integer-to-integer conversions, if the source value cannot be exactly represented in the target type, a runtime panic occurs.

{{ rule(id="4.13:100", cat="dynamic-semantics") }}

For float-to-float conversions (e.g., `f64` to `f32`), the value is narrowed or widened following IEEE 754 rounding rules. Precision loss during narrowing is silent (no panic). Narrowing a value outside the target range produces infinity.

{{ rule(id="4.13:101", cat="dynamic-semantics") }}

For integer-to-float conversions, the integer value is converted to the closest representable floating-point value. Loss of precision for large integer values is silent (no panic).

{{ rule(id="4.13:102", cat="dynamic-semantics") }}

For float-to-integer conversions, the float value is truncated toward zero. A runtime panic occurs if the value is NaN or if the truncated value is outside the representable range of the target integer type.

{{ rule(id="4.13:103") }}

```gruel
fn main() -> i32 {
    let x: f64 = 3.14;
    let y: f32 = @cast(x);      // f64 → f32 (narrowing)
    let z: f64 = @cast(y);      // f32 → f64 (widening)
    0
}
```

{{ rule(id="4.13:104") }}

```gruel
fn main() -> i32 {
    let n: i32 = 42;
    let f: f64 = @cast(n);      // i32 → f64
    let m: i32 = @cast(f);      // f64 → i32 (truncates toward zero)
    m
}
```

{{ rule(id="4.13:105") }}

```gruel
// This panics at runtime: NaN cannot be converted to an integer
fn main() -> i32 {
    let nan: f64 = 0.0 / 0.0;
    let n: i32 = @cast(nan);    // panic: float-to-integer cast overflow
    n
}
```

{{ rule(id="4.13:106") }}

```gruel
// This panics at runtime: value too large for target integer type
fn main() -> i32 {
    let big: f64 = 9999999999999999999999.0;
    let n: i32 = @cast(big);    // panic: float-to-integer cast overflow
    n
}
```

{{ rule(id="4.13:107") }}

```gruel
fn main() -> i32 {
    let x: i32 = 100;
    let y: u8 = @cast(x);       // Integer narrowing
    @cast(y)                     // Integer widening
}
```

## `@read_line`

{{ rule(id="4.13:33", cat="normative") }}

The `@read_line` intrinsic reads a line of text from standard input.

{{ rule(id="4.13:34", cat="normative") }}

`@read_line` accepts no arguments.

{{ rule(id="4.13:35", cat="normative") }}

The return type of `@read_line` is `String`.

{{ rule(id="4.13:36", cat="dynamic-semantics") }}

`@read_line` reads bytes from standard input until a newline character (`\n`) is encountered or end-of-file is reached.

{{ rule(id="4.13:37", cat="dynamic-semantics") }}

The returned `String` does **not** include the trailing newline character.

{{ rule(id="4.13:38", cat="dynamic-semantics") }}

If end-of-file is reached with some data read, the partial line is returned.

{{ rule(id="4.13:39", cat="dynamic-semantics") }}

If end-of-file is reached with no data read, a runtime panic occurs with the message "unexpected end of input".

{{ rule(id="4.13:40", cat="informative") }}

If a read error occurs, a runtime panic occurs with the message "input error". (This behavior is documented but not tested, as I/O errors cannot be reliably simulated in portable test environments.)

{{ rule(id="4.13:41") }}

```gruel
fn main() -> i32 {
    @dbg("What is your name?");
    let name = @read_line();
    @dbg("Hello, ");
    @dbg(name);
    0
}
```

{{ rule(id="4.13:42") }}

Reading multiple lines:

```gruel
fn main() -> i32 {
    let line1 = @read_line();  // First line
    let line2 = @read_line();  // Second line
    @dbg(line1);
    @dbg(line2);
    0
}
```

## Integer Parsing Intrinsics

{{ rule(id="4.13:43", cat="normative") }}

The integer parsing intrinsics convert a string to an integer value.

{{ rule(id="4.13:44", cat="normative") }}

The following parsing intrinsics are available:
- `@parse_i32` returns `i32`
- `@parse_i64` returns `i64`
- `@parse_u32` returns `u32`
- `@parse_u64` returns `u64`

{{ rule(id="4.13:45", cat="normative") }}

Each parsing intrinsic accepts exactly one argument, which **MUST** be of type `String`.

{{ rule(id="4.13:46", cat="normative") }}

The string argument is borrowed, not consumed. The original string remains valid after parsing.

{{ rule(id="4.13:47", cat="normative") }}

The parsed string must match the following grammar:

```ebnf
integer_string = [ "-" ] digit { digit } ;
digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

{{ rule(id="4.13:48", cat="legality-rule") }}

Leading minus signs are only allowed for signed types (`@parse_i32`, `@parse_i64`).

{{ rule(id="4.13:49", cat="dynamic-semantics") }}

A runtime panic occurs if:
- The string is empty
- The string contains non-digit characters (other than an optional leading minus)
- The value overflows the target type
- A negative value is parsed for an unsigned type

{{ rule(id="4.13:50") }}

```gruel
fn main() -> i32 {
    let s = "42";
    let n = @parse_i32(s);
    n  // returns 42
}
```

{{ rule(id="4.13:51") }}

```gruel
fn main() -> i32 {
    let s = "-17";
    let n = @parse_i32(s);
    n  // returns -17
}
```

{{ rule(id="4.13:52") }}

```gruel
fn main() -> i32 {
    let s = "42";
    // String is borrowed, not consumed
    let n = @parse_i32(s);
    @dbg(s);  // s is still valid
    n
}
```

{{ rule(id="4.13:53") }}

```gruel
// This panics at runtime: invalid character
fn main() -> i32 {
    let s = "12abc";
    let n = @parse_i32(s);  // panic: invalid character
    n
}
```

{{ rule(id="4.13:54") }}

```gruel
// This panics at runtime: negative for unsigned
fn main() -> i32 {
    let s = "-17";
    let n: u32 = @parse_u32(s);  // panic: negative value for unsigned type
    @cast(n)
}
```

## `@random_u32`

{{ rule(id="4.13:55", cat="normative") }}

The `@random_u32` intrinsic generates a random unsigned 32-bit integer.

{{ rule(id="4.13:56", cat="normative") }}

`@random_u32` accepts no arguments.

{{ rule(id="4.13:57", cat="normative") }}

The return type of `@random_u32` is `u32`.

{{ rule(id="4.13:58", cat="dynamic-semantics") }}

Each call to `@random_u32` returns a non-deterministic value using a platform-provided cryptographically-secure entropy source.

{{ rule(id="4.13:59", cat="dynamic-semantics") }}

If the platform entropy source is unavailable or fails, a runtime panic occurs.

{{ rule(id="4.13:60") }}

```gruel
fn main() -> i32 {
    let secret: u32 = (@random_u32() % 100) + 1;  // Random number 1-100
    @dbg(secret);
    0
}
```

{{ rule(id="4.13:61") }}

Using `@random_u32` in a guessing game:

```gruel
fn main() -> i32 {
    let secret: u32 = (@random_u32() % 100) + 1;  // 1-100
    @dbg("Guess the number between 1 and 100!");

    let mut guesses = 0;
    loop {
        let input = @read_line();
        let guess = @parse_u32(input);
        guesses = guesses + 1;

        if guess < secret {
            @dbg("Too low!");
        } else if guess > secret {
            @dbg("Too high!");
        } else {
            @dbg("You got it!");
            break;
        }
    }

    @cast(guesses)
}
```

## `@random_u64`

{{ rule(id="4.13:62", cat="normative") }}

The `@random_u64` intrinsic behaves identically to `@random_u32` but returns a random unsigned 64-bit integer.

{{ rule(id="4.13:63", cat="normative") }}

`@random_u64` accepts no arguments.

{{ rule(id="4.13:64", cat="normative") }}

The return type of `@random_u64` is `u64`.

{{ rule(id="4.13:65") }}

```gruel
fn main() -> i32 {
    let large_random = @random_u64();
    @dbg(large_random);
    0
}
```

## `@target_arch`

{{ rule(id="4.13:66", cat="normative") }}

The `@target_arch` intrinsic returns the target architecture as an `Arch` enum value.

{{ rule(id="4.13:67", cat="normative") }}

`@target_arch` accepts no arguments.

{{ rule(id="4.13:68", cat="normative") }}

The return type of `@target_arch` is `Arch`.

{{ rule(id="4.13:69", cat="normative") }}

The `Arch` enum is a built-in enum with the following variants, in order:
- `Arch::X86_64` - x86-64 / AMD64
- `Arch::Aarch64` - ARM64 / AArch64
- `Arch::X86` - 32-bit x86
- `Arch::Arm` - 32-bit ARM
- `Arch::Riscv32` - 32-bit RISC-V
- `Arch::Riscv64` - 64-bit RISC-V
- `Arch::Wasm32` - 32-bit WebAssembly
- `Arch::Wasm64` - 64-bit WebAssembly

Variant indices are stable: existing variants keep their position and new
variants are appended.

{{ rule(id="4.13:70", cat="normative") }}

The value returned by `@target_arch` is determined at compile time based on the compilation target.

{{ rule(id="4.13:71") }}

```gruel
fn main() -> i32 {
    match @target_arch() {
        Arch::X86_64 => 1,
        Arch::Aarch64 => 2,
        _ => 0,
    }
}
```

## `@target_os`

{{ rule(id="4.13:72", cat="normative") }}

The `@target_os` intrinsic returns the target operating system as an `Os` enum value.

{{ rule(id="4.13:73", cat="normative") }}

`@target_os` accepts no arguments.

{{ rule(id="4.13:74", cat="normative") }}

The return type of `@target_os` is `Os`.

{{ rule(id="4.13:75", cat="normative") }}

The `Os` enum is a built-in enum with the following variants, in order:
- `Os::Linux` - Linux
- `Os::Macos` - macOS / Darwin
- `Os::Windows` - Microsoft Windows
- `Os::Freestanding` - no operating system (bare metal)
- `Os::Wasi` - WebAssembly System Interface

Variant indices are stable: existing variants keep their position and new
variants are appended.

{{ rule(id="4.13:76", cat="normative") }}

The value returned by `@target_os` is determined at compile time based on the compilation target.

{{ rule(id="4.13:77") }}

```gruel
fn main() -> i32 {
    match @target_os() {
        Os::Linux => 1,
        Os::Macos => 2,
        _ => 0,
    }
}
```

{{ rule(id="4.13:78") }}

Combining `@target_arch` and `@target_os` for platform-specific code:

```gruel
fn main() -> i32 {
    match @target_arch() {
        Arch::X86_64 => {
            match @target_os() {
                Os::Linux => 99,
                Os::Macos => 88,
                _ => 0,
            }
        },
        Arch::Aarch64 => {
            match @target_os() {
                Os::Linux => 77,
                Os::Macos => 66,
                _ => 0,
            }
        },
        _ => 0,
    }
}
```

## `@range`

{{ rule(id="4.13:89", cat="normative") }}

The `@range` intrinsic constructs a `Range(T)` value representing an integer range, for use with for-in loops.

{{ rule(id="4.13:90", cat="normative") }}

`@range` accepts 1, 2, or 3 integer arguments:

| Form | Meaning |
|------|---------|
| `@range(end)` | `0` to `end`, exclusive, stride 1 |
| `@range(start, end)` | `start` to `end`, exclusive, stride 1 |
| `@range(start, end, stride)` | `start` to `end`, exclusive, step by `stride` |

{{ rule(id="4.13:91", cat="legality-rule") }}

All arguments to `@range` **MUST** be the same integer type `T`. The result has type `Range(T)`.

{{ rule(id="4.13:92", cat="normative") }}

`Range(T)` is a builtin comptime type constructor parameterized by an integer type. It has fields `start`, `end`, `stride` of type `T`, and `inclusive` of type `bool`. The `.inclusive()` method returns a new range with `inclusive` set to `true`.

{{ rule(id="4.13:93") }}

```gruel
fn main() -> i32 {
    let mut sum = 0;
    for i in @range(10) {
        sum = sum + i;
    }
    sum  // 45
}
```

{{ rule(id="4.13:94") }}

```gruel
fn main() -> i32 {
    let mut sum = 0;
    for i in @range(0, 10, 2) {
        sum = sum + i;
    }
    sum  // 20 (0+2+4+6+8)
}
```

## `@import`

{{ rule(id="4.13:79", cat="normative") }}

The `@import` intrinsic imports a module from another source file.

{{ rule(id="4.13:80", cat="normative") }}

`@import` accepts exactly one argument. The argument **MUST** be either a string literal or an expression of type `comptime_str` specifying the module path. Expressions of type `comptime_str` are evaluated by the compile-time interpreter; this enables conditional imports driven by `@target_os()`, `@target_arch()`, or any other comptime-known data.

{{ rule(id="4.13:81", cat="normative") }}

The return type of `@import` is a module struct type containing all `pub` declarations from the imported file.

{{ rule(id="4.13:82", cat="normative") }}

Module path resolution follows this order:
1. Standard library: `@import("std")` resolves to the bundled standard library
2. A file `{path}.gruel` relative to the importing file's directory
3. A directory module `_{path}.gruel` with subdirectory `{path}/`

{{ rule(id="4.13:83", cat="legality-rule") }}

It is a compile-time error if the module path does not resolve to an existing file.

{{ rule(id="4.13:84", cat="legality-rule") }}

It is a compile-time error to pass an argument to `@import` that is neither a string literal nor a `comptime_str` expression. Passing a runtime value (e.g. a `String` parameter or a local bound to a runtime expression) is a compile-time error because the module path must be resolvable during semantic analysis.

{{ rule(id="4.13:85") }}

```gruel
// math.gruel
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn sub(a: i32, b: i32) -> i32 { a - b }
fn helper() -> i32 { 42 }  // private, not exported

// main.gruel
fn main() -> i32 {
    let math = @import("math");
    math.add(1, 2)  // returns 3
}
```

{{ rule(id="4.13:86") }}

Private declarations (those without `pub`) are not visible to importers:

```gruel
// main.gruel
fn main() -> i32 {
    let math = @import("math");
    // math.helper()  // Error: `helper` is not visible
    0
}
```

{{ rule(id="4.13:87") }}

The imported module can be bound to any name:

```gruel
fn main() -> i32 {
    let m = @import("math");
    m.add(1, 2)
}
```

{{ rule(id="4.13:88") }}

Nested paths are supported for importing from subdirectories:

```gruel
fn main() -> i32 {
    let strings = @import("utils/strings");
    0
}
```

{{ rule(id="4.13:100") }}

A `comptime_str` argument enables platform-conditional imports. The expression is evaluated by the compile-time interpreter before module resolution:

```gruel
fn main() -> i32 {
    let sys = @import(comptime {
        if @target_os() == Os::Linux {
            "sys_linux"
        } else {
            "sys_macos"
        }
    });
    0
}
```

## `@embed_file`

{{ rule(id="4.13:130", cat="normative") }}

The `@embed_file` intrinsic embeds the contents of a file at compile time as a read-only byte slice.

{{ rule(id="4.13:131", cat="normative") }}

`@embed_file` accepts exactly one argument. The argument **MUST** be a string literal specifying the path to the file. Path resolution is relative to the source file containing the `@embed_file` call; absolute paths are used as-is.

{{ rule(id="4.13:132", cat="normative") }}

The return type of `@embed_file` is `Slice(u8)`. The slice's pointer references a binary-baked global; the bytes have effectively static lifetime and **MUST NOT** be mutated.

{{ rule(id="4.13:133", cat="legality-rule") }}

It is a compile-time error if the path argument is not a string literal, or if the file cannot be read at the time semantic analysis runs.

{{ rule(id="4.13:134") }}

```gruel
fn main() -> i32 {
    let data: Slice(u8) = @embed_file("greeting.txt");
    @cast(data[0], i32)  // first byte of greeting.txt
}
```
