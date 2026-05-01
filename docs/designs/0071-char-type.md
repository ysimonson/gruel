---
id: 0071
title: char — Unicode Scalar Value Type
status: implemented
tags: [types, primitives, unicode, utf8, niches]
feature-flag: char_type
created: 2026-05-01
accepted: 2026-05-01
implemented: 2026-05-01
spec-sections: ["3.14"]
superseded-by:
---

# ADR-0071: char — Unicode Scalar Value Type

## Status

Implemented.

## Summary

Introduce `char` as a primitive type representing a single **Unicode scalar value** — any codepoint in `U+0000..=U+D7FF` or `U+E000..=U+10FFFF` (i.e., excluding surrogates). Storage is a 32-bit value; the forbidden ranges (`U+D800..=U+DFFF` and `U+110000..=U+FFFFFFFF`) become *niches* exposed to the layout abstraction (ADR-0069), so types like `Option(char)` can be 4 bytes with no discriminant. Char literals use single-quoted syntax (`'a'`, `'\n'`, `'\u{1F600}'`); the lexer produces a new `CharLit` token. Conversions are: infallible `c.to_u32() -> u32`, fallible `char::from_u32(n) -> Result(char, u32)` (the offending `u32` is preserved on failure for diagnostics, courtesy of ADR-0070), and a UTF-8 encoder `c.encode_utf8(buf: &mut [u8; 4]) -> usize` (returns the byte count, 1–4; caller constructs `&buf[..n]` if they want a slice view, since ADR-0064 disallows returning `Slice` from non-intrinsic functions). The minimum useful method surface ships in v1 (`to_u32`, `from_u32`, `len_utf8`, `is_ascii`, `encode_utf8`); Unicode classification (`is_alphabetic`, `to_lowercase`, etc.) is deferred until there's a real reason to ship the Unicode tables. The primary downstream consumer is `String`: with `char` available, `String::push(c: char)` becomes the safe, UTF-8-preserving mutator, and `push_byte` (ADR-0072) drops back to a niche escape hatch in `checked` blocks.

## Context

### Where Gruel sits today

- No `char` type exists. Gruel's primitives are `i8/i16/i32/i64`, `u8/u16/u32/u64`, `usize`, `bool`, `unit`. The lexer has no `'...'` token (single quotes are unused).
- `String` (ADR-0020 / ADR-0072) carries (or, after ADR-0072, *will* carry) a UTF-8 invariant, but has no way to mutate by codepoint — only `push_byte`.
- The layout abstraction (ADR-0069) exists and consumes a `Layout { size, align, niches }` shape. Today it has exactly two niche-bearing types (`bool`, small unit-only enums); the framework was deliberately built to absorb more.
- `Option(T)` (ADR-0065) and (proposed) `Result(T, E)` (ADR-0070) both want niche optimization — but `Option(Ptr(T))` and `Result(NonNull(T), E)` are blocked on `NonNull` / nullability. **`char` is the next type that supplies free niches without further type-system work.**

### Why now

Three forcing functions:

1. **ADR-0072 needs a safe `push`.** Today's `String::push(byte: u8)` is being renamed to `push_byte` and gated behind `checked`. Without `char`, there is no safe path to extend a `String` by a single codepoint — users must construct a one-character `String` literal and `push_str` it, which is awkward and pointless. With `char`, `String::push(c: char)` writes 1–4 UTF-8 bytes safely.
2. **The layout abstraction is starved of niches.** ADR-0069 explicitly notes (§"What Gruel sits like today", point about `char`): *"`char` does not exist. Function pointer types do not exist. `Ptr(T)` is nullable. `Ref(T)` cannot be stored. So `bool` and small unit enums are genuinely the entire candidate niche-bearer set today."* Adding `char` is the largest immediate niche win available — its forbidden ranges are vast (over 4 billion invalid u32 values), so `Option(char)` becomes free, and any `enum E { A, B(char) }` can pack the discriminant into the surrogate range.
3. **It's a small, well-bounded primitive.** Unlike strings or collections, `char` is a single 32-bit value with a short method list. The implementation cost is mostly lexer/parser work plus codegen for a handful of operations. Doing it now, before more APIs accumulate that "could have taken `char`," keeps the migration cheap.

### What this ADR does *not* attempt

