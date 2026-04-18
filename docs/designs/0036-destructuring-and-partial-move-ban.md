---
id: 0036
title: Struct Destructuring and Partial Move Ban
status: proposal
tags: [types, semantics, ownership, destructors]
feature-flag: destructuring
created: 2026-04-18
accepted:
implemented:
spec-sections: ["5.1", "3.9"]
superseded-by:
---

# ADR-0036: Struct Destructuring and Partial Move Ban

## Status

Proposal

## Summary

Ban partial field moves of non-copy struct types and introduce struct let-destructuring as the alternative. This follows the Austral approach: to consume individual fields of a struct, you must destructure the entire struct. This eliminates the need for drop flags and fixes the current unsoundness where partially-moved structs can leak undropped fields.

## Context

### The Partial Move Problem

Gruel currently allows partial moves — consuming a single non-copy field from a struct while leaving other fields live:

```gruel
struct Pair { a: Inner, b: Inner }

fn example() {
    let p = Pair { a: Inner { ... }, b: Inner { ... } };
    consume(p.a);    // Partial move of field `a`
    // p.b is still live but p is "partially moved"
}   // BUG: p.b is never dropped
```

This is unsound. The CFG builder tracks drops per-slot (one slot = one local variable), not per-field. When `p.a` is moved, sema marks `p` as having a partial move, and at scope exit the entire variable `p` is either dropped or not — there's no mechanism to drop only `p.b`.

### Why Not Drop Flags?

Rust solves this with drop flags: runtime booleans that track whether each field is still live, with conditional drops at scope exit. This is complex:

- Requires `ConditionalDrop` instruction in the IR
- Requires allocating and managing extra slots for flags
- Requires conditional branching at every drop site
- Interacts poorly with loops and branches (flag state depends on control flow)

For Gruel's current stage, this complexity isn't justified. We can revisit drop flags if/when the need arises.

### The Austral Approach

Austral takes a simpler stance: you cannot move individual fields out of a struct. To access non-copy fields individually, you must destructure the entire struct, which consumes it and binds all fields:

```gruel
let Pair { a, b } = p;   // Consumes p, binds a and b
consume(a);                // a is now an independent value
// b is dropped at scope exit
```

This eliminates partial moves entirely. Every value is either fully live or fully consumed — no in-between state.

### Function Parameter Drops

A related bug: function parameters with destructors are never dropped. Parameters are not added to the CFG builder's `scope_stack`, so `emit_drops_for_all_scopes` at `Ret` never drops them. Additionally, at the call site, `forget_local_slot` is only called for `StructInit` operands, not for function call arguments — so if we add parameter drops without fixing the caller side, non-copy locals passed as arguments would be double-dropped (once by the callee, once at scope exit by the caller).

This is a pre-existing bug independent of the destructuring decision, and should be fixed first.

## Decision

### 1. Fix Function Parameter Drops (Bug Fix)

**Caller side**: When a non-copy local is passed as a function argument, the CFG builder must call `forget_local_slot` to remove it from the caller's scope tracking. This prevents the caller from dropping a value whose ownership has transferred to the callee.

**Callee side**: Add function parameters to the CFG builder's `scope_stack` at function entry (alongside `StorageLive` for locals). At `Ret`, `emit_drops_for_all_scopes` will then drop any parameters that haven't been moved.

The sema layer already tracks parameter moves via `moved_vars`. When a parameter is moved (passed to another function, returned, etc.), it's marked as moved. The CFG builder should emit `Drop` for parameters that are still live at function exit, same as it does for locals.

### 2. Ban Partial Field Moves

When accessing a non-copy field of a struct as a value (not a reference, not a copy), the compiler will error:

```gruel
struct Pair { a: Inner, b: Inner }

fn example() {
    let p = Pair { ... };
    consume(p.a);   // ERROR: cannot move field `a` out of `Pair`
                     //        use destructuring: `let Pair { a, b } = p;`
}
```

The error message should suggest destructuring as the alternative.

**Copy fields remain accessible** — reading a copy field is not a move:

```gruel
struct Tagged { tag: i32, data: Inner }

fn example() {
    let t = Tagged { ... };
    let x = t.tag;     // OK: i32 is copy
    consume(t.data);   // ERROR: cannot move field `data` out of `Tagged`
}
```

**Implementation**: In `analyze_ops.rs`, the partial move code path (lines ~2042-2068) currently calls `mark_path_moved`. Under the preview gate, this path should instead emit a compile error. The existing `mark_path_moved` with partial field paths, `VariableMoveState.partial_moves`, and `merge_union` for partial moves become dead code under this feature and can be removed when it stabilizes.

### 3. Add Struct Let-Destructuring

New syntax for destructuring a struct in a let binding:

```gruel
let TypeName { field1, field2, field3 } = expr;
```

Semantics:
- The expression must evaluate to the named struct type
- All fields must be listed — no partial destructuring (Austral rule)
- Each field name becomes a new local binding of the field's type
- The struct value is consumed (no longer accessible)
- Each bound field is an independent value with its own lifetime

**Renaming** via `field: new_name`:

```gruel
let Point { x: px, y: py } = point;
// px and py are now in scope, point is consumed
```

**Wildcard** via `field: _`:

```gruel
let Pair { a, b: _ } = pair;
// a is bound, b is immediately dropped
```

When a field is bound to `_`, its destructor runs immediately (if the type has one). This is consistent with how `let _ = expr;` works for full values.

