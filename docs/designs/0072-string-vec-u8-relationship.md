---
id: 0072
title: String as a Newtype Wrapper over Vec(u8)
status: proposal
tags: [types, strings, ffi, utf8, collections, builtins]
feature-flag: string_vec_bridge
created: 2026-05-01
accepted:
implemented:
spec-sections: ["7.4"]
superseded-by:
---

# ADR-0072: String as a Newtype Wrapper over Vec(u8)

## Status

Proposal

## Summary

Redefine `String` as a synthetic newtype wrapper over `Vec(u8)`:

```gruel
synthetic struct String {
    bytes: Vec(u8)   // private field — inaccessible from user code
}
```

The wrapper carries a **UTF-8 invariant** that `Vec(u8)` does not. Layout is identical to today's String (a struct containing one `Vec(u8)` field is `{ptr, len, cap}`, 24 bytes), so the change is structural, not representational. The consequences cascade:

- **Conversions are field-level operations, not codegen retags.** `s.into_bytes() ≡ self.bytes` (a struct field move). `String::from_utf8_unchecked(v) ≡ String { bytes: v }` (struct construction).
- **Method delegation is composition, not a "dispatch marker."** `s.bytes_len() ≡ self.bytes.len()` is just a method body. `s.contains(needle) ≡ self.bytes.contains(needle.bytes)`. No new sema infrastructure.
- **Drop/eq/cmp/alloc are inherited.** `String`'s drop runs Vec(u8)'s drop on the field. Equality and ordering operate on the contained Vec(u8). The bespoke `__gruel_str_*` helpers go away by composition.
- **The String runtime collapses.** Today's ~490 LOC in `gruel-runtime/src/string.rs` shrinks to the genuinely UTF-8-specific surface: validation (`__gruel_utf8_validate`), `from_c_str` ingest, and `terminated_ptr`'s NUL-write step.

The UTF-8 invariant is enforced by a small wall of methods that own the construction sites: `push(c: char)` (encodes via ADR-0071), `from_utf8(v: Vec(u8)) -> Result(String, Vec(u8))` (validates; uses ADR-0070), `from_utf8_unchecked` and `push_byte` (`checked`-block escape hatches). Direct access to the `bytes` field is rejected by sema — that's the load-bearing privacy mechanism this ADR introduces (a small `private: bool` flag on `BuiltinField`, no full visibility system required).

The result: Rust-style invariants on the safe path, Zig-style "you name the trust" on the FFI path, zero validation cost at any boundary where the producer can vouch for the bytes, and **a single byte-buffer implementation** underneath both types — by structural composition, not by maintained parallelism.

## Context

### Where Gruel sits today

- `String` is a synthetic struct in `gruel-builtins` with three exposed fields `{ ptr: u64, len: u64, cap: u64 }`. The fields are technically user-accessible (no privacy mechanism); in practice users don't touch them, but nothing stops them.
- `Vec(T)` (ADR-0066) is a generic monomorphized type with the same `{ptr, len, cap}` layout.
- `String` ships ~490 LOC of bespoke runtime: 14 `String__*` FFI functions and a small zoo of byte-level helpers (`__gruel_str_eq`, `__gruel_str_cmp`, `__gruel_string_alloc`, `__gruel_string_realloc`, `__gruel_string_clone`, `__gruel_drop_String`). Almost every one duplicates logic `Vec(u8)` has or naturally gains.
- `String` is *not* UTF-8-validated at the type level — `push(byte: u8)` accepts arbitrary bytes.
- No conversions exist between `String` and `Vec(u8)`.
- `Vec(T)::terminated_ptr(s: T)` exists for null-terminated FFI handoff (ADR-0066).
- `Option(T)` exists (ADR-0065). **`Result(T, E)` is introduced by ADR-0070** and is a hard prereq for the validated-conversion APIs here.
- **`char` is introduced by ADR-0071** and is a hard prereq for the safe `push(c: char)` mutator.

### What's missing

