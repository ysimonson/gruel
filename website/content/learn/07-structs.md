+++
title = "Structs"
weight = 7
template = "learn/page.html"
+++

# Structs

Structs let you create custom types by grouping related data together.

## Defining Structs

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };

    @dbg(origin.x);  // prints: 0
    @dbg(target.y);  // prints: 4

    0
}
```

## Structs in Functions

Pass structs to functions and return them:

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn distance_squared(p1: Point, p2: Point) -> i32 {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    dx * dx + dy * dy
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };

    let dist_sq = distance_squared(origin, target);
    @dbg(dist_sq);  // prints: 25 (distance is 5)

    dist_sq
}
```

## Nested Structs

Structs can contain other structs:

```gruel
struct Point {
    x: i32,
    y: i32,
}

struct Rectangle {
    origin: Point,
    width: i32,
    height: i32,
}

fn area(rect: Rectangle) -> i32 {
    rect.width * rect.height
}

fn main() -> i32 {
    let rect = Rectangle {
        origin: Point { x: 10, y: 20 },
        width: 100,
        height: 50,
    };

    @dbg(area(rect));          // prints: 5000
    @dbg(rect.origin.x);       // prints: 10

    0
}
```

## Mutable Struct Fields

If a struct variable is mutable, you can modify its fields:

```gruel
struct Counter {
    value: i32,
}

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value = c.value + 1;

    @dbg(c.value);  // prints: 2
    c.value
}
```

## Move Semantics

By default, structs *move* when assigned or passed to functions. After a move, the original variable can't be used:

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn use_point(p: Point) {
    @dbg(p.x);
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    use_point(p1);    // p1 moves here
    // use_point(p1); // ERROR: value already moved

    0
}
```

You cannot move a non-copy field out of a struct individually. To access individual fields as independent values, use destructuring (see below).

If you want a type to be copyable instead, mark it with `@derive(Copy)`:

```gruel
@derive(Copy)
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    let mut p2 = p1;  // p2 is a copy of p1

    p2.x = 100;

    @dbg(p1.x);  // prints: 1 (unchanged)
    @dbg(p2.x);  // prints: 100

    0
}
```

## Struct Destructuring

To break a struct into its individual fields, use a destructuring let binding. This consumes the struct and binds each field to a new variable:

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    let Point { x, y } = p;  // p is consumed

    x + y  // 42
}
```

All fields must be listed. Use `_` to discard a field you don't need:

```gruel
struct Pair {
    first: i32,
    second: i32,
}

fn main() -> i32 {
    let p = Pair { first: 42, second: 0 };
    let Pair { first, second: _ } = p;  // discard second

    first
}
```

You can rename fields during destructuring with `field: new_name`:

```gruel
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    let Point { x: px, y: py } = p;

    px * px + py * py  // 25
}
```

Destructuring is especially important for structs with non-copy fields, since you can't move individual fields out directly:

```gruel
struct Names {
    first: String,
    last: String,
}

fn greet(name: String) {
    @dbg(name);
}

fn main() -> i32 {
    let n = Names { first: "Ada", last: "Lovelace" };
    // greet(n.first);  // ERROR: cannot move field `first` out of `Names`

    let Names { first, last: _ } = n;  // destructure instead
    greet(first);  // OK

    0
}
```