- **Unicode classification methods.** `is_alphabetic`, `is_digit`, `to_lowercase`, `to_titlecase`, `is_whitespace`, etc. require shipping (or linking) Unicode property tables — ~50KB compressed. Out of scope. v1 ships only `is_ascii`, which needs no tables.
- **`chars()` / `char_indices()` iterators on `String`.** That requires Gruel's iterator story to be in place (no general iterator interface yet — `for ... in` works on slices and ranges via per-construct lowering). Future work, *enabled* by this ADR.
- **`grapheme cluster` or `extended grapheme` types.** Far out of scope; codepoint-level is the right primitive layer.
- **Type-level handling of UTF-16 surrogate pairs or UCS-2.** Gruel commits to UTF-8 / Unicode scalar values; there's no `u16char` or `wchar` plan.
- **Pattern matching on `char` ranges** (`match c { 'a'..='z' => ... }`). Useful but a separate pattern-matching extension; deferred. v1 supports equality match (`match c { 'a' => ..., _ => ... }`) via the existing literal-pattern machinery.

### Where Gruel lands

- **Rust:** `char` = Unicode scalar value, 32-bit, surrogates excluded. Same model. Rust pays the full Unicode-classification surface; we don't, until needed.
- **Swift:** `Character` is an extended grapheme cluster, not a scalar. Different layer; we mirror Rust's choice of "the primitive is the codepoint."
- **Go:** `rune` = `int32` alias, no validity invariant. Pragmatic but loses the niche win.
- **C/C++:** `char` is a byte; `wchar_t` is platform-dependent. We deliberately don't reuse the name `char` for "byte" — `u8` is byte. Naming `char` for the scalar value matches Rust / Swift and pairs cleanly with String's UTF-8 invariant.

## Decision

### 1. Type and storage

`char` is a primitive type. Storage: 4 bytes (32 bits), aligned to 4 bytes.

The valid value set is `U+0000..=U+D7FF` ∪ `U+E000..=U+10FFFF` (i.e., all Unicode scalar values; surrogates and codepoints ≥ 0x110000 are forbidden).

The forbidden bit patterns become **niches** exposed via the ADR-0069 `Layout` abstraction:

```rust
// In gruel-air's primitive Layout registry:
char => Layout {
    size: 4,
    align: 4,
    niches: smallvec![
        NicheRange { start: 0xD800, end: 0xDFFF },        // surrogates
        NicheRange { start: 0x110000, end: 0xFFFFFFFF },  // out-of-Unicode
    ],
}
```

This makes `Option(char)`, `Result(char, char)`, and similar types tag-free in the layout layer with no further work — the niche-filling logic from ADR-0069 already handles `NicheRange` lists.

### 2. Literal syntax

```gruel
let a: char = 'a';
let nl: char = '\n';
let tab: char = '\t';
let quote: char = '\'';
let bs: char = '\\';
let null: char = '\0';
let smiley: char = '\u{1F600}';      // 😀
let cr: char = '\r';
let zwj: char = '\u{200D}';
```

Lexer-level rules:
- A char literal is `'` followed by a single character or escape, followed by `'`.
- Recognized escapes: `\n`, `\r`, `\t`, `\\`, `\'`, `\"`, `\0`, `\u{H...H}` (1–6 hex digits, value must be a valid scalar).
- The contained "single character" is one Unicode scalar in source (which is UTF-8). Multi-byte source characters (e.g., `'é'`) are valid as long as they decode to one scalar.
- Lexer emits `TokenKind::CharLit(char)` carrying the resolved value.
- Compile error: empty literal (`''`), unterminated literal, multi-character literal (`'ab'`), invalid escape, `\u{}` value outside the valid scalar range.

Grammar appendix gains:
```
char-literal ::= "'" ( char-content | char-escape ) "'"
char-content ::= <any Unicode scalar except "'", "\", LF, CR>
char-escape  ::= "\n" | "\r" | "\t" | "\\" | "\'" | "\"" | "\0"
               | "\u{" hex-digit (1-6 times) "}"
```

### 3. Operators

Comparison operators are defined by codepoint value (numeric u32 comparison):
- `==`, `!=`, `<`, `<=`, `>`, `>=` between two `char` values

No arithmetic operators (`c + 1` is rejected — `c.to_u32() + 1` then `char::from_u32` is the explicit path). Rationale: arithmetic on `char` is a footgun (bumps across the surrogate gap silently); making it explicit is cheap.

### 4. Method surface (v1)

