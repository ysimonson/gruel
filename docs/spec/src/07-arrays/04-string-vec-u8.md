+++
title = "String / Vec(u8) Relationship"
weight = 4
+++

# String / Vec(u8) Relationship

This section documents `String` as a newtype wrapper over `Vec(u8)` per
ADR-0072. Everything in this section is gated behind the
`string_vec_bridge` preview feature; using any of these APIs without
`--preview string_vec_bridge` is a compile-time error.

## Newtype Definition

{{ rule(id="7.4:1", cat="normative") }}

`String` is a synthetic struct injected by the compiler. Conceptually:

```gruel
synthetic struct String {
    bytes: Vec(u8)   // private
}
```

The `bytes` field is **private**: outside of `String`'s own methods,
sema rejects any field-access or assignment that names `bytes` on a
`String` value with a "private field" diagnostic. Public access goes
through the conversion API (§7.4:5–7) and the method surface
inherited by composition.

The runtime layout is identical to `Vec(u8)` — a single
`{ ptr, len, cap }` aggregate (24 bytes on 64-bit targets, 8-byte
aligned). `String` is affine; drop runs the contained `Vec(u8)`'s drop.

## UTF-8 Invariant

{{ rule(id="7.4:2", cat="normative") }}

Every well-formed `String` value upholds the invariant:

> The bytes in `self.bytes[0..self.bytes.len()]` form a valid UTF-8
> sequence.

The invariant is established at construction time:

- `String::new()` and `String::with_capacity(n)` produce empty buffers,
  which are trivially valid UTF-8.
- String literals are UTF-8 by source-file enforcement (§2.1).
- `String::from_utf8(v)` validates `v`'s contents at runtime and only
  yields `Ok` when validation succeeds.
- `String::push(c: char)` and `String::from_char(c)` encode a Unicode
  scalar value into UTF-8 by construction (§3.x).
- The `checked` constructors (`from_utf8_unchecked`, `push_byte`,
  `from_c_str_unchecked`) shift the obligation to the caller.

Methods that mutate the buffer (`push_str`, `concat`, `clear`,
`reserve`, `clone`, `push`, `push_byte`) preserve the invariant by
appending only valid-UTF-8 byte sequences to an already-valid buffer
(`push_byte` is the documented exception, see §7.4:8).

## Method Surface

{{ rule(id="7.4:3", cat="normative") }}

`String`'s method surface is defined by composition over the inner
`Vec(u8)`:

| Method | Effect |
|---|---|
| `String::new() -> String` | Empty `String`. |
| `String::with_capacity(n) -> String` | Empty `String` with `cap >= n`. |
| `s.bytes_len() -> usize` | Byte count (not codepoint count). |
| `s.bytes_capacity() -> usize` | Byte capacity. |
| `s.is_empty() -> bool` | `bytes_len() == 0`. |
| `s.clone() -> String` | Deep copy of the inner buffer. |
| `s.contains(needle: String) -> bool` | Byte-substring search. |
| `s.starts_with(prefix: String) -> bool` | Byte-prefix check. |
| `s.ends_with(suffix: String) -> bool` | Byte-suffix check. |
| `s.concat(other: String) -> String` | Allocate `len(self)+len(other)` bytes; copy both. |
| `s.push_str(other: String) -> Self` | Append `other`'s bytes in place. |
| `s.clear() -> Self` | Set `len = 0`; `cap` preserved. |
| `s.reserve(n: usize) -> Self` | Ensure `cap >= len + n`. |

Equality (`==`, `!=`) and ordering (`<`, `<=`, `>`, `>=`) on
`String` operate on the inner `Vec(u8)` lexicographically.

The legacy `s.len()` and `s.capacity()` accessors remain available as
synonyms for `bytes_len` and `bytes_capacity`. Future `chars_len` will
provide codepoint counting once iterators land.

## Vec(u8) Method Additions

{{ rule(id="7.4:4", cat="normative") }}

For `String`'s composition surface to delegate cleanly, `Vec(u8)`
gains the following methods alongside its existing surface:

- `Vec(T)::contains(borrow self, needle: borrow Slice(T)) -> bool`
- `Vec(T)::starts_with(borrow self, prefix: borrow Slice(T)) -> bool`
- `Vec(T)::ends_with(borrow self, suffix: borrow Slice(T)) -> bool`
- `Vec(T)::concat(borrow self, other: borrow Slice(T)) -> Vec(T)`
- `Vec(T)::extend_from_slice(inout self, other: borrow Slice(T)) -> ()`

These are byte/element-level operations and apply uniformly to any
`Vec(T)` whose element type supports byte-comparison (`u8`, etc.); the
v1 instantiation targets `Vec(u8)`. They are not gated behind
`string_vec_bridge` — they are independent `Vec(T)` improvements.

## Conversions: String → Vec(u8)

{{ rule(id="7.4:5", cat="normative") }}

