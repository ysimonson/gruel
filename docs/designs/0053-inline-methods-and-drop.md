---
id: 0053
title: Unified Inline Methods and Drop Functions (Retire `impl` and Top-Level `drop fn`)
status: proposal
tags: [types, methods, syntax, destructors, enums]
feature-flag: inline_type_members
created: 2026-04-23
accepted:
implemented:
spec-sections: ["3.9", "6.3", "6.4"]
superseded-by:
---

# ADR-0053: Unified Inline Methods and Drop Functions

## Status

Proposal

## Summary

Make the four type-definition forms — named structs, named enums, anonymous structs, anonymous enums — uniform in how members are attached. Every type definition gets inline methods and an optional inline `drop fn(self) { ... }` destructor inside its body. The top-level `drop fn TypeName(self) { ... }` syntax and any remaining spec references to `impl` blocks are retired. The result: one syntactic position for everything that belongs to a type, and symmetric behaviour across structs and enums.

## Context

Members are currently attached to types in three different ways, depending on the type form:

| Type form         | Inline methods | Inline `drop fn` | Top-level `drop fn` | `impl` blocks |
|-------------------|----------------|------------------|---------------------|---------------|
| Named struct      | ✅ (ADR-0009 migrated) | ❌ | ✅ | removed |
| Named enum        | ❌             | ❌               | ✅ (implicitly, never used in practice) | removed |
| Anonymous struct  | ✅ (ADR-0029)  | ❌               | ❌ (no name to target) | never existed |
| Anonymous enum    | ✅ (ADR-0039)  | ❌               | ❌ (no name to target) | never existed |

Observations:

1. `impl` blocks are **already gone from the implementation** — `impl` is not a keyword in the lexer and the parser has no `ImplBlock` item. One stale example (`impl Counter { fn handle(self) ... }`) remains in `docs/spec/src/03-types/08-move-semantics.md:212`, and destructor spec rule `3.9:25` still says "outside of any `impl` block". These are documentation debt, not language features.
2. Named enums lack inline methods — a gap left when ADR-0029 and ADR-0039 added methods to the anonymous forms. Users who want methods on a named enum currently have no way to attach them.
3. Destructors live at the top level, disconnected from the type they belong to. Anonymous types cannot have user-defined destructors at all because they have no name to write after `drop fn`. This is listed as future work in ADR-0029.

Unifying on a single "everything is inside the type body" model closes all three gaps at once and removes special cases from the parser, RIR generator, and sema.

## Decision

### Single rule

Anything that belongs to a type — fields/variants, methods, associated functions, the destructor — is declared inside the type body. There is no separate item form for attaching members to a type.

### Inline methods on named enums

Named enums accept the same method syntax as named structs. Methods follow the last variant; they may be separated from variants by either commas or nothing (methods need no trailing comma):

```gruel
enum Option {
    Some(i32),
    None,

    fn is_some(self) -> bool {
        match self {
            Self::Some(_) => true,
            Self::None => false,
        }
    }

    fn unwrap_or(self, default: i32) -> i32 {
        match self {
            Self::Some(v) => v,
            Self::None => default,
        }
    }
}
```

`Self` resolves to the enclosing enum type. Associated functions (no `self`) are called as `Option::origin()`. Semantics, structural-equality rules (N/A for named), and method resolution match anonymous enums (ADR-0039).

### Inline `fn drop` on all four type forms

A destructor is written inline as an ordinary-looking method named `drop`:

```gruel
struct FileHandle {
    fd: i32,

    fn drop(self) {
        close(self.fd);
    }
}

enum Resource {
    File(i32),
    Socket(i32),

    fn drop(self) {
        match self {
            Self::File(fd) => close(fd),
            Self::Socket(fd) => close(fd),
        }
    }
}

fn Box(comptime T: type) -> type {
    struct {
        ptr: RawPtr,

        fn drop(self) {
            __gruel_free(self.ptr, sizeof(T));
        }
    }
}
```

`drop` is currently a reserved keyword (used by the top-level `drop fn` item being retired). Once that item is gone, `drop` loses its keyword status and becomes a plain identifier that is **privileged only in method-name position inside a type body**: the compiler recognizes a method called `drop` as the type's destructor and enforces destructor-specific rules against it. Elsewhere — as a variable, field, or free-function name — `drop` is an ordinary identifier. This keeps the syntax uniform with other methods while preserving a clear, discoverable name.

Rules:

- **Only affine types may declare `fn drop`.** A compile-time error is raised if a `@copy` type declares `fn drop` — a copy type duplicates via bitwise copy, and a destructor would run multiple times (double-free). This preserves the existing rule from ADR-0010. A `linear` type also cannot declare `fn drop` — linear values are never implicitly dropped; they must be explicitly consumed, so an automatic destructor would be unreachable. Cleanup for linear types happens at the consumption site (see ADR-0010 open question on linear-type consumption hooks). Result: `fn drop` is legal only on the default affine case.
- At most one `fn drop` per type. Duplicate → compile error.
- Signature must be exactly `fn drop(self)` with implicit unit return. Any extra parameters, type annotations, or a non-unit return type → compile error with a pointed diagnostic.
- `fn drop` cannot be called directly with method-call syntax (`x.drop()`) — it is invoked only by drop elaboration. Attempting `x.drop()` is an error; users who want to force disposal use the existing mechanisms for that (out of scope here).
- A struct/enum may declare a destructor even if all its fields/variants are trivially droppable.
- Destructor bodies may call other methods of the same type and read fields of `self`.
- Running order is unchanged from ADR-0010: the user-defined destructor runs first, then field/variant destructors in declaration order.
- `fn drop` does **not** participate in structural equality for anonymous types (same rule as method bodies in ADR-0029/0039: signatures matter, bodies do not, and the destructor signature is fixed so it contributes nothing).

### Removal of the top-level `drop fn TypeName(self)` form

The top-level `drop fn TypeName(self) { body }` item is removed. The parser no longer accepts it; attempting to use it produces a diagnostic that points at the inline form.

Existing user-defined destructors in the spec tests (`crates/gruel-spec/cases/types/destructors.toml`, `move-semantics.toml`) migrate to the inline form. This is a breaking change to the language surface but the set of affected tests is small and lives entirely within this repository.

### Retirement of `impl` references

`impl` blocks are already unimplemented. This ADR finishes the cleanup:

- Delete `docs/spec/src/06-items/04-impl-blocks.md` if it describes `impl` (verify; otherwise update).
- Rewrite the `@handle` example in `docs/spec/src/03-types/08-move-semantics.md:212` to use the inline method form.
- Rewrite destructor rule `3.9:25` to describe the inline `drop fn(self)` placement instead of "outside of any `impl` block".

### Grammar (EBNF delta)

```ebnf
struct_def   = [ directives ] [ "pub" ] [ "linear" ] "struct" IDENT "{" type_body "}" ;
enum_def     = [ "pub" ] "enum" IDENT "{" enum_body "}" ;
anon_struct  = "struct" "{" type_body "}" ;
anon_enum    = "enum"   "{" enum_body "}" ;

type_body    = field_list? method_def* ;         (* for structs *)
enum_body    = variant_list? method_def* ;       (* for enums *)

method_def   = [ directives ] "fn" IDENT "(" method_params? ")" [ "->" type ] block ;
```

The destructor is a `method_def` whose name is the identifier `drop` with the required signature `fn drop(self)`; the compiler picks it out by name during type registration. Removed productions: the top-level `drop fn IDENT "(" "self" ")" block` item, and the `"drop"` keyword token (demoted to an identifier).

### Representation changes

- `EnumDecl` gains `methods: Vec<Method>` (mirroring `StructDecl`).
- No separate `DropFn` AST node. The destructor is a regular `Method` whose `name` is the interned string `"drop"`; sema identifies it by name during type registration and stores it in the existing per-type destructor slot.
- The `DropFn` AST struct, `Item::DropFn`, and RIR `InstData::DropFnDecl` are removed.
- The `Drop` token is removed from the lexer; `drop` becomes an ordinary identifier.
- In sema, the existing per-`StructId` destructor slot stays; the only change is *where* it is populated (from a method named `drop` inside the type body instead of a top-level item). Enums get the same slot.

### Preview gate

Gate the entire change behind `PreviewFeature::InlineTypeMembers`. Stabilize once phases 1–5 are green. Because this is also a breaking change (removes the old top-level `drop fn`), the migration is done in lockstep with the preview flip: as soon as we stabilize, the old form disappears.

## Implementation Phases

- [ ] **Phase 1: Parser + AST**
  - Add `EnumDecl::methods`; teach the enum body parser to accept methods after variants (reuse `method_parser_with_expr`, mirror struct body shape).
  - Demote `drop` from keyword to identifier in the lexer, so `fn drop(self)` parses as a normal method.
  - Keep `Item::DropFn` parseable for one pre-removal diagnostic turn that says "destructors are now declared as `fn drop(self)` inside the type body" (removed entirely in phase 4).
  - Unit tests at the parser level.

- [ ] **Phase 2: RIR + Sema (named enums with methods)**
  - RIR: emit method decls when lowering named enums; no new instruction kinds needed (reuse what ADR-0029/0039 set up for anonymous enums).
  - Sema: extend enum registration to register methods keyed by `(EnumId, Spur)`. Resolve `Self` inside method bodies. Mirror the named-struct path.
  - Spec tests covering: basic method, associated function, `Self::Variant`, match-on-self, comptime parity with anonymous enums.

