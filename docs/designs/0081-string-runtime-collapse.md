---
id: 0081
title: String Runtime Collapse onto Vec(u8)
status: implemented
tags: [stdlib, strings, runtime, builtins]
feature-flag:
created: 2026-05-07
accepted: 2026-05-07
implemented: 2026-05-08
spec-sections: ["7.4"]
superseded-by:
---

# ADR-0081: String Runtime Collapse onto Vec(u8)

## Status

Implemented

## Summary

Retire `STRING_TYPE` from `BUILTIN_TYPES` and migrate `String` to a regular Gruel `pub struct String { bytes: Vec(u8) }` declaration in `prelude/string.gruel`, with all method bodies expressed as `self.bytes.method(...)` compositions. The 14 `String__*` runtime FFI functions, the 6 `BuiltinOperator` registry entries that route to `__gruel_str_eq` / `__gruel_str_cmp`, and the bespoke `__gruel_drop_String` / `__gruel_string_alloc` / `__gruel_string_realloc` / `__gruel_string_clone` helpers are all deleted. Operator overloading from ADR-0078 (`Eq` / `Ord` interfaces) replaces the registry-driven `==` / `<` dispatch via `eq` / `cmp` methods on the new struct. The two genuinely UTF-8-specific runtime entries (`__gruel_utf8_validate`, `__gruel_cstr_to_vec`) stay; they are already called from prelude code via the `@utf8_validate` / `@cstr_to_vec` intrinsics. As a prerequisite, `Vec(T)` (where `T: Copy`) gains seven new methods — `eq`, `cmp`, `contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice` — five of which were explicitly anticipated and deferred from ADR-0072 Phase 2. The two new comparison methods (`eq`, `cmp`) make `Vec(T)` itself an `Eq`-/`Ord`-conforming type as a side effect.

`Vec(T)` and its codegen-inline lowering are **out of scope**. The full Vec collapse (Vec as a comptime-generic Gruel struct calling `@alloc`/`@realloc`/`@free`) requires substantial new compiler infrastructure — comptime-generic struct syntax, `@alloc`/`@drop` intrinsics, generalized `Index` overloading, generalized scope-bound types for Slice — and is left to a separate ADR.

**LOC impact.** Roughly 640 LOC removed from `gruel-runtime/src/string.rs` (out of 751), ~110 LOC removed from `gruel-builtins/src/lib.rs` (the entire `STRING_TYPE` constant), and ~80 LOC of new inline-LLVM codegen added across the seven new Vec methods, ~120 LOC of Gruel added to `prelude/string.gruel` (the full String struct body). Net Rust LOC out: ~670; net Gruel LOC in: ~120. The structural value is bigger than the line count: every existing String semantic is now expressed in Gruel, and `String` joins `Option` / `Result` as a regular prelude type.

## Context

### Where things sit today

- **`STRING_TYPE`** is a 279-LOC `BuiltinTypeDef` constant in `gruel-builtins/src/lib.rs:336–614`. It declares one private field (`bytes: Vec(u8)`, post ADR-0072), 5 associated functions, 14 instance methods, and 6 operator entries. Every method routes to a `String__*` extern via the `runtime_fn: &'static str` field on `BuiltinMethod`. There is no inline-Gruel-body mechanism on `BuiltinMethod` today.
- **`gruel-runtime/src/string.rs`** is 751 LOC. ADR-0072 Phase 2 anticipated this would shrink to ~50 LOC; in practice the runtime collapse was deferred at stabilization because the existing `String__*` functions remained bit-compatible with the new `{ Vec(u8) }` layout. The collapse is now overdue.
- **The 14 instance methods** break into three groups by the runtime work they actually do:
  - **Pure byte ops on `{ ptr, len, cap }`**: `len`, `capacity`, `is_empty`, `clone`, `contains`, `starts_with`, `ends_with`, `concat`, `push_str`, `clear`, `reserve`, `bytes_len`, `bytes_capacity`. These are byte-buffer primitives that have nothing to do with UTF-8.
  - **UTF-8-encoded mutation**: `push(c: char)` (encodes a `char` to 1–4 UTF-8 bytes via `encode_utf8`, then appends).
  - **FFI / `checked` escape hatches**: `push_byte`, `terminated_ptr`, `into_bytes`.
