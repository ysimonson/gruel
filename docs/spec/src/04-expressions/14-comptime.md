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

## Comptime Enum Support

{{ rule(id="4.14:38", cat="normative") }}

A comptime block can construct unit enum variants. The enum type must be defined and the variant must exist. The resulting value is a compile-time enum value.

```gruel
enum Color { Red, Green, Blue }

fn main() -> i32 {
    comptime {
        let c = Color::Green;
        match c {
            Color::Red => 1,
            Color::Green => 2,
            Color::Blue => 3,
        }
    }
}
```

{{ rule(id="4.14:39", cat="normative") }}

A comptime block can construct tuple-style enum variants with associated data. The arguments must be compile-time evaluable and their types must match the variant's field types.

```gruel
enum IntOption { Some(i32), None }

fn main() -> i32 {
    comptime {
        let x = IntOption::Some(42);
        match x {
            IntOption::Some(v) => v,
            IntOption::None => 0,
        }
    }
}
```

{{ rule(id="4.14:40", cat="normative") }}

A comptime block can construct struct-style enum variants with named fields. All fields must be provided and their values must be compile-time evaluable.

```gruel
enum Shape {
    Rect { w: i32, h: i32 },
    Circle { r: i32 },
}

fn main() -> i32 {
    comptime {
        let s = Shape::Rect { w: 6, h: 7 };
        match s {
            Shape::Rect { w, h } => w * h,
            Shape::Circle { r } => r * r,
        }
    }
}
```

## Comptime Pattern Matching

{{ rule(id="4.14:41", cat="normative") }}

A comptime block can use `match` expressions to branch on compile-time values. The scrutinee must be compile-time evaluable. Each arm's pattern is tested in order, and the first matching arm's body is evaluated. All pattern types supported in runtime `match` are also supported in comptime: wildcards, integer literals, boolean literals, enum variant paths, tuple data variant destructuring, and struct variant destructuring.

```gruel
fn main() -> i32 {
    comptime {
        let x = 3;
        match x {
            1 => 10,
            2 => 20,
            3 => 42,
            _ => 0,
        }
    }
}
```

## Comptime Generic Function Calls

{{ rule(id="4.14:42", cat="normative") }}

A comptime block can call generic functions. Type parameters are resolved at compile time and made available for struct and enum resolution within the callee body. The callee's body is interpreted with the concrete types substituted for the type parameters.

```gruel
fn identity(comptime T: type, x: T) -> T {
    x
}

fn main() -> i32 {
    comptime {
        identity(i32, 42)
    }
}
```

## Comptime Struct Destructuring

{{ rule(id="4.14:43", cat="normative") }}

A comptime block can destructure structs using `let Type { fields } = expr;` syntax. The initializer must evaluate to a compile-time struct value. Each named field is bound to a local variable; wildcard bindings (`field: _`) are skipped.

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    comptime {
        let p = Point { x: 10, y: 32 };
        let Point { x, y } = p;
        x + y
    }
}
```

## Comptime Method Calls

{{ rule(id="4.14:44", cat="normative") }}

A comptime block can call methods on compile-time struct values. The receiver is bound to `self` in the method body. The method body is interpreted with the same comptime evaluation rules. Methods that mutate fields operate on the comptime heap.

```gruel
struct Counter {
    value: i32,

    fn get(self) -> i32 { self.value }
}

fn main() -> i32 {
    comptime {
        let c = Counter { value: 42 };
        c.get()
    }
}
```

## Comptime Integer Casts

{{ rule(id="4.14:45", cat="normative") }}

A comptime block can use `@intCast` and `@cast` intrinsics. Since all comptime integer values are stored as `i64`, integer casts are pass-through operations that preserve the value.

```gruel
fn cast_to_i32(x: i64) -> i32 {
    @intCast(x)
}

