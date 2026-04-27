+++
title = "Unchecked Code Syntax"
weight = 1
template = "spec/page.html"
+++

# Unchecked Code Syntax

This section describes the syntax for unchecked code constructs.

## Unchecked Functions

{{ rule(id="9.1:1", cat="normative") }}

A function **MAY** be marked with the `unchecked` modifier to indicate that calling it requires a `checked` block.

{{ rule(id="9.1:2", cat="syntax") }}

```ebnf
function = [ "pub" ] [ "unchecked" ] "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
```

{{ rule(id="9.1:3", cat="legality-rule") }}

A call to an `unchecked` function is a compile-time error unless it appears inside a `checked` block.

{{ rule(id="9.1:4", cat="example") }}

```gruel
unchecked fn dangerous_operation() -> i32 {
    42
}

pub unchecked fn public_dangerous() -> i32 {
    0
}
```

## Checked Blocks

{{ rule(id="9.1:5", cat="normative") }}

A `checked` block is an expression that enables unchecked operations within its body.

{{ rule(id="9.1:6", cat="syntax") }}

```ebnf
checked_expr = "checked" "{" block "}" ;
```

{{ rule(id="9.1:7", cat="dynamic-semantics") }}

A `checked` block evaluates its body and returns the value of the final expression. The type of a `checked` block is the type of its body expression.

{{ rule(id="9.1:8", cat="legality-rule") }}

Pointer intrinsics (`@raw`, `@raw_mut`, `@ptr_read`, `@ptr_write`, `@ptr_offset`, `@ptr_to_int`, `@int_to_ptr`, `@null_ptr`, `@is_null`, `@ptr_copy`, `@syscall`) are compile-time errors unless they appear inside a `checked` block.

{{ rule(id="9.1:9", cat="example") }}

```gruel
fn main() -> i32 {
    let x = checked {
        let a = 10;
        let b = 32;
        a + b
    };
    x
}
```

## Raw Pointer Types

{{ rule(id="9.1:10", cat="normative") }}

Gruel provides two raw pointer types for low-level memory access:
- `Ptr(T)` - a pointer to immutable data of type `T`
- `MutPtr(T)` - a pointer to mutable data of type `T`

`Ptr` and `MutPtr` are built-in compiler-recognized type constructors;
they share the call-style surface form with comptime-generic user types
(e.g. `Vec(T)`), but their lowering is hard-wired in the compiler. See
ADR-0061. Originally introduced as `ptr const T` / `ptr mut T` keyword
syntax (ADR-0028); that surface form has been replaced.

{{ rule(id="9.1:11", cat="syntax") }}

```ebnf
ptr_type = ( "Ptr" | "MutPtr" ) "(" type ")" ;
```

{{ rule(id="9.1:12", cat="example") }}

```gruel
fn takes_ptr(p: Ptr(i32)) -> i32 { 0 }
fn takes_mut_ptr(p: MutPtr(i32)) -> i32 { 0 }
unchecked fn get_ptr() -> Ptr(i32) { @panic() }
```
