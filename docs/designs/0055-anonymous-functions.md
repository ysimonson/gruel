---
id: 0055
title: Anonymous Functions (Closures-as-Structs)
status: proposal
tags: [expressions, types, comptime, generics]
feature-flag: anon_functions
created: 2026-04-24
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0055: Anonymous Functions (Closures-as-Structs)

## Status

Proposal

## Summary

Add anonymous function expressions (lambdas) that desugar to a fresh anonymous
struct with a single `__call` method. Each lambda site produces a distinct type.
Lambdas close over comptime parameters of the enclosing function (the same way
anonymous struct methods do under ADR-0029) but **do not** close over runtime
locals. This lets generic methods like `Vec(T).map(f)` be written today without
introducing a function-pointer type, a closure environment, traits, or any new
runtime support.

## Context

### The problem

ADR-0029 closed Open Question #3 by deferring generic methods like `map`:

> Generic methods are desirable but blocked by the lack of function type syntax
> in Gruel. The `f: fn(T) -> U` syntax shown above is not legal Gruel - there's
> no function pointer or closure type yet.

Today you can write:

```gruel
fn Vec(comptime T: type) -> type {
    struct {
        /* ... */
        fn map(self, comptime U: type, /* how do we take f here? */) -> Vec(U) { ... }
    }
}
```

…and get stuck at the `f` parameter. There is no type we can write whose values
are "a thing you can call that takes `T` and returns `U`." Users who want `map`
must fall back to writing free functions and passing values through, which does
not chain and does not compose with the struct-method model of the language.

### What "closure" needs to mean in Gruel

Two things are usually conflated under the word "closure":

1. **A callable thing with an inline body** — a lambda expression you can pass
   to a higher-order function.
2. **An environment capture mechanism** — the ability for that body to
   implicitly reference runtime locals from the enclosing scope.

The second is what makes Rust's closures hard: they force a choice between
`Fn`/`FnMut`/`FnOnce` traits, capture-mode inference, lifetime parameters, and
boxing. Our current language has none of those pieces and we do not want to
commit to them now.

The first is entirely expressible in terms of ADR-0029's anonymous struct
methods:

```gruel
// Conceptually, |x| x + 1 is equivalent to:
let f = struct {
    fn __call(self, x: i32) -> i32 { x + 1 }
} {};
```

An anonymous struct with no fields is a zero-sized value; its `__call` method
is an ordinary function. If we make each such expression produce a *distinct*
type, then `map`'s signature `fn map(self, comptime F: type, f: F) -> Vec(U)`
just works via existing monomorphization — each call site monomorphizes to the
concrete lambda type. This is essentially Zig's model: generics see "a thing
with a `__call` method" and the body is checked per-specialization.

### The capture question

Runtime capture would require deciding how values are moved/borrowed into the
environment (ADR-0008 affine types, ADR-0013 borrow modes), how the closure's
lifetime relates to its captures, and possibly how they are laid out on the
heap. Those are downstream ADRs. This ADR deliberately avoids all of them by
giving anonymous functions exactly the capture surface that anonymous struct
methods already have: **comptime parameters only**.

Users who need to "capture" a runtime value pass it explicitly, either as an
argument or by constructing a named struct with a `__call` method themselves.
That escape hatch already exists; this ADR just makes the common case
(stateless transforms like `|x| x + 1`) ergonomic.

### Current state

- Anonymous struct methods: implemented (ADR-0029).
- Anonymous enum methods: implemented (ADR-0039).
- Comptime parameters and monomorphization: implemented (ADR-0025).
- Function-pointer / closure types: **do not exist** in the type system.
- Nested functions: not a language feature today.

## Decision

### Syntax

Anonymous function literals reuse the `fn` keyword, dropping the name:

```gruel
fn(x: i32) -> i32 { x + 1 }
fn(x: i32, y: i32) -> i32 { x + y }
fn() -> i32 { 42 }
fn(x: i32) { dbg(x) }           // unit return
```

Exactly the same shape as a named function (`fn foo(x: i32) -> i32 { x + 1 }`),
with the name omitted. All the same rules apply:

- Parameter type annotations are **required** (same as named functions).
- Return type annotation (`-> T`) is optional; if omitted, defaults to `()`
  as it does for named functions. Unlike the previously-considered `|...|`
  syntax, the body is **always** a block — again, for consistency with named
  functions.