fn main() -> i32 {
    comptime {
        cast_to_i32(42)
    }
}
```

## Comptime Type Intrinsics

{{ rule(id="4.14:46", cat="normative") }}

A comptime block can use `@size_of` and `@align_of` type intrinsics. These compute the size and alignment of a type at compile time. Sizes are returned in bytes (8 bytes per slot for scalar types). Zero-sized types have 1-byte alignment.

```gruel
fn main() -> i32 {
    comptime {
        @size_of(i32)
    }
}
```

{{ rule(id="4.14:47", cat="normative") }}

A comptime block can use the `@dbg` intrinsic to emit debug output during compile-time evaluation. `@dbg` accepts zero or more comptime-evaluable arguments, formats each, and joins the results with single ASCII space characters. Integer values are formatted as signed decimal, boolean values as `true` or `false`, `comptime_str` values as their contents, and `()` as `()`. See [`@dbg`](@/04-expressions/13-intrinsics.md) for the full specification.

{{ rule(id="4.14:47a", cat="normative") }}

Compile-time `@dbg` calls are written to the compiler's standard error with a `comptime dbg: ` prefix as each call is evaluated. The compiler additionally collects the formatted messages in a buffer accessible through the compilation result, and emits a "debug statement present" warning for each call. A compiler-driver flag (`--capture-comptime-dbg`) suppresses the on-the-fly stderr print while leaving the buffer intact.

```gruel
fn main() -> i32 {
    comptime {
        let x = 42;
        @dbg(x);              // compiler stderr: comptime dbg: 42
        x
    }
}
```

## Comptime Diagnostics

{{ rule(id="4.14:48", cat="normative") }}

The `@compileError` intrinsic emits a user-defined compile error during comptime evaluation. It takes a single string literal or `comptime_str` argument and has type `!` (never), terminating compilation of the current comptime block.

{{ rule(id="4.14:49", cat="legality-rule") }}

It is a compile-time error if `@compileError` is called with an argument that is not a string literal or `comptime_str` value, or with a number of arguments other than one.

{{ rule(id="4.14:50", cat="normative") }}

Unreachable `@compileError` calls are never evaluated. Only `@compileError` calls on taken branches produce errors.

{{ rule(id="4.14:51") }}

```gruel
fn Matrix(comptime rows: i32, comptime cols: i32) -> type {
    if rows <= 0 {
        @compileError("Matrix rows must be positive");
    }
    struct { data: [i32; rows * cols] }
}
```

{{ rule(id="4.14:52", cat="normative") }}

The behavior previously provided by `@compileLog` is subsumed by `@dbg`. See [`@dbg`](@/04-expressions/13-intrinsics.md) for the unified compile-time debug-print intrinsic, which accepts variadic arguments, prints to the compiler's standard error with a `comptime dbg: ` prefix, and emits a "debug statement present" warning for each call.

{{ rule(id="4.14:53", cat="legality-rule") }}

It is a compile-time error to call `@compileLog`. The diagnostic suggests `@dbg` as the replacement.

{{ rule(id="4.14:54", cat="example") }}

```gruel
fn compute(comptime n: i32) -> i32 {
    comptime { @dbg("computing with n =", n); }
    n * 2
}

