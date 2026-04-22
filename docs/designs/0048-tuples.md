---
id: 0048
title: First-Class Tuples
status: proposal
tags: [types, syntax, destructuring]
feature-flag: tuples
created: 2026-04-22
accepted:
implemented:
spec-sections: ["3.12", "4.15"]
superseded-by:
---

# ADR-0048: First-Class Tuples

## Status

Proposal

## Summary

Add Rust-like first-class tuple types (`(T, U, V)`), tuple literals (`(1, true, "hi")`), numeric
field access (`t.0`, `t.1`), and tuple let-destructuring (`let (x, y) = t;`). Tuples are
structurally typed, heterogeneous, fixed-arity product types. Internally they reuse the
anonymous-struct infrastructure with numeric field names, so layout, drop-glue, copy semantics,
and ownership checking fall out of existing machinery.

## Context

Gruel today has unit (`()`), fixed-size arrays, named structs, anonymous structs, and enums with
tuple-shaped variants (`Some(T, U)`), but no anonymous heterogeneous product type. Every
lightweight "pair of things" has to either be a named struct (ceremony) or an anonymous struct
with made-up field names.

This shows up in several places:

- **Multiple return values**: there is no way to return two values from a function without
  declaring a struct or using `inout` parameters.
- **Comptime APIs**: generic helpers that want to return "a pair" currently need to synthesize
  anon structs with invented field names like `{ first: T, second: U }`.
- **Pattern matching (future)**: tuple patterns are the natural shape for matching multiple
  scrutinees at once (`match (a, b) { (0, 0) => ..., ... }`). Without tuple types the
  `match (a, b)` form can't be expressed.
- **No anonymous product type at all**: `Some(1, 2)` (a two-field tuple variant) works today,
  but `(1, 2)` as a bare value does not. The tuple-variant syntax rhymes with what users
  expect tuples to look like, but tuple variants are *not* tuples — they are variants with
  N positional fields. Adding tuples closes the gap without unifying the two.

Since ADR-0036 banned partial moves of struct fields, any product type we add must also ship
with destructuring so non-copy tuples can be consumed field-by-field.

### Existing infrastructure we can reuse

- **Anonymous structs (ADR-0020, ADR-0029)** already provide structural equality, interning,
  layout, method dispatch, drop glue, and destructuring (ADR-0036) for structural product types.
  A tuple is naturally an anonymous struct with field names `0`, `1`, `2`, ... and no methods.
- **`LetPattern::Struct`** already exists for struct destructuring. A tuple destructure is
  almost the same shape, with positional bindings instead of named ones.
- **Numeric identifiers**: the lexer already tokenizes integer literals, so `.0` and `.1` parse
  naturally once we teach the field-access path to accept integer literals as field names.

## Decision

### Syntax

**Tuple types** are parenthesised, comma-separated lists of types:

```gruel
let p: (i32, bool) = (1, true);
let triple: (i32, i32, i32) = (1, 2, 3);
```

- `()` is the unit type (unchanged). The empty tuple and unit are the same type.
- `(T,)` with a trailing comma is a 1-tuple, distinct from the parenthesised type `(T)` which
  is just `T`. This mirrors Rust and disambiguates grouping from tupling.
- Arity ≥ 2: no trailing comma required, but it is allowed (`(T, U,)` == `(T, U)`).

**Tuple literals** use the same rules at the expression level:

```gruel
let u: () = ();              // unit
let one: (i32,) = (42,);     // 1-tuple
let pair = (1, true);         // (i32, bool) inferred
```

A bare `(e)` remains a parenthesised expression, not a 1-tuple.

**Field access** uses `.N` with a non-negative integer literal:

```gruel
let p = (1, true);
let x = p.0;   // i32
let y = p.1;   // bool
```

Indices are bounds-checked at compile time: `p.2` on a 2-tuple is an error. Leading zeros and
non-decimal literals (`0x1`, `1_0`) are rejected in this position — only canonical decimal
digits are allowed, matching Rust.

**Float-literal ambiguity (parser).** Gruel's lexer tokenises `0.1` and `1e10` as single
`Float` tokens. That means `t.0.1` and `t.1e10` — which a user might expect to be
nested-tuple accesses — instead tokenise as `Ident Dot Float`, and the parser will reject
them. The first landing of tuples requires parentheses for nested access: `(t.0).1`. A
future refinement can teach the parser to re-split a `Float` token that appears immediately
after a `.` in field-access position (this is how rustc handles it), but that parser/lexer
coupling is not worth it for the initial ADR. Single-level `t.0` and `t.5` are not affected,
because the float regex requires a digit *before* the dot.

**Destructuring** mirrors struct destructuring from ADR-0036, with positional bindings:

```gruel
let (a, b, c) = (1, 2, 3);
let (x, _, z) = (1, 2, 3);   // middle element dropped (if drop-typed)
let (mut head, tail) = (0, rest);
```

- All elements must be listed. Partial destructuring is an error (same rule as structs).
- `_` in a position drops that element immediately if its type has a destructor.
- No `..` rest pattern for now (matches the struct rule; revisit if/when patterns grow).

