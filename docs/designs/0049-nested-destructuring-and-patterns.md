---
id: 0049
title: Nested Destructuring and Pattern Matching
status: proposal
tags: [syntax, patterns, destructuring, pattern-matching, tuples]
feature-flag: nested_patterns
created: 2026-04-22
accepted:
implemented:
spec-sections: ["4.7", "5.1"]
superseded-by:
---

# ADR-0049: Nested Destructuring and Pattern Matching

## Status

Proposal

## Summary

Generalise patterns so they nest. Today, `let`-destructuring and `match` patterns are
flat: you can write `let Point { x, y } = p;` or `match o { Some(v) => ... }`, but not
`let Point { inner: Inner { x, y }, tag } = ...;`, not `match o { Some(Some(x)) => ... }`,
and not `match pair { (0, _) => ... }`. This ADR unifies let-patterns and match-patterns
into a single recursive grammar, adds tuple patterns everywhere, and lets struct / tuple
/ variant patterns contain sub-patterns at each binding position. It also adds three
related shape at the same time — **rest patterns (`..`)** for skipping fields/elements.
It delivers the three "Future Work" items from ADR-0048 (tuple patterns in `match`,
nested tuple destructuring) and the "Nested patterns" item from ADR-0037. It also
fixes a small sema bug where struct destructuring fails when the struct's type name
is a local type alias of an anonymous struct (the ADR-0039-style workflow works for
anon enums in `match` today but not for anon structs in `let`).

## Context

### What exists

- **ADR-0036** added flat struct let-destructuring (`let Point { x, y } = p;`) and banned
  partial moves.
- **ADR-0037** added enum data variants and match binding patterns (`Option::Some(x)`,
  `Shape::Circle { radius }`) but scoped out nested patterns (`Some(Some(x))`).
- **ADR-0048** added first-class tuples with flat let-destructuring (`let (a, b) = t;`) and
  explicitly listed tuple patterns in `match` and nested tuple destructuring as future work.
- **ADR-0029 / ADR-0039** added anonymous struct and anonymous enum types. The
  intended workflow is to bind the comptime-returned type to a local alias and then
  use that alias in patterns (`let Opt = Option(i32); match x { Opt::Some(v) => ... }`).
  This works for anon enums today but silently fails for anon structs in let-destructure
  — `let PairI32 { first, second } = p;` reports a `__anon_struct_N` vs `PairI32`
  type mismatch even though the alias is the same type. A small sema fix folded into
  this ADR closes the gap without new syntax.

### The gap

These three exclusions compound. In practice users end up writing chains of intermediate
bindings just to peel one layer:

```gruel
// Today — a nested Option requires manual stepping
match outer {
    IntOption::Some(inner) => match inner {
        IntOption::Some(v) => v,
        IntOption::None   => 0,
    },
    IntOption::None => 0,
}

// Today — destructuring a struct that holds a tuple
let Config { name, dims } = cfg;
let (w, h) = dims;

// Today — can't match on a tuple at all
// match (a, b) { (0, 0) => ... }   <-- parse error
```

Once we have pattern matching for enums and destructuring for structs/tuples, refusing to
nest them is an arbitrary limitation of the AST, not a semantic one. The ownership rules
(ADR-0036) are already per-leaf: every binding in a flat destructure is already an
independent value. Nesting just lets the tree of bindings match the tree of types.

### Why now

Tuples (ADR-0048) just landed, and they are most natural in `match`: a tuple of scrutinees
is the conventional way to match two or more values against each other. Shipping tuples
without `match (a, b) { ... }` support is the biggest felt gap from that ADR.

### Existing infrastructure we can reuse

- **`LetPattern::Struct` / `LetPattern::Tuple`** already carry the field → binding mapping;
  generalising `binding` from `Ident | Wildcard` to a recursive `Pattern` is the core change.
- **`Pattern::DataVariant` / `Pattern::StructVariant`** (match arms) already carry per-field
  bindings via `PatternBinding`; same generalisation applies.
- **`InstData::StructDestructure`** in RIR already handles the tuple case via the
  `__tuple__` sentinel (ADR-0048 Phase 3). Nested destructuring becomes a tree of
  `StructDestructure` instructions where sub-pattern fields recurse.
- **Exhaustiveness checking** for match already walks variant space. Extending it to
  descend into tuple/struct sub-patterns is a natural recursion.