fn main() -> i32 {
    compute(21)  // compiles with warning: debug statement present
}
```

## Comptime Strings

{{ rule(id="4.14:55", cat="normative") }}

The `comptime_str` type represents a string value that exists only at compile time. String literals inside `comptime` blocks are promoted to `comptime_str` values.

{{ rule(id="4.14:56", cat="normative") }}

When a `comptime { }` block evaluates to a `comptime_str` value, the compiler auto-materializes it as a runtime `String` constant. The comptime string's content is emitted as a string constant using the same mechanism as string literals. This allows `comptime_str` values to escape comptime blocks as runtime `String` values.

{{ rule(id="4.14:56a", cat="example") }}

```gruel
fn describe(comptime T: type) -> String {
    comptime { @typeName(T) }   // comptime_str materialized as runtime String
}
```

{{ rule(id="4.14:57", cat="normative") }}

The `comptime_str` type supports the comparison operators `==`, `!=`, `<`, `<=`, `>`, `>=`. Comparisons use lexicographic byte ordering.

{{ rule(id="4.14:58", cat="normative") }}

The `comptime_str` type provides the following methods: `len() -> i32` returns the byte length, `is_empty() -> bool` returns whether the string is empty, `contains(needle: comptime_str) -> bool` checks for substring presence, `starts_with(prefix: comptime_str) -> bool` checks for a prefix, `ends_with(suffix: comptime_str) -> bool` checks for a suffix, `concat(other: comptime_str) -> comptime_str` concatenates two strings, and `clone() -> comptime_str` copies the string.

{{ rule(id="4.14:59") }}

```gruel
fn check_name(comptime name: comptime_str) -> i32 {
    if name.len() == 0 {
        @compileError("name must not be empty");
    }
    name.len()
}
```

{{ rule(id="4.14:59a", cat="legality-rule") }}

It is a compile-time error to call runtime-only mutation methods (`push_str`, `push`, `clear`, `reserve`) on a `comptime_str` value. The compiler produces a diagnostic suggesting `.concat()` as the immutable alternative. Calling `.capacity()` on a `comptime_str` is also a compile-time error, since compile-time strings have no allocation.

## Comptime Integers

{{ rule(id="4.14:80", cat="normative") }}

The `comptime_int` type represents an integer value that exists only at compile time. Integer expressions evaluated inside `comptime` blocks produce `comptime_int` values. Internally, `comptime_int` values are stored as signed 64-bit integers.

{{ rule(id="4.14:81", cat="normative") }}

When a `comptime_int` value flows into a runtime context, it is implicitly coerced to the expected integer type. The target type is determined by type inference. If no type constraint is present, the value defaults to `i32`.

{{ rule(id="4.14:82", cat="legality-rule") }}

It is a compile-time error if a `comptime_int` value does not fit in the target integer type.

{{ rule(id="4.14:83", cat="example") }}

```gruel
fn main() -> i32 {
    let x: u64 = comptime { 100 };   // comptime_int coerces to u64
    let y: i32 = comptime { 42 };    // comptime_int coerces to i32
    @intCast(x) + y
}
```

{{ rule(id="4.14:84", cat="normative") }}

Captured comptime integer parameters (e.g., `comptime N: i32`) are represented as `comptime_int` values inside the comptime system. When referenced in runtime code, they coerce to the type expected by the surrounding context.

## Type Reflection

{{ rule(id="4.14:60", cat="normative") }}

The `@typeName(T)` intrinsic accepts a type argument and returns a `comptime_str` containing the type's name. For primitive types, this is the type keyword (e.g., `"i32"`, `"bool"`). For struct and enum types, this is the declared name.

{{ rule(id="4.14:61", cat="legality-rule") }}

`@typeName` requires the `comptime_meta` preview feature. It **MUST** be called with exactly one type argument using the `@typeName(T)` syntax.

{{ rule(id="4.14:62", cat="normative") }}

The `@typeInfo(T)` intrinsic accepts a type argument and returns a comptime struct describing the type's structure. The returned struct always contains a `kind` field of type `TypeKind` and a `name` field of type `comptime_str`.

{{ rule(id="4.14:63", cat="legality-rule") }}

`@typeInfo` requires the `comptime_meta` preview feature. It **MUST** be called with exactly one type argument using the `@typeInfo(T)` syntax.

{{ rule(id="4.14:64", cat="normative") }}

The `TypeKind` enum is a built-in enum with the following variants: `Struct`, `Enum`, `Int`, `Bool`, `Unit`, `Never`, `Array`. It is used to discriminate type kinds in `@typeInfo` results.

{{ rule(id="4.14:65", cat="normative") }}

For struct types, `@typeInfo` returns a struct with fields: `kind: TypeKind` (always `TypeKind::Struct`), `name: comptime_str`, `field_count: i32`, and `fields: [FieldInfo; N]` where N is the number of fields. Each `FieldInfo` is a struct with fields `name: comptime_str` and `field_type: type`.

{{ rule(id="4.14:66", cat="normative") }}

For enum types, `@typeInfo` returns a struct with fields: `kind: TypeKind` (always `TypeKind::Enum`), `name: comptime_str`, `variant_count: i32`, and `variants: [VariantInfo; N]` where N is the number of variants. Each `VariantInfo` is a struct with fields `name: comptime_str` and `fields: [FieldInfo; M]` where M is the number of fields for that variant (0 for unit variants).

{{ rule(id="4.14:67", cat="normative") }}

For integer types, `@typeInfo` returns a struct with fields: `kind: TypeKind` (always `TypeKind::Int`), `name: comptime_str`, `bits: i32` (the bit width), and `is_signed: bool`.

{{ rule(id="4.14:68", cat="normative") }}

For other primitive types (`bool`, `unit`, `!`), `@typeInfo` returns a struct with fields: `kind: TypeKind` and `name: comptime_str`.

{{ rule(id="4.14:69") }}

```gruel
fn describe(comptime T: type) -> i32 {
    let info = @typeInfo(T);
    match info.kind {
        TypeKind::Struct => info.field_count,
        TypeKind::Int => info.bits,
        _ => 0,
    }
}
```

## Compile-Time Loop Unrolling

{{ rule(id="4.14:70", cat="normative") }}

The `comptime_unroll for` expression evaluates a compile-time iterable and unrolls the loop body once for each element. The iterable must be a `comptime` block that evaluates to a comptime array. The loop variable is bound to a comptime value for each iteration and is accessible within the body.

```ebnf
comptime_unroll_for = "comptime_unroll" "for" IDENT "in" comptime_expr block ;
```

{{ rule(id="4.14:71", cat="normative") }}

The body of a `comptime_unroll for` is runtime code, not comptime code. Each unrolled iteration generates independent runtime instructions. The loop variable holds a comptime value that can be used in comptime intrinsics within the body (such as `@field`).

{{ rule(id="4.14:72", cat="normative") }}

`@range` can be used as the iterable in a `comptime_unroll for`. When evaluated at compile time, `@range(end)`, `@range(start, end)`, or `@range(start, end, stride)` produces a comptime array of integers.

{{ rule(id="4.14:73") }}

```gruel
fn main() -> i32 {
    let mut total: i32 = 0;
    comptime_unroll for i in comptime { @range(5) } {
        total = total + 1;
    }
    total
}
```

{{ rule(id="4.14:74", cat="legality-rule") }}

It is a compile-time error if the iterable expression in a `comptime_unroll for` does not evaluate to a comptime array.

{{ rule(id="4.14:75", cat="legality-rule") }}

`comptime_unroll for` requires the `comptime_meta` preview feature.

## Dynamic Field Access

{{ rule(id="4.14:76", cat="normative") }}

The `@field(value, field_name)` intrinsic accesses a field of a struct value using a compile-time known field name. The first argument is a runtime struct value. The second argument is a `comptime_str` that names a field on that struct. The field name is resolved at compile time to a concrete field index, and the result type is the type of the named field.

{{ rule(id="4.14:77", cat="legality-rule") }}

`@field` requires the `comptime_meta` preview feature. It **MUST** be called with exactly two arguments. The first argument **MUST** be a struct value. The second argument **MUST** evaluate to a `comptime_str` at compile time.

{{ rule(id="4.14:78", cat="legality-rule") }}

It is a compile-time error if the second argument to `@field` names a field that does not exist on the struct type of the first argument.

{{ rule(id="4.14:79") }}

```gruel
struct Point { x: i32, y: i32 }

fn sum_fields(comptime T: type, val: T) -> i32 {
    let mut total: i32 = 0;
    comptime_unroll for field in comptime { @typeInfo(T).fields } {
        total = total + @field(val, field.name);
    }
    total
}

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    sum_fields(Point, p)
}
```
