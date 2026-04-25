+++
title = "Anonymous Interfaces"
weight = 6
template = "spec/page.html"
+++

# Anonymous Interfaces

Anonymous interfaces (ADR-0057) are interface types constructed inline
inside `fn ... -> type` comptime bodies. They parallel anonymous structs
and anonymous enums: a comptime function returning `type` may produce
an interface whose method signatures are parameterized by the
function's comptime arguments.

{{ rule(id="6.6:1", cat="normative") }}

An anonymous interface expression is a `TypeExpr` of the form
`interface { ... }` containing zero or more method signatures. It is
legal only inside a comptime context that yields a `type` value.

{{ rule(id="6.6:2", cat="syntax") }}

```ebnf
anon_interface_type_expr = "interface" "{" { method_sig } "}" ;
method_sig               = "fn" IDENT "(" "self" [ "," params ] ")"
                           [ "->" type ] ";" ;
```

{{ rule(id="6.6:3", cat="dynamic-semantics") }}

Two anonymous interfaces with the same method requirements (name,
parameter types, return type, in declaration order) refer to the same
`InterfaceId`. Distinct parameterizations of a comptime constructor
(e.g., `Sized(i32)` vs. `Sized(i64)`) produce distinct interfaces with
their own vtables.

{{ rule(id="6.6:4", cat="syntax") }}

A parameterized type call `Name(arg1, arg2, ...)` is legal in any type
position when `Name` is a comptime function returning `type`. The call
is evaluated at compile time with the supplied arguments, and the
resulting type is used in place of the call.

{{ rule(id="6.6:5", cat="legality-rule") }}

Anonymous interfaces are subject to the same restrictions as named
interfaces: they appear in parameter positions only with `borrow` /
`inout` mode, and not as struct field types, return types, or local
binding types directly. Use a comptime alias if needed.

{{ rule(id="6.6:6", cat="example") }}

```gruel
fn Sized(comptime T: type) -> type {
    interface { fn size(self) -> T; }
}

struct Box {
    fn size(self) -> i32 { 42 }
}

fn use_sized(borrow s: Sized(i32)) -> i32 {
    s.size()
}

fn main() -> i32 {
    let b = Box {};
    use_sized(borrow b)  // 42, dispatched dynamically
}
```