- [ ] **Phase 3: RIR + Sema (inline `fn drop`, all four forms)**
  - During type registration, sema pulls any method named `drop` out of the method list and stores it in the destructor slot instead.
  - Enforce the exact-signature rule (`fn drop(self)`, no return) and forbid direct method-call syntax `x.drop()`. Emit pointed diagnostics for each.
  - Enforce the affine-only rule: reject `fn drop` on `@copy` structs and on `linear` structs/enums with a diagnostic that names the offending directive/modifier.
  - Preserve existing drop-elaboration, codegen, and "user destructor runs first, then fields/variants in declaration order" semantics from ADR-0010.
  - Spec tests covering: inline `fn drop` on named struct, named enum, anonymous struct, anonymous enum; bad-signature error; direct-call error.

- [ ] **Phase 4: Remove top-level `drop fn`**
  - Delete `Item::DropFn`, its parser, `InstData::DropFnDecl`, and its sema/analysis arms.
  - Migrate every `drop fn TypeName(self)` in `crates/gruel-spec/cases/` (destructors.toml, move-semantics.toml) to the inline `fn drop(self)` form.
  - Delete the one-turn migration diagnostic added in Phase 1.

- [ ] **Phase 5: Spec cleanup + stabilization**
  - New spec section for named-enum methods (6.3 addendum, mirroring 6.4 for structs).
  - Rewrite 3.9:24–27 (destructor declaration rules) to describe inline placement.
  - Rewrite 3.8:44 (`@handle` example) to use inline methods; remove the `impl Counter { ... }` block.
  - Audit `docs/spec/src/06-items/04-impl-blocks.md`: rename/retitle to cover inline methods uniformly across structs and enums, or delete if redundant with 6.4.
  - Traceability: every new/changed rule must have covering tests.
  - Remove `PreviewFeature::InlineTypeMembers`; remove `preview = "inline_type_members"` from tests.

## Consequences

### Positive

- **One mental model.** Members live with the type, period. Fewer forms to learn, teach, or remember.
- **Closes the named-enum gap.** `enum Option { Some(i32), None, fn is_some(...) { ... } }` finally works.
- **Unlocks destructors on anonymous types.** `Box(T)`, generic `Vec`, and any user-defined generic container can now clean up after itself without naming hacks.
- **Simplifies the compiler.** No top-level `drop fn` item; destructors are registered as part of type registration. Removes a name-resolution step.
- **Finishes the `impl` migration.** The spec stops lying about `impl` blocks.

### Negative

- **Breaking change** to top-level `drop fn TypeName(self)`. Mitigated by the in-repo-only user base and a one-turn migration diagnostic.
- **Parser complexity** slightly increases in the enum-body path (methods + drop mixed in with variants). Mirrored from the existing struct-body parser, so the cost is modest.
- **Spec churn** in chapters 3.8, 3.9, and 6.x.

### Neutral

- Drop-elaboration, codegen, and runtime contract from ADR-0010 are unchanged. This is a surface-syntax and registration refactor, not a semantic one.
- Structural equality for anonymous types is unchanged (method signatures participate, bodies do not, destructor signature is fixed).

## Open Questions

- **Allow a leading separator before the first method inside an enum body?** Structs currently allow methods after fields without a comma. Keep enums consistent.
- **Trivially-droppable warning when a type declares `fn drop` but all fields are trivial?** Probably not — users may want a destructor purely for side effects (logging, fd close). Leave it silent.
- **Deprecation window for top-level `drop fn`?** Proposal is a single diagnostic turn (phase 1) and hard removal in phase 4, since the surface area is tiny and entirely in-repo. Revisit if that assumption becomes wrong.
- **Reserve `drop` as a method name even outside the destructor slot?** The proposal demotes `drop` to an identifier and only privileges it in the type-body method-name position. An alternative is to keep `drop` globally reserved so users can never accidentally write a method called `drop` with the wrong signature. Leaning toward the permissive option, since the signature check already catches the only real mistake.

## Future Work

- **Linear-type consumption hooks** — still open from ADR-0010; orthogonal to where the destructor is written.
- **`Drop` trait** if/when traits land; the `drop fn` syntax would become sugar for an impl.
- **Visibility on methods** is inherited from ADR-0029 open questions and is out of scope here.

## References

- [ADR-0009: Struct Methods](0009-struct-methods.md) — original `impl` blocks, later migrated to inline.
- [ADR-0010: Destructors](0010-destructors.md) — destructor semantics (unchanged).
- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md) — source of the inline-method model.
- [ADR-0039: Anonymous Enum Types](0039-anonymous-enum-types.md) — inline methods on anonymous enums.
