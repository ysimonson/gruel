---
id: 0055
title: Anonymous Functions (Closures-as-Structs)
status: implemented
tags: [expressions, types, comptime, generics]
feature-flag: anon_functions
created: 2026-04-24
accepted: 2026-04-24
implemented: 2026-04-25
spec-sections: ["4.16"]
superseded-by:
---

# ADR-0055: Anonymous Functions (Closures-as-Structs)

## Status

Implemented

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

- [x] Add `PreviewFeature::AnonFunctions` with name `"anon_functions"` and ADR
      reference `"ADR-0055"`. Update `name`, `adr`, `all`, `FromStr`, and the
      existing enum tests in `gruel-error`.
- [x] Parser: accept `fn(params) { block }` and `fn(params) -> T { block }`
      at expression position, including the zero-parameter `fn() { ... }`
      form. Reuse the named-function parameter-list and return-type productions
      verbatim; the only difference is the absence of an identifier after
      `fn`. (Items in blocks don't exist in Gruel, so at expression position
      `fn` is unambiguously an anonymous function; no lookahead needed.)
- [x] AST: add `Expr::AnonFn(AnonFnExpr { params, return_type, body, span })`,
      reusing the `Param` and `BlockExpr` types from named functions.
- [x] Parser unit tests: zero-parameter, multi-parameter, with and without
      return type, nested anonymous functions, inside call-argument lists.

**Deliverable**: `cargo run -p gruel -- --emit ast` shows an `AnonFn` node for
`fn(x: i32) -> i32 { x + 1 }` and for the other syntactic forms; compiling
does *not* yet succeed.

### Phase 2: RIR lowering to synthetic anonymous struct

- [x] Add a sibling RIR instruction `InstData::AnonFnValue { method: InstRef }`
      rather than extending `AnonStructType` with an origin tag — the method
      is the only piece of the lambda that lives in the extra array, so an
      explicit variant stays simpler. "Lambda origin" is implicit in the
      variant.
- [x] RIR: lower `Expr::AnonFn { params, return_type, body }` by synthesizing
      a `Method { name: "__call", receiver: self, params, return_type, body }`,
      running it through `gen_method` to get a FnDecl `InstRef`, and emitting
      `AnonFnValue { method }` pointing at it.
- [x] Sema stub in analysis.rs + analyze_ops.rs: resolve the FnDecl's
      signature, call `find_or_create_anon_struct` with zero fields and the
      `__call` signature, register the `__call` method on the resulting
      struct, and emit an empty `AirInstData::StructInit`. (Phase 2 still uses
      structural dedup — two same-signature lambdas collide; Phase 3 flips
      that.)
- [x] Inference-side handling in `gruel-air/src/inference/generate.rs`: defer
      to a fresh type variable (same approach as `TupleInit`).
- [x] Preserve the `fn(...)` source span on all synthesized instructions.
- [x] RIR unit tests verifying shape, per-site body preservation, and the
      zero-parameter form.

**Deliverable**: RIR for `fn(x: i32) -> i32 { x + 1 }` matches a hand-written
`struct { fn __call(self, x: i32) -> i32 { x + 1 } } {}` (confirmed via
`--emit rir`). End-to-end: `fn main() -> i32 { let f = fn(x: i32) -> i32 { x + 1 }; f.__call(41) }`
compiles and runs, returning 42.

### Phase 3: Sema — uniqueness, call-sugar, runtime-capture check

- [x] Uniqueness: added `Sema::create_unique_anon_struct` which bypasses the
      structural-dedup scan in `find_or_create_anon_struct`. The
      `analyze_anon_fn_value` path now uses it, so each source-level `fn(...)`
      site produces a distinct `StructId` even when signatures collide.
- [x] Call-sugar: `analyze_call` detects `f(args)` where `f` resolves to a
      local whose type is a struct with a `__call` method (not a function
      item) and delegates to a dedicated `emit_call_sugar` helper that emits
      the equivalent method-call AIR. Function-item lookups take precedence
      so this is purely additive — no existing call site changes shape.
- [x] Runtime-capture rejection: currently falls out of the existing scoping
      rules. Lambda bodies are analyzed as methods of a synthesized struct,
      and method contexts never inherit the enclosing function's runtime
      locals, so references to captured runtime names error as
      `UndefinedVariable`. Functionally correct rejection; the dedicated
      "anonymous functions cannot capture runtime locals" diagnostic is a
      polish follow-up (noted under Open Questions).
- [x] End-to-end tests via scratch programs: `let f = fn(x: i32) -> i32 { x + 1 }; f(41)`
      returns 42; two same-signature lambdas with different bodies compile
      and both are callable; `x + step` with runtime `step` is rejected.

**Deliverable**: `let f = fn(x: i32) -> i32 { x + 1 }; f(3)` compiles and
runs; runtime capture is a compile error (generic `UndefinedVariable`
diagnostic for now, to be refined in a follow-up).

