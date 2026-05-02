+++
title = "Structs"
weight = 2
template = "spec/page.html"
+++

# Structs

{{ rule(id="6.2:1", cat="normative") }}

A struct is defined using the `struct` keyword.

{{ rule(id="6.2:2", cat="normative") }}

```ebnf
struct_def = [ "pub" ] "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields = struct_field { "," struct_field } [ "," ] ;
struct_field = [ "pub" ] IDENT ":" type ;
```

## Struct Definition

{{ rule(id="6.2:3", cat="legality-rule") }}

Field names **MUST** be unique within a struct.

{{ rule(id="6.2:4") }}

```gruel
struct Point {
    x: i32,
    y: i32,
}
```

## Struct Instantiation

{{ rule(id="6.2:5", cat="legality-rule") }}

All fields **MUST** be initialized when creating a struct instance.

{{ rule(id="6.2:6", cat="normative") }}

Field initializers **MAY** be provided in any order.

{{ rule(id="6.2:7") }}

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    // Fields can be initialized in any order
    let p = Point { y: 20, x: 10 };
    p.x + p.y
}
```

## Struct Usage

{{ rule(id="6.2:8", cat="normative") }}

Struct fields are accessed using dot notation.

{{ rule(id="6.2:9", cat="normative") }}

Mutable struct values allow field reassignment.

{{ rule(id="6.2:10") }}

```gruel
struct Counter { value: i32 }

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value
}
```

## Field Visibility

{{ rule(id="6.2:11", cat="dynamic-semantics") }}

(ADR-0073, preview `field_method_visibility`.) A field declaration **MAY**
be prefixed with the `pub` keyword. A field marked `pub` is accessible from
any module that can name the enclosing struct. A field without `pub` is
accessible only from within the same module as the struct definition (per
the same module-equivalence rule used by item visibility in ADR-0026).

{{ rule(id="6.2:12", cat="informative") }}

Field access (`expr.field`), field assignment (`lhs.field = rhs`), struct
literal construction (`T { field: ... }`), and pattern matching that
mentions a field by name (`T { field, .. }`) are all subject to the same
visibility check.

{{ rule(id="6.2:13", cat="informative") }}

Wildcard struct patterns (`T { .. }`) and patterns that do not name a
particular field do not constitute access to that field and require no
visibility privilege.

{{ rule(id="6.2:14", cat="informative") }}

```gruel
// in module a/lib.gruel
pub struct Account {
    pub id: u64,
    balance: i64,        // module-private
}

// in module b/main.gruel
const a = @import("a/lib.gruel");

fn main() -> i32 {
    let acc = a.Account { id: 1, balance: 0 };  // ERROR: `balance` is private
    acc.balance                                   // ERROR: `balance` is private
}
```
