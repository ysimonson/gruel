+++
title = "Interfaces and Derives"
weight = 21
template = "learn/page.html"
+++

# Interfaces and Derives

Interfaces describe a set of methods a type must provide. Derives bundle method definitions you can splice into a struct without typing them out by hand. Together they're the building blocks for polymorphism in Gruel.

## Interfaces

An interface is a named list of method signatures with no bodies:

```gruel
interface Greeter {
    fn greet(self);
}

interface Shape {
    fn area(self) -> i32;
    fn perimeter(self) -> i32;
}
```

Interface bodies declare *signatures only* — bodies aren't allowed. Conformance is structural: a type satisfies an interface if it has every required method. There's no `impl` keyword and no separate "implementation" item — if your struct has the right methods, it conforms.

```gruel
interface Shape {
    fn area(self) -> i32;
}

struct Square {
    side: i32,

    fn area(self) -> i32 { self.side * self.side }
}
// Square structurally conforms to Shape.
```

## Generic Functions Constrained by an Interface

A function can declare a comptime type parameter constrained by an interface. The compiler monomorphizes one specialization per concrete type argument and checks conformance at the call site:

```gruel
interface Greeter {
    fn greet(self);
}

struct Foo {
    fn greet(self) {}
}

fn use_greeter(comptime T: Greeter, t: T) {
    t.greet();
}

fn main() -> i32 {
    use_greeter(Foo, Foo {});
    0
}
```

If the type passed in doesn't have the required methods, you get a compile error at the call site — not deep inside the generic body.

## Derives

A `derive` is a method *template*: a list of methods that can be attached to a struct via `@derive(Name)`. Instead of writing the same boilerplate on every struct, you write it once in a derive item and stamp it onto each host:

```gruel
derive Tagger {
    fn tag(self) -> i32 { 7 }
}

@derive(Tagger)
struct Buffer {
    capacity: i32,
}

fn main() -> i32 {
    let b = Buffer { capacity: 0 };
    b.tag()  // 7
}
```

When the compiler sees `@derive(Tagger)` on `Buffer`, it splices the methods from `Tagger` into `Buffer`'s method list. After splicing, those methods are indistinguishable from methods defined directly on `Buffer`.

A struct may stack multiple derives:

```gruel
@derive(Copy)
@derive(Tagger)
struct Box { inner: i32 }
```

If two derives — or a derive and a hand-written method — would attach the same name, the compiler rejects the conflict.

Inside a derive body the host type isn't known yet, so direct field access on `self` is not allowed. Use `@field(self, "name")` and the comptime reflection intrinsics (`@type_info`, `@ownership`) to write methods that adapt to the host.

## Built-in Derives: `Copy` and `Drop`

Two built-in interfaces describe ownership posture:

```gruel
interface Drop { fn drop(self); }
interface Copy { fn copy(self: Ref(Self)) -> Self; }
```

Every struct or enum has exactly one of three postures, determined by which interface it conforms to:

| Conforms to `Copy`? | Has `fn drop`? | Posture |
|---|---|---|
| yes | no  | **Copy** — values may be implicitly duplicated |
| no  | yes | **Affine** — values move on use, dropped at end of scope |
| no  | no  | **Affine** with synthesized recursive drop |

`Copy` and `Drop` are mutually exclusive: a type that's both copyable and has a destructor would run its destructor on every copy, releasing the same resource many times.

The `linear` keyword is a separate opt-in posture that requires explicit consumption (see [Linear Types](@/learn/16-linear-types.md)).

### `@derive(Copy)`

The standard library defines a `Copy` derive that synthesizes a `fn copy` body via comptime reflection. Use it on any struct whose fields are all `Copy`:

```gruel
@derive(Copy)
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    let a = p;     // p is duplicated, not moved
    let b = p;     // still fine
    a.x + b.y      // 7
}
```

If any field of the struct isn't `Copy`, `@derive(Copy)` produces a compile-time error.

### Custom Destructors

A struct that needs cleanup gets a `Drop` posture by defining `fn drop(self)` (see [Destructors](@/learn/destructors.md)). You don't need to write `@derive(Drop)` — the presence of the method is what makes it conform to the interface.

## When to Use What

| You want… | Reach for… |
|----------|-----------|
| A type that can be freely duplicated | `@derive(Copy)` |
| A type that owns a resource | Inline `fn drop(self)` |
| Boilerplate methods reused across structs | A `derive` item + `@derive(Name)` |
| Generic code that takes any type with method `m` | An `interface` + `comptime T: I` |

For the formal rules, see ADRs [0056](@/learn/references/adrs/0056-structural-interfaces.md), [0058](@/learn/references/adrs/0058-comptime-derives.md), and [0059](@/learn/references/adrs/0059-drop-and-copy-interfaces.md).