- **The 6 operator entries** (`==`, `!=`, `<`, `<=`, `>`, `>=`) all route to `__gruel_str_eq` (3 args: ptr1/len1 vs ptr2/len2 → bool) or `__gruel_str_cmp` (returns i8, with comparison-specific result-flag interpretation). These are pure byte-equality / lexicographic-byte-compare.
- **`prelude/string.gruel`** is 44 LOC today and contains only the validated `String__from_utf8` / `String__from_c_str` conversions (ADR-0072 Phase 3) plus the `Utf8DecodeError` wrapper struct.
- **ADR-0078** moved built-in interface declarations (`Drop`, `Copy`, `Clone`, `Handle`) and built-in enum declarations (`Arch`, `Os`, `TypeKind`, `Ownership`) to Gruel, established the prelude as a `prelude/` directory of `.gruel` files, and added `Eq` / `Ord` interfaces with operator desugaring for non-built-in types. Per ADR-0078's binop dispatch ladder (`crates/gruel-air/src/sema/analysis.rs:4404+`): step 3 (BUILTIN_TYPES registry) wins for `String` today, before step 4 / 5 (Eq / Ord interface fallthrough) ever runs. Removing `STRING_TYPE` makes step 3 miss for String, so the new `eq` / `cmp` methods on the prelude struct take over via step 4 / 5.

### What's missing

1. **Vec(u8) byte-search and byte-comparison primitives.** ADR-0072 Phase 2 explicitly deferred adding `contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice` to `Vec(T)`. Without them, the String composition rewrite stalls — `s.contains(needle)` has nothing on `Vec(u8)` to delegate to. Two more (`eq`, `cmp`) are needed so `Vec(T)` itself can satisfy `Eq` / `Ord`, which lets `String::eq` write `self.bytes == other.bytes` directly through ADR-0078's binop dispatch.
2. **A way for `String` to live in the prelude.** Today STRING_TYPE is recognized by the compiler via `BUILTIN_TYPES` registry membership. Migration replaces this with name resolution — the prelude declares a `pub struct String`, and sema name-resolves user code's `String` to the prelude declaration (the same path `Option` / `Result` already use).
3. **Removal of the now-defunct registry-driven path.** Once String moves, the `BuiltinTypeDef` mechanism has zero consumers (Vec / Slice use `BuiltinTypeConstructorKind`, a different mechanism). The structures and sema injection paths for `BuiltinTypeDef` can be retired in a stabilization phase.

### What this ADR does *not* attempt