**Function return types**:

```gruel
fn divmod(a: i32, b: i32) -> (i32, i32) {
    (a / b, a % b)
}

let (q, r) = divmod(17, 5);
```

### Semantics

- **Structural typing**: `(i32, bool)` is `(i32, bool)` regardless of where it was constructed.
  Two tuple types are equal iff they have the same arity and element types in order.
- **Unit identity**: `()` *is* the unit type, not a distinct zero-tuple. No change to existing
  unit semantics.
- **Copy**: a tuple is copy iff every element type is copy. Falls out of the anon-struct
  model unchanged.
- **Move / partial moves**: same rule as structs (ADR-0036). Non-copy elements can only be
  consumed via destructuring; `let x = t.0;` where `t.0` is a non-copy field is an error
  suggesting destructuring.
- **Drop order**: tuple elements are dropped in whatever order struct fields are dropped.
  Today that's declaration order (index 0, then 1, ...); see
  `crates/gruel-compiler/src/drop_glue.rs`. This ADR does not revisit that choice — if Gruel
  ever switches struct field drop order to match Rust's reverse-declaration convention,
  tuples follow automatically by construction.
- **Layout**: identical to the corresponding anonymous struct. No guarantees beyond what
  anon structs already give (no tuple-specific ABI promise).
- **No methods**: tuples do not support inline methods or external `impl` blocks. They are
  pure data. Users who want methods should define a named or anonymous struct.
- **Not unified with enum tuple variants**: `enum E { V(i32, bool) }` is a variant with two
  positional fields, constructed `V(1, true)`. To hold a tuple in a variant you write
  `enum E { V((i32, bool)) }` and construct `V((1, true))`. These remain distinct, matching
  Rust. The `V(x, y)` form is not sugar for `V((x, y))` and this ADR does not propose
  making it so.

### Internal representation

Tuples are lowered early — at the AST-to-RIR boundary — into anonymous structs whose fields
are named `0`, `1`, `2`, ... as symbols (`Spur`s interned from the strings `"0"`, `"1"`, ...).
From RIR onward, a tuple is indistinguishable from the equivalent anon struct.

This means:

- `TypeExpr` gets a new `Tuple { elems: Vec<TypeExpr>, span: Span }` variant (and likewise
  for tuple literal expressions and patterns) in the AST.
- In `astgen`, tuple type expressions and tuple literals lower to the existing anon-struct
  RIR instructions, with synthetic field names.
- `FieldAccess` is extended to accept a numeric integer-literal field. In astgen the integer
  is stringified (`"0"`, `"1"`, ...) and resolved as a normal field name.
- Sema's structural interning already deduplicates these, so `(i32, bool)` and a
  user-written `struct { 0: i32, 1: bool }` would collide — but `struct` field names must be
  identifiers, so users cannot write `0:` directly. Synthetic tuple names are safe from
  collision.
- Pretty-printers (AST `Display`, diagnostic rendering) detect tuple-shaped anon structs
  (fields named `0..N` in order, no methods) and print them in tuple form. This is cosmetic,
  not semantic.

This "sugar over anon structs" approach is the same pattern used by Rust's own compiler and
keeps the IR surface area minimal. The alternative — a dedicated `Type::Tuple` variant — would
require duplicating drop, layout, destructuring, and copy logic throughout sema and CFG.

### Diagnostics

- `p.5` on a 3-tuple: `error: tuple index out of bounds: length is 3 but index is 5`.
- `let (a, b) = t;` where `t: (i32, i32, i32)`:
  `error: tuple destructure has 2 elements but type has 3`.
- Suggesting destructuring when moving a non-copy tuple field reuses the existing
  ADR-0036 diagnostic, formatted for tuples:
  `note: use destructuring: let (a, b) = t;`.

## Implementation Phases

Behind `PreviewFeature::Tuples` until Phase 5.

- [x] **Phase 1: Parser & AST**
  - Add `TypeExpr::Tuple`, `Expr::Tuple`, and a tuple variant of `LetPattern` (positional).
  - Extend field-access parsing to accept integer-literal field names (`.0`, `.1`).
  - Require trailing comma for 1-tuples; forbid leading-zero / non-decimal indices.
  - Register `PreviewFeature::Tuples`. (Sema gate wired in Phase 2 when real lowering
    lands — Phase 1 stubs tuple values to unit so nothing observable reaches users.)
  - Unit tests for parser + pretty-printer round-trip.

- [x] **Phase 2: RIR/AIR lowering as anon structs**
  - In astgen, lower tuple types to anon-struct types with fields `0..N`.
  - Lower tuple literals to anon-struct literals (via a new `InstData::TupleInit`
    that sema resolves to a `StructInit` against an anon struct).
  - Lower `t.N` to field-access with the stringified index as the field symbol
    (reuses existing `InstData::FieldGet`).
  - Sema's `resolve_type` recognises `(T, U, ...)` syntax and creates an anon
    struct via `find_or_create_anon_struct`, so structural interning
    deduplicates tuples.
  - `PreviewFeature::Tuples` gate wired at the two entry points: `resolve_type`
    for tuple type syntax and `analyze_tuple_init` for tuple literals.