- **ADR-0036 partial-move ban** applies unchanged: each leaf binding is independent,
  the whole scrutinee is consumed, wildcards at any depth drop immediately.

## Decision

### 1. Unify let and match patterns

Replace the two pattern ASTs (`LetPattern`, `Pattern`) with a single recursive `Pattern`
AST. A `Pattern` is one of:

```
Pattern ::=
    | "_"                                   (* wildcard *)
    | ".."                                  (* rest, only inside a sequence *)
    | ["mut"] IDENT                         (* binding *)
    | INT | "-" INT | BOOL                  (* literal, refutable *)
    | path "::" IDENT                       (* unit variant, refutable *)
    | path "::" IDENT "(" Elem ("," Elem)* [","] ")"          (* data variant, refutable *)
    | path "::" IDENT "{" FieldPat ("," FieldPat)* [","] "}"  (* struct variant, refutable *)
    | IDENT "{" FieldPat ("," FieldPat)* [","] "}"            (* named struct, irrefutable *)
    | "(" Elem ("," Elem)+ [","] ")"                          (* tuple, arity ≥ 2 *)
    | "(" Elem "," ")"                                        (* 1-tuple *)

Elem     ::= Pattern | ".."                 (* ".." permitted at most once per sequence *)
FieldPat ::= IDENT [ ":" Pattern ] | ".."   (* shorthand when binding is omitted; ".." at most once *)
```

One entry is new compared to existing flat patterns:

- **Rest patterns (`..`)** may appear at most once inside any tuple / struct / variant
  field sequence. They match the remaining positions and drop any non-copy values at
  those positions (see §5).

Anonymous struct and anonymous enum values are matched and destructured via the
existing named forms, using a local type alias (see §4 examples and Context). No new
pattern syntax is introduced for them; a sema fix covers the struct case (§8).

Shorthand rules:

- `field` in a struct pattern binds `field` as the local name, equivalent to `field: field`.
- `field: _` drops the field (irrefutable wildcard, matches today's `DestructureBinding::Wildcard`).
- `field: pat` recursively destructures the field with `pat`.
- `..` as a field-pattern skips (and drops non-copy values of) all remaining fields.
- `..` as a positional element skips (and drops non-copy values of) all positions not
  already listed; at most one `..` per sequence.
- `mut` is only valid immediately before a binding ident: `mut x`, `field: mut x`,
  `Some(mut x)`, `(mut a, b)`. Not `mut (a, b)` — that's a nonsense pattern.
- `path::V` may omit the path prefix if unambiguous (existing rule for variants).

### 2. Refutability

Every pattern is classified as **refutable** or **irrefutable**:

| Pattern                 | Irrefutable iff                               |
|-------------------------|-----------------------------------------------|
| `_`                     | always                                        |
| `..`                    | always (only appears inside a sequence)       |
| `x`, `mut x`            | always                                        |
| `INT`, `BOOL`, `-INT`   | never                                         |
| `Enum::V` (unit)        | iff `Enum` has exactly one variant, that one  |
| `Enum::V(...)`          | iff `Enum` has one variant AND each sub-pattern is irrefutable |
| `Enum::V { ... }`       | same as data variant                          |
| `S { ... }`             | iff every field sub-pattern is irrefutable    |
| `(p1, p2, ...)`         | iff every `pi` is irrefutable                 |

**Let bindings require irrefutable patterns.** A refutable pattern in a `let` is an error:

```
error: refutable pattern in let binding: matches only a subset of possible values
  --> 3:9
   |
 3 |     let Option::Some(x) = opt;
   |         ^^^^^^^^^^^^^^^ pattern is refutable
   |
   = help: use `if let` (not yet available) or a `match` expression
```

Match arms accept any pattern; exhaustiveness is enforced at the match level, not per-arm.

**Future `if let`** (ADR-0037 Future Work) will be the way to write refutable let-like
bindings. Out of scope here.

### 3. Syntax: tuple patterns

Tuple patterns follow the same rules as tuple types/literals from ADR-0048:

- `()` — matches unit (irrefutable, trivial).
- `(p,)` — 1-tuple, trailing comma required to disambiguate from a parenthesised pattern.
- `(p1, p2, ...)` — arity ≥ 2, trailing comma optional.
- `(p)` — parenthesised pattern (not a 1-tuple), useful as a grouping form.

Tuple patterns are only valid against tuple-typed scrutinees. Arity mismatch is a sema-time
error (same rule as today's flat tuple destructure).

### 4. Nesting examples

```gruel
// Nested let destructuring
let Pair { a: Inner { x, y }, b: (w, h) } = big;

// Nested tuple destructure
let ((a, b), c) = ((1, 2), 3);

// Mixing
let (Point { x, y }, tag) = ...;

// Enum-in-enum
match nested {
    IntOption::Some(IntOption::Some(v)) => v,
    IntOption::Some(IntOption::None)    => -1,
    IntOption::None                      => 0,
}

// Tuple of scrutinees
match (a, b) {
    (0, 0) => 0,
    (0, _) => 1,
    (_, 0) => 2,
    _      => 3,
}

// Tuple inside variant
match outcome {
    Result::Ok((x, y))  => x + y,
    Result::Err(code)   => code,
}

// Wildcards at any depth
let (_, Inner { x, y: _ }) = pair;   // drops .0 and .y at their leaves

// Mut at any depth
match x {
    Some((mut a, b)) => { a += 1; a + b }
    None             => 0,
}

// Rest patterns
let (first, .., last) = quintuple;          // drops the middle three
let Point3 { x, .. } = p;                   // drops y and z (if non-copy)
match opt {
    Some(Point { x, .. }) => x,             // drops remaining Point fields
    None                  => 0,
}

// Anonymous struct / enum destructuring via local alias (no new syntax)
fn Pair(comptime T: type) -> type { struct { first: T, second: T } }
let PairI32 = Pair(i32);
let p: PairI32 = ...;
let PairI32 { first, second } = p;          // works once §8 sema fix lands

fn Option(comptime T: type) -> type { enum { Some(T), None } }
let Opt = Option(i32);
match find_opt() {
    Opt::Some(v) => v,                      // already works today
    Opt::None    => -1,
}
```

### 5. Semantics

- **Consumption** is unchanged from ADR-0036: the *root* scrutinee is consumed. Every
  binding anywhere in the tree is an independent local of the corresponding field's type.
- **Drop semantics** are unchanged. `_` at any depth drops that sub-value immediately
  (destructor runs if the leaf type has one). A named binding transfers ownership to the
  new local; it drops at its enclosing scope's exit unless moved.
- **Copy types** are unchanged: copy leaves are copied rather than moved; non-copy leaves
  move.
- **Evaluation order** of leaf reads is left-to-right, depth-first (matches struct/tuple
  destructuring today). This is user-observable only via drop order when some leaves are
  `_`, which matches ADR-0048 §Drop order.
- **No partial destructuring without `..`**: a struct pattern must list every field,
  and a tuple / variant pattern must list every positional element, *unless* a `..`
  is present (inherited rule from ADR-0036 / ADR-0048, relaxed only by `..`).
- **Rest pattern (`..`) semantics**: `..` matches the positions or fields not
  explicitly listed in its enclosing sequence. Those matched positions have their
  values *dropped immediately* (destructors run at leaves of copy types this is a
  no-op; for non-copy types the leaf is moved into a temporary and dropped). This
  upholds the ADR-0036 invariant that the root is fully consumed and no field escapes
  un-dropped. At most one `..` per sequence; enclosing sequences may each carry their
  own `..`. `..` is never valid at the top level of a `let` or `match` arm (only
  inside a tuple / struct / variant pattern).
- **No or-patterns (`A | B`)**: out of scope.
- **No range patterns**: out of scope.

### 6. Exhaustiveness for `match`

Exhaustiveness checking is extended to descend into sub-patterns. A simple recursive
algorithm suffices for the shapes above:

- **Tuples and structs** of irrefutable leaves are trivially exhausted by a single
  all-wildcard pattern. With refutable sub-patterns (enum variants, literals), we need the
  cross-product: a `match (a, b)` on `(bool, bool)` requires coverage of the four literal
  combinations (or a wildcard at that position).
- **Enums** are exhausted when every variant is covered by some arm, considering each arm's
  sub-patterns. If variant `V(p1, p2)` is covered by `V(_, _)` in one arm, that variant is
  exhausted regardless of other arms that cover it with more specific patterns.
- **Literals** (`Int`, `Bool`) are only exhausted by a wildcard (integers have infinite
  range; booleans are exhausted by `true` + `false` or a wildcard).

The existing usefulness/exhaustiveness checker in sema (already used for unit variants and
data variants) generalises by recursing into `Pattern` fields. The "witnesses" it reports
become nested patterns in diagnostics (e.g. `note: pattern `Some(None)` not covered`).

### 7. AST changes (`gruel-parser`)

#### 7.1 Unified `Pattern` enum

Delete `LetPattern` and `PatternBinding` / `PatternFieldBinding`. Replace with a single
recursive `Pattern`:

```rust
pub enum Pattern {
    Wildcard(Span),
    Ident { is_mut: bool, name: Ident, span: Span },
    Int(IntLit),
    NegInt(NegIntLit),
    Bool(BoolLit),
    Path(PathPattern),                                // unit variant with path
    DataVariant {
        base: Option<Box<Expr>>,
        type_name: Ident,
        variant: Ident,
        fields: Vec<TupleElemPattern>,                // sub-patterns + possible `..`
        span: Span,
    },
    StructVariant {
        base: Option<Box<Expr>>,
        type_name: Ident,
        variant: Ident,
        fields: Vec<FieldPattern>,                    // named + optional `..`
        span: Span,
    },
    Struct {
        type_name: Ident,
        fields: Vec<FieldPattern>,
        span: Span,
    },
    Tuple { elems: Vec<TupleElemPattern>, span: Span },
}

/// One position in a tuple-like sequence: a sub-pattern or `..`.
pub enum TupleElemPattern {
    Pattern(Pattern),
    Rest(Span),
}

pub struct FieldPattern {
    /// `None` for the `..` sentinel; `Some` for named fields.
    pub field_name: Option<Ident>,
    /// `None` = shorthand (binding has same name as field, irrefutable ident),
    ///   or the `..` sentinel when `field_name` is None.
    pub sub: Option<Pattern>,
    /// Only meaningful when `sub` is None or `sub` is `Pattern::Ident`.
    pub is_mut: bool,
    pub span: Span,
}
```

`LetStatement.pattern` becomes `Pattern`. Match arms' `pattern` field becomes `Pattern`.
The parser replaces its two pattern entry points with one recursive `parse_pattern()`
reused from both contexts. Sema enforces refutability, `..`-at-most-once per sequence,
and `..`-not-at-top-level.

### 8. RIR/AIR lowering

**RIR:** `RirPattern` already has `DataVariant` / `StructVariant` with per-field bindings.
Generalise each binding slot to hold a nested `RirPattern` (recursive). The existing
`RirPatternBinding` / `RirStructPatternBinding` structs become carriers for either a leaf
binding (ident or wildcard) or a sub-pattern.

For let-destructuring, the existing `InstData::StructDestructure` generalises: each
`RirDestructureField` gains an optional `sub_pattern: Option<RirPattern>` (stored out-of-band
in the RIR extra array). When present, the field is not a leaf binding — instead the
lowerer emits a child `StructDestructure` / tuple destructure for the sub-pattern,
threading the field-read value through.

**Rest patterns** are elaborated in RIR rather than persisted as a distinct pattern
kind. At each sequence level, the presence of a `..` causes the lowerer to emit
implicit drop/wildcard fields for every position / field not otherwise covered.
Equivalently: `Point { x, .. }` lowers like `Point { x, y: _, z: _ }` (with the
skipped fields' `_` wildcards emitting Drop when their types are non-copy). This
keeps the rest-pattern semantics entirely inside astgen and avoids touching sema's
refutability / exhaustiveness algorithms beyond recognising the source-level `..`.

**Anon-struct let-destructure via local alias.** Today,
`let PairI32 { first, second } = p;` where `PairI32` is a `let`-bound alias of
`Pair(i32)` (an anonymous struct) fails with a type mismatch between `__anon_struct_N`
and `PairI32`. The fix is in the struct-destructure type-check: resolve the pattern's
`type_name` through the value-scope's type aliases before comparing to the init's
inferred type. The corresponding match-arm path for anon enums already does this,
which is why `Opt::Some(x)` works — this just brings struct-pattern resolution into
line. No new pattern kind, no new AST shape.

Concretely, astgen lowers `let Pair { a: Inner { x, y }, b } = p;` as:

```
%p      = <eval p>
%p.a    = field_get %p "a"               (synthetic local, not user-visible)
%p.b    = field_get %p "b"
%x      = field_get %p.a "x"             (StorageLive + Store)
%y      = field_get %p.a "y"
%b      = <stored as user local b>
forget_slot(%p)                          (ownership transferred through tree)
```

The `forget_slot` rule already exists; it now applies at the root of any destructuring
pattern tree. Synthetic intermediates (like `%p.a`) that are fully consumed by their child
destructure also have their slots forgotten.

**AIR:** `AirPattern` is the dispatch key for the match lowering. Today's four cases
(Wildcard, Int, Bool, EnumVariant) handle dispatch; bindings are separately materialised
in CFG. Extend `AirPattern` with:

```rust
AirPattern::Tuple { elems: Vec<AirPattern> }
AirPattern::Struct { struct_id: StructId, fields: Vec<(FieldIndex, AirPattern)> }
AirPattern::EnumDataVariant {
    enum_id, variant_index,
    fields: Vec<AirPattern>,   // per-field sub-pattern; bindings live at leaves
}
```

Bindings are leaves represented by a new `AirPattern::Bind { is_mut, name, inner: Option<Box<AirPattern>> }`
(`inner: None` is a bare binding; `Some(p)` is `name @ p` — not syntactically exposed in
this ADR, but the AIR shape is useful and costs nothing).

Alternative considered: keep `AirPattern` as a dispatch-only tag and lower bindings via
CFG projections (the same way ADR-0037 did for flat data-variant bindings). This works but
becomes cumbersome once patterns can refute at nested positions — the CFG would need
cascading switch dispatch manually. Making `AirPattern` recursive and emitting the
cascading switch from a single walk is simpler.

**CFG/codegen:** the match lowerer becomes a recursive descent that, for each
(scrutinee-place, pattern) pair, emits either a field projection + recursive lower (for
irrefutable wrapper patterns) or a `switch` / `icmp` + branch (for refutable ones). For
let, the tree walk is strictly projection + bind, no branches.

### 9. Diagnostics

- **Refutable in let:** `error: refutable pattern in let binding` with a help pointing at
  `match` or future `if let`.
- **Arity mismatch anywhere in the tree:** point at the inner pattern. `note: tuple type
  `(i32, bool)` has 2 elements but pattern has 3`.
- **Missing field in nested struct pattern:** same diagnostic as flat struct destructure,
  but pointing at the nested span.
- **Non-exhaustive match:** witnesses render as nested patterns. `note: patterns `Some(Some(_))`
  and `None` not covered`.
- **Wrong pattern for type:** `error: expected pattern of type `Point`, found tuple pattern`.

### 10. Interaction with existing features

- **ADR-0036 (partial moves):** unchanged. Every leaf binding owns its value; the root is
  consumed.
- **ADR-0037 (enum data/struct variants):** bindings in variants become sub-patterns,
  generalising `PatternBinding`. Backward-compatible: a bare ident in variant-field
  position is still a binding (now modelled as `Pattern::Ident`).
- **ADR-0048 (tuples):** tuple patterns now work everywhere; flat tuple let-destructure
  is a special case of the general recursive form.
- **ADR-0029 (anonymous struct methods) / ADR-0039 (anonymous enum types):** no new
  pattern syntax. The intended workflow — bind the comptime-returned type to a local
  alias, then use the alias in patterns (`Opt::Some(v)` / `PairI32 { first, second }`)
  — works for anon enums today; the §8 sema fix makes it work for anon structs too.
- **Copy/linear types:** inherit unchanged from ADR-0036.

## Implementation Phases

Behind `PreviewFeature::NestedPatterns` until Phase 7. Early phases establish the
recursive core; Phase 6 layers in `..`. The anon-struct alias sema fix (Phase 7) can
ship without the preview gate since it's a bug fix, not a new feature.

- [x] **Phase 1: AST unification**
  - Introduce the unified `Pattern` enum in `gruel-parser/src/ast.rs` with
    `TupleElemPattern` and `FieldPattern` helpers (§7). Include `TupleElemPattern::Rest`
    and the `..`-sentinel field-pattern shape from the start so Phase 6 only adds
    behaviour, not shapes.
  - Migrate `LetPattern` usages to `Pattern` (preserve existing flat forms — nesting
    and rest patterns open up in later phases but the AST is ready).
  - Update Display and round-trip unit tests.
  - Register `PreviewFeature::NestedPatterns` in `gruel-error`.

- [x] **Phase 2: Parser — nested syntax**
  - Make the pattern parser recursive: accept sub-patterns inside struct field
    bindings, variant field positions, and tuple elements.
  - Accept tuple patterns (`(p1, p2, ...)`, `(p,)`) in both let and match contexts.
  - Flat patterns still parse as before (so existing tests pass).
  - Parser-only tests for each shape; no sema wiring yet.

- [x] **Phase 3: Refutability classifier**
  - Preview gate was wired in Phase 2 (in the parser, not sema) so that item is
    moot.
  - Refutability classifier and `RefutablePatternInLet` diagnostic landed as a
    post-parse AST walker. Applies unconditionally — a let binding with a
    refutable pattern is always an error, whether or not `nested_patterns` is
    enabled.
  - **Deferred to Phase 4**: recursive sema type-checking of sub-patterns,
    leaf-binding introduction, and inner-span arity / type-mismatch diagnostics.
    These depend on the recursive RIR/AIR shapes that Phase 4 lands, so they're
    architecturally part of that phase rather than a standalone sema pass on the
    current RIR.

- [x] **Phase 4a: Nested let-destructure via astgen elaboration**
  - `AstGen::emit_let_destructure_into` recursively lowers nested let patterns
    into a tree of flat `StructDestructure` instructions with synthetic
    `__nested_pat_N` intermediate bindings threaded via `VarRef`. The outer
    destructure binds each non-leaf position to a synthetic local; the child
    destructure consumes it.
  - `gen_block` threads through a new `gen_statement_into` that lets a single
    AST let-statement expand to multiple top-level RIR instructions, so
    intermediates stay visible in the block's scope.
  - No RIR / AIR / CFG / codegen changes needed for nested let — reuses the
    existing flat `StructDestructure` end-to-end, including the `__tuple__`
    sentinel (ADR-0048).
  - Spec tests (5 positive + 2 refutability error cases) cover nested
    struct-in-struct, tuple-of-tuples, struct-in-tuple, tuple-in-struct, and
    nested wildcard-drop.

- [x] **Phase 4b: Nested irrefutable patterns in match arms via astgen elaboration**
  - Mirrors Phase 4a's approach: nested sub-patterns in match arms are
    elaborated at astgen time into existing flat shapes, keeping RIR / AIR /
    CFG untouched.
  - `AstGen::gen_match_arm_pattern` walks each arm's pattern. For any
    variant-field sub-pattern that is itself a `Struct` or `Tuple`
    destructure, a fresh `__nested_pat_N` binding replaces the sub-pattern
    in the RIR match pattern, and the sub-pattern is captured for body
    elaboration. After the arm body is generated, it is wrapped in a Block
    that prepends `emit_let_destructure_into` calls against `VarRef`s to
    each synthetic binding.
  - Top-level `Struct`, `Tuple`, and `Ident` patterns in single-arm matches
    are elaborated in `try_elaborate_irrefutable_match`: the whole match
    becomes `{ let <pattern> = scrutinee; body }` (or `{ let name = scr;
    body }` for `Ident`). Multi-arm matches with a top-level
    Struct/Tuple/Ident pattern still panic — those require recursive CFG
    dispatch (Phase 5).
  - Refutable nested sub-patterns (literal, unit variant, or data/struct
    variant appearing inside a variant field position) continue to panic
    with an explicit "ADR-0049 Phase 5" message. Those require true
    recursive pattern dispatch and are deferred.
  - Spec rules 4.7:23, 4.7:24, 4.7:25 document the nested-patterns
    semantics for match arms. Four positive spec tests in
    `expressions/match.toml` exercise struct-in-data-variant, struct-in-
    struct-variant, top-level struct, and wildcard-in-variant-field under
    the `nested_patterns` preview.

- [x] **Phase 5a: Tuple patterns at match root via if-chain elaboration**
  - `AstGen::try_elaborate_tuple_match` detects any match expression whose
    arms include a tuple-root pattern and elaborates it into a block with
    `let __match_scr_N = <scrutinee>` followed by a reverse-folded if/else
    chain. Each tuple element maps to either an equality test (Int / NegInt
    / Bool), a `let` binding (Ident), or nothing (Wildcard). Predicates at
    one position are joined with `And` (short-circuit).
  - Requires the final arm to be unconditional (wildcard, ident, or a tuple
    of all-irrefutable leaves). Non-exhaustive tuple matches return `None`
    from the elaborator so they fall through to the normal match path and
    produce the usual "top-level Tuple not yet supported" message. A
    runtime `@panic` fallback was implemented but disabled due to an
    unrelated CFG-builder bug that misorders `Alloc` before a
    `Branch`-with-`Never` else; the pragmatic choice is to ship exhaustive
    tuple matches first.
  - Side fix: type inference (`gruel-air/src/inference/generate.rs`) now
    infers `@panic` and `@compile_error` as `Never` rather than `Unit`.
    Previously, `if cond { 42 } else { @panic("...") }` type-checked as
    `Unit`, breaking any use of `@panic` in a value-returning else. This
    fix is independent of Phase 5a but came out of exploring the CFG bug
    above.
  - Spec tests (4) cover: integer literals with wildcard last arm, ident
    bindings, bool literal exhaustion via the 2x2 matrix, and negative
    integer literals.

- [ ] **Phase 5b: Refutable nested sub-patterns + exhaustiveness**
  - Refutable nested sub-patterns (`Some(Some(v))`, `Some(0)`): require
    cascading switch dispatch — cannot be elaborated to flat match without
    arm duplication.
  - Extend the exhaustiveness checker to consider nested pattern
    combinations (witnesses become nested patterns in diagnostics).
  - Revisit the CFG-builder bug around `let` + `if-else-with-panic` so the
    tuple-match elaboration can emit a real `@panic` fallback for
    non-exhaustive matches.
  - The ADR's originally-planned approach — recursive `AirPattern` plus
    recursive CFG descent — remains the right direction for these shapes.

- [x] **Phase 6: Rest patterns (`..`) in let destructures**
  - Parser already accepts `..` in tuple / struct / variant field lists
    (Phase 2); Phase 6 lights them up end-to-end for `let` destructures.
  - Astgen: in `emit_let_destructure_into`, a `..` struct-field or
    trailing `..` tuple-element emits a synthetic `RirDestructureField`
    with the sentinel field name `..`. The `RirDestructureField` type is
    re-exported from `gruel-rir` so sema can synthesize new ones.
  - Sema: `analyze_struct_destructure` strips the sentinel field, sets
    `has_rest`, waives the "all fields required" rule, and synthesizes
    wildcard `RirDestructureField`s for every unlisted struct field. This
    reuses the existing alloc/drop code path so non-copy skipped fields
    are dropped exactly once.
  - Deferred to a future pass (still panics in astgen):
    - `..` in the middle of a tuple destructure (`(a, .., b)`) — would
      need sema to fill in positions from the inferred tuple arity.
    - `..` in match-arm variant / struct-variant patterns.
  - Spec tests: struct rest dropping integer fields, struct rest dropping
    a non-copy String field, trailing tuple rest.

- [x] **Phase 7: Anon-struct alias sema fix (no preview gate)**
  - `analyze_struct_destructure` in `gruel-air/src/sema/analyze_ops.rs` now
    resolves the pattern's `type_name` through `ctx.comptime_type_vars`
    before comparing to `init_type`'s `StructId`. When the pattern name is
    a local alias of an anonymous struct (e.g. `let PairI32 = Pair(i32);
    let PairI32 { ... } = p;`), the alias's StructId is compared to the
    init's StructId. If the alias isn't a comptime type var, we fall back
    to the old name-based comparison, so genuine name mismatches still
    error with `expected X, found Y`.
  - Spec tests: `anon_struct_alias_destructure` (success),
    `anon_struct_alias_destructure_with_nested` (combines with Phase 4b),
    `struct_destructure_wrong_name_still_errors` (regression guard).
  - Ships unconditionally — no preview gate needed, since this is a bug
    fix for an existing workflow.

- [ ] **Phase 8: Spec, tests, stabilization**
  - Revise `docs/spec/src/05-statements/01-let-statements.md` (5.1) for nested let
    patterns and `..` in let.
  - Revise `docs/spec/src/04-expressions/07-match-expressions.md` (4.7) for tuple
    patterns, nested variant patterns, and `..`.
  - Full spec-test matrix covering everything above plus: nested witnesses in
    exhaustiveness errors, move/copy/drop interactions at depth, `mut` at depth.
  - UI tests for the new diagnostic wording.
  - Run traceability (`cargo run -p gruel-spec -- --traceability`).
  - Remove `PreviewFeature::NestedPatterns` and all `preview` / `preview_should_pass`
    fields from the new tests.

## Consequences

### Positive

- **Ergonomic wins**: the common "destructure a struct that contains a tuple / struct" case
  drops from two let-bindings to one. Nested `Option` matches drop from an outer + inner
  match to a single match.
- **Tuples become useful in `match`**: the natural `match (a, b) { ... }` form — flagged
  in ADR-0048 as a prerequisite — is delivered.
- **Unified pattern AST**: future pattern features (`if let`, or-patterns, range patterns,
  rest patterns) have one extension point instead of two.
- **No ownership-model changes**: the ADR-0036 / ADR-0037 / ADR-0048 rules apply unchanged
  — only the AST reach is extended.

### Negative

- **Exhaustiveness checker grows**: moving from flat variant-coverage to tree-coverage is
  the biggest engineering ask. Mitigated by the canonical usefulness algorithm, which is
  well-studied.
- **Refutability needs to be explicit**: introduces a new sema pass (or predicate) that
  didn't need to exist when flat patterns made the distinction obvious from the pattern
  kind. Small but new surface area.
- **AIR pattern encoding grows**: `AirPattern` becomes recursive; the extra-array encoding
  needs tree serialisation. Mild overhead only at match-lowering time.

### Neutral

- **No new runtime machinery**: all lowering expresses in existing CFG primitives
  (projections, switches, StorageLive, Drop, forget_local_slot).
- **Backwards compatible**: every flat pattern that parses today continues to parse and
  type-check identically. The preview gate only triggers when a nested or tuple-in-match
  pattern is encountered.

## Open Questions

1. **Binding modes (`ref`, `ref mut`, `@`)**. Rust allows `Some(x)` to either move or
   borrow based on a binding mode; it also allows `name @ pattern` to bind the whole
   while matching. **Proposed for this ADR:** skip both. All bindings move/copy (per
   ADR-0036); `@`-patterns can be revisited if a concrete need appears.

2. **Nested patterns inside enum struct variants when a field holds a tuple / struct.**
   E.g. `Shape::Rect { size: (w, h), tag: Inner { a, b } }`. **Proposed:** yes, this
   falls out of the recursive grammar for free. No separate decision needed.

3. **Parser depth limit.** Pathological programs could nest patterns arbitrarily deep.
   **Proposed:** reuse the existing recursion-depth limit used elsewhere in the parser;
   no new limit.

4. **`..` drop order across skipped fields.** Drops of skipped non-copy fields happen
   in struct-field declaration order (same as explicit destructure today, per
   ADR-0048 §Drop order). **Proposed:** no change; if the language ever flips
   struct-field drop order to Rust's reverse-declaration convention, `..` follows by
   construction.

## Future Work

- **`if let` / `while let`** — the natural consumer of refutable patterns outside
  `match`.
- **Or-patterns (`A | B`)** — independent extension of the pattern grammar.
- **Range patterns (`1..=5`)** — literal-family extension.
- **`.0.1` chaining in field access** — independent of patterns; carried over from
  ADR-0048 Open Question 1.
- **Tuple structs** — would get pattern support for free once added.
- **Binding modes (`ref`, `@`)** — see Open Question 1.

## References

- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md) — anonymous struct infrastructure (§7 sema fix)
- [ADR-0036: Struct Destructuring and Partial Move Ban](0036-destructuring-and-partial-move-ban.md)
- [ADR-0037: Enum Data Variants and Full Pattern Matching](0037-enum-data-variants-and-full-pattern-matching.md)
- [ADR-0039: Anonymous Enum Types](0039-anonymous-enum-types.md) — intended workflow (alias the comptime-returned type, then pattern-match)
- [ADR-0048: First-Class Tuples](0048-tuples.md) — tuple semantics and the future-work list this ADR closes
- [Rust Reference: Patterns](https://doc.rust-lang.org/reference/patterns.html)
- [Warnings for pattern matching — Luc Maranget, JFP 2007](http://moscova.inria.fr/~maranget/papers/warn/warn.pdf) — standard usefulness/exhaustiveness algorithm
