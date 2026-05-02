+++
title = "Methods"
weight = 4
template = "spec/page.html"
+++

# Methods

{{ rule(id="6.4:1", cat="normative") }}

Methods are functions defined inside a struct block that can be called on instances of that struct.

{{ rule(id="6.4:2", cat="syntax") }}

```ebnf
struct_def = [ directives ] [ "pub" ] "struct" IDENT "{" [ field_list ] [ method_list ] "}" ;
field_list = field_def { "," field_def } [ "," ] ;
field_def = [ "pub" ] IDENT ":" type ;
method_list = method_def { method_def } ;
method_def = [ directives ] [ "pub" ] "fn" IDENT "(" [ method_params ] ")" [ "->" type ] block ;
method_params = method_param { "," method_param } [ "," ] ;
method_param = "self" | ( IDENT ":" type ) ;
```

## Method Definition

{{ rule(id="6.4:3", cat="normative") }}

A method is a function defined inside a struct block that takes `self` as its first parameter.

{{ rule(id="6.4:4", cat="normative") }}

The `self` parameter represents the receiver value and has the type of the enclosing struct.

{{ rule(id="6.4:5", cat="example") }}

```gruel
struct Point {
    x: i32,
    y: i32,

    fn get_x(self) -> i32 {
        self.x
    }
}

fn main() -> i32 {
    let p = Point { x: 42, y: 10 };
    p.get_x()  // Returns 42
}
```

## Method Calls

{{ rule(id="6.4:6", cat="normative") }}

Methods are called using dot notation: `receiver.method(args)`.

{{ rule(id="6.4:7", cat="dynamic-semantics") }}

A method call `receiver.method(args)` is desugared to a function call with the receiver as the first argument.

{{ rule(id="6.4:8", cat="normative") }}

Methods **MAY** have additional parameters after `self`.

{{ rule(id="6.4:9", cat="example") }}

```gruel
struct Point {
    x: i32,
    y: i32,

    fn add(self, dx: i32, dy: i32) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    let p2 = p.add(32, 0);
    p2.x  // Returns 42
}
```

## Method Chaining

{{ rule(id="6.4:10", cat="normative") }}

When a method returns the same struct type, method calls **MAY** be chained.

{{ rule(id="6.4:11", cat="example") }}

```gruel
struct Counter {
    value: i32,

    fn inc(self) -> Counter {
        Counter { value: self.value + 1 }
    }
}

fn main() -> i32 {
    let c = Counter { value: 39 };
    c.inc().inc().inc().value  // Returns 42
}
```

## Associated Functions

{{ rule(id="6.4:12", cat="normative") }}

A function in a struct block that does not take `self` as its first parameter is an associated function.

{{ rule(id="6.4:13", cat="normative") }}

Associated functions are called using path notation: `Type::function(args)`.

{{ rule(id="6.4:14", cat="example") }}

```gruel
struct Point {
    x: i32,
    y: i32,

    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

fn main() -> i32 {
    let p = Point::origin();
    p.x  // Returns 0
}
```

## Multiple Methods

{{ rule(id="6.4:15", cat="normative") }}

A struct may have multiple methods defined in its block.

{{ rule(id="6.4:16", cat="legality-rule") }}

Method names **MUST** be unique within a struct definition.

{{ rule(id="6.4:17", cat="example") }}

```gruel
struct Point {
    x: i32,
    y: i32,

    fn get_x(self) -> i32 { self.x }

    fn get_y(self) -> i32 { self.y }
}

fn main() -> i32 {
    let p = Point { x: 42, y: 10 };
    p.get_x()  // Returns 42
}
```

## Error Conditions

{{ rule(id="6.4:20", cat="legality-rule") }}

Calling a method on a non-struct type is a compile-time error.

{{ rule(id="6.4:21", cat="legality-rule") }}

Calling an undefined method is a compile-time error.

{{ rule(id="6.4:22", cat="legality-rule") }}

Calling an associated function with method call syntax (receiver.function()) is a compile-time error.

{{ rule(id="6.4:23", cat="legality-rule") }}

Calling a method with associated function syntax (Type::method()) is a compile-time error.

## Method Visibility

{{ rule(id="6.4:24", cat="informative") }}

(ADR-0073, preview `field_method_visibility`.) A method definition **MAY**
be prefixed with the `pub` keyword. A method marked `pub` is callable from
any module that can name the enclosing struct or enum. A method without
`pub` is callable only from within the same module as the type definition.

{{ rule(id="6.4:25", cat="informative") }}

The visibility check applies to both instance method calls
(`receiver.method(...)`) and associated function calls
(`Type::function(...)`). Interface methods (declared in `interface`
blocks) are not subject to this check; an interface's methods are part of
the interface's public contract wherever the interface itself is in scope.

{{ rule(id="6.4:26", cat="informative") }}

```gruel
pub struct Counter {
    value: i32,

    pub fn new() -> Counter { Counter { value: 0 } }     // public
    pub fn get(self) -> i32 { self.value }               // public
    fn validate(self) -> bool { self.value >= 0 }        // module-private
}
```
