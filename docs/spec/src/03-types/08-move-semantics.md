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

## The `@derive(Copy)` Directive

{{ rule(id="3.8:14", cat="normative") }}

A struct type **MAY** be declared as a Copy type using the `@derive(Copy)`
directive before the struct definition (ADR-0059). `Copy` is a
compiler-recognized structural interface (see §6.5) whose method shape is
`fn copy(self: Ref(Self)) -> Self`.

{{ rule(id="3.8:15", cat="syntax") }}

```ebnf
copy_struct = "@derive(Copy)" struct_def ;
```

{{ rule(id="3.8:16", cat="normative") }}

A struct that conforms to `Copy` is a Copy type. Using a Copy struct value
does not consume it; the value is implicitly duplicated.

{{ rule(id="3.8:17", cat="example") }}

```gruel
@derive(Copy)
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is copied, not moved
    let r = p;      // p can be used again
    p.x + q.x + r.x // all three are valid
}
```

{{ rule(id="3.8:18", cat="legality-rule") }}

A struct marked with `@derive(Copy)` **MUST** contain only fields that are
themselves Copy types. It is a compile-time error if any field has a type
that does not conform to `Copy`.

{{ rule(id="3.8:19", cat="example") }}

```gruel
struct Inner { value: i32 }  // move type (no @derive(Copy))

@derive(Copy)
struct Outer { inner: Inner }  // ERROR: field 'inner' has non-Copy type 'Inner'
```

{{ rule(id="3.8:20", cat="normative") }}

A `@derive(Copy)` struct **MAY** contain fields of primitive Copy types
(integers, booleans, unit), enum types, arrays of Copy types, or other
`@derive(Copy)` struct types.

{{ rule(id="3.8:21", cat="example") }}

```gruel
@derive(Copy)
struct Point { x: i32, y: i32 }

@derive(Copy)
struct Rect { top_left: Point, bottom_right: Point }  // OK: Point is Copy

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

A linear struct **MUST NOT** be marked with `@derive(Copy)`. Linear types cannot be implicitly copied.

{{ rule(id="3.8:38", cat="example") }}

```gruel
@derive(Copy)
linear struct Invalid { value: i32 }  // ERROR: linear struct cannot be marked `@derive(Copy)`
```

{{ rule(id="3.8:39", cat="informative") }}

Linear types are useful for:
- Resources that must be explicitly released (file handles, database transactions)
- Protocol enforcement (ensuring state machine transitions are completed)
- Results that must be checked (similar to `must_use` attributes)

## The `Handle` Interface

{{ rule(id="3.8:40", cat="normative") }}

`Handle` is a compiler-recognized structural interface (ADR-0075). A type conforms to `Handle` when it provides a method named `handle` with the signature `fn handle(self: Ref(Self)) -> Self`. Conformance enables explicit duplication via a `.handle()` call: the receiver is borrowed and a freshly owned value is returned.

{{ rule(id="3.8:41", cat="syntax") }}

There is no directive for `Handle`. The interface is opted into by defining the conforming method directly on the struct or enum:

```ebnf
handle_method = "fn" "handle" "(" "borrow" "self" ")" "->" "Self" block ;
```

{{ rule(id="3.8:42", cat="normative") }}

A method named `handle` with the receiver mode `self: Ref(Self)`, no other parameters, and return type `Self` makes the host type conform to `Handle`. A method named `handle` with any other shape does **NOT** make the host type conform — it is an ordinary method, and `@implements(T, Handle)` returns `false` for that type.

{{ rule(id="3.8:43", cat="legality-rule") }}

It is a compile-time error to call `.handle()` on a value whose type does not have a `handle` method.

{{ rule(id="3.8:44", cat="example") }}

```gruel
struct Counter {
    count: i32,

    fn handle(self: Ref(Self)) -> Counter {
        Counter { count: self.count }
    }
}