**Mutability**: Individual bindings can be made mutable:

```gruel
let TypeName { mut field1, field2 } = expr;
```

**All fields required**: Omitting a field is a compile error:

```gruel
struct Triple { a: Inner, b: Inner, c: Inner }
let Triple { a, b } = t;   // ERROR: missing field `c` in destructuring of `Triple`
```

This ensures the programmer explicitly decides what happens to every field.

#### AST Representation

Extend `LetPattern` with a struct destructuring variant:

```rust
pub enum LetPattern {
    Ident(Ident),
    Wildcard(Span),
    Struct {
        type_name: Ident,
        fields: Vec<DestructureField>,
        span: Span,
    },
}

pub struct DestructureField {
    pub field_name: Ident,          // The struct field being bound
    pub binding: DestructureBinding, // How it's bound
    pub is_mut: bool,               // Whether the binding is mutable
}

pub enum DestructureBinding {
    Shorthand,           // `field` — bind to same name
    Renamed(Ident),      // `field: new_name`
    Wildcard(Span),      // `field: _`
}
```

#### IR Lowering

A struct destructure lowers to:
1. Evaluate the struct expression into a temporary
2. For each field: emit a field read from the temporary
3. For fields bound to `_` with destructors: emit an immediate `Drop`
4. For fields bound to names: emit `StorageLive` + `Store` for the new local
5. The struct temporary's slot is forgotten (removed from scope tracking via `forget_local_slot`) since ownership of all fields has been transferred

### 4. Interaction with Existing Features

**Copy structs (`@copy`)**: Destructuring a copy struct copies each field. The original remains accessible. This is consistent — copy types are never consumed.

**Linear structs**: Already require full consumption. Destructuring is the natural way to consume a linear struct. The existing `check_unconsumed_linear_values` continues to work: if a linear struct is destructured, all its fields become independent linear values that must themselves be consumed.

**Mutable variables**: A mutable variable can still be reassigned as a whole (`x = new_value`), which drops the old value per ADR-0010. But individual fields of non-copy types cannot be moved out.

**Field assignment**: Writing to a field (`x.field = new_value`) continues to work and drops the old field value. This is assignment, not a move out.

**Enums**: This ADR only covers structs. Enum destructuring (pattern matching) will come with a future ADR.

## Implementation Phases

Epic: gruel-wjha

- [x] **Phase 1: Fix function parameter drops** — Add params to CFG `scope_stack`, call `forget_local_slot` for non-copy args at call sites. Add spec tests. No preview gate (this is a bug fix).

- [x] **Phase 2: Add `PreviewFeature::Destructuring`** — Register the preview feature in `gruel-error`. Add spec test placeholders with `preview = "destructuring"`. Behind this gate, emit an error when a non-copy field is used as a move (the partial move path in `analyze_ops.rs`). Error message should suggest destructuring.

- [x] **Phase 3: Parse struct destructuring** — Extend `LetPattern` with `Struct` variant. Parse `let TypeName { fields } = expr;`. Validate all fields present, no duplicates. Gate behind `PreviewFeature::Destructuring`.

- [x] **Phase 4: Lower struct destructuring** — RIR and AIR lowering: decompose into field reads + local bindings. Handle `_` fields (immediate drop). Handle rename bindings. Remove struct temporary from scope tracking.

- [ ] **Phase 5: Spec, tests, stabilization** — Write spec paragraphs for destructuring syntax and semantics. Full test coverage for all cases (happy path, errors, copy types, linear types, nested structs, wildcard drops). Run traceability. When stable, remove preview gate.

## Consequences

### Positive

- **Sound drop semantics**: No more leaked fields from partial moves
- **No drop flags**: Simpler compiler, simpler IR, no runtime overhead
- **Explicit ownership transfer**: Programmer must account for every field
- **Incremental**: Parameter drop fix ships first as a standalone bug fix

### Negative

- **Less flexible than Rust**: Can't move one field and drop the rest implicitly
- **Verbose for single-field access**: Must destructure entire struct even if only one field is needed (mitigated by `_` wildcard)
- **Breaking change**: Code using partial moves will need to be rewritten (mitigated by preview gate)

### Neutral

- **Consistent with Austral/Hylo**: Well-established approach in value-oriented languages
- **Path to pattern matching**: Struct destructuring is a stepping stone toward full pattern matching in match expressions

## Resolved Questions

1. **Nested destructuring**: Should `let Outer { inner: Inner { x, y } } = o;` be allowed? No, just flat.

2. **Destructuring in function parameters**: Should `fn foo(Point { x, y }: Point)` be allowed? No, just let bindings.

3. **Exhaustive field check timing**: Should the "all fields required" check happen at parse time or sema? Sema is more practical since the parser doesn't know the struct's fields. The parser only validates syntax; sema validates completeness.

## Future Work

- **Pattern matching**: `match` expressions with struct patterns (separate ADR)
- **Enum destructuring**: Requires pattern matching
- **Drop flags**: If partial moves prove necessary for ergonomics, add them as a future feature

## References

- [ADR-0010: Destructors](0010-destructors.md) — Drop infrastructure
- [ADR-0008: Affine Types and MVS](0008-affine-types-mvs.md) — Ownership foundation
- [Austral Language](https://austral-lang.org/) — No partial moves, complete destructuring required
- [Hylo Deinitialization](https://github.com/hylo-lang/hylo) — MVS approach to cleanup
