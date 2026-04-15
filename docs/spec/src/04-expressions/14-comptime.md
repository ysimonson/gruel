+++
title = "Compile-Time Expressions"
weight = 14
template = "spec/page.html"
+++

# Compile-Time Expressions

{{ rule(id="4.14:1", cat="normative") }}

A compile-time expression is an expression marked with the `comptime` keyword that **MUST** be fully evaluated at compile time.

{{ rule(id="4.14:2", cat="normative") }}

```ebnf
comptime_expr = "comptime" "{" expression "}" ;
```

The expression inside a comptime block is evaluated during compilation. The following operations are supported within comptime blocks:

- Integer literals
- Boolean literals (`true`, `false`)
- Arithmetic operators (`+`, `-`, `*`, `/`, `%`)
- Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Logical operators (`&&`, `||`, `!`)
- Bitwise operators (`&`, `|`, `^`, `<<`, `>>`)

{{ rule(id="4.14:3", cat="normative") }}

A comptime expression can be used anywhere an expression is expected. The result of the comptime evaluation replaces the comptime block.

```gruel
fn main() -> i32 {
    let x: i32 = comptime { 21 * 2 };
    x
}
```

## Comptime Restrictions

{{ rule(id="4.14:4", cat="legality-rule") }}

It is a compile-time error if an expression inside a comptime block cannot be evaluated at compile time. This includes:

- References to runtime variables
- Function calls (except to comptime-evaluable functions in future versions)
- Operations that would panic at runtime

```gruel
fn main() -> i32 {
    let x = 10;
    comptime { x + 1 }  // ERROR: x cannot be known at compile time
}
```

## Comptime Parameters

{{ rule(id="4.14:5", cat="normative") }}

Function parameters can be marked with `comptime`, requiring the caller to provide a compile-time known value. The parameter's value is available as a compile-time constant within the function body.

```ebnf
parameter = [ "comptime" ] IDENT ":" type ;
```

Comptime parameters can have any type, including the special `type` type (see below).

```gruel
fn multiply(comptime n: i32, value: i32) -> i32 {
    n * value
}

fn main() -> i32 {
    multiply(6, 7)  // n is known at compile time
}
```

Comptime parameters enable monomorphization: each unique combination of comptime arguments creates a specialized version of the function.

The keyword `type` is a comptime-only type whose values are types themselves. A parameter of type `type` must be marked `comptime`.

```gruel
fn identity(comptime T: type, x: T) -> T {
    x
}

fn main() -> i32 {
    identity(i32, 42)
}
```

When a function has a `comptime T: type` parameter, occurrences of `T` in parameter types and return types are substituted with the concrete type at each call site.

{{ rule(id="4.14:6", cat="legality-rule") }}

It is a compile-time error to pass a runtime value to a comptime parameter.

```gruel
fn double(comptime n: i32) -> i32 { n * 2 }

fn main() -> i32 {
    let x = 21;
    double(x)  // ERROR: comptime parameter requires a compile-time known value
}
```

Type values cannot exist at runtime. It is a compile-time error to attempt to store a type value in a runtime variable.

```gruel
fn main() -> i32 {
    let t = comptime { i32 };  // ERROR: type values cannot exist at runtime
    0
}
```

## Anonymous Struct Types

{{ rule(id="4.14:7", cat="normative") }}

A comptime function that returns `type` can construct an anonymous struct type using the following syntax:

```ebnf
anon_struct_type = "struct" "{" struct_field { "," struct_field } "}" ;
struct_field = IDENT ":" type ;
```

```gruel
fn Point() -> type {
    struct { x: i32, y: i32 }
}

fn main() -> i32 {
    let P = Point();
    let p: P = P { x: 10, y: 32 };
    p.x + p.y
}
```

Anonymous structs can be parameterized using comptime type parameters:

```gruel
fn Pair(comptime T: type) -> type {
    struct { first: T, second: T }
}

fn main() -> i32 {
    let IntPair = Pair(i32);
    let p: IntPair = IntPair { first: 20, second: 22 };
    p.first + p.second
}
```

{{ rule(id="4.14:8", cat="normative") }}

Two anonymous struct types are structurally equal if and only if they have the same field names in the same order with the same types.

