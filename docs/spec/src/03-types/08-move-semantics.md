+++
title = "Move Semantics"
weight = 8
+++

# Move Semantics

This section describes how values are moved and copied in Gruel.

## Value Categories

{{ rule(id="3.8:1", cat="normative") }}

Types in Gruel are categorized by how they behave when used:
- **Copy types** can be implicitly duplicated when used. Using a Copy type does not consume the original value.
- **Move types** (also called affine types) are consumed when used. After using a move type value, the original binding becomes invalid.

{{ rule(id="3.8:2", cat="normative") }}

The following types are Copy types:
- All integer types (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`)
- The boolean type (`bool`)
- The unit type (`()`)
- Enum types (all variants of an enum)
- Array types `[T; N]` where `T` is a Copy type

{{ rule(id="3.8:3", cat="normative") }}

User-defined struct types are move types by default. Using a struct value consumes it.

{{ rule(id="3.8:4", cat="example") }}

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is moved to q
    // p is no longer valid here
    q.x + q.y
}
```

## The `@copy` Directive

{{ rule(id="3.8:14", cat="normative") }}

A struct type **MAY** be declared as a Copy type using the `@copy` directive before the struct definition.

{{ rule(id="3.8:15", cat="syntax") }}

```ebnf
copy_struct = "@copy" struct_def ;
```

{{ rule(id="3.8:16", cat="normative") }}

A struct marked with `@copy` is a Copy type. Using a `@copy` struct value does not consume it; the value is implicitly duplicated.

{{ rule(id="3.8:17", cat="example") }}

```gruel
@copy
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is copied, not moved
    let r = p;      // p can be used again
    p.x + q.x + r.x // all three are valid
}
```

{{ rule(id="3.8:18", cat="legality-rule") }}

A `@copy` struct **MUST** contain only fields that are themselves Copy types. It is a compile-time error to mark a struct as `@copy` if any of its fields are move types.

{{ rule(id="3.8:19", cat="example") }}

```gruel
struct Inner { value: i32 }  // move type (no @copy)

@copy
struct Outer { inner: Inner }  // ERROR: field 'inner' has non-Copy type 'Inner'
```

{{ rule(id="3.8:20", cat="normative") }}

A `@copy` struct **MAY** contain fields of primitive Copy types (integers, booleans, unit), enum types, arrays of Copy types, or other `@copy` struct types.

{{ rule(id="3.8:21", cat="example") }}

```gruel
@copy
struct Point { x: i32, y: i32 }

@copy
struct Rect { top_left: Point, bottom_right: Point }  // OK: Point is @copy

fn main() -> i32 {
    let r = Rect {
        top_left: Point { x: 0, y: 0 },
        bottom_right: Point { x: 10, y: 10 }
    };
    let r2 = r;     // r is copied
    r.top_left.x    // r is still valid
}
```

## Linear Types

{{ rule(id="3.8:30", cat="normative") }}

A struct type **MAY** be declared as a linear type using the `linear` keyword before the struct definition.

{{ rule(id="3.8:31", cat="syntax") }}

```ebnf
linear_struct = "linear" "struct" IDENT "{" [ struct_fields ] "}" ;
```

{{ rule(id="3.8:32", cat="normative") }}

A linear type **MUST** be explicitly consumed. It is a compile-time error for a linear value to go out of scope without being consumed by a function call.

{{ rule(id="3.8:33", cat="normative") }}

A linear value is consumed when it is:
- Passed as an argument to a function (the function is the consumer)
- Returned from a function (the caller becomes responsible for consuming it)
- Destructured via a `let` destructuring binding (all fields are transferred)
- A field is accessed (the linear value is consumed by the access)

{{ rule(id="3.8:34", cat="example") }}

```gruel
linear struct MustUse { value: i32 }

fn consume(m: MustUse) -> i32 { m.value }

fn main() -> i32 {
    let m = MustUse { value: 42 };
    consume(m)  // OK: m is consumed
}
```

{{ rule(id="3.8:35", cat="legality-rule") }}

It is a compile-time error to allow a linear value to be implicitly dropped.

{{ rule(id="3.8:36", cat="example") }}

```gruel
linear struct MustUse { value: i32 }

fn main() -> i32 {
    let m = MustUse { value: 1 };  // ERROR: linear value dropped without being consumed
    0
}
```

{{ rule(id="3.8:37", cat="legality-rule") }}

A linear struct **MUST NOT** be marked with `@copy`. Linear types cannot be implicitly copied.

{{ rule(id="3.8:38", cat="example") }}

```gruel
@copy
linear struct Invalid { value: i32 }  // ERROR: linear types cannot be @copy
```

{{ rule(id="3.8:39", cat="informative") }}

Linear types are useful for:
- Resources that must be explicitly released (file handles, database transactions)
- Protocol enforcement (ensuring state machine transitions are completed)
- Results that must be checked (similar to `must_use` attributes)

## The `@handle` Directive

{{ rule(id="3.8:40", cat="normative") }}

A struct type **MAY** be declared as a handle type using the `@handle` directive before the struct definition. Handle types support explicit duplication via a `.handle()` method.

{{ rule(id="3.8:41", cat="syntax") }}

```ebnf
handle_struct = "@handle" struct_def ;
```

{{ rule(id="3.8:42", cat="normative") }}

A struct marked with `@handle` **MUST** provide a method named `handle` with the following signature:

```gruel
fn handle(self) -> T
```

where `T` is the handle struct type. It is a compile-time error to mark a struct with `@handle` if it does not provide this method.

{{ rule(id="3.8:43", cat="legality-rule") }}

The `handle` method **MUST** take exactly one parameter (`self` of the struct type) and **MUST** return the same struct type. It is a compile-time error if the method signature differs.

{{ rule(id="3.8:44", cat="example") }}

```gruel
@handle
struct Counter {
    count: i32,

    fn handle(self) -> Counter {
        Counter { count: self.count }
    }
}

fn main() -> i32 {
    let a = Counter { count: 1 };
    let b = a.handle();  // explicit duplication
    b.count
}
```

{{ rule(id="3.8:45", cat="normative") }}

Calling `.handle()` on a handle type does not consume the receiver and returns a new owned value. Both the original and the returned value are valid after the call.

{{ rule(id="3.8:46", cat="informative") }}

Handle types are useful for:
- Reference-counted types (Rc, Arc) where duplication increments the count
- Interned strings where duplication is cheap
- Shared resources where explicit duplication makes cost visible

{{ rule(id="3.8:47", cat="normative") }}

A `@copy` struct implicitly supports handle semantics. Any `@copy` type can be explicitly duplicated, although the `.handle()` method is not required.

{{ rule(id="3.8:48", cat="informative") }}

The difference between `@copy` and `@handle`:
- `@copy` types are duplicated implicitly when used
- `@handle` types require explicit `.handle()` calls for duplication
- `@copy` is appropriate for small, cheap-to-copy types (like `Point`)
- `@handle` is appropriate for types where duplication has visible cost (like reference-counted types)

{{ rule(id="3.8:49", cat="normative") }}

A linear struct **MAY** be marked with `@handle` if explicit duplication is meaningful (e.g., forking a transaction).

## Use After Move

{{ rule(id="3.8:5", cat="legality-rule") }}

It is a compile-time error to use a value that has been moved.

{{ rule(id="3.8:6", cat="example") }}

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is moved
    let r = p;      // ERROR: use of moved value 'p'
    0
}
```

{{ rule(id="3.8:7", cat="normative") }}

A value is considered moved when it is:
- Assigned to another variable
- Passed as an argument to a function
- Returned from a function

{{ rule(id="3.8:8", cat="example") }}

```gruel
struct Data { value: i32 }