1. **A clear UTF-8 contract on `String`.** Without one, `String` provides no guarantee that any future `chars()`, `format!`, or codepoint-indexed slicing primitive can rely on. Adding such operations later without an invariant means each callsite re-validates or risks UB.
2. **A way to pass bytes between `String` and `Vec(u8)` without paying for it.** The two types share a layout — moving a buffer between them should cost nothing. Today users have no API.
3. **A C-interop story for strings.** `Vec(u8)` already has `terminated_ptr(0)` in a `checked` block, but `String` has no equivalent.
4. **A consistent safety model for invariant-breaking operations.** `String::push(byte: u8)` is the only existing API that could write a non-UTF-8 byte. Once UTF-8 is an invariant, this call site has to be marked.
5. **Structural unification.** The shared layout is currently a coincidence the implementation maintains by hand. Each new method on `String` is an opportunity to drift from `Vec(u8)`'s behavior. Making the relationship structural — `String` literally *contains* a `Vec(u8)` — turns "do these two types stay consistent?" from a discipline question into a non-question.

### What this ADR does *not* attempt

- **The `char` type itself.** Defined in ADR-0071. This ADR consumes it.
- **The `Result(T, E)` type itself.** Defined in ADR-0070. This ADR consumes it.
- **A general visibility / `pub`-`priv` system.** This ADR introduces only a `private: bool` flag on `BuiltinField` so synthetic builtins can hide internal fields. User-defined structs are unaffected; full visibility waits for the module system.
- **Codepoint iteration (`s.chars()`).** Requires Gruel's iterator story. Future work, *enabled* by the invariant established here.
- **`&str` / borrowed string slices.** Future work, paired with codepoint iteration.
- **Stabilizing UTF-8 enforcement on `.rodata` literals.** Source files are UTF-8 (already enforced by the lexer), so literals are already valid. No new check needed.

### Where Gruel lands relative to other languages

- **Rust.** `String` is `Vec<u8>` with a UTF-8 invariant — same shape. The internal `Vec<u8>` is private; conversions are `into_bytes`, `from_utf8`, `from_utf8_unchecked`, `as_bytes`. C strings are a separate `CString` type. Gruel matches Rust's structural model exactly. Differences: no `&str` (yet), no separate `CString` (the FFI conversion is a method on `String` directly, mirroring `Vec(T)::terminated_ptr`).
- **Zig.** Strings are `[]const u8` / `[]u8` with no UTF-8 invariant. Validation is a library call. We take the *invariant* from Rust and the *boundary-conversion* posture from Zig.
- **Go.** `string` is immutable bytes, conventionally UTF-8 but not enforced. Conversion to `[]byte` copies. Gruel rejects the copy — the structural composition makes the conversion free.
- **C++.** `std::string` is a NUL-terminated byte buffer with no UTF-8 invariant. Gruel rejects the maintained-NUL invariant (pay-on-every-mutation for a moment-of-handoff property; same reasoning as ADR-0066 for `Vec(T)`).

## Decision

### 1. Definition

`String` is a synthetic struct injected by `gruel-builtins`:

```gruel
synthetic struct String {
    bytes: Vec(u8)   // private
}
```

Layout: identical to `Vec(u8)`, since the struct has exactly one field of that type. 24 bytes on a 64-bit target, alignment 8.

The `bytes` field is **private**: sema rejects user code that writes `s.bytes` for any String value `s`. The only access is through methods defined by the builtin itself (which is permitted to read/write the field at the sema level).

### 2. Field privacy mechanism

`gruel-builtins` extends `BuiltinField` with a `private: bool` flag:

```rust
pub struct BuiltinField {
    pub name: &'static str,
    pub ty: BuiltinFieldType,
    pub private: bool,   // new
}
```

`BuiltinFieldType` gains a variant for referencing other built-in or generic types:

```rust
pub enum BuiltinFieldType {
    U64, U8, Bool,
    BuiltinType(&'static str),    // e.g., "Vec(u8)"
}
```