- All existing named-function syntax (pattern parameters, default values if/
  when added, etc.) applies uniformly.

Rationale for `fn(...)` over `|...|`:
- Syntactic consistency: an anonymous function looks exactly like a named one
  minus the name. One production in the grammar, one mental model for users.
- No new delimiter vocabulary (`|` already means bitwise-or / logical-or
  elsewhere).
- The desugaring in this ADR literally produces a `fn __call(self, ...)`
  inside a struct, so the source form matching the desugared form is a plus.

**Grammar disambiguation.** At item position (top-level, inside a struct/enum
body), `fn <ident>(...)` is a named function item and `fn(` with no identifier
after the `fn` is not currently legal — so reserving `fn(` at that position
for "an anonymous-function expression used as a statement" is a non-breaking
extension. At expression position, the parser already looks at what follows
the current token to disambiguate; `fn` followed by `(` parses as an
anonymous-function expression. There is no ambiguity with any existing
expression form, because `fn` was previously not an expression starter at all.

### Desugaring

Each anonymous-function expression desugars into:

1. A fresh **anonymous struct type** with:
   - No fields.
   - Exactly one method: `fn __call(self, <params>) -> <ret> { <body> }`.
2. An instance of that struct, constructed as the empty literal.

The expression's static type is that fresh struct type. The expression's value
is the ZST instance.

```gruel
// User writes:
let f = fn(x: i32) -> i32 { x + 1 };

// Compiler treats it as roughly:
let f = __lambda_N {};  // where __lambda_N is:
//   struct {
//       fn __call(self, x: i32) -> i32 { x + 1 }
//   }
```

### Type uniqueness (deliberate divergence from ADR-0029)

ADR-0029 says two anonymous structs are structurally equal iff they have the
same fields and same method signatures. That rule is good for `Vec(T)` but
wrong for lambdas: two lambdas with identical signatures but different bodies
must be different values of different types, otherwise `|x| x + 1` and
`|x| x * 2` would be the same type and the compiler could not pick a body.

**Rule**: anonymous-function structs are tagged as *non-dedup*. Each `fn(...)`
expression gets its own `StructId` and is never unified with any other struct
(including other `fn(...)` expressions with the same signature). Internally
this is an `origin: AnonStructOrigin` field on `StructDef` with variants
`Explicit` (the ADR-0029 behavior, dedup on structural equality) and `Lambda`
(each instance unique).

This means anonymous-function values are *not* interchangeable: passing one to
a generic function monomorphizes that function for *this specific* source
site. That is what we want for zero-cost `map`.

### Calling convention

An anonymous-function value `f` is called with the ordinary call syntax:

```gruel
let f = fn(x: i32) -> i32 { x + 1 };
let y = f(3);  // -> 4
```

The parser already produces a `Call` AST node for `f(3)`. Sema looks up `f`'s
type; if it is a struct with a method named `__call`, the call is rewritten
to `f.__call(3)`. This is a targeted sugar: only the single method named
`__call` triggers it, and only for struct-typed callees (so ordinary
function-name calls are unchanged).

The method name `__call` is reserved in the sense that any struct defining a
method called `__call` becomes callable via function-call syntax. This is
intentional — it lets users define their own callable types (e.g., a counter
struct with runtime state and `fn __call(self, x: i32) -> i32`) and use them
with higher-order APIs like `map`. The `__` prefix follows the existing
Gruel convention for compiler-reserved names (e.g., `__gruel_drop_String` in
the runtime), making it clear at a glance that this identifier has magic
meaning and should not collide with ordinary user method names.

### Comptime capture (inherited from ADR-0029)

Because the desugared struct method is defined inside the same comptime scope
as the lambda, references to the enclosing `comptime T: type` (or any other
comptime parameter) work without any new mechanism:

```gruel
fn adder(comptime T: type, step: T) -> ??? {
    // Not this — `step` is runtime. See next section.
}

fn map_incr(comptime T: type, v: Vec(T)) -> Vec(T) {
    v.map(T, fn(x: T) -> T { x + 1 })  // T is captured at comptime
}
```

### Runtime capture is a compile error

