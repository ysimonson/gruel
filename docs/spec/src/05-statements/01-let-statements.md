+++
title = "Let Statements"
weight = 1
template = "spec/page.html"
+++

# Let Statements

{{ rule(id="5.1:1", cat="normative") }}

A let statement introduces a new variable binding.

{{ rule(id="5.1:2", cat="normative") }}

```ebnf
let_stmt = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
```

## Immutable Bindings

{{ rule(id="5.1:3", cat="legality-rule") }}

By default, variables are immutable. An immutable variable **MUST NOT** be reassigned.

{{ rule(id="5.1:4", cat="normative") }}

```gruel
fn main() -> i32 {
    let x = 42;
    x
}
```

## Mutable Bindings

{{ rule(id="5.1:5", cat="normative") }}

The `mut` keyword creates a mutable binding that **MAY** be reassigned.

{{ rule(id="5.1:6") }}

```gruel
fn main() -> i32 {
    let mut x = 10;
    x = 20;
    x
}
```

## Type Annotations

{{ rule(id="5.1:7", cat="normative") }}

Type annotations are optional when the type can be inferred from the initializer.

{{ rule(id="5.1:8", cat="legality-rule") }}

When a type annotation is present, the initializer **MUST** be compatible with that type.

{{ rule(id="5.1:9") }}

```gruel
fn main() -> i32 {
    let x: i32 = 42;      // explicit type
    let y = 10;           // type inferred as i32
    let z: i64 = 100;     // 100 inferred as i64
    x + y
}
```

## Shadowing

{{ rule(id="5.1:10", cat="normative") }}

A variable **MAY** shadow a previous variable of the same name in the same scope.

{{ rule(id="5.1:11", cat="normative") }}

When shadowing, the new variable **MAY** have a different type.

{{ rule(id="5.1:12", cat="normative") }}

The scope of a binding introduced by a let statement begins after the complete let statement, including its initializer. The initializer expression is evaluated before the new binding is introduced, so references to a shadowed name within the initializer resolve to the previous binding.

{{ rule(id="5.1:13") }}

```gruel
fn main() -> i32 {
    let x = 10;
    let x = x + 5;  // shadows previous x, initializer uses old x
    x  // 15
}
```

## Struct Destructuring

{{ rule(id="5.1:14", cat="normative") }}

A let statement may destructure a struct value, binding each field to a new variable. The struct type name must be specified, and all fields must be listed.

{{ rule(id="5.1:15", cat="syntax") }}

```ebnf
let_destructure = "let" type_name "{" field_bindings "}" "=" expression ";" ;
field_bindings  = field_binding { "," field_binding } [ "," ] ;
field_binding   = [ "mut" ] IDENT [ ":" ( IDENT | "_" ) ] ;
```

{{ rule(id="5.1:16", cat="normative") }}

The expression must evaluate to the named struct type. The struct value is consumed by the destructuring — it is no longer accessible after destructuring.

{{ rule(id="5.1:17", cat="legality-rule") }}

All fields of the struct **MUST** be listed in the destructuring pattern. Omitting a field is a compile-time error.

{{ rule(id="5.1:18", cat="normative") }}

A field binding of the form `field` (shorthand) binds the field value to a new variable with the same name. A binding of the form `field: name` binds the field value to a variable named `name`. A binding of the form `field: _` discards the field value, dropping it immediately if the type has a destructor.

{{ rule(id="5.1:19", cat="example") }}

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    let Point { x, y } = p;    // p is consumed
    x + y                       // 42
}
```

{{ rule(id="5.1:20", cat="example") }}

```gruel
struct Pair { a: i32, b: i32 }

fn main() -> i32 {
    let p = Pair { a: 1, b: 2 };
    let Pair { a: first, b: _ } = p;   // rename a to first, discard b
    first                                // 1
}
```
