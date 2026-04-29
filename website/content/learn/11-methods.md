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

## `self` Consumes the Receiver

By default, calling a method moves the receiver — after the call, the original binding is no longer usable. This matches the move semantics of regular function arguments:

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

This works well for transformations: each call returns a new value. The shadowing rebinding (`let c = c.incremented()`) keeps the surface name in scope.

## Calling a Method Many Times: `@derive(Copy)`

When a type is small and stateless, mark it `@derive(Copy)` so values are duplicated rather than moved when used:

```gruel
@derive(Copy)
struct Rectangle {
    width: i32,
    height: i32,

    fn area(self) -> i32 {
        self.width * self.height
    }

    fn perimeter(self) -> i32 {
        2 * (self.width + self.height)
    }
}

fn main() -> i32 {
    let r = Rectangle { width: 6, height: 4 };

    @dbg(r.area());       // prints: 24
    @dbg(r.perimeter());  // prints: 20

    // r is still usable because Rectangle is Copy
    r.area()
}
```

A Copy type can have all its fields read freely, methods called any number of times, and instances passed to functions without thinking about ownership. Most types with only primitive fields are good candidates for `@derive(Copy)`. See [Interfaces and Derives](@/learn/21-interfaces.md) for more on derive.

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

    fn area(self) -> i32 {
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

## A Note on `&self` and `&mut self`

Gruel's grammar accepts `&self` and `&mut self` as method receivers (sugar for `self: Ref(Self)` and `self: MutRef(Self)`; see [ADR-0062](@/learn/references/adrs/0062-reference-types.md)). They will eventually allow you to call multiple methods on a non-Copy value without consuming it. In the current implementation they parse but the borrow-checker still treats the call as a move, and method bodies cannot yet write through `&mut self`. For now, prefer `@derive(Copy)` when you need repeated method calls, and write transformations as `fn name(self) -> Self` that return new values.

## Summary

Methods live inline in struct or enum bodies. By default `self` consumes the receiver, so methods are typically transformations that return a new value. Use `@derive(Copy)` to opt into duplication when calling many methods on the same value. Functions without a `self` parameter become associated functions, called with `Type::name()`.