- **Vec(T) reformulation as a comptime-generic Gruel struct.** Per the task scope conversation: this requires comptime-generic struct syntax (currently only functions are generic), `@alloc`/`@realloc`/`@free`/`@drop` Gruel-callable intrinsics (today they are runtime-only FFI symbols emitted inline by codegen at `gruel-codegen-llvm/src/codegen.rs:5237`), an `Index`/`IndexMut` interface for overloaded `[]` (ADR-0078's operator overloading is comparisons-only), generalized scope-bound types so `Slice(T)` keeps its borrow-checker treatment, and a mechanism to expose a `BuiltinMethod` body as inline Gruel rather than a runtime FFI symbol. Each is a real piece of language work. Sequencing them all in one ADR risks a mega-landing; instead, this ADR ships the immediately tractable String collapse, and a sibling ADR (call it 0082) takes on the Vec/Slice work as its own multi-phase project.
- **A general `Index` / `IndexMut` interface.** Out of scope here; the new String type doesn't have indexing operators (codepoint-indexed slicing is future work per ADR-0072 §"Future Work").
- **Codepoint iteration (`s.chars()`).** Same as ADR-0072: blocked on Gruel's iterator story.
- **`PartialEq` / `PartialOrd` for floats.** Same as ADR-0078: out of scope.
- **Spec rewrites of section 7.4.** The observable semantics of String are unchanged (this is the load-bearing property of the migration); section 7.4 needs a small note about the implementation move, no normative paragraph changes.

### Why now

ADR-0072 left a deferral note ("Final composition + runtime collapse is queued for stabilization") and ADR-0078 cleared the last prerequisite (operator overloading for non-built-in types). The 751 LOC of `string.rs` is the single largest chunk of bespoke type-runtime in the codebase; collapsing it is the next obvious tightening. Doing it now also reduces friction on future String additions: today, adding `replace` or `find` means writing a runtime `String__*` function, exposing its symbol in `BuiltinMethod`, and re-deploying the registry. After this ADR, it's "edit `prelude/string.gruel`."

## Decision

### 1. New Vec(T) methods (Phase 1)

Add to `gruel-codegen-llvm`'s Vec method dispatch (lines 4140–4192 in `codegen.rs`) and to `crates/gruel-air/src/sema/vec_methods.rs`'s `dispatch_vec_method_call`, gated to `T: Copy`:

| Method | Signature | Codegen |
|---|---|---|
| `eq` | `(self: Ref(Self), other: Ref(Self)) -> bool` | len equality + element-by-element primitive `==` (memcmp for byte-sized `T`) |
| `cmp` | `(self: Ref(Self), other: Ref(Self)) -> Ordering` | element-by-element lex compare + len tiebreak; emits `Ordering::Less` / `Ordering::Equal` / `Ordering::Greater` AIR enum-variant constructions (re-using `builtin_ordering_id` cached by ADR-0078) |
| `contains` | `(self: Ref(Self), needle: Slice(T)) -> bool` | linear scan: for each `i in 0..len-needle.len`, memcmp(ptr+i, needle.ptr, needle.len) |
| `starts_with` | `(self: Ref(Self), prefix: Slice(T)) -> bool` | prefix.len ≤ len + memcmp(ptr, prefix.ptr, prefix.len) |
| `ends_with` | `(self: Ref(Self), suffix: Slice(T)) -> bool` | suffix.len ≤ len + memcmp(ptr+len-suffix.len, suffix.ptr, suffix.len) |
| `concat` | `(self: Ref(Self), other: Slice(T)) -> Vec(T)` | `__gruel_alloc(self.len + other.len)` + 2 memcpys + return `{ ptr, len = self.len+other.len, cap = self.len+other.len }` |
| `extend_from_slice` | `(self: MutRef(Self), other: Slice(T))` | `reserve(other.len)` + memcpy at `ptr+self.len` + `len += other.len` |

Each gets a new `IntrinsicId` (`VecEq`, `VecCmp`, `VecContains`, `VecStartsWith`, `VecEndsWith`, `VecConcat`, `VecExtendFromSlice`) registered in `crates/gruel-intrinsics/src/lib.rs`. Sema's `dispatch_vec_method_call` adds match arms keyed on the method name. Codegen adds match arms in the Vec-intrinsic dispatch and a `translate_vec_*` function per intrinsic. None of these need a runtime FFI symbol — all lower inline.

The `T: Copy` constraint matches the existing `Vec(T).clone()` v1 limitation (ADR-0066 Phase 11). For the byte-slice search ops, `Copy` is sufficient: element comparison is a primitive `==` for any Copy `T`. Generalizing to `T: Eq` (via interface dispatch in the inner loop) is future work, with the same shape as the deferred non-Copy clone path.

**Side effect: `Vec(T)` joins `Eq` / `Ord`.** The new `eq` / `cmp` methods exactly match the structural shape ADR-0078 looks for (`fn eq(self: Ref(Self), other: Ref(Self)) -> bool` and `fn cmp(self: Ref(Self), other: Ref(Self)) -> Ordering`). Sema's binop dispatch step 4 / 5 will pick them up automatically. After Phase 1, `let v1: Vec(i32) = @vec(1, 2); let v2: Vec(i32) = @vec(1, 2); v1 == v2` compiles and returns true. This is a small but useful language-wide gain that comes free.

### 2. New `prelude/string.gruel` (Phase 2)

Replace the current 44-LOC file with a full struct declaration. The shape mirrors `prelude/option.gruel` / `prelude/result.gruel` but for a non-generic struct rather than an enum (per the syntax confirmed by `crates/gruel-spec/cases/modules/field_method_visibility.toml`):

```gruel
// ADR-0072 + ADR-0081: String is a thin wrapper over Vec(u8).
// The `bytes` field is non-pub — accessible only inside this prelude file
// per ADR-0073's unified is_accessible check.
pub struct String {
    bytes: Vec(u8),

    pub fn new() -> Self {
        Self { bytes: Vec(u8)::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { bytes: Vec(u8)::with_capacity(n) }
    }

    pub fn from_char(c: char) -> Self {
        let mut s = Self::new();
        s.push(c);
        s
    }

    pub fn len(self: Ref(Self)) -> usize { self.bytes.len() }
    pub fn capacity(self: Ref(Self)) -> usize { self.bytes.capacity() }
    pub fn is_empty(self: Ref(Self)) -> bool { self.bytes.is_empty() }
    pub fn bytes_len(self: Ref(Self)) -> usize { self.bytes.len() }
    pub fn bytes_capacity(self: Ref(Self)) -> usize { self.bytes.capacity() }

    pub fn clone(self: Ref(Self)) -> Self {
        Self { bytes: self.bytes.clone() }
    }

    pub fn contains(self: Ref(Self), needle: Ref(Self)) -> bool {
        // Borrow the inner Vec(u8) as a Slice(u8) and forward to Vec(u8).contains.
        self.bytes.contains(&needle.bytes[..])
    }
    pub fn starts_with(self: Ref(Self), prefix: Ref(Self)) -> bool {
        self.bytes.starts_with(&prefix.bytes[..])
    }
    pub fn ends_with(self: Ref(Self), suffix: Ref(Self)) -> bool {
        self.bytes.ends_with(&suffix.bytes[..])
    }
    pub fn concat(self: Ref(Self), other: Ref(Self)) -> Self {
        Self { bytes: self.bytes.concat(&other.bytes[..]) }
    }
    pub fn push_str(self: MutRef(Self), other: Ref(Self)) {
        self.bytes.extend_from_slice(&other.bytes[..])
    }
    pub fn clear(self: MutRef(Self)) {
        self.bytes.clear()
    }
    pub fn reserve(self: MutRef(Self), additional: usize) {
        self.bytes.reserve(additional)
    }

    // Safe codepoint-aware mutator (ADR-0072 §7).
    // `c.encode_utf8(...)` is the existing prelude function from char.gruel
    // that fills a 4-byte buffer and returns the byte count.
    pub fn push(self: MutRef(Self), c: char) {
        let mut buf: [u8; 4] = [0u8; 4];
        let n: usize = char__encode_utf8(c, &mut buf[..]);
        self.bytes.extend_from_slice(&buf[..n])
    }

    // Eq / Ord conformance — picked up by ADR-0078's binop dispatch
    // step 4/5 once STRING_TYPE is removed from BUILTIN_TYPES.
    pub fn eq(self: Ref(Self), other: Ref(Self)) -> bool {
        self.bytes == other.bytes
    }
    pub fn cmp(self: Ref(Self), other: Ref(Self)) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }

    // O(1) move-out: single struct-field move.
    pub fn into_bytes(self) -> Vec(u8) {
        self.bytes
    }

    // ADR-0072: validated UTF-8 conversion (was String__from_utf8 free fn).
    pub fn from_utf8(v: Vec(u8)) -> Result(String, Utf8DecodeError) {
        let valid: bool = checked {
            let p = v.ptr();
            let n = v.len();
            let s: Slice(u8) = @parts_to_slice(p, n);
            @utf8_validate(s)
        };
        if valid {
            let s: Self = checked { Self::from_utf8_unchecked(v) };
            Result(Self, Utf8DecodeError)::Ok(s)
        } else {
            Result(Self, Utf8DecodeError)::Err(Utf8DecodeError { bytes: v })
        }
    }

    pub fn from_c_str(p: Ptr(u8)) -> Result(String, Utf8DecodeError) {
        let v: Vec(u8) = checked { @cstr_to_vec(p) };
        Self::from_utf8(v)
    }

    // checked-only escape hatches (ADR-0072 §5.3, §6.1, §7).
    pub fn from_utf8_unchecked(v: Vec(u8)) -> Self {
        Self { bytes: v }
    }
    pub fn from_c_str_unchecked(p: Ptr(u8)) -> Self {
        Self::from_utf8_unchecked(@cstr_to_vec(p))
    }
    pub fn push_byte(self: MutRef(Self), byte: u8) {
        self.bytes.push(byte)
    }
    pub fn terminated_ptr(self: MutRef(Self)) -> Ptr(u8) {
        self.bytes.terminated_ptr(0u8)
    }
}

pub struct Utf8DecodeError {
    bytes: Vec(u8),
}
```

### 3. Compiler changes (Phase 2)

- **`gruel-builtins/src/lib.rs`**: delete `STRING_TYPE` (lines 336–614). `BUILTIN_TYPES` becomes empty (`&[]`); the constant stays for breadcrumb purposes (future builtins may re-populate). The `BuiltinTypeDef` / `BuiltinField` / `BuiltinMethod` / `BuiltinAssociatedFn` / `BuiltinOperator` / `BuiltinReturnType` / `BuiltinParam` / `BuiltinParamType` / `BuiltinFieldType` / `ReceiverMode` types stay (they have no consumers but removing them is bigger surface area; queue for retirement in Phase 4).
- **Sema**: the `inject_builtin_types` path that synthesizes `StructDef` entries for `BUILTIN_TYPES` becomes a no-op when the slice is empty. The `analyze_builtin_method` / operator-routing paths (which today route `==` on String through `BuiltinOperator.runtime_fn`) become unreachable for String; the binop analyzer's step 3 (BUILTIN_TYPES match) misses, step 4 / 5 (Eq / Ord interface dispatch) picks up the prelude struct's `eq` / `cmp`. No new code; existing dispatch chain just falls through one step further.
- **Sema name resolution**: `String` resolves to the prelude struct via the same mechanism that resolves `Option` / `Result` today (prelude top-level items go into the global resolution table under `FileId::PRELUDE`). No change.
- **Codegen**: no String-specific codegen paths exist today (everything went through the runtime FFI). Nothing to remove.
- **Drop synthesis**: today `STRING_TYPE.drop_fn = "__gruel_drop_String"` is a registry-driven drop hook. After migration, drop is auto-synthesized from struct contents — the prelude `String` has one field of type `Vec(u8)`, whose drop runs the per-element drop loop and frees the buffer. This is exactly the structural posture ADR-0072 §3 already established as the goal.
- **String literal lowering**: source like `"hello"` produces a `String` value at the AST/RIR stage. Sema types it as the prelude `String`; codegen produces a stack-initialized `{ bytes: { ptr, len, cap } }` with `ptr` pointing into `.rodata` and `cap = len` (the `cap == len` non-allocated form is already supported by `Vec(u8)`'s drop, which only frees when `cap > 0`). Verify the existing literal-lowering path in `gruel-codegen-llvm` still emits this layout when the type is the new struct rather than the registry-described `STRING_TYPE` — it should, since the LLVM type is the same, but Phase 2 must include a spec test that exercises a string literal in an empty-allocation context.

### 4. Runtime cleanup (Phase 3)

Delete from `gruel-runtime/src/string.rs`:

| Symbol | LOC | Reason |
|---|---|---|
| `__gruel_str_eq` | 16 | Replaced by `Vec(T).eq` (Phase 1) routed via ADR-0078 step 4 |
| `__gruel_str_cmp` | 20 | Replaced by `Vec(T).cmp` (Phase 1) routed via ADR-0078 step 5 |
| `String__contains` | 23 | Replaced by `Vec(u8).contains` |
| `String__starts_with` | 18 | Replaced by `Vec(u8).starts_with` |
| `String__ends_with` | 19 | Replaced by `Vec(u8).ends_with` |
| `String__concat` | 48 | Replaced by `Vec(u8).concat` |
| `String__push_str` | 27 | Replaced by `Vec(u8).extend_from_slice` |
| `String__push` | 13 | Replaced by `Vec(u8).push` (used today as `push_byte`) |
| `String__clear` | 7 | Replaced by `Vec(u8).clear` |
| `String__reserve` | 15 | Replaced by `Vec(u8).reserve` |
| `String__len` | 3 | Replaced by `Vec(u8).len` |
| `String__capacity` | 3 | Replaced by `Vec(u8).capacity` |
| `String__is_empty` | 3 | Replaced by `Vec(u8).is_empty` |
| `String__clone` | 25 | Replaced by `Vec(u8).clone` |
| `String__new` | 7 | Replaced by `Vec(u8)::new` + struct construction |
| `String__with_capacity` | 13 | Replaced by `Vec(u8)::with_capacity` + struct construction |
| `String__from_char` | 10 | Replaced by `Self::new` + `push(c)` composition |
| `String__push_char` | 16 | Replaced by `char__encode_utf8` + `Vec(u8).extend_from_slice` |
| `String__from_utf8_unchecked` | 12 | Replaced by struct construction `Self { bytes: v }` |
| `String__from_c_str_unchecked` | 13 | Replaced by `from_utf8_unchecked(@cstr_to_vec(p))` |
| `String__terminated_ptr` | 14 | Replaced by `Vec(u8).terminated_ptr(0u8)` |
| `String__into_bytes` | 7 | Replaced by single struct-field move |
| `__gruel_string_alloc` | 8 | No callers after above deletions (`__gruel_alloc` directly serves Vec) |
| `__gruel_string_realloc` | 5 | Same |
| `__gruel_string_clone` | 13 | Same |
| `__gruel_drop_String` | 5 | Replaced by auto-synthesized struct drop running Vec(u8)'s drop on `bytes` |
| `string_ensure_capacity` | 29 | No callers after above deletions |
| `encode_utf8` | 22 | No callers (String__from_char and String__push_char are gone; `char__encode_utf8` in `prelude/char.gruel` is a separate Gruel-level encoder used by `Self::push`) |
| `StringResult` struct | 10 | sret payload; no callers |

**Total: ~430 LOC removed from `string.rs`.** Combined with the `BUILTIN_TYPES` registry deletion (~280 LOC of static data in `gruel-builtins/src/lib.rs`), the registered-method-dispatch sema paths (~40 LOC of operator-routing logic in `sema/analysis.rs`'s step 3 path that becomes unreachable for the deleted entry; the dispatch ladder itself stays for any future builtin), and the doc-generator iteration over `BUILTIN_TYPES` (~30 LOC), total Rust LOC retired is ~780. New Rust LOC added in Phase 1 (Vec method codegen + sema dispatch + intrinsic registration): ~110.

Kept in `gruel-runtime/src/string.rs`:
- `__gruel_utf8_validate` (60 LOC) — called from `String::from_utf8` prelude body via `@utf8_validate` intrinsic
- `__gruel_cstr_to_vec` (32 LOC) — called from `String::from_c_str` / `String::from_c_str_unchecked` prelude bodies via `@cstr_to_vec` intrinsic
- `VecU8Result` struct (10 LOC) — sret payload for `__gruel_cstr_to_vec`
- The thin `__gruel_alloc` / `__gruel_realloc` / `__gruel_free` shims (`heap::*` delegators, ~14 LOC) — used by Vec(T) codegen, unchanged

Final size of `gruel-runtime/src/string.rs`: ~120 LOC. Renaming the file to `utf8.rs` is a Phase 4 cleanup (the remaining symbols are all UTF-8 / FFI-conversion specific; "string.rs" no longer describes the contents).

## Implementation Phases

Each phase ships independently behind the `string_runtime_collapse` preview gate, ends with `make test` green, and quotes its LOC delta in the commit message.

- [x] **Phase 1: Preview gate + Vec(T) byte-comparison and search methods**
  - Add `PreviewFeature::StringRuntimeCollapse` to `gruel-error/src/lib.rs`. Wire `name()`, `adr()`, `all()`, `FromStr` impl.
  - Add 7 new `IntrinsicId` variants (`VecEq`, `VecCmp`, `VecContains`, `VecStartsWith`, `VecEndsWith`, `VecConcat`, `VecExtendFromSlice`) and `INTRINSICS` entries in `crates/gruel-intrinsics/src/lib.rs`. Each `Expr` kind, `T: Copy` constraint enforced at sema (no preview gate on the methods themselves — they're useful to all Vec users from day one).
  - Add 7 match arms to `dispatch_vec_method_call` in `crates/gruel-air/src/sema/vec_methods.rs`. The `eq` / `cmp` arms wire into ADR-0078's binop dispatch automatically (the analyzer looks for methods named `eq` / `cmp` with the right shape on user struct/enum types — Vec qualifies once the methods exist, **but** see Open Questions §3 about whether sema's Eq/Ord interface check recognizes built-in `TypeKind::Vec(_)` receivers; if not, a small extension to the interface-conformance lookup is part of this phase).
  - Add 7 codegen entries in `gruel-codegen-llvm/src/codegen.rs` Vec dispatch (lines 4140–4192). Each `translate_vec_*` lowering emits inline LLVM. `eq` and `cmp` use a fast path of `len equality` + `memcmp` for `T == u8` and a per-element loop for larger `T`.
  - `cmp` codegen constructs `Ordering::Less` / `Ordering::Equal` / `Ordering::Greater` AIR enum-variant nodes using the cached `builtin_ordering_id` (added by ADR-0078 Phase 4).
  - Spec tests at `crates/gruel-spec/cases/vec/byte_methods.toml`: each new method tested for `Vec(u8)`, `Vec(i32)`, and at least one struct case (Copy struct). Operator coverage: `Vec(i32) == Vec(i32)`, `Vec(u8) < Vec(u8)` returning the right Ordering values.
  - `make test` green.

- [x] **Phase 2: Migrate String to prelude**
  - Replace `prelude/string.gruel` with the full struct declaration from §2. Move `Utf8DecodeError` to live alongside.
  - Delete `STRING_TYPE` from `gruel-builtins/src/lib.rs`. `BUILTIN_TYPES` becomes `&[]`.
  - Sema verification: walk the existing String spec tests (`crates/gruel-spec/cases/types/strings.toml`, `mutable-strings.toml`, `char_string.toml`, `string_vec_bridge.toml`) and confirm every test still passes against the prelude struct. Any failures here are the migration's regression surface.
  - String literal lowering verification: a string literal in an unallocated context (`let s = "x";`) still produces a layout-compatible `String` value. The Vec(u8)'s drop must handle `cap == 0` correctly (already does, per ADR-0066 §"Drop").
  - Operator overloading verification: `s1 == s2`, `s1 < s2`, etc. compile and route through ADR-0078's step 4 / 5 to call `String::eq` / `String::cmp`.
  - `make test` green. Expected delta: 0 spec test changes, 0 UI test changes, ~280 LOC out of `gruel-builtins/src/lib.rs`, ~120 LOC into `prelude/string.gruel`.

- [x] **Phase 3: Delete obsolete runtime functions**
  - Delete the 28 symbols listed in the §4 table from `gruel-runtime/src/string.rs`. Total ~430 LOC out.
  - Rename `gruel-runtime/src/string.rs` → `gruel-runtime/src/utf8.rs` (the contents are now exclusively UTF-8 / FFI-conversion specific). Update `gruel-runtime/src/lib.rs` `mod` declaration.
  - Verify no references to deleted symbols remain (`grep -r 'String__\|__gruel_str_\|__gruel_string_\|__gruel_drop_String' crates/`).
  - Doc generator (`docs/generated/builtins-reference.md`): `BUILTIN_TYPES` is now empty, so the iterator-driven section becomes static text or is removed. Update `make gen-builtins-docs` and `make check-builtins-docs`.
  - `make test` green.

- [x] **Phase 4: Stabilize**
  - Remove `PreviewFeature::StringRuntimeCollapse` from `gruel-error/src/lib.rs`.
  - Retire `BuiltinTypeDef` / `BuiltinField` / `BuiltinMethod` / `BuiltinAssociatedFn` / `BuiltinOperator` / `BuiltinReturnType` / `BuiltinParam` / `BuiltinParamType` / `BuiltinFieldType` / `ReceiverMode` types from `gruel-builtins/src/lib.rs` if no other consumer has emerged (they have none today). The corresponding `inject_builtin_types` and `analyze_builtin_method` paths in sema retire alongside. Total ~150 LOC out across builtins + sema.
  - ADR status → `implemented`.
  - Spec section 7.4 gets a note that String is now defined in `prelude/string.gruel`; no normative paragraph changes (observable semantics unchanged).
  - ADR-0072's "Future Work" line about `String__*` runtime collapse marks this ADR as the resolution.

## Consequences

### Positive

- **`gruel-runtime/src/string.rs` shrinks from 751 → ~120 LOC** (~84% reduction). The remaining content is exclusively the two UTF-8-specific helpers (`__gruel_utf8_validate`, `__gruel_cstr_to_vec`) and their sret structs.
- **`gruel-builtins/src/lib.rs` loses ~280 LOC of static registry data** (the entire `STRING_TYPE` constant). After Phase 4, an additional ~150 LOC of `BuiltinTypeDef` infrastructure retires.
- **String becomes a normal source file.** Adding `replace`, `find`, `to_lowercase`, etc. is now an edit to `prelude/string.gruel`, not a four-step process across registry / runtime / sema / docs.
- **`Vec(T)` gains 7 useful methods** as a permanent language win, not String-specific. `Vec(i32) == Vec(i32)` and `Vec(u8).contains(...)` both ship for all users.
- **`Vec(T)` joins `Eq` / `Ord` conformance.** A free side-effect of the new methods. Any code that wanted to compare two Vecs gets it for free.
- **Removes a maintenance hazard.** The 14 `String__*` runtime functions and their bit-compatibility with the new `{ Vec(u8) }` layout was a coincidence the implementation was holding together by hand (per ADR-0072 Phase 2's deferral note). After this ADR, the structural relationship is also the implementation relationship.
- **Validates ADR-0078's stdlib path.** The "Gruel-resident generic types in the prelude" pattern (Option, Result) extends cleanly to a non-generic, layout-fixed type with an invariant. Future stdlib types (e.g., a Rust-style `Box<T>`-equivalent if needed) can follow the same playbook.

### Negative

- **Phase 2 has the largest regression surface.** ~130 spec tests touch String semantics. Any subtle layout difference between the registry-described `STRING_TYPE` and the prelude struct will surface as test failures. Mitigated by: (a) the layout is provably identical (both are `{ ptr, len, cap }` 24-byte aggregates), and (b) Phase 2 includes explicit verification against existing tests with no expected behavior changes.
- **Method-call performance loses one layer of inlining freedom.** Today, `s.contains(needle)` is a direct extern call to `String__contains`, which the linker resolves to a 23-LOC Rust function. After the collapse, it's `s.bytes.contains(&needle.bytes[..])` — a struct field access + Slice construction + Vec(u8) method call. LLVM's inliner should collapse this in optimized builds (all the indirection is statically resolvable), but debug builds may show a small slowdown. Acceptable: the tax is small, debug perf is not load-bearing, and the code-quality win is large.
- **`@utf8_validate` and `@cstr_to_vec` continue to be FFI calls.** This ADR keeps the runtime-validation path as an extern symbol; the SIMD-optimized validator is non-trivial Rust. A future ADR could reformulate `@utf8_validate` as Gruel + a small `bytes_pattern_match` intrinsic, but that's significant work for a small win.
- **`BuiltinTypeDef` mechanism becomes unused after Phase 4 retirement.** If a future builtin needs the same shape (say, a `Box(T)` wrapper in `gruel-builtins`), the mechanism would have to be rebuilt. Acceptable: the YAGNI tax of keeping it around outweighs the resurrection cost if it ever becomes needed; cheap to re-add.
- **Slight expansion of operator-overloading reach.** Today, `Vec(i32) == Vec(i32)` is a type error (no Eq/Ord on Vec). After Phase 1, it works. This is a desired expansion, but it's behavior change: a user who relied on the type error to catch bugs would now get successful compilation. Document in the changelog; mitigated by spec tests that exercise the new behavior explicitly.

### Neutral

- **Spec section 7.4 unchanged in normative content.** The String invariants (UTF-8, layout, conversion APIs) are observable-semantics-identical. Spec only gains an informative note pointing to `prelude/string.gruel`.
- **The runtime preview gate (`string_runtime_collapse`) is internal.** It exists to stage Phases 1–3, but no user-facing language behavior changes during the staging — the existing String semantics ship at every phase boundary.
- **`Vec(u8)` method count grows from ~16 to ~23.** Doc surface increases but the new methods are simple and individually well-shaped.

## Open Questions

1. **`Vec(T).eq` / `Vec(T).cmp` and the Eq/Ord interface conformance lookup.** ADR-0078's interface dispatch (in `analyze_comparison`) looks up `eq` / `cmp` methods on user struct/enum types. Does it currently inspect `TypeKind::Vec(_)` receivers? If conformance is keyed off "is a user-declared struct with the right method", Vec's built-in nature may need a small carve-out. Options: (a) extend the lookup to include built-in types with the right method shape (clean generalization, ~10 LOC), or (b) add `Vec(T)` as a recognize-by-name conformance similar to how `Drop` / `Copy` are recognized. Resolve in Phase 1's first commit; the answer affects whether the new `eq` / `cmp` methods are immediately usable via `==` / `<` syntax.

2. **Should `Vec(T).contains` / `starts_with` / `ends_with` take `Slice(T)` or `Ref(Self)`?** The decision matters for ergonomics: `s.contains(needle)` where `needle: String` — does the body write `self.bytes.contains(&needle.bytes[..])` (slice), or `self.bytes.contains(&needle.bytes)` (ref to Vec)? Slice is the more general primitive (any slice-able source — array, slice, Vec — works). Ref-to-Vec couples to the Vec type. Lean toward `Slice(T)`; the call site cost is one slice borrow.

3. **Should the new `Vec(T)` methods be preview-gated alongside `string_runtime_collapse`?** They land independently as a language addition (not a String-specific feature). Argument for gating: keeps Phase 1 atomic — if a regression surfaces, one flag rolls everything back. Argument against: they're useful to non-String users and benefit from early exposure. Lean toward shipping ungated since they're additive (no existing call site changes behavior); the gate covers the *removal* of `STRING_TYPE`, not the addition of the methods.

4. **What happens to `gruel-runtime/src/lib.rs`'s test coverage of String functions?** Today there are integration tests at the runtime layer that exercise `String__*` symbols directly. After Phase 3 these tests must be deleted (their targets are gone) or rewritten to test `__gruel_utf8_validate` / `__gruel_cstr_to_vec` only. Probably a Phase 3 sub-step; flag here so it's not forgotten.

5. **Spec section 7.4 update timing.** The spec currently describes String as having registry-driven runtime functions. The text should be updated to describe it as a prelude struct — but observable semantics are identical, so the change is informative-only. Open question: should this happen in Phase 2 (when the migration lands) or Phase 4 (when stabilization completes)? Lean toward Phase 4 since the preview gate may roll back; speculative spec updates are wasted churn.

## Future Work

- **Vec(T) collapse onto a comptime-generic Gruel struct.** The big remaining piece, deferred from this ADR's scope. Requires comptime-generic struct syntax, `@alloc` / `@realloc` / `@free` Gruel-callable intrinsics, `@drop(value)` intrinsic for per-element drop loops in user code, an `Index` / `IndexMut` interface for overloadable `[]`, generalized scope-bound types so `Slice(T)` keeps its borrow-checker treatment, and an inline-Gruel-method-body mechanism on `BuiltinMethod` (or full Vec migration to prelude). ~5 prerequisite language features, probably 2–3 ADRs.
- **Slice(T) collapse alongside Vec.** Same constraint set as Vec; lands together.
- **Generalize `Vec(T)` byte methods to non-Copy `T: Eq`.** Requires per-element interface dispatch in the inner comparison loop. Same shape as the deferred non-Copy clone synthesis from ADR-0066 Phase 11.
- **Rename `gruel-runtime/src/string.rs` → `utf8.rs` after Phase 3.** Cosmetic but accurate.
- **Retire `BuiltinTypeDef` infrastructure entirely** (Phase 4 cleanup; flagged as conditional on no other consumer emerging).
- **Reformulate `@utf8_validate` as Gruel + `bytes_pattern_match` intrinsic.** Significant work for small win; speculative.
- **Codepoint iteration (`s.chars()`)** and **`&str` borrowed slices** — same future work items as ADR-0072. Independent of the runtime collapse.

## References

- ADR-0020: Built-in Types as Synthetic Structs (the mechanism this ADR retires for `String`)
- ADR-0064: Slices (`Slice(T)`/`MutSlice(T)`) — argument shape for the new Vec byte methods
- ADR-0066: `Vec(T)` (the substrate; deferred clone-for-non-Copy is the same shape as deferred non-Copy byte methods here)
- ADR-0070: `Result(T, E)` (return shape for `String::from_utf8` / `from_c_str`)
- ADR-0071: `char` (consumed by `String::push(c: char)`)
- ADR-0072: String as Vec(u8) Newtype (this ADR is the deferred Phase 2 of that ADR)
- ADR-0073: Field/method visibility (privacy of `String::bytes` survives the migration via `is_pub: false`)
- ADR-0078: Stdlib MVP (operator overloading via Eq/Ord, prelude as `prelude/` directory — both are direct prerequisites)
- Spec ch. 7.4: String / Vec(u8) Relationship
