+++
title = "Variables and Types"
weight = 3
template = "learn/page.html"
+++

# Variables and Types

Gruel is statically typed—every variable has a type known at compile time.

## Integer Types

Gruel has the integer types you'd expect, plus pointer-sized variants:

```gruel
fn main() -> i32 {
    // Signed integers: i8, i16, i32, i64, isize
    let x: i32 = 42;
    let big: i64 = 1000000000000;

    // Unsigned integers: u8, u16, u32, u64, usize
    let index: usize = 0;     // pointer-sized — used for array/slice indexing
    let byte: u8 = 255;

    @dbg(x);
    x
}
```

The number after `i` or `u` is the bit width. Signed integers (`i`) can be negative; unsigned integers (`u`) cannot. `isize` and `usize` are pointer-sized — `usize` is the type used for array and slice indices.

## Floating-Point Types

Gruel has three floating-point types: `f16`, `f32`, and `f64`. Float literals contain a decimal point or exponent; without one, a literal is an integer.

```gruel
fn main() -> i32 {
    let pi: f64 = 3.14159;
    let small: f32 = 1.5;
    @dbg(pi);
    0
}
```

## Type Inference

You don't always need to write types explicitly. The compiler can often infer them:

```gruel
fn main() -> i32 {
    let x = 42;        // inferred as i32 (the default)
    let y = true;      // inferred as bool

    @dbg(x);
    x
}
```

When there's no context, integer literals default to `i32`.

## Booleans

Boolean values are either `true` or `false`:

```gruel
fn main() -> i32 {
    let flag: bool = true;
    let done = false;

    @dbg(flag);   // prints: 1 (true)
    @dbg(done);   // prints: 0 (false)

    0
}
```

## Numeric Casts

To convert between integer or float types, use `@cast`. The target type is inferred from context:

```gruel
fn main() -> i32 {
    let big: i64 = 1000;
    let small: i32 = @cast(big);     // i64 -> i32

    let index: i32 = 5;
    let as_u64: u64 = @cast(index);  // i32 -> u64

    @dbg(small);   // prints: 1000
    let n: i32 = @cast(as_u64);
    @dbg(n);       // prints: 5
    0
}
```

If the value doesn't fit in the target type, the program panics at runtime:

```gruel
fn main() -> i32 {
    let x: i32 = 300;
    let y: u8 = @cast(x);  // panics: 300 doesn't fit in u8 (max 255)
    @cast(y)
}
```

## Mutability

Variables are immutable by default. Use `let mut` to make them mutable:

```gruel
fn main() -> i32 {
    let mut count = 0;
    count = count + 1;
    count = count + 1;
    @dbg(count);  // prints: 2
    count
}
```

Trying to assign to an immutable variable is a compile error:

```gruel
fn main() -> i32 {
    let x = 42;
    x = 43;  // Error: cannot assign to immutable variable
    x
}
```

This helps catch bugs—if a value shouldn't change, the compiler enforces it.
