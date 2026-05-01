+++
title = "Char Type"
weight = 14
template = "spec/page.html"
+++

# The `char` Type

{{ rule(id="3.14:1", cat="normative") }}

The `char` type represents a single Unicode scalar value: any codepoint in
`U+0000..=U+D7FF` or `U+E000..=U+10FFFF`. The forbidden ranges
(`U+D800..=U+DFFF`, the surrogate range, and `U+110000..=U+FFFFFFFF`,
beyond Unicode) are not valid `char` values; the compiler must reject any
construction path that would produce them, and the runtime never observes
such bit patterns in a well-formed program.

{{ rule(id="3.14:2", cat="normative") }}

A `char` occupies 4 bytes (32 bits) of storage with 4-byte alignment.
The value is stored as the codepoint itself, encoded as a u32. The
forbidden bit patterns are exposed as niches via the layout abstraction
(ADR-0069); types like `Option(char)` therefore occupy 4 bytes with no
discriminant byte (§3.14:8).

{{ rule(id="3.14:3", cat="syntax") }}

Char literals use single-quoted syntax:

```
char-literal ::= "'" ( char-content | char-escape ) "'"
char-content ::= <any Unicode scalar except "'", "\", LF, CR>
char-escape  ::= "\n" | "\r" | "\t" | "\\" | "\'" | "\"" | "\0"
               | "\u{" hex-digit (1-6 times) "}"
```

The contained character is one Unicode scalar value. A multi-byte source
character (e.g. `'é'`) is valid as long as it decodes to a single scalar.

{{ rule(id="3.14:4", cat="legality-rule") }}

The following are compile-time errors:

- An empty char literal (`''`).
- An unterminated char literal (no closing `'` before end of line or
  end of input).
- A char literal containing more than one Unicode scalar
  (`'ab'`, `'\\u{1F600}\\u{1F600}'`).
- An invalid escape sequence (e.g. `'\x'`).
- A `\u{...}` escape whose value is a surrogate or exceeds `0x10FFFF`.

{{ rule(id="3.14:5", cat="normative") }}

`char` is `Copy` (no destructor, 4-byte bitwise copy). `char` is `Clone`
via the auto-conformance rule for `Copy` types (§3.8:71).

{{ rule(id="3.14:6", cat="normative") }}

The comparison operators `==`, `!=`, `<`, `<=`, `>`, `>=` are defined
between two `char` values by codepoint magnitude (treating the storage
as an unsigned u32). Arithmetic operators (`+`, `-`, `*`, `/`, `%`) and
bitwise operators (`&`, `|`, `^`, `~`, `<<`, `>>`) are **not** defined
on `char`. Conversion through `to_u32` and `from_u32` is the explicit
path for codepoint arithmetic.

{{ rule(id="3.14:7", cat="normative") }}

`char` ships with the following methods, defined on the primitive type:

- `fn to_u32(self) -> u32` — infallible cast to the underlying
  codepoint value.
- `fn len_utf8(self) -> usize` — returns 1, 2, 3, or 4: the number of
  bytes needed to encode `self` as UTF-8.
- `fn is_ascii(self) -> bool` — returns true iff `self.to_u32() < 128`.
- `fn encode_utf8(self, buf: &mut [u8; 4]) -> usize` — writes the
  UTF-8 encoding of `self` into `buf` and returns the byte count
  (1-4, equal to `len_utf8()`).

Associated functions:

- `char::from_u32(n: u32) -> Result(char, u32)` — returns `Ok(c)` if
  `n` is a valid Unicode scalar value; otherwise `Err(n)` (the
  offending input is preserved for diagnostics).
- `char::from_u32_unchecked(n: u32) -> char` — only callable inside a
  `checked` block. Caller asserts that `n` is a valid scalar value;
  invoking this with a surrogate or `n > 0x10FFFF` is undefined
  behavior.

Casts via `as` between `char` and `u32` are not provided; use the
explicit method calls above.

{{ rule(id="3.14:8", cat="informative") }}

Because `char` carries niches at `U+D800..=U+DFFF` and
`U+110000..=U+FFFFFFFF`, the layout pass (ADR-0069) elides the
discriminant byte for niche-filling enums whose only unit variant fits
into one of these ranges. Concretely:

- `Option(char)` is 4 bytes, alignment 4 (no discriminant).
- `Option(Option(char))` is 4 bytes (consumes two niche values).
- `Result(char, ())` is 4 bytes.

{{ rule(id="3.14:9", cat="dynamic-semantics") }}

`char::from_u32(n)` performs a runtime range check: if
`0xD800 <= n <= 0xDFFF` or `n > 0x10FFFF`, the function returns
`Result::Err(n)`. Otherwise it returns `Result::Ok(c)` where the bit
pattern of `c` equals `n`.

`char::from_u32_unchecked(n)` performs no validation: the bit pattern
of `n` is reinterpreted as a `char`. If `n` is not a valid scalar
value, the resulting `char` violates §3.14:1 and any subsequent use
(in particular `encode_utf8` or pattern matching) is undefined.

{{ rule(id="3.14:10", cat="dynamic-semantics") }}

`encode_utf8(c, buf)` writes the canonical UTF-8 encoding of `c` to
`buf`:

- 1 byte (`c.to_u32() < 0x80`): `0xxxxxxx`.
- 2 bytes (`0x80..=0x7FF`): `110xxxxx 10xxxxxx`.
- 3 bytes (`0x800..=0xFFFF`): `1110xxxx 10xxxxxx 10xxxxxx`.
- 4 bytes (`0x10000..=0x10FFFF`): `11110xxx 10xxxxxx 10xxxxxx 10xxxxxx`.

The unwritten suffix of `buf` (bytes at indices >= the returned count)
is left unchanged.
