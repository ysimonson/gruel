+++
title = "Methods"
weight = 11
template = "learn/page.html"
+++

# Methods

Methods let you define functions that belong to a type, called with dot syntax. They keep related operations grouped with the data they operate on.

## Defining Methods

In Gruel, methods are defined **inside** the struct body, alongside its fields:

```gruel
struct Point {
    x: i32,
    y: i32,

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

The first parameter `self` is the receiver. Methods are called with dot syntax: `p.distance_squared()`.

## Receiver Modes

The `self` parameter has three forms, mirroring how arguments work in regular function signatures:

| Receiver | Meaning | Use when |
|----------|---------|----------|
| `self` | The method takes ownership of the value | The method consumes or transforms the value |
| `self: Ref(Self)` | Read-only reference | The method only reads from the value |
| `self: MutRef(Self)` | Mutable reference | The method modifies the value in place |

`Ref` and `MutRef` here are the same reference types introduced in [References](@/learn/09-borrow-and-inout.md), just applied to the receiver. `Self` refers to the enclosing type — inside `struct Counter { ... }`, writing `Self` is the same as writing `Counter`.

### Read-Only Methods (`self: Ref(Self)`)

Use `Ref(Self)` when a method only needs to read. The caller's value remains usable after the call:

```gruel
struct Rectangle {
    width: i32,
    height: i32,

    fn area(self: Ref(Self)) -> i32 {
        self.width * self.height
    }

    fn perimeter(self: Ref(Self)) -> i32 {
        2 * (self.width + self.height)
    }
}

fn main() -> i32 {
    let r = Rectangle { width: 6, height: 4 };

    @dbg(r.area());       // prints: 24
    @dbg(r.perimeter());  // prints: 20

    // r is still usable — Ref(Self) didn't consume it
    r.area()
}
```

### Mutating Methods (`self: MutRef(Self)`)

Use `MutRef(Self)` when a method modifies the struct. Unlike free functions (where the caller writes `&mut` at the call site), method receivers are implicit — call the method on a `let mut` binding:

```gruel
struct Counter {
    value: i32,

    fn increment(self: MutRef(Self)) {
        self.value = self.value + 1;
    }

    fn reset(self: MutRef(Self)) {
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

### By-Value (`self`)

A bare `self` consumes the receiver. After the call, the original binding is no longer usable. This form is useful for transformations that produce a new value:

```gruel
struct Counter {
    value: i32,

    fn incremented(self) -> Counter {
        Counter { value: self.value + 1 }
    }
}

fn main() -> i32 {
    let c = Counter { value: 0 };
    let c = c.incremented().incremented().incremented();

    @dbg(c.value);  // prints: 3
    c.value
}
```

## Associated Functions

Functions inside a struct body without a `self` parameter are *associated functions*. They're called with `Type::function()` syntax and are typically used for constructors:

```gruel
struct Point {
    x: i32,
    y: i32,

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

## Method Chaining

Methods that return the struct can be chained:

```gruel
struct Point {
    x: i32,
    y: i32,

    fn new(x: i32, y: i32) -> Point {
        Point { x: x, y: y }
    }

    fn translated(self, dx: i32, dy: i32) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }

    fn scaled(self, factor: i32) -> Point {
        Point { x: self.x * factor, y: self.y * factor }
    }
}

fn main() -> i32 {
    let p = Point::new(1, 2).translated(3, 4).scaled(2);
    // (1+3)*2=8, (2+4)*2=12
    @dbg(p.x);  // prints: 8
    @dbg(p.y);  // prints: 12
    p.x
}
```

## Methods on Enums

Enums use the same syntax — methods go inside the enum body:

```gruel
enum Shape {
    Circle(i32),
    Square(i32),

    fn area(self: Ref(Self)) -> i32 {
        match self {
            Shape::Circle(r) => 3 * r * r,   // close enough
            Shape::Square(s) => s * s,
        }
    }
}

fn main() -> i32 {
    let s = Shape::Square(5);
    s.area()  // 25
}
```

## Summary

Methods live inline in struct or enum bodies. The receiver is one of `self`, `self: Ref(Self)`, or `self: MutRef(Self)`, depending on whether the method consumes, reads, or mutates the value. Functions without a `self` parameter become associated functions, called with `Type::name()`.
