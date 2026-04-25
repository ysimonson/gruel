+++
title = "Interfaces"
weight = 5
template = "spec/page.html"
+++

# Interfaces

Interfaces are structurally typed sets of method requirements (ADR-0056).
A type *conforms* to an interface when its method set covers every
required signature; conformance is checked at use sites and is never
declared up front. Interfaces are usable both as comptime constraints
(monomorphized) and as runtime parameter types behind a borrowing mode
(dynamically dispatched through a vtable).

{{ rule(id="6.5:1", cat="normative") }}

An interface is a structurally typed set of method requirements. It is
declared at module scope with the `interface` keyword.

{{ rule(id="6.5:2", cat="syntax") }}

```ebnf
interface_def  = [ "pub" ] "interface" IDENT "{" { method_sig } "}" ;
method_sig     = "fn" IDENT "(" "self" [ "," params ] ")" [ "->" type ] ";" ;
```

{{ rule(id="6.5:3", cat="legality-rule") }}

Method signatures inside an interface declaration do not have a body. The
declaration `fn name(self) { ... }` (with a block body) is rejected at parse
or analysis time.

{{ rule(id="6.5:4", cat="legality-rule") }}

Method names within a single interface declaration must be unique. Two method
signatures with the same name in the same interface are a compile error.

{{ rule(id="6.5:5", cat="example") }}

```gruel
interface Drop {
    fn drop(self);
}
```

## Comptime Constraints

{{ rule(id="6.5:7", cat="normative") }}

An interface name may appear in place of `type` as the bound on a comptime
type parameter: `comptime T: I`. At every call site, the concrete type bound
to `T` must structurally conform to `I`.

{{ rule(id="6.5:8", cat="legality-rule") }}

Conformance is checked at the call site. The concrete type `C` conforms to
interface `I` iff for every method signature `fn name(self [, params]) [-> R]`
in `I`, type `C` has a method with the same name, the same parameter types
in declaration order, and the same return type. Any missing method or
signature mismatch is a compile error at the call site.

{{ rule(id="6.5:9", cat="dynamic-semantics") }}

Comptime constraint usage is fully erased at codegen. Each call site
monomorphizes the function for the concrete type that satisfies the bound;
no vtable or fat pointer is materialized.

{{ rule(id="6.5:10", cat="example") }}

```gruel
// Compiled with --preview interfaces
interface Greeter {
    fn greet(self);
}

struct Foo {
    fn greet(self) {}
}

fn use_greeter(comptime T: Greeter, t: T) {
    t.greet();
}

fn main() -> i32 {
    use_greeter(Foo, Foo {});
    0
}
```

## Runtime Dispatch

{{ rule(id="6.5:11", cat="normative") }}

An interface name may also appear as a parameter type with a borrowing
mode: `borrow t: I` or `inout t: I`. The parameter is then passed as a
fat pointer `(data_ptr, vtable_ptr)` and method calls on it dispatch
dynamically through the vtable.

{{ rule(id="6.5:12", cat="legality-rule") }}

Interface-typed parameters require a borrowing mode. By-value `t: I` is
rejected. Interface types are not legal as struct field types, return
types, or local-binding types in the current implementation; they appear
only in parameter positions.

{{ rule(id="6.5:13", cat="legality-rule") }}

At each call site that passes a concrete `C` to a parameter of type `I`,
the compiler verifies `C` conforms to `I` (same structural check as the
comptime path). Non-conforming arguments are rejected at the call site.

{{ rule(id="6.5:14", cat="dynamic-semantics") }}

The fat pointer's data field references the caller's storage; passing
through `borrow t: I` does not copy the underlying value. The vtable
field is a static, deduplicated global per `(concrete type, interface)`
pair.

{{ rule(id="6.5:15", cat="example") }}

```gruel
// Compiled with --preview interfaces
interface Marker {}

struct Foo {}

fn ignore(borrow t: Marker) {
}

fn main() -> i32 {
    let f = Foo {};
    ignore(borrow f);
    0
}
```

{{ rule(id="6.5:16", cat="dynamic-semantics") }}

A method call on an interface receiver loads the function pointer from
the receiver's vtable at the slot determined by the method's declaration
order in the interface, then calls it with the receiver's data pointer
as the implicit first argument followed by the explicit arguments. The
result is the return value of the dispatched function.

{{ rule(id="6.5:17", cat="example") }}

```gruel
// Compiled with --preview interfaces
interface Counter {
    fn count(self) -> i32;
}

struct One {
    fn count(self) -> i32 { 1 }
}

struct Five {
    fn count(self) -> i32 { 5 }
}

fn invoke(borrow t: Counter) -> i32 {
    t.count()
}

fn main() -> i32 {
    let a = One {};
    let b = Five {};
    invoke(borrow a) + invoke(borrow b)  // 6
}
```