- [x] **Phase 3: Destructuring**
  - `let (a, b, ...) = expr;` lowered in astgen to the existing
    `InstData::StructDestructure` with synthetic field names "0", "1", ... and
    a sentinel `type_name = "__tuple__"`.
  - Sema recognises the sentinel in `analyze_struct_destructure`, skips the
    nominal-name check, and resolves the struct type from the init's inferred
    type. `PreviewFeature::Tuples` gated here.
  - Wildcard (`_`), `mut`, and singleton (`(x,)`) patterns all work; arity
    mismatches surface via the existing missing-field / unknown-field paths.

- [ ] **Phase 4: Diagnostics polish**
  - Pretty-print anon structs whose fields are `0..N` as tuples in type errors.
  - Dedicated out-of-bounds-index error for tuple field access.
  - Update the partial-move error to suggest tuple destructuring when the receiver is a tuple.

- [ ] **Phase 5: Spec & stabilization**
  - Add spec section `3.12: Tuple Types` and `4.15: Tuple Expressions`.
  - Document `let` tuple destructuring in `05-statements`.
  - Full spec tests (construction, access, arity mismatch, destructuring, copy/move, drop
    order, nested tuples, tuple return types, tuple-of-tuples).
  - UI tests for diagnostics.
  - Traceability coverage for all normative paragraphs.
  - When green, remove the preview gate.

## Consequences

### Positive

- **Multiple return values** without declaring one-off structs.
- **Prerequisite for `match (a, b) { ... }`** tuple patterns in a future pattern-matching ADR.
- **Minimal IR growth**: reusing anon structs avoids duplicating drop/layout/ownership logic.
- **Structural typing** means no cross-crate coordination needed — two crates' `(i32, bool)`
  are automatically the same type.

### Negative

- **Numeric field access is a parser wart**: integer literals as field names are a special
  case in the lexer/parser (`.0.1` chaining, float-literal lookalikes). Worth it for
  consistency with Rust.
- **Anon-struct / tuple pretty-printing ambiguity**: we have to recognize tuple-shaped anon
  structs and render them specially, or diagnostics will show `struct { 0: i32, 1: bool }`
  for tuples. Cosmetic but visible.
- **Another product type in the language**: users now have unit, tuples, arrays, anon
  structs, named structs. Documenting "when to use what" becomes a thing.

### Neutral

- **No tuple structs** (`struct Pair(T, U)`): explicitly out of scope. Can be layered on later
  as sugar over structs with numeric field names, or skipped entirely.
- **No tuple-specific traits** (like Rust's `Fn`/`Index` impls on tuples): no trait system yet.

## Open Questions

1. **`.0.1` chaining on nested tuples.** The lexer tokenises `0.1` as a single `Float`, so
   `t.0.1` and `t.1e10` fail to parse as nested field access. Rust fixes this by re-splitting
   a `Float` token in field-access position inside the parser. **Proposed for this ADR:**
   require parens (`(t.0).1`). Promote to token-resplitting in a follow-up ADR if nested
   tuples turn out to be common enough that the paren noise hurts.

2. **Trailing comma in 0-tuple?** `( , )` is nonsense; `()` stays the only form. No ambiguity,
   just confirming.

3. **Should `(x)` ever mean a 1-tuple?** **Proposed:** no, always parenthesised expression.
   Matches Rust; avoids a footgun where `return (value);` changes type.

4. **Tuple indexing with a comptime-known non-literal index?** e.g. `t.(N)` where `N` is
   comptime. **Proposed:** no — `.N` accepts only an integer literal. Generic tuple-index
   operations are a comptime metaprogramming concern (ADR-0042) and can be addressed there.

## Future Work

- **Tuple patterns in `match`**: `match (a, b) { (0, _) => ..., (_, 0) => ..., _ => ... }`.
  Blocked on pattern matching expansion (ADR-0037 covers enums; tuples are a natural
  extension).
- **Nested tuple destructuring**: `let ((a, b), c) = ...`. Currently out of scope for
  parity with struct destructuring which is flat only (per ADR-0036).
- **Tuple structs** as sugar over structs with positional fields.
- **`..` rest patterns** when we add variadic generics or pattern matching grows.

## References

- [ADR-0020: Built-in Types as Structs](0020-builtin-types-as-structs.md) — synthetic-struct pattern
- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md) — anon-struct infrastructure
- [ADR-0036: Struct Destructuring and Partial Move Ban](0036-destructuring-and-partial-move-ban.md) — destructuring model we reuse
- [ADR-0037: Enum Data Variants and Pattern Matching](0037-enum-data-variants-and-full-pattern-matching.md) — the pattern-matching work tuples will plug into
- [Rust Reference: Tuple Types](https://doc.rust-lang.org/reference/types/tuple.html)