fn consume(d: Data) -> i32 { d.value }

fn main() -> i32 {
    let d = Data { value: 42 };
    let result = consume(d);  // d is moved into the function
    // d is no longer valid here
    result
}
```

## Copy Types and Multiple Uses

{{ rule(id="3.8:9", cat="normative") }}

Copy types can be used multiple times without being consumed.

{{ rule(id="3.8:10", cat="example") }}

```gruel
fn main() -> i32 {
    let x = 42;
    let a = x;  // x is copied
    let b = x;  // x is copied again
    a + b       // 84
}
```

{{ rule(id="3.8:11", cat="normative") }}

Function parameters of Copy types receive a copy of the argument. Function parameters of move types receive ownership of the argument.

## Partial Moves (Field-Level Moves)

{{ rule(id="3.8:22", cat="legality-rule") }}

It is a compile-time error to move a non-Copy field out of a struct. To access non-Copy fields individually, the struct must be destructured using a `let` destructuring binding, which consumes the entire struct and binds all fields.

{{ rule(id="3.8:23", cat="example") }}

```gruel
struct Inner { x: i32 }
struct S { a: Inner, b: Inner }

fn consume(i: Inner) -> i32 { i.x }

fn main() -> i32 {
    let s = S { a: Inner { x: 1 }, b: Inner { x: 2 } };
    consume(s.a)   // ERROR: cannot move field `a` out of `S`
}
```

{{ rule(id="3.8:24", cat="informative") }}

This restriction eliminates partial moves — a value is either fully live or fully consumed. To access individual non-Copy fields, destructure the struct:

```gruel
struct Inner { x: i32 }
struct S { a: Inner, b: Inner }

fn consume(i: Inner) -> i32 { i.x }

fn main() -> i32 {
    let S { a, b } = S { a: Inner { x: 1 }, b: Inner { x: 2 } };
    consume(a)   // OK: a is now an independent value
    // b is dropped at scope exit
}
```

{{ rule(id="3.8:28", cat="normative") }}

Accessing Copy-type fields does not move them. Copy-type fields can be accessed any number of times without affecting the struct's move state.

{{ rule(id="3.8:29", cat="example") }}

```gruel
struct S { a: i32, b: i32 }

fn main() -> i32 {
    let s = S { a: 1, b: 2 };
    let x = s.a;   // s.a is copied
    let y = s.a;   // s.a can be copied again
    let z = s.b;   // s.b is also valid
    x + y + z      // 4
}
```

## Shadowing and Moves

{{ rule(id="3.8:12", cat="normative") }}

Shadowing a variable does not prevent it from being moved. A moved variable remains invalid even if a new variable with the same name is introduced in an inner scope.

{{ rule(id="3.8:13", cat="example") }}

```gruel
struct Data { value: i32 }

fn main() -> i32 {
    let d = Data { value: 1 };
    let x = d;  // d is moved
    {
        let d = Data { value: 2 };  // New 'd' shadows, but doesn't restore old 'd'
        d.value
    }
    // Original 'd' is still invalid here
}
```