fn main() -> i32 {
    let a = Counter { count: 1 };
    let b = a.handle();  // explicit duplication
    a.count + b.count    // a is still valid
}
```

{{ rule(id="3.8:45", cat="normative") }}

Calling `.handle()` on a `Handle` type does not consume the receiver and returns a new owned value. Both the original and the returned value are valid after the call. This is enforced by the `self: Ref(Self)` receiver: the type system treats `.handle()` as a borrow, not a move.

{{ rule(id="3.8:46", cat="informative") }}

`Handle` is useful for:
- Reference-counted types (`Rc`, `Arc`) where duplication increments the count
- Interned strings where duplication is cheap
- Shared resources where explicit duplication makes cost visible

{{ rule(id="3.8:47", cat="normative") }}

A `Copy` struct implicitly supports handle-like semantics: any `Copy` value can be duplicated by use, with no `.handle()` call required. `Copy` types are not required to conform to `Handle`.

{{ rule(id="3.8:48", cat="informative") }}

The difference between `Copy` and `Handle`:
- `Copy` types are duplicated implicitly when used
- `Handle` types require explicit `.handle()` calls for duplication
- `Copy` is appropriate for small, cheap-to-copy types (like `Point`)
- `Handle` is appropriate for types where duplication has visible cost (like reference-counted types)

`Handle` shares its signature shape with `Clone` (both are `fn name(self: Ref(Self)) -> Self`); they remain distinct interfaces because `Handle` is permitted on linear types whereas `Clone` is not (§3.8:49).

{{ rule(id="3.8:49", cat="normative") }}

A linear struct **MAY** conform to `Handle`. Explicit duplication of a linear value is the canonical use case for `Handle` (e.g., forking a transaction).

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

## `Drop` and `Copy` as Interfaces (ADR-0059)

{{ rule(id="3.8:60", cat="normative") }}

Gruel's three ownership postures are mediated by two compiler-recognized
structural interfaces: `Copy` (`fn copy(self: Ref(Self)) -> Self`) and `Drop`
(`fn drop(self)`). Conformance to these interfaces is computed by the
compiler — built-in types acquire conformance through synthetic rules,
user types via `@derive(Copy)` or by defining the corresponding inline
method.

{{ rule(id="3.8:61", cat="normative") }}

For every struct or enum `T`:

- `T` conforms to `Copy` iff `T` is not `linear` and every constituent
  field is itself `Copy`.
- `T` conforms to `Drop` iff `T` is not `linear` and `T` does not conform
  to `Copy`. Affine types always conform to `Drop`; their drop body is
  either user-written via `fn drop(self)` (ADR-0053) or the compiler's
  recursive field-drop synthesis.

{{ rule(id="3.8:62", cat="legality-rule") }}

`Copy` and `Drop` are mutually exclusive: a single type **MUST NOT**
conform to both. `@derive(Copy)` on a struct that declares `fn drop(self)`
is rejected at the declaration site. Linear types conform to neither.

{{ rule(id="3.8:63", cat="informative") }}

Generic code may constrain on these interfaces directly:
`fn process(comptime T: Copy, t: T)` accepts any `Copy` type and rejects
non-conforming types at the call site. This is the same conformance
machinery as user-defined interfaces (§6.5).

## `Clone` Interface (ADR-0065)

{{ rule(id="3.8:70", cat="normative") }}

Gruel exposes a third compiler-recognized structural interface, `Clone`
(`fn clone(self: Ref(Self)) -> Self`). `Clone` formalizes "explicit deep
duplication" for affine types and is the single conformance every
collection method, generic constraint, and built-in clone helper resolves
against.

{{ rule(id="3.8:71", cat="normative") }}

For every type `T`:

- `T` conforms to `Clone` iff `T` is not `linear` and any of the
  following holds:
  - `T` conforms to `Copy` (the synthesized `clone` is the bitwise copy);
  - `T` is a built-in type whose registered method set contains a
    `clone` method (e.g. `String`); or
  - `T` is a struct or enum that defines a method with the signature
    `fn clone(self: Ref(Self)) -> Self` (written inline, spliced via
    `@derive(Clone)`, or hand-written in an extension block).

{{ rule(id="3.8:72", cat="legality-rule") }}

Linear types **MUST NOT** conform to `Clone`. The conformance check
unconditionally rejects them; `@derive(Clone)` on a `linear` declaration
is also rejected at the declaration site.

{{ rule(id="3.8:73", cat="informative") }}

Generic code may constrain on `Clone` exactly as on `Copy` or `Drop`:
`fn duplicate(comptime T: Clone, x: T) -> T { x.clone() }` accepts any
type whose conformance check passes and rejects non-conforming types at
the call site.

## Canonical `Option(T)` (ADR-0065)

{{ rule(id="3.8:80", cat="normative") }}

The compiler unconditionally registers a canonical `Option(T)` generic enum
in every compilation. The definition is equivalent to

```gruel
fn Option(comptime T: type) -> type {
    enum { Some(T), None }
}
```

and is available without any `import` or `use` directive. User code **MUST
NOT** redefine the name `Option`; doing so is a duplicate-definition error
at the redefinition site.

{{ rule(id="3.8:81", cat="informative") }}

`Option(T)` flows through the standard enum-with-data machinery (§4.7).
Pattern matching, exhaustiveness checks, and codegen do not special-case
it. Future ADRs may introduce layout optimizations (e.g. null-pointer-as
-`None` for `Option(Ptr(T))`); v1 ships the naive `{ tag, payload }`
layout.

{{ rule(id="3.8:82", cat="normative") }}

`Option(T)` ships with the following methods, defined in the prelude:

- `fn is_some(self) -> bool` — true iff the receiver is `Some`.
- `fn is_none(self) -> bool` — true iff the receiver is `None`.
- `fn unwrap(self) -> T` — returns the contained value, or panics if
  `None`.
- `fn unwrap_or(self, default: T) -> T` — returns the contained value
  if `Some`, otherwise returns `default`.

Each method consumes the receiver. Because `Option(T)` is treated as
`Copy` (§3.8:2 — all enum types), receivers are implicitly duplicated at
the call site, so a name remains usable after a query method.

The companion canonical type `Result(T, E)` is documented in §3.13.
