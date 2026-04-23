+++
title = "Comptime and Generics"
weight = 15
template = "learn/page.html"
+++

# Comptime and Generics

Gruel's `comptime` feature lets you evaluate expressions at compile time and write generic functions that work with any type. This is inspired by Zig's unified compile-time execution model.

## Compile-Time Constants

Use `const` to define a named compile-time constant:

```gruel
const MAX_SIZE: i32 = 1024;
const HALF: i32 = MAX_SIZE / 2;

fn main() -> i32 {
    @dbg(MAX_SIZE);  // prints: 1024
    @dbg(HALF);      // prints: 512
    0
}
```

Constants are evaluated at compile time and can be used anywhere a value is expected, including array sizes.

## Comptime Blocks

The `comptime` keyword forces an expression to be evaluated at compile time:

```gruel
const FLAGS: i32 = comptime { 1 | 2 | 4 };

fn main() -> i32 {
    let buffer_size = comptime { 16 * 1024 };
    @dbg(buffer_size);  // prints: 16384
    0
}
```

If a `comptime` expression can't be evaluated at compile time (for example, because it reads runtime input), the compiler reports an error.

## Generic Functions

The real power of `comptime` is writing functions that work with multiple types. Mark a parameter `comptime T: type` to accept a type as an argument:

```gruel
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { a } else { b }
}

fn main() -> i32 {
    let x = max(i32, 10, 20);   // T = i32
    let y = max(i64, 3, 4);     // T = i64
    @dbg(x);  // prints: 20
    @dbg(y);  // prints: 4
    x
}
```

When you call `max(i32, 10, 20)`, the compiler generates a specialized version of `max` for `i32`. Each unique set of type arguments produces a separate compiled function—this is called *monomorphization*.

## Multiple Type Parameters

Functions can have more than one type parameter:

```gruel
fn first_of_two(comptime T: type, comptime U: type, a: T, b: U) -> T {
    a
}

fn main() -> i32 {
    first_of_two(i32, bool, 42, true)
}
```

## Comptime Value Parameters

`comptime` isn't limited to types—any parameter can be comptime. This lets the compiler specialize functions based on constant values:

```gruel
fn sum_up_to(comptime n: i32) -> i32 {
    let mut total = 0;
    let mut i = 0;
    while i <= n {
        total = total + i;
        i = i + 1;
    }
    total
}

fn main() -> i32 {
    let s = sum_up_to(10);  // Computed at compile time: 55
    @dbg(s);
    s
}
```

Because `n` is comptime-known, the compiler can evaluate the entire loop at compile time and replace the call with a constant.

## Generic Structs via Type-Returning Functions

You can write a function that takes type parameters and returns a new type. This is how generic data structures work in Gruel:

```gruel
fn Pair(comptime T: type, comptime U: type) -> type {
    struct {
        first: T,
        second: U,
    }
}

fn main() -> i32 {
    let p: Pair(i32, bool) = Pair(i32, bool) { first: 42, second: true };
    @dbg(p.first);   // prints: 42
    p.first
}
```

The function `Pair` runs at compile time and returns a struct type whose fields have the provided types.

## Mixing Comptime and Runtime Parameters

Functions can freely mix comptime and runtime parameters:

```gruel
fn repeat_value(comptime T: type, value: T, comptime n: i32) -> T {
    let mut result = value;
    let mut i = 1;
    while i < n {
        // In a real use case, you'd accumulate somehow
        i = i + 1;
    }
    result
}

fn main() -> i32 {
    // T and n are compile-time; value is runtime
    repeat_value(i32, 7, 3)
}
```

## Target Detection

`@target_arch()` and `@target_os()` return the compilation target as enum values. They are evaluated at compile time, so the unused branches are eliminated before code generation:

```gruel
fn main() -> i32 {
    let arch_id = match @target_arch() {
        Arch::X86_64  => 1,
        Arch::Aarch64 => 2,
    };

    let os_id = match @target_os() {
        Os::Linux => 10,
        Os::Macos => 20,
    };

    @dbg(arch_id);
    @dbg(os_id);
    0
}
```

Because the match is resolved at compile time, only the branch for the actual target ends up in the binary. This is the idiomatic way to write platform-specific code in Gruel.

## When to Use Comptime

Use `const` for named compile-time values (sizes, flags, lookup tables). Use `comptime { }` when you need to force compile-time evaluation of an expression. Use `comptime` parameters when you want to write a function that works with different types without code duplication. Use `@target_arch()`/`@target_os()` when behaviour needs to differ between platforms.