Sema walks the anonymous-function body and resolves every name. If a name
resolves to a **runtime local** in an enclosing function (i.e., a `let`
binding or runtime parameter, as opposed to a comptime parameter, a
module-level item, or a name introduced inside the anonymous function
itself), it is a compile error:

```
error: anonymous functions cannot capture runtime locals
  --> file.gruel:5:27
   |
 4 |     let step = 1;
 5 |     fn(x: i32) -> i32 { x + step }
   |                             ^^^^ `step` is a runtime local of `outer`
   = note: pass runtime values explicitly, or define a struct with a `__call` method.
```

References to comptime parameters, module-level items (functions, constants,
types), and names introduced inside the anonymous function itself are all
fine.

### Generic higher-order methods

With lambdas in place, `Vec(T).map` becomes:

```gruel
fn Vec(comptime T: type) -> type {
    struct {
        data: *T,
        len: usize,
        cap: usize,

        fn map(self, comptime U: type, comptime F: type, f: F) -> Vec(U) {
            let out: Vec(U) = Vec(U)::with_capacity(self.len);
            let i: usize = 0;
            while i < self.len {
                out = out.push(f.__call(self.data[i]));
                i = i + 1;
            }
            out
        }
    }
}
```

- `F` is a comptime type parameter; the monomorphizer specializes `map` for
  each concrete lambda type at each call site.
- `f.__call(...)` is checked per-specialization: if the concrete `F` has no
  `__call` method with the right signature, that specialization fails.
- Callers write `v.map(U, fn(x: T) -> U { ... })`; comptime-F inference from
  the argument type is desirable but not required in the first pass (see Open
  Questions).

### What this ADR does **not** include

- No runtime environment capture.
- No named function-pointer type like `fn(i32) -> i32`. (Module-level `fn`s
  remain "items," not values of a function type. A later ADR can add a
  conversion from `fn` items to a struct-with-`__call` form if we want them to
  interop with lambda-accepting APIs.)
- No `Fn`/`FnMut`/`FnOnce` traits. There is no trait system.
- No type inference for lambda parameter types from context; they must be
  annotated.
- No `move`/borrow capture keywords; there is no capture.

## Implementation Phases

Each phase is independently committable and ends at a green `make test`. All
phases are gated by preview feature `anon_functions`.

### Phase 1: Preview flag + lexer/parser

- [ ] Add `PreviewFeature::AnonFunctions` with name `"anon_functions"` and ADR
      reference `"ADR-0055"`. Update `name`, `adr`, `all`, `FromStr`, and the
      existing enum tests in `gruel-error`.
- [ ] Parser: accept `fn(params) { block }` and `fn(params) -> T { block }`
      at expression position, including the zero-parameter `fn() { ... }`
      form. Reuse the named-function parameter-list and return-type productions
      verbatim; the only difference is the absence of an identifier after
      `fn`. At item position, continue to require an identifier and produce a
      clear error (not a silent reinterpretation) if one is missing at top
      level.
- [ ] AST: add `Expr::AnonFn { params: Vec<Param>, ret: Option<TypeExpr>, body: Block }`,
      reusing the `Param` and `Block` types from named functions.
- [ ] Parser unit tests for each form: zero-parameter, multi-parameter, with
      and without return type, nested anonymous functions, inside `match`
      arms, inside call-argument lists, and used as a statement expression.

**Deliverable**: `cargo run -p gruel -- --emit ast` shows an `AnonFn` node for
`fn(x: i32) -> i32 { x + 1 }` and for the other syntactic forms; compiling
does *not* yet succeed.

### Phase 2: RIR lowering to synthetic anonymous struct

- [ ] Extend `InstData::AnonStructType` (or add a sibling) with an
      `origin: AnonStructOrigin { Explicit, Lambda }` tag.
- [ ] RIR: lower `Expr::AnonFn { params, ret, body }` to the sequence:
      (a) a lambda-tagged anonymous struct type with one method
      `fn __call(self, <params>) -> <ret> { <body> }`,
      (b) an empty struct-literal construction of that type.
- [ ] Ensure the lowered struct preserves the source span of the original
      `fn(...)` expression so error messages point at the user's source, not
      the synthesized struct.