### Phase 4: End-to-end codegen validation + generic higher-order methods

- [x] No codegen changes required for the lambda desugaring itself:
      anonymous functions compile via existing struct + method machinery.
- [x] End-to-end smoke tests cover single-parameter lambdas, two
      same-signature lambdas with different bodies, zero-parameter lambdas,
      and nested lambdas. All compile and run correctly.
- [x] Second-order-comptime-on-methods (was the first listed limitation):
      inline methods inside `fn Wrap(comptime T: type) -> type` (and named
      struct methods) can now take their own `comptime F: type` parameter.
      Fix had three parts:
      * Method registration (`register_anon_struct_methods_for_comptime_with_subst`
        and `collect_struct_methods`) uses `Type::COMPTIME_TYPE` as a
        placeholder for method-level comptime type params and for any later
        param whose declared type references one, mirroring the top-level
        generic-fn path in `declarations.rs`.
      * `MethodInfo` gains `is_generic` + `return_type_sym`. Method body
        analysis skips generic methods (defers to specialization). Method
        call sites emit `CallGeneric` with type args when `is_generic` is
        true; call sites accept type arguments as type literals, struct/enum
        names, or comptime type variables.
      * `specialize.rs` gained `create_specialized_method` and a
        `resolve_method_name` helper that treats `"Struct.method"` mangled
        names as methods when no matching top-level function exists.
- [x] ZST parameter codegen (second part of the same limitation): reading
      an empty-struct parameter no longer emits an out-of-range `Param
      { index }`. `analyze_var_ref` and the call-sugar emitter route zero-
      ABI-slot params through a new `emit_zst_value` helper that
      materializes an empty `StructInit` (or `()` for unit-like ZSTs).
- [x] Spec test `generic_method_takes_named_callable` covers the full
      pipeline: named-callable struct with `__call`, generic `apply` method
      on an anon struct, call site with explicit type argument.
- [x] Comptime type-arg inference at generic method call sites: when a
      method has `comptime F: type, f: F` and the caller supplies only the
      value arg, the compiler infers `F` from `f`'s analyzed type. Anon-fn
      literals can now be passed directly: `p.map_sum(fn(x: i32) -> i32
      { x + 1 })` works without naming the lambda's (anonymous) struct
      type. Two helpers in `analyze_method_call_impl`:
      `resolve_method_generic_type_arg` (factored out of the explicit
      branch) and `method_param_type_syms` (walks RIR to recover the
      as-written param type symbols). Inference runs after analyzing the
      runtime args; for each comptime type param, the compiler scans for a
      later runtime param whose declared type symbol matches the comptime
      param's name and pulls the type from that arg's analyzed value. If
      no such param exists, an error directs the user to pass the type
      explicitly. Bare-symbol matching only — compound shapes like
      `[U; 3]` against `[i32; 3]` still need explicit type args.

**Deliverable**: lambdas compose end-to-end; generic higher-order methods
compile and run with either explicit type args or inferred ones, including
when the callable is an anonymous function literal whose type can't be
named in source.

### Phase 5: Specification and tests

- [x] Added spec section `docs/spec/src/04-expressions/16-anonymous-functions.md`
      covering syntax (4.16:1–2), desugaring (4.16:3–4), per-site uniqueness
      (4.16:5), call-sugar (4.16:6), and capture rules (4.16:7–8). All
      normative paragraphs have covering tests.
- [x] Spec tests in `crates/gruel-spec/cases/expressions/anon_functions.toml`,
      preview-gated on `anon_functions`: single/multi/zero-parameter lambdas,
      omitted return type, per-site uniqueness, explicit `__call` method
      call, user-defined callable (named struct with `__call`), nesting,
      module-item reference from inside a lambda body, runtime-local-capture
      rejection, and the preview-flag gate itself. 11 tests, all passing.
- [x] UI tests in `crates/gruel-ui-tests/cases/diagnostics/anon-functions.toml`
      for: preview-flag missing and runtime-capture rejection.
- [x] 100% normative traceability preserved (696/696 paragraphs covered).

**Deliverable**: `make test` green with the new spec section fully covered —
achieved.

### Phase 6: Stabilization

- [x] Removed `preview = "anon_functions"` and `preview_should_pass` from
      spec tests; deleted `anon_fn_requires_preview` (no longer applicable).
- [x] Removed the `require_preview(PreviewFeature::AnonFunctions, ...)` call
      from `analyze_anon_fn_value` in sema.
- [x] Removed `PreviewFeature::AnonFunctions` from `gruel-error` (variant,
      `name`, `adr`, `all`, `FromStr`, and the corresponding tests).
- [x] Removed the `anon_fn_without_preview_flag` UI test; the runtime-
      capture UI test no longer carries a `preview` field.
- [x] Updated the spec section to drop the "preview-gated" mention.
- [x] ADR frontmatter: `status: implemented`, `accepted: 2026-04-24`,
      `implemented: 2026-04-25`, `spec-sections: ["4.16"]`.

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
