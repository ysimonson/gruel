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
- Calls to generic functions (functions with `comptime T: type` parameters) in a non-generic comptime context
- Operations that would overflow or panic at runtime
- System calls, external functions, or raw pointer dereferences

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

## Anonymous Enum Types

{{ rule(id="4.14:30", cat="normative") }}

A comptime function that returns `type` can construct an anonymous enum type using the following syntax:

```ebnf
anon_enum_type = "enum" "{" enum_variant { "," enum_variant } [ method_def { method_def } ] "}" ;
enum_variant = IDENT [ "(" type { "," type } ")" ] [ "{" struct_field { "," struct_field } "}" ] ;
```

Anonymous enums support unit variants, tuple variants, and struct variants, matching the same variant forms as named enums.

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}

fn main() -> i32 {
    let Opt = Option(i32);
    let x = Opt::Some(42);
    match x {
        Opt::Some(v) => v,
        Opt::None => 0,
    }
}
```

{{ rule(id="4.14:31", cat="normative") }}

Two anonymous enum types are structurally equal if and only if they have the same variant names in the same order with the same field types, and the same method signatures.

{{ rule(id="4.14:32", cat="legality-rule") }}

It is a compile-time error to define an anonymous enum type with no variants.

{{ rule(id="4.14:33", cat="normative") }}

An anonymous enum type can include method definitions. Inside these methods, `Self` refers to the anonymous enum type being defined. `Self` can be used in type annotations, return types, and in variant construction and pattern matching using the `Self::Variant` syntax.

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,

        fn unwrap_or(self, default: T) -> T {
            match self {
                Self::Some(v) => v,
                Self::None => default,
            }
        }

        fn none() -> Self {
            Self::None
        }
    }
}
```

{{ rule(id="4.14:34", cat="normative") }}

Methods inside anonymous enums can access comptime parameters from the enclosing function, just like anonymous struct methods.

{{ rule(id="4.14:35", cat="legality-rule") }}

It is a compile-time error to define two methods with the same name in an anonymous enum type.

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

## Comptime Loops

{{ rule(id="4.14:18", cat="normative") }}

A comptime block can contain `while` loops. The condition and body must be compile-time evaluable. The loop is unrolled at compile time.

```gruel
fn main() -> i32 {
    comptime {
        let mut sum = 0;
        let mut i = 1;
        while i <= 9 {
            sum = sum + i;
            i = i + 1;
        }
        sum   // evaluates to 45 at compile time
    }
}
```

{{ rule(id="4.14:19", cat="normative") }}

A comptime block can contain `loop` expressions. The body must be compile-time evaluable. A `break` statement exits the loop.

```gruel
fn main() -> i32 {
    comptime {
        let mut x = 0;
        loop {
            x = x + 1;
            if x == 42 { break; }
        }
        x   // evaluates to 42 at compile time
    }
}
```

{{ rule(id="4.14:20", cat="normative") }}

`break` and `continue` are supported within comptime loops and have their usual semantics: `break` exits the innermost loop, `continue` skips to the next iteration.

{{ rule(id="4.14:21", cat="legality-rule") }}

It is a compile-time error if a comptime loop executes more than 1,000,000 iterations. This prevents infinite loops from causing the compiler to hang.

```gruel
fn main() -> i32 {
    comptime {
        let mut x = 0;
        while true { x = x + 1; }  // ERROR: exceeds step budget
        x
    }
}
```

## Comptime Function Calls

{{ rule(id="4.14:22", cat="normative") }}

A comptime block can call non-generic functions. The called function's body is evaluated at compile time. All arguments must be compile-time evaluable.

```gruel
fn double(x: i32) -> i32 {
    x * 2
}

fn main() -> i32 {
    comptime { double(21) }   // evaluates to 42 at compile time
}
```

{{ rule(id="4.14:23", cat="normative") }}

A comptime block can call functions that themselves call other functions, forming a call chain. Each function in the chain is evaluated at compile time.

```gruel
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn sum_of_products(x: i32, y: i32, z: i32) -> i32 {
    add(x * y, y * z)
}

fn main() -> i32 {
    comptime { sum_of_products(2, 3, 7) }   // evaluates to 27 at compile time
}
```

{{ rule(id="4.14:24", cat="normative") }}

A comptime function call can use `return` to exit the function early. The returned value becomes the result of the call.

```gruel
fn clamp(x: i32, lo: i32, hi: i32) -> i32 {
    if x < lo { return lo; }
    if x > hi { return hi; }
    x
}

fn main() -> i32 {
    comptime { clamp(100, 0, 42) }   // evaluates to 42 at compile time
}
```

{{ rule(id="4.14:25", cat="legality-rule") }}

It is a compile-time error if the comptime call stack exceeds 64 frames.

{{ rule(id="4.14:29", cat="normative") }}

A comptime parameter may receive the result of a function call, provided the call can be fully evaluated at compile time. The callee must be a non-generic function whose arguments are themselves compile-time known. This prevents infinite recursion from causing the compiler to hang.

```gruel
fn infinite(x: i32) -> i32 {
    infinite(x + 1)  // ERROR: call stack depth exceeded
}

fn main() -> i32 {
    comptime { infinite(0) }
}
```

## Comptime Composite Values

{{ rule(id="4.14:26", cat="normative") }}

A comptime block can create struct instances and access their fields. All field expressions must be compile-time evaluable. The struct type must be statically known.

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    comptime {
        let p = Point { x: 10, y: 32 };
        p.x + p.y   // evaluates to 42 at compile time
    }
}
```

{{ rule(id="4.14:27", cat="normative") }}

A comptime block can create array instances and read their elements. All element expressions and the index must be compile-time evaluable. The index must be within bounds.

```gruel
fn main() -> i32 {
    comptime {
        let arr = [10, 20, 12];
        arr[0] + arr[1] + arr[2]   // evaluates to 42 at compile time
    }
}
```

{{ rule(id="4.14:28", cat="legality-rule") }}

It is a compile-time error if an array index is out of bounds in a comptime expression.

```gruel
fn main() -> i32 {
    comptime {
        let arr = [1, 2, 3];
        arr[5]   // ERROR: array index 5 out of bounds (length 3)
    }
}
```

{{ rule(id="4.14:36", cat="normative") }}

A comptime block can mutate struct fields via assignment. The base expression must refer to a mutable local variable holding a comptime struct, and the assigned value must be compile-time evaluable. The mutation modifies the struct in place on the comptime heap.

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    comptime {
        let mut p = Point { x: 0, y: 0 };
        p.x = 10;
        p.y = 32;
        p.x + p.y   // evaluates to 42 at compile time
    }
}
```

{{ rule(id="4.14:37", cat="normative") }}

A comptime block can mutate array elements via index assignment. The base expression must refer to a mutable local variable holding a comptime array, and the index and value must be compile-time evaluable. The index must be within bounds. The mutation modifies the array in place on the comptime heap.

```gruel
fn main() -> i32 {
    comptime {
        let mut arr = [0, 0, 0];
        arr[0] = 10;
        arr[1] = 20;
        arr[2] = 12;
        arr[0] + arr[1] + arr[2]   // evaluates to 42 at compile time
    }
}
```
