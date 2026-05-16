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
(monomorphized) and as runtime parameter types when wrapped in
`Ref(I)` / `MutRef(I)` (dynamically dispatched through a vtable).

{{ rule(id="6.5:1", cat="normative") }}

An interface is a structurally typed set of method requirements. It is
declared at module scope with the `interface` keyword.

{{ rule(id="6.5:2", cat="syntax") }}

```ebnf
interface_def  = [ "pub" ] "interface" IDENT "{" { method_sig } "}" ;
method_sig     = "fn" IDENT "(" receiver [ "," params ] ")" [ "->" type ] ";" ;
receiver       = "self" [ ":" ( "Self" | "Ref" "(" "Self" ")" | "MutRef" "(" "Self" ")" ) ] ;
```

The type used in a method signature's parameter list or return position
may be the keyword `Self`, which stands for the type that conforms to
the enclosing interface (see 6.5:18).

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
    fn __drop(self);
}
```

## Comptime Constraints

{{ rule(id="6.5:7", cat="normative") }}

An interface name may appear in place of `type` as the bound on a comptime
type parameter: `comptime T: I`. At every call site, the concrete type bound
to `T` must structurally conform to `I`.

{{ rule(id="6.5:8", cat="legality-rule") }}

Conformance is checked at the call site. The concrete type `C` conforms to
interface `I` iff for every method signature
`fn name(receiver [, params]) [-> R]` in `I`, type `C` has a method with the
same name, the same receiver mode (6.5:19), the same parameter types in
declaration order after substituting `Self` with `C` (6.5:18), and the same
return type after the same substitution. Any missing method, mismatched
receiver mode, or signature mismatch is a compile error at the call site.

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

An interface name may also appear as a parameter type wrapped in a
reference type: `t: Ref(I)` or `t: MutRef(I)`. The parameter is then
passed as a fat pointer `(data_ptr, vtable_ptr)` and method calls on
it dispatch dynamically through the vtable.

{{ rule(id="6.5:12", cat="legality-rule") }}

Interface-typed parameters **MUST** be wrapped in `Ref(...)` or
`MutRef(...)`. By-value `t: I` is rejected. Interface types are not
legal as struct field types, return types, or local-binding types in
the current implementation; they appear only inside `Ref(I)` /
`MutRef(I)` parameter positions.

{{ rule(id="6.5:13", cat="legality-rule") }}

At each call site that passes a concrete `C` to a parameter of type `I`,
the compiler verifies `C` conforms to `I` (same structural check as the
comptime path). Non-conforming arguments are rejected at the call site.

{{ rule(id="6.5:14", cat="dynamic-semantics") }}

The fat pointer's data field references the caller's storage; passing
through `t: Ref(I)` does not copy the underlying value. The vtable
field is a static, deduplicated global per `(concrete type, interface)`
pair.

{{ rule(id="6.5:15", cat="example") }}

```gruel
// Compiled with --preview interfaces
interface Marker {}

struct Foo {}

fn ignore(t: Ref(Marker)) {
}

fn main() -> i32 {
    let f = Foo {};
    ignore(&f);
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

fn invoke(t: Ref(Counter)) -> i32 {
    t.count()
}

fn main() -> i32 {
    let a = One {};
    let b = Five {};
    invoke(&a) + invoke(&b)  // 6
}
```

## `Self` and Receiver Modes (ADR-0060)

{{ rule(id="6.5:18", cat="normative") }}

Inside an interface method signature, the keyword `Self` is a type that
stands for the candidate type being checked for conformance. At
conformance check time (6.5:8) every occurrence of `Self` in a parameter
or return type is replaced by the candidate type before comparing
against the candidate's method. `Self` has no other meaning; it is not a
runtime type and may not appear outside an interface method signature.

{{ rule(id="6.5:19", cat="normative") }}

An interface method's receiver is one of `self`, `self: MutRef(Self)`, or
`self: Ref(Self)`. The receiver mode is part of the method's required
signature: a candidate type's method conforms only if its receiver mode
is identical to the interface's. Mismatched receiver modes are a compile
error at the call site, distinct from a parameter or return type
mismatch.

{{ rule(id="6.5:20", cat="legality-rule") }}

The keyword `Self` is reserved inside an interface body and refers only
to the candidate type. Using `Self` in any other position (free
functions, struct field types, top-level type aliases) is rejected at
analysis time as an unknown type.

{{ rule(id="6.5:21", cat="example") }}

```gruel
// Compiled with --preview interfaces
interface Cloner {
    fn clone(self: Ref(Self)) -> Self;
}

struct Buf {
    n: i32,

    fn clone(self: Ref(Self)) -> Buf {
        Buf { n: self.n }
    }
}

fn use_cloner(comptime T: Cloner, t: T) {
}

fn main() -> i32 {
    let b = Buf { n: 1 };
    use_cloner(Buf, b);
    0
}
```

{{ rule(id="6.5:22", cat="example") }}

A candidate whose return type is not the candidate itself fails to
conform when the interface declares `-> Self`:

```gruel
// Compile error: type `Buf` does not conform to interface `Cloner`
interface Cloner {
    fn clone(self: Ref(Self)) -> Self;
}

struct Buf {
    fn clone(self: Ref(Self)) -> i32 { 0 }
}
```

{{ rule(id="6.5:23", cat="example") }}

A candidate with the wrong receiver mode is rejected even when the
parameter and return types align:

```gruel
// Compile error: type `Buf` does not conform to interface `Reader`
interface Reader {
    fn read(self: Ref(Self)) -> i32;
}

struct Buf {
    fn read(self) -> i32 { 0 }
}
```

## `@mark(unchecked)` on interface method signatures (ADR-0088)

{{ rule(id="6.5:24", cat="normative") }}

An interface method signature **MAY** carry `@mark(unchecked)` in
its directive list. The directive declares that every call to the
method must wrap the call in a `checked { }` block, identical to
the `@mark(unchecked)` rule on regular fn / method declarations
(9.2:1, 9.2:2).

{{ rule(id="6.5:25", cat="legality-rule") }}

Conformance is strict on `@mark(unchecked)`: a candidate method's
`@mark(unchecked)` status must match the interface signature's
exactly. A checked interface method may not be satisfied by an
`@mark(unchecked)` implementation, and vice versa.

{{ rule(id="6.5:26", cat="example") }}

```gruel
interface UnsafeReader {
    @mark(unchecked) fn read(self: Ref(Self)) -> i32;
}

struct Foo {
    val: i32,

    @mark(unchecked)
    pub fn read(self: Ref(Self)) -> i32 { self.val }
}

fn copy_unchecked(comptime T: UnsafeReader, r: T) -> i32 {
    checked { r.read() }
}
```