```gruel
fn make_point1() -> type { struct { x: i32, y: i32 } }
fn make_point2() -> type { struct { x: i32, y: i32 } }

fn main() -> i32 {
    let P1 = make_point1();
    let P2 = make_point2();
    let p1: P1 = P1 { x: 10, y: 20 };
    let p2: P2 = p1;  // OK: P1 and P2 are structurally equal
    p2.x + p2.y
}
```

Anonymous structs with different field names or different field types are different types and are not assignable to each other.

{{ rule(id="4.14:9", cat="legality-rule") }}

It is a compile-time error to define an anonymous struct type with no fields and no methods.

```gruel
fn empty() -> type {
    struct { }  // ERROR: empty struct
}
```

## Anonymous Struct Methods

{{ rule(id="4.14:10", cat="normative") }}

An anonymous struct type can include method definitions using the following syntax:

```ebnf
anon_struct_type = "struct" "{" [ struct_field { "," struct_field } ] [ method_def { method_def } ] "}" ;
method_def = "fn" IDENT "(" [ param { "," param } ] ")" [ "->" type ] block ;
```

Methods defined inside an anonymous struct type become methods on that struct type:

```gruel
fn Counter() -> type {
    struct {
        value: i32,

        fn increment(self) -> Self {
            Self { value: self.value + 1 }
        }

        fn get(self) -> i32 {
            self.value
        }
    }
}

fn main() -> i32 {
    let C = Counter();
    let c: C = C { value: 0 };
    let c2 = c.increment();
    c2.get()
}
```

{{ rule(id="4.14:11", cat="normative") }}

Inside an anonymous struct's method definitions, `Self` refers to the anonymous struct type being defined. `Self` can be used as a type annotation, in struct literal expressions, and as a return type.

```gruel
fn Pair(comptime T: type) -> type {
    struct {
        first: T,
        second: T,

        fn swap(self) -> Self {
            Self { first: self.second, second: self.first }
        }
    }
}
```

{{ rule(id="4.14:12", cat="normative") }}

Methods inside anonymous structs can access comptime parameters from the enclosing function:

```gruel
fn Array(comptime T: type, comptime N: i32) -> type {
    struct {
        len: i32,

        fn capacity(self) -> i32 {
            N  // Captured from enclosing comptime context
        }
    }
}
```

{{ rule(id="4.14:13", cat="normative") }}

Functions defined without a `self` parameter are associated functions, called using the `Type::function()` syntax:

```gruel
fn Point() -> type {
    struct {
        x: i32,
        y: i32,

        fn origin() -> Self {
            Self { x: 0, y: 0 }
        }
    }
}

fn main() -> i32 {
    let P = Point();
    let p = P::origin();
    p.x
}
```

{{ rule(id="4.14:14", cat="legality-rule") }}

It is a compile-time error to define two methods with the same name in an anonymous struct type.

{{ rule(id="4.14:15", cat="normative") }}

Two anonymous struct types are structurally equal if and only if they have:
1. The same field names in the same order with the same types, AND
2. The same method names with the same parameter types and return types

Method bodies do not affect structural equality—only signatures matter.

```gruel
fn A() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x } }
}

fn B() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x + 1 } }  // Same type as A()
}

fn C() -> type {
    struct { x: i32, fn get(self) -> i64 { self.x as i64 } }  // Different type (i64 vs i32)
}
```

## Comptime Blocks with Local State

{{ rule(id="4.14:16", cat="normative") }}

A comptime block can contain local variable declarations (`let`) and assignments. Variables declared inside a comptime block are only accessible within that block. The block evaluates all statements in order and returns the value of the final expression.

```gruel
fn main() -> i32 {
    comptime {
        let a = 20;
        let b = 22;
        a + b   // evaluates to 42 at compile time
    }
}
```

Mutable variables may be re-assigned within a comptime block:

```gruel
fn main() -> i32 {
    comptime {
        let mut x = 40;
        x = x + 2;
        x   // evaluates to 42 at compile time
    }
}
```

{{ rule(id="4.14:17", cat="normative") }}

A comptime block can contain `if` and `if`/`else` expressions. The condition must be a compile-time evaluable boolean. Both branches are compile-time evaluable expressions.

```gruel
fn main() -> i32 {
    comptime {
        let x = 10;
        if x > 5 { x * 4 + 2 } else { 0 }   // evaluates to 42 at compile time
    }
}
```
