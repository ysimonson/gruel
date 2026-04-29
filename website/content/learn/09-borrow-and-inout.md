+++
title = "Borrow and Inout"
weight = 9
template = "learn/page.html"
+++

# Borrow and Inout Parameters

When you read code, you want to understand what it does without tracing through every function. Gruel makes mutation visible at the call site by requiring you to spell out when a function is allowed to read or write a borrowed value.

## Reading Code at a Glance

Look at this code:

```gruel
fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(inout values);
    values[0]
}
```

Without seeing `double_all`'s definition, you already know it modifies `values`. The `inout` keyword tells you: this function will change my data.

Compare that to languages where mutation is invisible:

```go
// Go: does this modify values? You can't tell without reading sort()
sort.Ints(values)
```

```python
# Python: same problem
values.sort()
```

In Gruel, mutation is always explicit at the call site.

## How It Works

### `inout`: Modify in Place

Use `inout` when a function needs to modify its argument. Both the parameter declaration and the call site spell out the keyword:

```gruel
fn double_all(inout arr: [i32; 3]) {
    let mut i: usize = 0;
    while i < 3 {
        arr[i] = arr[i] * 2;
        i = i + 1;
    }
}

fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(inout values);

    @dbg(values[0]);  // prints: 20
    @dbg(values[1]);  // prints: 40
    @dbg(values[2]);  // prints: 60

    values[0]
}
```

There's no way to accidentally miss that mutation is happening.

### `borrow`: Read Without Copying

Use `borrow` when a function only needs to read:

```gruel
fn sum_array(borrow arr: [i32; 5]) -> i32 {
    let mut total = 0;
    let mut i: usize = 0;
    while i < 5 {
        total = total + arr[i];
        i = i + 1;
    }
    total
}

fn main() -> i32 {
    let numbers = [1, 2, 3, 4, 5];
    let sum = sum_array(borrow numbers);
    @dbg(sum);  // prints: 15
    sum
}
```

With `borrow`, the function won't change your data, and the original binding is still usable after the call.

## Combining Them

You can mix borrow and inout in a single function:

```gruel
fn copy_into(borrow src: [i32; 3], inout dst: [i32; 3]) {
    let mut i: usize = 0;
    while i < 3 {
        dst[i] = src[i];
        i = i + 1;
    }
}

fn main() -> i32 {
    let source = [1, 2, 3];
    let mut dest = [0, 0, 0];

    copy_into(borrow source, inout dest);

    @dbg(dest[0]);  // prints: 1
    @dbg(dest[1]);  // prints: 2
    @dbg(dest[2]);  // prints: 3

    0
}
```

Reading the call site, you immediately know: `source` is read, `dest` is modified.

## With Structs

These work with any type, not just arrays:

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn translate(inout p: Point, dx: i32, dy: i32) {
    p.x = p.x + dx;
    p.y = p.y + dy;
}

fn print_point(borrow p: Point) {
    @dbg(p.x);
    @dbg(p.y);
}

fn main() -> i32 {
    let mut pos = Point { x: 10, y: 20 };

    print_point(borrow pos);          // prints: 10, 20
    translate(inout pos, 5, -3);
    print_point(borrow pos);          // prints: 15, 17

    0
}
```

## Aliasing Rules

`borrow` and `inout` are scope-bound, and the compiler enforces a few rules to keep them safe:

1. **`inout` requires `let mut`.** You can only pass a binding declared with `let mut` as `inout`. Passing an immutable binding is a compile-time error.
2. **No overlapping mutable views.** Within a single call, the same value cannot be passed twice as `inout`, nor as both `borrow` and `inout`.
3. **References don't escape.** A function cannot return a borrowed parameter — the view exists only for the call.

These rules prevent the data races and aliasing bugs that show up in unrestricted-mutation languages, without requiring a garbage collector.

## A Note on Reference Types

Gruel also has reference *types* spelled `Ref(T)` and `MutRef(T)`, with construction operators `&x` and `&mut x` (see [ADR-0062](@/learn/references/adrs/0062-reference-types.md)). These are the long-term direction for borrowing in Gruel and you may see them in newer code:

```gruel
fn touch(r: MutRef(i32)) -> i32 { 42 }   // parses, but can't yet write through r

fn main() -> i32 {
    let mut x: i32 = 0;
    touch(&mut x)
}
```

In the current implementation, reference types work as type-level pass-through — you can construct a reference and forward it — but reading or writing through one inside the callee is gated on a deref operator that is still future work. Until that lands, use `borrow` / `inout` as shown above when the function body actually needs to use the value.

Method receivers are the exception: `&self` and `&mut self` are sugar for `borrow self` and `inout self` and read-access through `&self` works today. See [Methods](@/learn/11-methods.md).