Sema, when resolving `expr.field` where `expr` has a built-in struct type, checks the `private` flag of the resolved field. If `private == true` and the access site is *not* inside a method of that type's own builtin definition, sema reports an error: `field 'bytes' of 'String' is private`. Method bodies registered in `BuiltinTypeDef` are exempt.

This is **deliberately narrower** than a general visibility system. It hides exactly the fields that need hiding (right now: `String::bytes`; future built-ins may follow). It commits to no syntax (no `pub` keyword, no module system). When the broader visibility / module story arrives, `private: bool` is replaced by whatever visibility model lands; the invariant guarantee survives the migration unchanged.

### 3. UTF-8 invariant

After this ADR, `String` carries the normative invariant:

> The bytes in `self.bytes[0..self.bytes.len()]` form a valid UTF-8 sequence.

Established at:
- **Compile time** for `.rodata` string literals (source is UTF-8; lexer already enforces).
- **Construction time** for `String::new()` and `String::with_capacity(n)` (empty buffer, trivially valid).
- **By validation** for `String::from_utf8(v: Vec(u8))` (O(n) UTF-8 scan; returns `Result(String, Vec(u8))`).
- **By construction** for `String::push(c: char)` and `String::from_char(c)` — `char` carries the scalar-value invariant (ADR-0071), so the encoder produces well-formed UTF-8 by definition.
- **By assertion** for `String::from_utf8_unchecked(v)` and `push_byte(b)` (caller's obligation; `checked` block only).

Preserved by all other methods because they ultimately operate on the private `bytes` field via append-of-valid-bytes (e.g., `push_str`, `concat`, `clone`, `clear`, `reserve`).

### 4. Method surface (everything is wrapper-thin)

The full method list, expressed as one-liners over the inner Vec(u8):

| Method | Body |
|---|---|
| `String::new() -> String` | `String { bytes: Vec(u8)::new() }` |
| `String::with_capacity(n) -> String` | `String { bytes: Vec(u8)::with_capacity(n) }` |
| `String::from_char(c: char) -> String` | `let mut s = String::new(); s.push(c); s` |
| `s.bytes_len() -> usize` | `self.bytes.len()` — byte count, not codepoint count. Explicit naming leaves room for future `s.chars_len()` once `chars()` lands. |
| `s.bytes_capacity() -> usize` | `self.bytes.capacity()` — byte capacity of the underlying buffer. |
| `s.is_empty() -> bool` | `self.bytes.is_empty()` |
| `s.clone() -> String` | `String { bytes: self.bytes.clone() }` |
| `s.contains(needle: String) -> bool` | byte-search on `self.bytes` against `needle.bytes` |
| `s.starts_with(prefix: String) -> bool` | same |
| `s.ends_with(suffix: String) -> bool` | same |
| `s.concat(other: String) -> String` | `String { bytes: self.bytes.concat(other.bytes) }` |
| `s.push_str(other: String) -> Self` | `self.bytes.extend_from_slice(&other.bytes[..])` |
| `s.push(c: char) -> Self` | `let mut buf = [0u8; 4]; let n = c.encode_utf8(&mut buf); self.bytes.extend_from_slice(&buf[..n])` |
| `s.clear() -> Self` | `self.bytes.clear()` |
| `s.reserve(n: usize) -> Self` | `self.bytes.reserve(n)` |
| `s == s'`, `s < s'`, ... | structural: `self.bytes == other.bytes`, `self.bytes < other.bytes` |
| `drop` | runs Vec(u8)::drop on `self.bytes` (auto-derived from struct drop glue) |

Methods that need to exist on `Vec(u8)` for these to delegate cleanly:

- `Vec(u8)::contains`, `starts_with`, `ends_with` — byte-search ops. (Today these are String-only via runtime; promoting to Vec(u8) is a Vec gain too.)
- `Vec(u8)::concat` — allocate + two memcpys. New.
- `Vec(u8)::extend_from_slice` — append a slice's bytes. New (general; not String-specific).

These additions are net wins for `Vec(u8)` independent of String. Estimated: ~80 LOC of inline LLVM in `gruel-codegen-llvm`.

### 5. Conversion API

#### 5.1 `String -> Vec(u8)` — always safe, O(1)

```gruel
fn String::into_bytes(self) -> Vec(u8) {
    self.bytes
}
```

A single struct-field move. No codegen support beyond what struct destructuring already provides.

#### 5.2 `Vec(u8) -> String` validated, O(n)

```gruel
fn String::from_utf8(v: Vec(u8)) -> Result(String, Vec(u8)) {
    if utf8_validate(&v[..]) {
        Result::Ok(String { bytes: v })
    } else {
        Result::Err(v)
    }
}
```

The `Err` arm hands the buffer back at zero copy — the call site can inspect, retry, or report without `clone()`-ing defensively beforehand. Requires ADR-0070.

#### 5.3 `Vec(u8) -> String` trusted, O(1)

```gruel
checked {
    fn String::from_utf8_unchecked(v: Vec(u8)) -> String {
        String { bytes: v }
    }
}
```

Pure struct construction. Caller asserts the UTF-8 invariant.

#### 5.4 Vec(u8) side (sugar)

```gruel
fn Vec(u8).into_string(self) -> Result(String, Vec(u8))      // = String::from_utf8(self)
checked {
    fn Vec(u8).into_string_unchecked(self) -> String         // = String::from_utf8_unchecked(self)
}
```

### 6. C interop

#### 6.1 String -> C: NUL-terminated handoff

```gruel
checked {
    fn String.terminated_ptr(&mut self) -> Ptr(u8) {
        self.bytes.terminated_ptr(0u8)
    }
}
```

Delegates to `Vec(T)::terminated_ptr` from ADR-0066. The implicit sentinel is `0u8` (NUL is the only sensible choice for C strings).

#### 6.2 C -> String: ingest

```gruel
checked {
    fn String::from_c_str(p: Ptr(u8)) -> Result(String, Vec(u8)) {
        let v = vec_from_c_str(p);              // strlen + copy into a Vec(u8)
        String::from_utf8(v)                     // delegates to 5.2
    }

    fn String::from_c_str_unchecked(p: Ptr(u8)) -> String {
        String::from_utf8_unchecked(vec_from_c_str(p))
    }
}
```

The `vec_from_c_str` helper (an intrinsic or runtime function) does `strlen` + allocate + copy and returns a `Vec(u8)`. Both `from_c_str` variants reuse `from_utf8` / `from_utf8_unchecked` — they don't need their own validation paths.

(Neither form can be zero-copy — Gruel can't take ownership of a foreign-allocated buffer without knowing its allocator.)

### 7. Mutation: safe and unsafe paths

```gruel
fn String.push(&mut self, c: char) -> Self           // safe; primary

checked {
    fn String.push_byte(&mut self, byte: u8) -> Self // niche escape hatch
}
```

`push(c: char)` is the safe codepoint-level primary (see §4 for the body). The invariant is preserved by construction: `c` is a valid scalar, the encoder produces well-formed UTF-8, and `extend_from_slice` appends those bytes to an already-valid sequence.

`push_byte(byte: u8)` is preserved as a niche escape hatch for callers writing binary protocols, parsing one byte at a time with their own UTF-8 invariant proof, or doing performance-critical construction where chunked encoding would force per-byte branching. Caller obligation: maintain the invariant.

Migration: today's `String::push(byte: u8)` is renamed twice over — `push` becomes the safe `char`-taking form, and the byte form moves to `push_byte` and into a `checked` block. Each call site of the old `push(byte: u8)` either upgrades to `push(c: char)` or wraps in `checked { s.push_byte(b) }`.

### 8. Runtime functions

After this ADR, the only String-specific runtime symbols are:

| Symbol | Purpose |
|---|---|
| `__gruel_utf8_validate(ptr: *const u8, len: u64) -> u8` | Returns 1 if valid UTF-8, 0 otherwise. Used by `from_utf8` and `from_c_str`. |
| `__gruel_vec_from_c_str(out: *mut VecU8Result, p: *const u8)` | strlen + allocate + copy. Returns a `Vec(u8)` via sret. |

That's it. Everything else delegates to `Vec(u8)` operations (drop, eq, cmp, alloc, realloc, clone) which already exist or are added as part of §4. The 14 `String__*` runtime functions and the byte-level `__gruel_str_*` helpers in today's `gruel-runtime/src/string.rs` are all deleted.

`__gruel_alloc`, `__gruel_realloc`, `__gruel_free` (existing shared allocator primitives) continue to back `Vec(u8)`'s storage.

Net: `gruel-runtime/src/string.rs` collapses from ~490 LOC to ~50 LOC.

### 9. Sema and codegen

- **Sema:** add the `private: bool` flag to `BuiltinField` and the `BuiltinFieldType::BuiltinType(&str)` variant. Field-access checks for built-in struct fields consult `private`. Method bodies in `BuiltinTypeDef` are sema-exempt from the privacy check. No other changes.
- **Codegen:** struct construction and field move/access already exist; nothing new needed for `into_bytes`, `from_utf8_unchecked`, or any of the wrapper methods. The new `Vec(u8)` byte-search methods (`contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice`) lower as inline LLVM in `gruel-codegen-llvm`'s Vec lowering path.
- **Spec:** new section `7.4 String / Vec(u8) Relationship` capturing the UTF-8 invariant, the conversion APIs, the `checked`-block requirements, and the field-privacy convention for built-ins.

## Implementation Phases

**Prerequisites:** ADR-0070 (`Result`) Phases 1–2 must land before this ADR's Phase 3. ADR-0071 (`char`) Phases 1–5 must land before this ADR's Phase 4. ADR-0071 itself depends on ADR-0070, so the natural ordering is: Result → char → String.

- [x] **Phase 1: Preview gate + spec scaffolding**
  - Add `PreviewFeature::StringVecBridge` to `gruel-error`.
  - Draft spec section 7.4 with rule IDs (no implementation yet).
- [x] **Phase 2: Field privacy + newtype redefinition**
  - Add `private: bool` to `BuiltinField`; add `BuiltinFieldType::BuiltinType(&str)`.
  - Sema check: reject `expr.field` for private built-in fields outside the type's own methods.
  - Replace `STRING_TYPE`'s field list with `[BuiltinField { name: "bytes", ty: BuiltinType("Vec(u8)"), private: true }]`.
  - Add the missing `Vec(u8)` methods (`contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice`) as inline LLVM in `gruel-codegen-llvm`. Spec tests for each. *(Deferred — promoted to a follow-up; the existing `String` runtime keeps working with the new layout, so the user-facing privacy + structural rename ships independently.)*
  - Rewrite all existing `String` methods as composition over `self.bytes` (the bodies in §4). Delete the old `String__*` runtime functions. *(Deferred — current `String__*` runtime functions are bit-compatible with the new `{ Vec(u8) }` layout, so they continue to work. Final composition + runtime collapse is queued for stabilization.)*
  - Spec tests: every existing String operation still works; private-field access from user code is rejected.
- [x] **Phase 3: Validated conversions** *(requires ADR-0070 Phases 1–2)*
  - `__gruel_utf8_validate` runtime fn.
  - `String::into_bytes`, `String::from_utf8_unchecked` (in `checked`). `Vec(u8) → bool` validation via `@utf8_validate(s: borrow Slice(u8))` intrinsic.
  - `Vec(u8).into_string` / `into_string_unchecked` sugar — *deferred* (see Open Question below).
  - `String::from_utf8` returning `Result(String, Vec(u8))` — *deferred* (see Open Question below).
  - Spec tests covering empty / non-empty / round-trip, both `compile_fail` (gating) and `exit_code` paths.

  *Open question:* `String::from_utf8 -> Result(String, Vec(u8))` is blocked on a pre-existing codegen bug in pass-by-value of types with destructors: a function parameter of type `Vec(T)` (or `String`) with a non-empty buffer crashes at runtime because the codegen emits the drop sequence multiple times per call (visible in the generated LLVM IR as repeated `vec_drop_alive` blocks). The natural prelude implementation of `from_utf8` takes `Vec(u8)` by value and is therefore unsafe to ship. The trusted-construction path (`from_utf8_unchecked`) and the byte-shape conversion (`into_bytes`) work today because they go through the builtin runtime FFI rather than user-visible function parameters. Fixing the multi-drop bug unblocks the validated `from_utf8` (and the matching `from_c_str` / `into_string` sugar) in a follow-up.
- [x] **Phase 4: Char-aware mutation** *(requires ADR-0071 Phases 1–5)*
  - `String.push(c: char)` — body per §4. *Already exists today as `push_char(c: char)` from ADR-0071.*
  - `String::from_char(c)`. *Already exists from ADR-0071.*
  - Rename today's `String::push(byte: u8)` → `push_byte`, gate to `checked`.
  - Migrate existing callers — *deferred* (see Open Question below).
  - Spec tests: 1- / 2-byte char pushes through `push_char`, `push_byte` rejected without `checked`, accepted inside.

  *Open question:* the dual rename `push_char` → `push` and `push` → `push_byte` is a source-breaking migration of every existing caller of `s.push(byte)`. v1 ships the new `push_byte` (gated to `checked` and `string_vec_bridge`) alongside the existing `push(byte: u8)` and `push_char(c: char)` — three names for the two byte/char operations — to avoid breaking in-tree call sites mid-stabilization. The rename-and-gate happens at stabilization time, when `push(byte: u8)` is removed (now redundant with `push_byte`) and `push_char` is renamed to `push`.
- [x] **Phase 5: C interop**
  - `__gruel_vec_from_c_str` runtime fn.
  - `String::terminated_ptr` (in `checked`) — runtime function lands; returns `Ptr(u8)`. *Open question:* the receiver is `&mut self`, but the existing `analyze_builtin_method` mutation path stores the returned value back to the receiver storage. For `terminated_ptr` we want the runtime to mutate the receiver in place (capacity may grow) AND return a separate `Ptr(u8)`; the current pathway only handles "ByMutRef + returns Self". Sema gates `terminated_ptr` correctly (preview + checked), and the runtime is in place; the storage-write path needs a follow-up that allows ByMutRef methods returning a non-Self value.
  - `String::from_c_str_unchecked` (in `checked`).
  - `String::from_c_str` (returns `Result(String, Vec(u8))`) — *deferred* alongside Phase 3's `from_utf8` for the same reason.
  - Spec tests: gating tests for `terminated_ptr`, `from_c_str_unchecked` ships as a registry assoc-fn.
- [ ] **Phase 6: Stabilize** — *Pending validated `from_utf8` / `from_c_str` and the `push` rename.*
  - Remove preview gate.
  - Finalize spec section 7.4.
  - Update ADR-0066's "future work" note pointing to this ADR as resolved.

## Consequences

### Positive

- `String` gains a real, enforceable invariant — unlocks future `chars()`, formatting, codepoint-indexed slicing without re-validation at every site.
- The relationship between `String` and `Vec(u8)` becomes structural, not maintained-by-hand. They cannot drift.
- Cross-type conversions are language-level operations (struct field move, struct construction). No codegen support needed beyond what already exists.
- `gruel-runtime/src/string.rs` shrinks from ~490 LOC to ~50 LOC. The 14 `String__*` FFI calls are replaced by inline LLVM via `Vec(u8)` (faster, no FFI overhead).
- `Vec(u8)` gains useful methods (`contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice`) as a side effect — a transferable win.
- The field-privacy mechanism is small (~50 LOC) and the right shape for future use (other builtins can hide internal fields without waiting for a full visibility system).
- FFI handoff is symmetric with `Vec(T)`: both use `terminated_ptr` with the same boundary-conversion posture.
- No new types introduced (no `CString`, no `&str`). Surface area stays small.

### Negative

- `String::push(byte: u8)` is renamed *and* its semantics shift (the new `push` takes `char`). Two source-breaking changes at one call site, but they land in the same phase, and the migration is mechanical.
- Phase 2 is a substantial restructure: every existing String method body is rewritten, and the runtime is gutted. Risk of subtle regressions in widely-used String operations. Mitigated by spec-test coverage on the safe path, and by Phase 2 being the *only* phase that touches existing behavior — Phases 3–5 add new APIs that don't affect existing callers.
- Hard dependency on two concurrent ADRs (0070 and 0071). If either slips, this ADR's Phase 3 or Phase 4 stalls. Mitigated by Phases 1, 2, and 5+ having no dependency on the other ADRs.
- `from_c_str` always copies — Gruel can't safely take ownership of foreign-allocated memory. This is correct, but worth flagging: programs that `read` a large file via libc and want zero-copy will need to allocate via `__gruel_alloc` and use `@parts_to_vec` (ADR-0066) instead.
- The privacy mechanism is narrower than a real visibility system. It works for "hide a synthetic field," but doesn't generalize. When the module/visibility ADR lands, the `private: bool` flag is replaced (not extended). Acceptable for v1 because the only consumer right now is `String::bytes`; the cost of replacement is small.

## Open Questions

- **Should `terminated_ptr` mirror `Vec`'s explicit-sentinel form for consistency, even though `0` is the only sensible choice for C strings?** I.e., `s.terminated_ptr(0u8)` vs `s.terminated_ptr()`. Leaning toward the no-arg form (NUL is implicit for strings); revisit if non-NUL-terminated FFI use cases emerge.
- **Should `from_utf8`'s `Err` carry a UTF-8-error position alongside the `Vec(u8)`?** v1 says no — just `Result(String, Vec(u8))`. A future `from_utf8_with_position` returning `Result(String, (Vec(u8), usize))` is cheap to add when there's demand.
- **Should the `private` flag default to `true` for new built-in fields, with public being opt-in?** v1 makes both opt-in (default `false`). When more built-ins use private fields, revisit the default — it's a one-line change.

## Future Work

- `s.chars() -> ...` codepoint iterator — enabled by `char` and the UTF-8 invariant established here, but waiting on Gruel's general iterator interface.
- Codepoint-indexed slicing operations (e.g., `s.char_at(i: usize)`).
- Borrowed `&str` / `Slice(u8)` views once references mature.
- Lossy variants: `String::from_utf8_lossy(v)` returning a `String` with `U+FFFD` replacements.
- `from_utf8_with_position` returning the byte index of the first invalid sequence on failure.
- General visibility / module system — at which point the `private: bool` flag is replaced by whatever model lands. The structural relationship between `String` and `Vec(u8)` survives the migration unchanged.

## References

- ADR-0020: Built-in types as synthetic structs (current `String` mechanism this ADR restructures).
- ADR-0064: Slices (`Slice(T)` / `MutSlice(T)`).
- ADR-0065: Clone and Option.
- ADR-0066: `Vec(T)` — owned, growable vector with on-demand sentinel (the substrate this ADR builds on; `terminated_ptr` precedent; "future work" note about migrating String onto Vec(u8) is what this ADR resolves).
- ADR-0069: Layout abstraction and niche-filling (relevant for `Result(String, Vec(u8))` layout compaction).
- ADR-0070: Result(T, E) (consumed by `from_utf8` and `from_c_str` return shapes).
- ADR-0071: char type (consumed by `String::push(c: char)` and `String::from_char(c)`).
- Rust's `String` ↔ `Vec<u8>` API: `into_bytes`, `from_utf8`, `from_utf8_unchecked`. Same structural model.
- Zig's `std.unicode.utf8ValidateSlice` and `[*:0]const u8` sentinel-typed pointers.
