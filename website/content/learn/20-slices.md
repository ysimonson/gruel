+++
title = "Slices"
weight = 20
template = "learn/page.html"
+++

# Slices

A slice is a borrowed view of a contiguous run of elements. Where an array's length is part of its type (`[i32; 5]`), a slice's length is a runtime value — so the same function can work over arrays of any size or over a subrange of an array.

Slices come in two flavors:

- `Slice(T)` — read-only view
- `MutSlice(T)` — read/write view

## Constructing a Slice

Build a slice by borrowing a range of an array:

```gruel
fn main() -> i32 {
    let arr: [i32; 5] = [10, 20, 30, 40, 50];
    let whole: Slice(i32) = &arr[..];      // all 5 elements
    let middle: Slice(i32) = &arr[1..4];   // 3 elements: [20, 30, 40]
    let _tail: Slice(i32) = &arr[2..];     // [30, 40, 50]
    let _head: Slice(i32) = &arr[..3];     // [10, 20, 30]
    let n: i32 = @cast(middle.len());
    n                                      // 3
}
```

The same syntax with `&mut` produces a `MutSlice(T)`:

```gruel
fn main() -> i32 {
    let mut arr: [i32; 5] = [1, 2, 3, 4, 5];
    let m: MutSlice(i32) = &mut arr[..];
    m[2] = 42;
    arr[2]   // 42
}
```

The `..` form is the whole array. `lo..hi` selects elements `lo` (inclusive) through `hi` (exclusive). The bounds must be `usize`.

## Slice Methods

Both slice kinds have:

- `s.len() -> usize` — number of elements
- `s.is_empty() -> bool` — whether `len()` is zero

Reads use `s[i]`, with bounds checking that panics on out-of-range indices. Writes through a `MutSlice(T)` use `m[i] = value`.

```gruel
fn first_two_sum(s: Slice(i32)) -> i32 {
    let n: usize = s.len();
    if n >= 2 {
        let mut total: i32 = 0;
        let mut i: usize = 0;
        for x in s {
            if i < 2 { total = total + x; }
            i = i + 1;
        }
        total
    } else {
        0
    }
}

fn main() -> i32 {
    let arr = [10, 20, 30, 40];
    first_two_sum(&arr[..])    // 30
}
```

> **Indexing through a slice parameter.** Currently `s[i]` works on a *local* slice variable but not on a slice received as a function parameter. To index in a function, iterate with `for x in s` (which works in both cases). This restriction will go away as the slice ABI is finished — see [ADR-0064](@/learn/references/adrs/0064-slices.md).

## Iterating Over a Slice

`for x in slice` walks the elements in order:

```gruel
fn sum(s: Slice(i32)) -> i32 {
    let mut total: i32 = 0;
    for x in s {
        total = total + x;
    }
    total
}

fn main() -> i32 {
    let arr: [i32; 5] = [10, 20, 30, 40, 50];
    sum(&arr[1..4])            // 90 (20 + 30 + 40)
}
```

The for-each form makes slices the natural way to write algorithms that don't care about array length.

## Generic Functions

Because a slice's length is dynamic, a single function can accept arrays of any size:

```gruel
fn max(s: Slice(i32)) -> i32 {
    let mut best: i32 = 0;
    let mut seen: bool = false;
    for x in s {
        if !seen || x > best {
            best = x;
            seen = true;
        }
    }
    best
}

fn main() -> i32 {
    let small: [i32; 3] = [3, 7, 1];
    let big: [i32; 6] = [9, 2, 4, 8, 5, 6];
    max(&small[..]) + max(&big[..])    // 7 + 9 = 16
}
```

This pattern — "take a slice, not an array" — is the standard way to write reusable code over collections.

## Slice Lifetimes

Like other reference forms, a slice is scope-bound: the underlying array must outlive every slice that views it. Slices can't be returned from a function, stored in a struct field that outlives the call, or otherwise allowed to escape. The compiler enforces this so a slice never points to memory that's been freed or reused.

For the formal rules, see the [Slices spec chapter](@/spec/07-arrays/_index.md) and [ADR-0064](@/learn/references/adrs/0064-slices.md).
