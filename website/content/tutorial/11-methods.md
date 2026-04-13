+++
title = "Methods"
weight = 11
template = "tutorial/page.html"
+++

# Methods

Methods let you define functions that belong to a type, called with dot syntax. They keep related operations grouped with the data they operate on.

## Defining Methods

Use `impl` blocks to add methods to a struct:

```gruel
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn distance_squared(self) -> i32 {
        self.x * self.x + self.y * self.y
    }
}

fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    let d = p.distance_squared();
    @dbg(d);  // prints: 25
    d
}
```

The first parameter `self` receives the struct value. Methods are called with dot syntax: `p.distance_squared()`.

## Methods with Parameters

Methods can take additional parameters beyond `self`:

```gruel
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn translate(self, dx: i32, dy: i32) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }
}

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p.translate(3, 4);

    @dbg(q.x);  // prints: 4
    @dbg(q.y);  // prints: 6

    q.x
}
```

## Mutating Methods with `inout self`

To modify a struct through a method, use `inout self`. Unlike free functions (where the caller writes `inout` at the call site), method receivers are implicit — you just call the method normally on a mutable variable:

```gruel
struct Counter {
    value: i32,
}

impl Counter {
    fn increment(inout self) {
        self.value = self.value + 1;
    }

    fn reset(inout self) {
        self.value = 0;
    }
}

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.increment();
    c.increment();
    c.increment();

    @dbg(c.value);  // prints: 3

    c.reset();

    @dbg(c.value);  // prints: 0

    0
}
```

## Read-Only Methods with `borrow self`

Use `borrow self` when a method only needs to read, not modify, the struct. This lets you call the method without consuming the value:

```gruel
struct Rectangle {
    width: i32,
    height: i32,
}

impl Rectangle {
    fn area(borrow self) -> i32 {
        self.width * self.height
    }

    fn perimeter(borrow self) -> i32 {
        2 * (self.width + self.height)
    }
}

fn main() -> i32 {
    let r = Rectangle { width: 6, height: 4 };

    @dbg(r.area());       // prints: 24
    @dbg(r.perimeter());  // prints: 20

    // r is still usable because borrow self didn't consume it
    r.area()
}
```

## Associated Functions

Functions inside `impl` blocks without a `self` parameter are associated functions. They're called with `Type::function()` syntax and are useful for constructors:

```gruel
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }

    fn new(x: i32, y: i32) -> Point {
        Point { x: x, y: y }
    }
}

fn main() -> i32 {
    let origin = Point::origin();
    let p = Point::new(3, 4);

    @dbg(origin.x);  // prints: 0
    @dbg(p.x);       // prints: 3

    0
}
```

## Chaining Methods

Methods that return the struct can be chained:

```gruel
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn translate(self, dx: i32, dy: i32) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }

    fn scale(self, factor: i32) -> Point {
        Point { x: self.x * factor, y: self.y * factor }
    }
}

fn main() -> i32 {
    let p = Point::new(1, 2).translate(3, 4).scale(2);
    // (1+3)*2=8, (2+4)*2=12
    @dbg(p.x);  // prints: 8
    @dbg(p.y);  // prints: 12
    p.x
}

impl Point {
    fn new(x: i32, y: i32) -> Point {
        Point { x: x, y: y }
    }
}
```

## Summary

| Parameter | Syntax | Use When |
|-----------|--------|----------|
| By value | `self` | Method consumes or transforms the value |
| Mutable | `inout self` | Method modifies the struct in place |
| Read-only | `borrow self` | Method only reads from the struct |

Methods keep related operations next to the data they work on, making code easier to discover and organize.