- [ ] RIR unit tests verifying the shape of the lowering.

**Deliverable**: RIR for `fn(x: i32) -> i32 { x + 1 }` matches a hand-written
`struct { fn __call(self, x: i32) -> i32 { x + 1 } } {}`.

### Phase 3: Sema — uniqueness, call-sugar, runtime-capture check

- [ ] Structural dedup in sema skips `AnonStructOrigin::Lambda` structs: each
      `fn(...)` expression produces a fresh `StructId`.
- [ ] Add call-sugar: when resolving `f(args...)` where `f`'s type is a struct
      with a method named `__call`, rewrite to `f.__call(args...)`. Add a
      clear error when `f` is not callable and does not have a `__call`
      method.
- [ ] Add the runtime-capture check: during sema of an anonymous-function
      body, flag any name resolution that binds to a runtime local in an
      enclosing function. Names resolving to comptime parameters, module
      items, or bindings introduced inside the anonymous function are allowed.
- [ ] Diagnostics: error code + message shown above, with a note pointing to
      the binding that was captured.
- [ ] Sema unit tests for uniqueness (two same-signature `fn(...)` expressions
      are different types), call-sugar, comptime-param use inside the body,
      and the runtime-capture rejection path.

**Deliverable**: `let f = fn(x: i32) -> i32 { x + 1 }; f(3)` compiles and
runs, and runtime capture is a compile error with a good message.

### Phase 4: Higher-order methods and codegen validation

- [ ] No codegen changes expected (the feature desugars to existing struct +
      method machinery). Phase confirms this with end-to-end tests.
- [ ] Add at least one generic higher-order method as an end-to-end test: a
      minimal `apply(self, comptime U: type, comptime F: type, f: F) -> U`
      returning `f.__call(self)` on a tiny wrapper struct, used with multiple
      `fn(...)` expressions in the same program to verify per-site
      monomorphization.
- [ ] Optional but recommended: implement `Vec(T).map` (or a toy equivalent if
      `Vec` is not yet in-tree) to exercise the feature at realistic scale.

**Deliverable**: `vec.map(i32, fn(x: i32) -> i32 { x + 1 })` (or the toy
equivalent) compiles, runs, and produces correct results.

### Phase 5: Specification and tests

- [ ] Add a spec section under expressions for anonymous functions, covering
      syntax, desugaring, type uniqueness, comptime-parameter reference, and
      the runtime-capture prohibition. Give the section an ID slot so existing
      traceability tooling links tests to paragraphs.
- [ ] Spec tests in `crates/gruel-spec/cases/expressions/anon_functions.toml`,
      preview-gated on `anon_functions`. Cover: single-parameter, multi-
      parameter, zero-parameter, omitted return type (unit), call-site sugar,
      comptime-param capture, per-call-site monomorphization, use inside
      `match`/`if` arms, nested anonymous functions, and generic higher-order
      methods.
- [ ] UI tests in `crates/gruel-ui-tests/cases/diagnostics/` for: runtime-
      capture error, `__call`-method missing, mismatched `__call` signature at
      monomorphization, missing parameter type annotation.
- [ ] 100% traceability for new normative paragraphs (required by
      `make test`).

**Deliverable**: `make test` green with the new spec section fully covered.

### Phase 6: Stabilization (follow-up, not this ADR's scope to land)

- [ ] Remove `preview = "anon_functions"` from spec tests and the
      `require_preview` call in sema.
- [ ] Remove `PreviewFeature::AnonFunctions`.
- [ ] Update ADR status to `implemented`.

## Consequences

### Positive

- **Unblocks `map` and friends** today, without a trait system, without
  function-pointer types, and without a capture model.
- **No new runtime support, no new IR kinds.** The feature is pure desugaring
  into ADR-0029 machinery plus a targeted call-sugar in sema.
- **Per-call-site monomorphization** gives us zero-overhead generic higher-
  order APIs — each lambda is its own type, so the optimizer can inline through
  `__call` just like any other method.
- **Consistent with the Zig-inspired model** already established by ADR-0029
  and ADR-0039.
- **Forward-compatible** with a future `move`/borrowing closure story: a
  later ADR can introduce runtime-capturing lambdas that desugar to a struct
  with *fields* plus `__call`, reusing this same machinery.