| Method | Receiver | Signature | Notes |
|---|---|---|---|
| `to_u32` | `self` (Copy) | `(self) -> u32` | Infallible cast. |
| `len_utf8` | `self` | `(self) -> usize` | Returns 1, 2, 3, or 4. Computed from codepoint range, not table. |
| `is_ascii` | `self` | `(self) -> bool` | `c.to_u32() < 128`. |
| `encode_utf8` | `self` | `(self, buf: &mut [u8; 4]) -> usize` | Writes UTF-8 bytes to `buf`, returns the byte count (1–4, equal to `len_utf8()`). Caller constructs `&buf[..n]` at the call site for a slice view. Returning `Slice` directly is barred by ADR-0064's non-escape rule for non-intrinsic functions. |

Associated functions:

| Function | Signature | Notes |
|---|---|---|
| `char::from_u32` | `(n: u32) -> Result(char, u32)` | `Err(n)` if `n` is a surrogate or `> 0x10FFFF`; `Ok(c)` otherwise. The offending `u32` is preserved in the `Err` arm for diagnostics. Requires ADR-0070 (`Result`) to land first. |
| `char::from_u32_unchecked` | `(n: u32) -> char` | `checked` block only. Caller asserts validity. Codegen: bitcast. |

Char is `Copy` (4 bytes, no destructors). Char is `Clone` (auto-conformance via Copy, per ADR-0065). No `Drop`.

`is_alphabetic`, `is_digit`, `to_lowercase`, `to_uppercase`, `to_ascii_lowercase`, `to_ascii_uppercase`, `is_whitespace`, etc. are deferred. ASCII variants (`to_ascii_lowercase`, `is_ascii_alphabetic`) are tractable without tables and may be added in a small follow-up.

### 5. UTF-8 encoding

`encode_utf8` lowers to a switch on `len_utf8`:
- 1 byte: `0xxxxxxx`
- 2 bytes: `110xxxxx 10xxxxxx`
- 3 bytes: `1110xxxx 10xxxxxx 10xxxxxx`
- 4 bytes: `11110xxx 10xxxxxx 10xxxxxx 10xxxxxx`

Implementation: inline LLVM in `gruel-codegen-llvm`, no runtime call. ~30 LOC of bit-twiddling.

### 6. `String` integration

Added (or, in concert with ADR-0072, replacing) on `String`:

```gruel
fn String.push(&mut self, c: char) -> Self
fn String::from_char(c: char) -> String
```