`String::into_bytes(self) -> Vec(u8)` consumes the `String` and yields
the underlying `Vec(u8)` in O(1). It is a struct-field move with no
allocation, no copy, and no validation cost.

```gruel
fn main() -> i32 {
    let s = String::from_char('A');
    let v: Vec(u8) = s.into_bytes();
    v.len() as i32
}
```

## Conversions: Vec(u8) → String validated

{{ rule(id="7.4:6", cat="normative") }}

`String::from_utf8(v: Vec(u8)) -> Result(String, Vec(u8))` performs an
O(n) UTF-8 scan over `v`'s live `[0..len]` range. On success it returns
`Result::Ok(s)` with `s` adopting `v`'s buffer (no copy). On failure it
returns `Result::Err(v)` and the buffer is handed back unchanged so the
caller may inspect, retry, or report without a defensive `clone`.

The `Vec(u8).into_string(self) -> Result(String, Vec(u8))` method is a
sugar synonym for `String::from_utf8(self)`.

## Conversions: Vec(u8) → String trusted

{{ rule(id="7.4:7", cat="normative") }}

Inside a `checked` block:

- `String::from_utf8_unchecked(v: Vec(u8)) -> String` constructs a
  `String` with `v` as the byte buffer in O(1) without validation.
- `Vec(u8).into_string_unchecked(self) -> String` is the
  method-call sugar.

The caller is obligated to uphold the UTF-8 invariant. Constructing an
ill-formed `String` via these APIs is undefined behavior — subsequent
calls that rely on the invariant (codepoint iteration, slicing) may
exhibit arbitrary behavior.

## Mutation: push and push_byte

{{ rule(id="7.4:8", cat="normative") }}

Two mutators write bytes into a `String`:

- `s.push(c: char) -> Self` — safe; encodes `c` to UTF-8 (1–4 bytes,
  per §3.x for `char`) and appends those bytes to the buffer. The
  invariant is preserved by construction.
- `s.push_byte(b: u8) -> Self` — only callable inside a `checked`
  block. Appends a single raw byte. The caller is obligated to
  preserve the UTF-8 invariant; the compiler does not validate.

The legacy `String::push(byte: u8)` is renamed to `push_byte` and
gated to `checked`. The new `push(c: char)` becomes the primary
codepoint-aware mutator.

```gruel
fn main() -> i32 {
    let mut s = String::new();
    s.push('H');     // 1 byte
    s.push('é');     // 2 bytes
    s.push('🦀');    // 4 bytes
    s.bytes_len() as i32   // 7
}
```

## C Interop

{{ rule(id="7.4:9", cat="normative") }}

Inside a `checked` block:

- `s.terminated_ptr() -> Ptr(u8)` — ensures `cap > len`, writes a NUL
  byte at `ptr[len]`, and returns the buffer pointer suitable for
  passing to a C function expecting a NUL-terminated string. The
  sentinel sits outside the live `[0..len]` range and is overwritten
  by the next mutating call. Delegates to `Vec(u8)::terminated_ptr(0u8)`.
- `String::from_c_str(p: Ptr(u8)) -> Result(String, Vec(u8))` —
  computes `strlen(p)`, allocates a `Vec(u8)` of that size, copies the
  bytes, then forwards to `from_utf8`.
- `String::from_c_str_unchecked(p: Ptr(u8)) -> String` — same copy,
  then forwards to `from_utf8_unchecked`.

```gruel
checked {
    fn write_label(s: String) {
        let mut s = s;
        let p = s.terminated_ptr();
        // pass `p` to a C function expecting `const char *`...
    }
}
```

`from_c_str` and `from_c_str_unchecked` always copy; Gruel cannot
adopt foreign-allocated buffers because it does not know the
allocator.

## Privacy of Synthetic Fields

{{ rule(id="7.4:10", cat="legality-rule") }}

Synthetic built-in struct fields may carry a private flag. Access to a
private field outside of the type's own methods is a compile-time
error with diagnostic kind "private field". The mechanism is narrow —
it exists to hide internal state of synthetic builtins (currently only
`String::bytes`) and is replaced by the general visibility / module
system when that lands. User-defined structs are unaffected; their
fields are public per §6.2.

## Examples

{{ rule(id="7.4:11", cat="example") }}

Round-trip a `String` through `Vec(u8)`:

```gruel
fn main() -> i32 {
    let s = String::from_char('Z');
    let v = s.into_bytes();
    match String::from_utf8(v) {
        Result::Ok(s2) => s2.bytes_len() as i32,
        Result::Err(_) => -1,
    }
}
```

{{ rule(id="7.4:12", cat="example") }}

Reject invalid UTF-8:

```gruel
fn main() -> i32 {
    checked {
        let mut v: Vec(u8) = Vec(u8)::with_capacity(2);
        v.push(0xFFu8);
        v.push(0xFEu8);
        match String::from_utf8(v) {
            Result::Ok(_) => 0,
            Result::Err(_) => 1,   // expected
        }
    }
}
```
