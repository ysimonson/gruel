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
