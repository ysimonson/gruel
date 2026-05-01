+++
title = "Result Type"
weight = 13
template = "spec/page.html"
+++

# The `Result(T, E)` Type

{{ rule(id="3.13:1", cat="normative") }}

The compiler unconditionally registers a canonical `Result(T, E)` generic
enum in every compilation. The definition is equivalent to

```gruel
fn Result(comptime T: type, comptime E: type) -> type {
    enum { Ok(T), Err(E) }
}
```

and is available without any `import` or `use` directive. User code **MUST
NOT** redefine the name `Result`; doing so is a duplicate-definition error
at the redefinition site.

{{ rule(id="3.13:2", cat="informative") }}

`Result(T, E)` flows through the standard enum-with-data machinery (§4.7).
Pattern matching, exhaustiveness checks, and codegen do not special-case
it. Niche optimizations (ADR-0069) apply transparently when `T` or `E`
carries forbidden bit patterns.

{{ rule(id="3.13:3", cat="normative") }}

`Result(T, E)` ships with the following methods, defined in the prelude:

- `fn is_ok(borrow self) -> bool` — true iff the receiver is `Ok`.
- `fn is_err(borrow self) -> bool` — true iff the receiver is `Err`.
- `fn unwrap(self) -> T` — returns the contained `Ok` value, or panics
  with `"called unwrap on an Err value"` if `Err`.
- `fn unwrap_err(self) -> E` — returns the contained `Err` value, or
  panics with `"called unwrap_err on an Ok value"` if `Ok`.
- `fn unwrap_or(self, default: T) -> T` — returns the contained `Ok`
  value if `Ok`, otherwise consumes and returns `default`.
- `fn expect(self, msg: String) -> T` — returns the contained `Ok`
  value, or panics with `msg` if `Err`.
- `fn expect_err(self, msg: String) -> E` — returns the contained
  `Err` value, or panics with `msg` if `Ok`.

Methods that consume the receiver follow the standard "all enum types
are Copy" simplification (§3.8:2), so the receiver is implicitly
duplicated at the call site for non-linear payloads. Linear payload
types fall back to exhaustive `match` at the use site (see §3.8 for
`Option(T:Linear)` precedent).

{{ rule(id="3.13:4", cat="normative") }}

Linearity propagates: `Result(T, E)` is linear if `T` or `E` is linear.
The propagation falls out of the standard `is_type_linear` recursion
through enum payloads (ADR-0067).

{{ rule(id="3.13:5", cat="informative") }}

`Result(T, E)` for linear `T` or `E` cannot be instantiated through the
prelude in v1: the borrow-`self` methods (`is_ok`, `is_err`) pattern
match with discard patterns (`Self::Ok(_)`, `Self::Err(_)`), which the
borrow checker rejects when the discarded variant carries a linear
payload — even though `_` consumes nothing. This is the same deferred
limitation ADR-0067 Phase 3 documents for `Option(T:Linear)`. A
follow-up ADR will lift it (either smarter discard-pattern handling on
borrowed values or per-`T` method gating in the prelude). Until then,
fallible-with-linear-payload code uses ad-hoc enum types declared at
the use site.

{{ rule(id="3.13:6", cat="normative") }}

`Result(T, E)` is `Clone` if `T: Clone` and `E: Clone`. Under v1's
"all enum types are Copy" simplification (§3.8:2), `Result(T, E)` is
`Copy` whenever both `T` and `E` are `Copy`, and therefore `Clone` via
the auto-conformance rule (§3.8:71). Once §3.8:2 is refined,
hand-written enum-clone synthesis (or `@derive(Clone)` on enums) will
take over — that work is shared with `Option(T)` (ADR-0065).
