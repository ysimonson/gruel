+++
title = "Interfaces"
weight = 5
template = "spec/page.html"
+++

# Interfaces

> This chapter is provisional. Interfaces are a preview feature gated behind
> `--preview interfaces` (ADR-0056). Phase 1 introduces the declaration syntax
> only; conformance, comptime constraints, and runtime dispatch are
> specified as later phases land.

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

{{ rule(id="6.5:5", cat="legality-rule") }}

Interface declarations require the `interfaces` preview feature. A program
that contains an `interface` declaration without `--preview interfaces`
enabled is rejected at compile time.

{{ rule(id="6.5:6", cat="example") }}

```gruel
// Compiled with --preview interfaces
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
