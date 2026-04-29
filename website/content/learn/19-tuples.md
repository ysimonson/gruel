+++
title = "Tuples and Destructuring"
weight = 19
template = "learn/page.html"
+++

# Tuples and Destructuring

A tuple groups a fixed number of values, possibly of different types, into a single value. Tuples are useful for returning multiple values from a function, for ad-hoc grouping, and as keys in patterns.

## Constructing Tuples

```gruel
fn main() -> i32 {
    let pair: (i32, i32) = (10, 32);
    pair.0 + pair.1  // 42
}
```

Access individual elements with `.0`, `.1`, `.2`, and so on. The element types and order are part of the tuple's type — `(i32, bool)` and `(bool, i32)` are different types.

Tuples may mix element types:

```gruel
fn main() -> i32 {
    let t = (7, true, 35);
    if t.1 { t.0 + t.2 } else { 0 }   // 42
}
```

A single-element tuple needs a trailing comma so the parser doesn't treat the parentheses as grouping:

```gruel
let one_tuple: (i32,) = (42,);
let just_grouped: i32 = (42);   // not a tuple
```

The unit type `()` is *not* a zero-tuple — it's its own type representing "no value."

## Returning Multiple Values

A tuple is the natural way to return more than one value from a function:

```gruel
fn divmod(a: i32, b: i32) -> (i32, i32) {
    (a / b, a % b)
}

fn main() -> i32 {
    let r = divmod(17, 5);
    @dbg(r.0);  // 3
    @dbg(r.1);  // 2
    0
}
```

## Destructuring in `let`

Rather than indexing into a tuple by hand, destructure it on assignment:

```gruel
fn divmod(a: i32, b: i32) -> (i32, i32) {
    (a / b, a % b)
}

fn main() -> i32 {
    let (q, r) = divmod(17, 5);
    @dbg(q);  // 3
    @dbg(r);  // 2
    0
}
```

Use `_` to ignore an element you don't need:

```gruel
let (a, _, c) = (40, 999, 2);
```

The same pattern works on structs:

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    let Point { x, y } = p;
    x + y  // 42
}
```

You must name every field; `_` skips the value but the name still appears in the pattern. Destructuring consumes the source value just like passing it to a function — `p` is no longer usable after `let Point { x, y } = p`.

## Patterns in `match`

Tuple and struct patterns also work in `match`, and they nest:

```gruel
fn classify(point: (i32, i32)) -> i32 {
    match point {
        (0, 0) => 0,            // origin
        (0, _) => 1,            // on the y-axis
        (_, 0) => 2,            // on the x-axis
        (_, _) => 3,            // somewhere else
    }
}
```

Combined with enum patterns from the [Enums](@/learn/08-enums.md) chapter, this gives you a single uniform way to take apart compound values — tuples, structs, and enum variants — at any depth.