`push(c: char)` lowers to: allocate a stack `[u8; 4]`, call `c.encode_utf8(&mut buf)` to get the byte count `n`, then append `&buf[..n]` via the existing `Vec(u8)` extend path (per ADR-0072's method-dispatch consolidation). The slice exists locally inside `push`'s scope, which is legal under ADR-0064.

This is the "safe push" referenced as missing in ADR-0072's Open Questions. With this method available, ADR-0072's `push_byte` recedes to a niche escape hatch (see ADR-0072 update).

`from_char(c)` is sugar for `let mut s = String::new(); s.push(c); s`.

A future `s.chars() -> ...` iterator is deferred (needs iterator infrastructure).

### 7. Niche registration

ADR-0069's niche infrastructure already supports multi-range niches. The work here is purely declarative:

- In `gruel-air/src/types.rs` (or wherever primitive layouts live), register `char`'s `Layout` with the two `NicheRange` entries above.
- The existing niche-filling pass for enums consumes these niches without modification.
- Verify via test: `Option(char)` reports size 4, alignment 4, and round-trips correctly through `Some(c)` / `None`.

Tests should also cover nested cases: `Option(Option(char))` → still 4 bytes (consumes two niche values).

### 8. Sema and codegen

- **Sema:** `char` becomes a `Type::Primitive(PrimitiveKind::Char)` (or extension of the existing primitive enum). Method resolution dispatches to the v1 surface. Casts `char as u32` and `u32 as char` are *not* allowed via `as` — only the explicit `to_u32` / `from_u32` calls. (This avoids the surrogate-bump footgun and matches the no-arithmetic decision.)
- **Codegen (LLVM):** `char` lowers to `i32`. Equality and ordering are unsigned i32 comparison. `to_u32` is a no-op bitcast. `from_u32_unchecked` is a no-op bitcast. `from_u32` lowers to a range check + `Result(char, u32)` constructor. `encode_utf8` is inline bit math.
- **Match patterns:** char literal patterns reuse the existing literal-pattern path (same as `match n { 0 => ..., 1 => ... }`).

### 9. Spec

New section `3.9 The char type` (or wherever the type chapter naturally extends) covering:
- Validity rules (scalar-value ranges).
- Literal syntax (with escape table).
- Operators (comparison only).
- Method surface (the v1 list above).
- Layout (4 bytes, 4-byte align, niches).
- Conversions to/from `u32`.

Grammar appendix gets the `char-literal` production.

## Implementation Phases

**Prerequisites:** ADR-0070 (`Result`) Phases 1–2 must land before this ADR's Phase 4 (`from_u32` returns `Result`). Phases 1–3 and 5–8 are independent of ADR-0070 and can proceed in parallel.

- [x] **Phase 1: Preview gate + spec scaffolding**
  - Add `PreviewFeature::CharType` to `gruel-error`.
  - Draft spec section 3.9 with rule IDs (no implementation yet). *Spec lives in `docs/spec/src/03-types/14-char-type.md` (section 3.14, since 3.9 was already destructors). Ten paragraphs covering type, layout, syntax, errors, ops, methods, niches, dynamic semantics. Frontmatter `spec-sections` updated to `["3.14"]`.*
- [x] **Phase 2: Lexer + parser**
  - `TokenKind::CharLit(u32)` (storing the scalar value as u32 for now).
  - Lexer recognizes `'...'` with all escapes, including `\u{}`. *Implementation in `gruel-lexer/src/logos_lexer.rs::process_char_from_quote`. New `LexError` variants and `ErrorKind` variants for empty/multi/unterminated/invalid-escape/invalid-unicode-escape.*
  - Parser produces `Expr::CharLit` AST node. *Added to `chumsky_parser.rs` literal alternatives; AST `Expr::Char(CharLit)` variant.*
  - Spec tests cover each escape, error tests cover bad literals. *Tests in `crates/gruel-spec/cases/types/char.toml`.*
- [x] **Phase 3: Type + comparison ops**
  - `Type::Primitive(Char)` in `gruel-air` and `gruel-rir`. *Added `TypeKind::Char` (tag 20), `Type::CHAR` constant, `InternedType::CHAR`, `InstData::CharConst(u32)`. Layout: 4-byte scalar; niches deferred to Phase 7.*
  - `char` is `Copy` (4 bytes). *Wired through `is_type_copy` in both sema/typeck and sema_context.*
  - Equality and ordering operators work. *`Type::CHAR` accepted by `analyze_comparison`; arithmetic ops still rejected (no entry in numeric type checks).*
  - `to_u32` method (lowers to bitcast). *`dispatch_char_method_call` emits `AirInstData::IntCast` from char to u32; LLVM lowering is a no-op (both are `i32`).*
  - Spec tests for declaration, comparison, basic flow. *In `char.toml`.*
- [x] **Phase 4: from_u32 + from_u32_unchecked** *(requires ADR-0070 Phases 1–2)*
  - `char::from_u32(n) -> Result(char, u32)`: range check + Result construction. *Implementation: prelude function `char__from_u32` performs the range check and constructs `Result(char, u32)`. Sema's `dispatch_char_assoc_fn_call` resolves `char::from_u32(n)` to a call to this prelude function.*
  - `char::from_u32_unchecked` in `checked` block. *Sema emits `IntCast` from u32 to char (no-op at LLVM level since both are i32). The `checked`-block requirement is enforced in `dispatch_char_assoc_fn_call`.*
  - Spec tests cover valid scalars, surrogates (rejected with `Err(n)`), out-of-range (rejected with `Err(n)`). *In `char_from_u32.toml` — 9 tests including round-trips, boundary cases, and the `checked`-block requirement.*
  - Parser support: `char::name(args)` syntax handled by a new `char_assoc_fn_call` parser rule (chumsky_parser.rs) since primitive type tokens previously didn't accept `::name(args)` suffixes.
- [x] **Phase 5: UTF-8 encoding**
  - `len_utf8` (4-arm switch on codepoint range). *Implemented as prelude function `char__len_utf8` for code-volume reasons; sema's `dispatch_char_method_call` routes `c.len_utf8()` to it.*
  - `encode_utf8` (inline LLVM bit-shifts). *Implemented as prelude function `char__encode_utf8(c, buf: MutRef([u8; 4]))` — the inline-LLVM approach was abandoned because the prelude version is portable Gruel code, expresses the bit-twiddling clearly, and passes the same tests.*
  - `is_ascii`. *Prelude function `char__is_ascii`.*
  - Spec tests cover 1/2/3/4-byte cases. *In `char_utf8.toml` — 12 tests covering ASCII, U+00E9, U+4E2D, U+1F600 across all three methods.*
- [x] **Phase 6: String integration**
  - `String::push(c: char)` in `gruel-builtins`. *Implemented as `push_char(c: char)` in v1 — the `push` name will be claimed by ADR-0072 Phase 4 when the existing `push(byte: u8)` gets renamed to `push_byte`. Runtime function `String__push_char(out, ptr, len, cap, codepoint)` UTF-8-encodes the codepoint and appends.*
  - `String::from_char(c)` in the prelude (or builtins). *Added as a `BuiltinAssociatedFn`. Runtime function `String__from_char(out, codepoint)` allocates a fresh String and writes the UTF-8 encoding.*
  - Spec tests cover push of ASCII / 2-byte / 3-byte / 4-byte chars and round-trip via `bytes()`. *7 tests in `char_string.toml` covering `from_char` lengths, `push_char` extension, and equality with string literals.*
  - Side effect: `BuiltinParamType` gained a `Char` variant so builtins can take char parameters.
- [x] **Phase 7: Niche registration**
  - Register `char`'s `Layout` with surrogate + out-of-range niches in `gruel-air`. *Layout: size 4, align 4, two NicheRange entries (`0xD800..=0xDFFF` and `0x110000..=0xFFFFFFFF`).*
  - Verify `Option(char)` is 4 bytes and `Option(Option(char))` is also 4 bytes. *Confirmed via round-trip tests — niche-filling layer in ADR-0069 picks up the new niches automatically.*
  - Spec tests for layout sizes; codegen tests for round-trip correctness. *5 tests in `char_niches.toml` covering Option(char) and Result(char, char) variants including a 4-byte UTF-8 codepoint (😀).*
- [x] **Phase 8: Stabilize**
  - Remove preview gate. *Removed `preview = "char_type"` and `preview_should_pass = true` from spec tests; removed `PreviewFeature::CharType` variant from `gruel-util/src/error.rs`.*
  - Finalize spec section 3.9. *Spec lives in section 3.14 (since 3.9 was already destructors). Frontmatter spec-sections updated to `["3.14"]`.*

## Consequences

### Positive

- `String` gains a safe codepoint-level mutator; `push_byte` becomes the niche escape hatch it should be.
- Layout abstraction gains its first richly-niched type. `Option(char)`, `Result(char, _)`, and any enum carrying `char` become tag-free.
- Foundation for future `chars()` iterator, pattern matching on char ranges (`'a'..='z'`), and per-character text manipulation.
- Small, well-bounded primitive — short implementation cost, no library dependency.

### Negative

- Lexer single-quote work is genuinely new — single quotes are currently unused, but this commits the syntax. (Anything else wanting `'...'` syntax in the future — labels? lifetimes? — has to coexist. Mitigation: Gruel doesn't have lifetimes (per ADR-0062's reference model), and labels are unlikely to use single quotes if added.)
- No `as` casts to/from `u32` is a small ergonomics tax — every conversion site writes `c.to_u32()` or `char::from_u32(n).unwrap()`. Justified by the surrogate-bump footgun.
- v1 ships without Unicode classification. Programs needing `is_alphabetic` etc. must wait for a follow-up, or hand-roll on `to_u32()`.

