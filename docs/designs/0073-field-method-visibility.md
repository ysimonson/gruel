---
id: 0073
title: Field and Method Visibility
status: proposal
tags: [visibility, modules, builtins, types]
feature-flag: field_method_visibility
created: 2026-05-01
accepted:
implemented:
spec-sections: ["6.2", "6.4"]
superseded-by:
---

# ADR-0073: Field and Method Visibility

## Status

Proposal

## Summary

Extend Gruel's existing item-level `pub` (ADR-0026) to **struct/enum fields** and
**methods**. A field or method without `pub` is visible only inside its own
module (intra-module access, exactly like today's private items); a `pub` field
or method is visible across module boundaries. The same rule applies to
synthetic built-in types: each built-in lives in a sentinel "builtin module"
that user code is never part of, so an unmarked built-in field or method is
unreachable from user code by the *same* mechanism that hides user-defined
private fields.

This subsumes and retires the ad-hoc `BuiltinField::private: bool` flag
introduced in ADR-0072 §2 — `String::bytes` becomes "a non-`pub` field in a
module the user can't reach," with no special-case sema code. Built-in methods
gain the same visibility knob (default `pub` for everything currently exposed),
which gives future built-ins room to express internal helpers without being
forced to exist outside the type.

The change is structural, not representational: the visibility check that
already exists for items (`SemaContext::is_accessible`, ADR-0026) is reused
verbatim for fields and methods.

## Context

### What exists today

ADR-0026 established Gruel's module system and visibility model:

- Items (functions, structs, enums, interfaces, constants) carry an
  `ast::Visibility { Private, Public }`, parsed from an optional `pub` keyword.
- Default is `Private`. Cross-module access requires `pub`. Intra-module
  (same-directory) access is always permitted, regardless of `pub`.
- The check is centralized in `SemaContext::is_accessible(accessing_file_id,
  target_file_id, is_pub) -> bool`.

This visibility never reached **fields** or **methods**:

- `parser::ast::FieldDecl` has `name`, `ty`, `span` — no `visibility`.
- `parser::ast::Method` has `directives`, `name`, `receiver`, `params`,
  `return_type`, `body`, `span` — no `visibility`.
- Sema treats every field and method as if it were `pub`. A struct exported
  with `pub struct Foo { x: i32 }` exposes its `x` to every module that can
  name `Foo`.

When ADR-0072 needed `String::bytes` to be inaccessible from user code, it
took a deliberately narrower path: add `BuiltinField::private: bool` (default
`false`) in `gruel-builtins`, mirror it to `StructField::is_private` in AIR,
and reject `expr.field` / field writes / construction-syntax for any field
where `is_private == true`. ADR-0072 §2 explicitly called the flag a
placeholder:

> When the broader visibility / module story arrives, `private: bool` is
> replaced by whatever visibility model lands.

That broader story is this ADR.

### Why now

1. **Built-in types are about to multiply.** ADR-0066 (`Vec(T)`), ADR-0070
   (`Result(T,E)`), ADR-0071 (`char`), ADR-0072 (`String`) all ship synthetic
   structs. Each has natural candidates for hidden state (Vec's `cap` is
   meaningful only via methods; Result's discriminant should be unobservable
   except through pattern matching). The ad-hoc `private` flag works for one
   field on one type; it does not scale.

2. **User-defined structs are an asymmetric trap.** A user can write
   `pub struct Account { balance: i64 }` today and watch every consumer reach
   in and mutate `balance` directly. Modules without field visibility cannot
   express invariants — exactly the gap ADR-0072 closed for `String` but only
   for `String`.

3. **The infrastructure already exists.** ADR-0026 has a working
   `is_accessible` check tied to file IDs; adding two new call sites (one for
   field access, one for method dispatch) and one new parse path (`pub` on
   field/method) is a much smaller change than designing a fresh visibility
   model.

### What this ADR does *not* attempt

- **Visibility levels beyond `pub` / module-private.** No `pub(crate)`,
  `pub(super)`, `pub(read)`, or friend-style exemptions. The cost/benefit
  argument from ADR-0026 ("simple pub/private covers 99% of use cases") still
  holds.
- **Visibility on enum variants.** Enum variants are nominally part of the
  enum's public interface; gating individual variants is a separate question
  about pattern-matching ergonomics that we can revisit when there's demand.
- **Visibility on associated constants** (when those land — not yet in the
  language). Will get the same `pub` treatment by default.
- **Field-level read/write asymmetry.** A field is either reachable or it
  isn't.
- **Re-exports of fields/methods.** ADR-0026's `pub const x = m.y` re-export
  pattern operates on items, not on field projections. No change here.

## Decision

### 1. Surface syntax

A `pub` keyword may precede a field declaration or a method definition. It is
optional; absence means "module-private."

```gruel
pub struct Account {
    pub id: u64,        // pub field — readable/writable from any module
    balance: i64,       // module-private — only this module can touch it

    pub fn balance(self) -> i64 { self.balance }      // pub method
    pub fn deposit(inout self, n: i64) { self.balance = self.balance + n }
    fn validate(self) -> bool { self.balance >= 0 }   // module-private helper
}
```

Grammar additions (spec §6.2 and §6.4):

```ebnf
struct_field = [ "pub" ] IDENT ":" type ;
method_def   = [ directives ] [ "pub" ] "fn" IDENT "(" [ method_params ] ")"
               [ "->" type ] block ;
```

The `pub` token already exists (`TokenKind::Pub`); no lexer change is needed.

### 2. AST changes

Add `visibility: Visibility` to:

- `parser::ast::FieldDecl`
- `parser::ast::Method`

Both default to `Visibility::Private` at parse time when `pub` is absent,
exactly as for items today.

`MethodSig` (interface methods) does **not** gain visibility — interface
methods are inherently part of the interface contract and are publicly
callable wherever the interface is in scope. (An interface itself has its own
`pub`-ness via ADR-0026.)

`EnumVariantField` (named fields inside struct-style enum variants) inherits
the rule from struct fields and gains the same `visibility` field.

### 3. Sema rule

For any field access — `expr.field` (read), `lhs.field = rhs` (write), or
construction `T { field: ... }` — sema computes:

- `accessing_file_id`: the file containing the access site.
- `target_file_id`: the file containing the **type definition** of `T`.
- `is_pub`: the resolved field's visibility.

Then:

```rust
if !ctx.is_accessible(accessing_file_id, target_file_id, is_pub) {
    return Err(CompileError::PrivateField { ... });
}
```

Identical logic for method calls and method-pointer references, against the
method's `is_pub`.

The check is invoked from the existing field- and method-resolution paths
(`analyze_ops.rs` for `FieldGet` / `FieldSet`, the struct-literal analyzer,
and the method-dispatch path in `analysis.rs::analyze_method_call`).

### 4. Built-in types: visibility, not privacy

`gruel-builtins` mirrors the user-facing model. Two tiny renames replace the
ad-hoc flag:

```rust
pub struct BuiltinField {
    pub name: &'static str,
    pub ty: BuiltinFieldType,
    pub is_pub: bool,   // was: `private: bool` (inverted polarity)
}

pub struct BuiltinMethod {
    pub name: &'static str,
    pub receiver_mode: ReceiverMode,
    pub params: &'static [BuiltinParam],
    pub return_ty: BuiltinReturnType,
    pub runtime_fn: &'static str,
    pub is_pub: bool,   // new — defaults to true for everything that exists
}

pub struct BuiltinAssociatedFn {
    // ... existing fields ...
    pub is_pub: bool,   // new — defaults to true for everything that exists
}
```

(Default polarity flips compared to the old `private` flag: explicitly listing
`is_pub: true` on every existing entry surfaces the audit, and the
"hide-by-default" preference for new internal fields is the right ergonomic.)

### 5. The "builtin module" identity

Built-ins do not live in a `.gruel` source file, but they need a stable
"home module" so the unified `is_accessible` check has a `target_file_id` to
compare against. Mechanism:

- Each `BuiltinTypeDef` is assigned a synthetic `FileId` when it is injected
  by `inject_builtin_types()` (one shared sentinel `FileId` for all built-ins
  is sufficient — there's no "intra-builtins" cross-access need).
- That sentinel `FileId` is registered in `SemaContext::file_paths` with a
  reserved path string (e.g., `"<builtin>"`).
- `SemaContext::get_module_identity` returns a distinct sentinel for the
  builtin path, ensuring no user file ever resolves to the same module.

User code, by construction, lives in a real file with a real path. It can
never share a module identity with `<builtin>`. So `is_accessible(user_file,
builtin_sentinel, is_pub=false)` always returns `false`, and the
non-`pub` fields/methods of every built-in are automatically inaccessible from
user code.

Built-in methods that are themselves Gruel-language method bodies (none today
— all built-in methods currently lower to runtime FFI calls — but the path
ADR-0072 §4 envisions for thinning the String runtime) will be sema-analyzed
with their `accessing_file_id` set to the builtin sentinel, giving them
unrestricted access to the type's own non-`pub` fields. This is identical to
how a regular Gruel function in a private module accesses other items in the
same module today.

The `BuiltinField::is_pub` and `BuiltinMethod::is_pub` flags carry through to
AIR's `StructField::is_pub` (renamed from `is_private`, polarity flipped).

### 6. Migration of `String`

`STRING_TYPE.fields[0]`:

- Before: `BuiltinField { name: "bytes", ty: BuiltinType("Vec(u8)"), private: true }`
- After:  `BuiltinField { name: "bytes", ty: BuiltinType("Vec(u8)"), is_pub: false }`

Every `BuiltinMethod` and `BuiltinAssociatedFn` on `String` (and on every
other existing built-in: `Vec`, `Ptr`, `Slice`, etc.) gets `is_pub: true` —
nothing changes about which methods are callable.

The `ErrorKind::PrivateField` variant is reused (rename to
`InaccessibleField` is *out of scope* — it's the same error, the wording can
stay). Its message is generalized from "private" to "not accessible from this
module"; the existing message ("field 'bytes' of 'String' is private") is
already module-correct since `<builtin>` is a different module.

Sema code paths that today branch on `struct_field.is_private` switch to
calling `is_accessible(...)` with the type's home file id, deleting the
hardcoded private-field arm. The result for user code targeting `String.bytes`
is the same error at the same site; the result for any future internal
built-in field is a one-line declaration in `BUILTIN_TYPES`.

### 7. Migration of user-defined structs

This is the source-breaking part: today's user-defined fields/methods are
implicitly public; after this ADR, they are implicitly module-private and
require `pub` to remain reachable cross-module.

The breakage is bounded:

- **Single-module programs** (every file in the same directory) see no change
  — intra-module access is permitted regardless of `pub`.
- **Multi-module programs** crossing directory boundaries that read or write
  fields of imported structs need `pub` on those fields.
- **Spec/UI tests** at `crates/gruel-spec/cases/` and `crates/gruel-ui-tests/cases/`
  will be audited and `pub`-ified where they assert cross-module field access.

Migration is gated by `--preview field_method_visibility` until Phase 6.
During preview, the new check fires only when the gate is on, so existing
programs are unaffected unless they opt in. At stabilization, the gate is
removed and the audit must be complete.

### 8. Construction and pattern matching

A struct literal `T { f: ..., g: ... }` mentions every field by name. A
pattern `T { f, g }` (or `T { f: pat, g: pat }`) likewise mentions fields by
name. Both are field references, so both are subject to the same access check
as `expr.field`. Mentioning a non-`pub` field of `T` from outside `T`'s
module is rejected.

Practical consequence: a struct with any non-`pub` field cannot be
constructed cross-module by literal syntax — you must call a `pub` associated
function. This matches Rust's exact rule and is the load-bearing mechanism
that lets `String` enforce its UTF-8 invariant: user code cannot build
`String { bytes: arbitrary_vec }`, only `String::from_utf8(v)` (which
validates) or `checked { String::from_utf8_unchecked(v) }`.

The wildcard `T { f, .. }` pattern remains the way to ignore unmentioned
fields, and crucially **it does not require those fields to be `pub`** — the
`..` doesn't reference any field by name.

### 9. Diagnostics

Errors reuse the existing `PrivateField` and `PrivateItem` infrastructure but
gain a "did you mean a `pub` accessor?" help line for fields whose owning
type also defines a `pub` getter/setter with a name resembling the field
(`balance` field → `balance()` method). This is purely a hint; not adding it
is non-blocking.

## Implementation Phases

Each phase is independently committable.

- [ ] **Phase 1: Preview gate + spec scaffolding**
  - Add `PreviewFeature::FieldMethodVisibility` to `gruel-error` (and
    `name()`, `adr()`, `all()`, `FromStr`).
  - Draft spec §6.2 and §6.4 deltas with rule IDs (no implementation yet):
    `pub` on `struct_field` and `method_def`, dynamic-semantics rules tying
    field/method access to the same module-equivalence rule used by items.

- [ ] **Phase 2: Parser**
  - Accept optional `pub` in `field_decl_parser` and `method_parser_with_expr`.
  - Add `visibility: Visibility` to `FieldDecl`, `Method`, and
    `EnumVariantField`. Default `Private` when absent.
  - Snapshot tests for both presence and absence; no behavior change yet.

- [ ] **Phase 3: User-defined sema check (gated)**
  - In sema, propagate field/method visibility into `StructField` /
    `StructDef::methods` (or wherever method visibility lives in AIR).
  - At every field-access and method-call site, call
    `ctx.is_accessible(accessing_file_id, type_home_file_id, is_pub)` and
    error with `PrivateField` / `PrivateMethod` (new variant) on failure.
    Gate the check on `PreviewFeature::FieldMethodVisibility` so existing
    programs continue to compile.
  - Spec tests under `cases/visibility/`: cross-module pub field accessible,
    cross-module non-pub field rejected, intra-module non-pub field
    accessible, struct literal across modules rejected, struct literal
    intra-module accepted.

- [ ] **Phase 4: Built-in unification**
  - Add `is_pub: bool` to `BuiltinField`, `BuiltinMethod`,
    `BuiltinAssociatedFn`. Replace `BuiltinField::private` with `is_pub`
    (inverted) at every declaration site in `gruel-builtins`. Remove the
    field.
  - Allocate the `<builtin>` sentinel `FileId` and register it in
    `SemaContext::file_paths` and the module-identity helper at the same
    initialization point that injects builtins.
  - Tag every existing built-in field/method with the right `is_pub`.
    `String::bytes` → `false`. Everything else → `true` (audit + flip).
  - In sema's field-access and method-dispatch paths, replace the
    `if struct_field.is_private { reject }` arm with the unified
    `is_accessible(...)` call. Both built-in and user-defined types route
    through the same code.
  - Verify ADR-0072's existing `String::bytes` privacy test still passes
    unchanged.

- [ ] **Phase 5: Stdlib audit**
  - `crates/gruel-spec/cases/`, `crates/gruel-ui-tests/cases/`, and any
    in-tree examples that perform cross-module field access add `pub` where
    needed.
  - `make test` clean with `--preview field_method_visibility` enabled by
    default in the spec runner (to catch missed audit cases before
    stabilization).

- [ ] **Phase 6: Stabilize**
  - Remove the preview gate; the new behavior becomes the default and the
    only behavior. Remove `PreviewFeature::FieldMethodVisibility`.
  - Update ADR-0072's status note: §2 (the ad-hoc `private` flag) is
    superseded by this ADR; the structural and invariant claims remain.
  - Spec sections 6.2 and 6.4 finalized.
  - The `BuiltinField::private` field name is gone; downstream documentation
    in `gruel-builtins/src/lib.rs` (e.g., the "Adding a new built-in type"
    walkthrough) updated.

## Consequences

### Positive

- Single visibility model across items, fields, methods, and built-in
  members — one mental concept, one sema check, one error path.
- The `BuiltinField::private` ad-hoc flag and its dedicated sema branch are
  retired; no compiler-internal special case for `String::bytes`.
- New built-in types get field/method visibility for free — adding a Vec
  internal helper or a Result discriminant getter is a one-line declaration.
- User-defined structs gain the ability to enforce invariants for the first
  time, closing the asymmetry where built-ins (post-ADR-0072) had a
  capability user types lacked.
- The visibility check is the same one used for items, so any future
  refinement of `is_accessible` (e.g., when a package system arrives) lifts
  field and method visibility along with it.

### Negative

- Source-breaking for multi-module programs that read fields of imported
  structs without `pub`. Mitigated by the preview gate (Phase 3 onward) and
  by intra-module being unchanged. The audit fits in one phase (Phase 5).
- Polarity flip on the built-in flag (`private` → `is_pub`) is mechanical
  but touches every built-in declaration. The compile errors guide the
  migration; risk is low.
- Adds a small amount of state to the AST (one `Visibility` field per
  field/method). Negligible.
- The `<builtin>` sentinel `FileId` is a synthetic concept that future
  package-system work will need to remain aware of. Documented in §5;
  ADR-0026's `is_accessible` already permits unknown paths to fall through
  permissively, so the failure mode is "too lenient" rather than "ICE."

### Neutral

- Compile-error messages for accessing a non-`pub` field from another module
  reuse the `PrivateField` machinery from ADR-0072. Wording will be
  generalized but no new diagnostic infrastructure is needed.

## Open Questions

- **Should non-`pub` methods participate in interface conformance checks?**
  Lean: no — a non-`pub` method is, by definition, not part of the type's
  public surface, and an interface is a cross-module contract. Open to
  revisiting if a same-module use case emerges.
- **Should `pub fn drop(self)` be required or implicit?** A `drop` method's
  visibility is observable only through the implicit drop path (which the
  compiler synthesizes). Lean: no `pub` required for `drop`; the destructor
  is always callable by the language.
- **Sentinel `FileId` for built-ins — one shared, or one per built-in
  type?** §5 picks one shared. If future built-ins need to access each
  other's non-`pub` members (e.g., `String::from_utf8` reaching into
  `Vec(u8)`'s internals), one shared sentinel makes that trivial; one per
  type would force `pub`-ing those internals. The shared model is the
  cheaper default.
- **Should `pub` on a tuple-struct field have a position-based syntax?**
  E.g., `struct Pair(pub i32, i32)`. Tuple structs aren't yet a first-class
  thing in Gruel (ADR-0048 covers tuples but tuple-shaped structs use named
  fields); revisit if/when we add tuple-struct sugar.

## Future Work

- **Coarser visibility tiers** (`pub(crate)`, `pub(super)`) — the ADR-0026
  rationale rejected these for items; same rationale applies here.
- **Cross-module struct construction shortcuts** (e.g., a `pub` "all-pub"
  associated `new` synthesized for structs whose every field is `pub`). Not
  needed; users can write the constructor.
- When a real package boundary lands (one beyond directory modules), the
  `is_accessible` check will be the single place that learns about it; field
  and method visibility ride along for free.

## References

- ADR-0023: Multi-file compilation (the flat-namespace predecessor).
- ADR-0026: Module system — establishes the `pub` / module-private model
  this ADR extends.
- ADR-0072: String as a newtype wrapper over Vec(u8) — introduced the
  ad-hoc `BuiltinField::private` flag this ADR retires.
- Rust's field/method visibility (`pub`, default-private) — same model.
- Hylo's intra-module-public, cross-module-`pub` posture — same model
  (cited in ADR-0026 as the reference design).