### Negative

- **Parameter types must be annotated.** `vec.map(i32, fn(x) { x + 1 })`
  doesn't type-check; users must write `fn(x: T) -> T { x + 1 }`. This matches
  named functions today, and we can relax it later (Open Questions).
- **The method name `__call` is now load-bearing.** Any struct whose method
  is named `__call` becomes callable via function syntax. The `__` prefix
  follows Gruel's compiler-reserved-name convention so this is unlikely to
  collide with ordinary user method names, but it is still a language
  commitment.
- **Anonymous struct origin is now a real concept.** Sema must track
  `Explicit` vs `Lambda` to know when to dedup. This is a small complexity tax
  inside sema.
- **No runtime capture is a real limitation.** Users who want something like
  `fn(x: i32) -> i32 { x + step }` with a runtime `step` must define a small
  named struct with a `__call` method. The error message should point at this
  pattern.

### Neutral

- Anonymous functions produce ZST values; a variable holding one costs zero
  stack bytes and passing one by value is a no-op.
- Call-site monomorphization can inflate generated code if the same generic
  function is instantiated with many distinct anonymous-function arguments.
  This is the normal cost/benefit of monomorphization and is no different
  from Rust generics.

## Open Questions

1. **Inferring parameter types from context.**

   With bidirectional types (ADR-0002), we could infer `fn(x) { ... }` when
   the surrounding expression tells us the expected signature — e.g., if
   `map` is specialized enough that the compiler knows the argument type.
   This would also diverge from named-function rules, which always require
   annotations. Do we want to pay the sema complexity (and the asymmetry with
   named `fn`s) for this in v1?

   **Tentative decision**: follow-up. Ship v1 with mandatory annotations,
   matching named functions exactly; revisit once we have real callers.

2. **Call-sugar scope: only `__call`, or a designated attribute?**

   Rust uses a trait (`Fn`); Python uses `__call__`; Zig uses only
   `@call`/method syntax. We chose `__call` — a reserved `__`-prefixed name
   consistent with existing compiler-internal identifiers — as the magic
   method name.

   **Tentative decision**: stick with `__call`. If we later adopt traits, we
   can formalize it as a `Callable` trait whose single method is `__call`
   without breaking source.

3. **Should module-level `fn foo` values be coercible to a lambda-like
   struct?**

   Today `foo` (the identifier) at an expression position is not a first-class
   value in Gruel. If we want to be able to write `vec.map(i32, increment)`
   for a top-level `fn increment`, we need a `fn`-item-to-struct coercion.

   **Tentative decision**: out of scope for this ADR. Note in Future Work.

4. **Should the desugared struct method's receiver be `self` or `&self`?**

   With zero fields, it does not matter for correctness or cost. Going with
   `self` (by value) keeps us out of the borrow-mode story (ADR-0013) for the
   first pass. A future runtime-capture ADR can revisit.

## Future Work

- **Runtime capture** in a later ADR, layered on top of this one: the
  desugared struct gets fields for captured values, and the ADR owns the
  capture-mode question (move vs. borrow, affine interactions) and whatever
  syntactic marker (e.g., a `move` prefix or a capture list) we adopt.
- **Function-item coercion** so `vec.map(i32, foo)` works when `foo` is a
  top-level `fn`.
- **Parameter-type inference** from context.
- **`FnOnce`-like one-shot semantics** if/when we get a trait system; until
  then, "stateful callables" are expressed as user-defined structs with
  `__call`.
- **`@call` intrinsic** for reflecting on arbitrary callable values (probably
  unnecessary once the sugar is in place).

## References

- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md) — the
  foundation; this ADR is the concrete resolution of its Open Question #3.
- [ADR-0039: Anonymous Enum Types](0039-anonymous-enum-types.md) — same
  comptime-capture rules inherited here.
- [ADR-0025: Compile-Time Execution](0025-comptime.md) — monomorphization is
  what makes per-call-site lambda typing free.
- [ADR-0002: Single-Pass Bidirectional Types](0002-single-pass-bidirectional-types.md)
  — relevant to the future parameter-inference work.
- [Zig Language Reference: Anonymous Struct Literals](https://ziglang.org/documentation/master/#Anonymous-Struct-Literals) — the model we're following.
