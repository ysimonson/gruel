+++
title = "References"
weight = 9
template = "learn/page.html"
+++

# References

When you read code, you want to understand what it does without tracing through every function. Gruel makes mutation visible at the call site by requiring you to spell out when a function is allowed to read or write through a borrowed value.

## Reading Code at a Glance

Look at this code:

```gruel
fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(&mut values);
    values[0]
}
```

Without seeing `double_all`'s definition, you already know it modifies `values`. The `&mut` operator at the call site says: this function is being given mutable access to my data.

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

Gruel has two reference types:

- `Ref(T)` — read-only access to a `T`. Construct with `&value`.
- `MutRef(T)` — read/write access to a `T`. Construct with `&mut value`.

The receiving function declares which kind of reference it expects, and the caller writes the matching operator.

### `Ref(T)`: Read Without Copying

Use `Ref(T)` when a function only needs to read:

```gruel
fn sum_array(arr: Ref([i32; 5])) -> i32 {
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
    let sum = sum_array(&numbers);
    @dbg(sum);  // prints: 15
    sum
}
```

A `Ref(T)` parameter is read-only — the function can't change the data and the original binding is still usable after the call.

### `MutRef(T)`: Modify in Place

Use `MutRef(T)` when a function needs to modify its argument:

```gruel
fn double_all(arr: MutRef([i32; 3])) {
    let mut i: usize = 0;
    while i < 3 {
        arr[i] = arr[i] * 2;
        i = i + 1;
    }
}

fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(&mut values);

    @dbg(values[0]);  // prints: 20
    @dbg(values[1]);  // prints: 40
    @dbg(values[2]);  // prints: 60

    values[0]
}
```

Both the function signature (`MutRef(...)`) and the call site (`&mut ...`) make the mutation visible. There's no way to accidentally miss it.

## Combining Them

A function can mix references:

```gruel
fn copy_into(src: Ref([i32; 3]), dst: MutRef([i32; 3])) {
    let mut i: usize = 0;
    while i < 3 {
        dst[i] = src[i];
        i = i + 1;
    }
}

fn main() -> i32 {
    let source = [1, 2, 3];
    let mut dest = [0, 0, 0];

    copy_into(&source, &mut dest);

    @dbg(dest[0]);  // prints: 1
    @dbg(dest[1]);  // prints: 2
    @dbg(dest[2]);  // prints: 3

    0
}
```

Reading the call site, you immediately know: `source` is read, `dest` is modified.

## With Structs

References work with any type:

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn translate(p: MutRef(Point), dx: i32, dy: i32) {
    p.x = p.x + dx;
    p.y = p.y + dy;
}

fn print_point(p: Ref(Point)) {
    @dbg(p.x);
    @dbg(p.y);
}

fn main() -> i32 {
    let mut pos = Point { x: 10, y: 20 };

    print_point(&pos);          // prints: 10, 20
    translate(&mut pos, 5, -3);
    print_point(&pos);          // prints: 15, 17

    0
}
```

## Aliasing Rules

References are scope-bound, and the compiler enforces a few rules to keep them safe:

1. **`&mut` requires `let mut`.** You can only construct `&mut x` when `x` was bound with `let mut`. Building a mutable reference to an immutable binding is a compile-time error.
2. **No overlapping mutable references.** Within a single call, the same value cannot be passed twice as `&mut`, nor as both `&` and `&mut`.
3. **References cannot escape.** A function cannot return a `Ref(T)` or `MutRef(T)` — references only live for the call.

```gruel
fn bad(_a: MutRef(i32), _b: MutRef(i32)) {}

fn main() -> i32 {
    let mut x: i32 = 0;
    bad(&mut x, &mut x);  // error: x is mutably borrowed twice
    0
}
```

These rules prevent the data races and aliasing bugs that show up in unrestricted-mutation languages, without requiring a garbage collector.

## Methods

Method receivers use the same syntax as any other parameter: `self: Ref(Self)` for read-only access and `self: MutRef(Self)` for mutable access. See [Methods](@/learn/11-methods.md) for the full picture.