## Open Questions

- **`as` casts?** Rejected for now (footgun). Open to revisiting if `to_u32()` / `from_u32(...).unwrap()` becomes painful at scale. Easy to relax later; hard to tighten.
- **Single-quoted strings as future ambiguity?** Some languages allow `'...'` as a string-literal alternative. Gruel commits single quotes to char literals exclusively here. If Gruel later wants raw or alternative-quoted strings, it picks a different sigil (e.g., `r"..."`).

## Future Work

- Unicode classification surface (`is_alphabetic`, `to_lowercase`, etc.) — separate ADR, ships with the Unicode property tables.
- ASCII-only classification surface (`is_ascii_alphabetic`, `to_ascii_lowercase`, etc.) — small follow-up, no tables needed.
- `String::chars()` iterator — paired with general iterator interface ADR.
- Char range patterns (`'a'..='z'`) — pattern-matching extension.
- `char` arithmetic via opt-in (`c.checked_add(1) -> Option(char)`) for the rare cases it's actually wanted.

## References

- ADR-0020: Built-in types as synthetic structs.
- ADR-0065: Clone interface and canonical `Option(T)`.
- ADR-0067: Linear types in containers (relevant for `Option(char)` since char is Copy, but the propagation rules apply).
- ADR-0069: Layout abstraction and niche-filling for enums.
- ADR-0072: String / Vec(u8) relationship.
- ADR-0070 (proposed): Result(T, E).
- Rust's `char` documentation and `char::from_u32`.
- Unicode Standard, definition D76 (Unicode scalar value).
