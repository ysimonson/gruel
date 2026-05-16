---
id: 0088
title: `@mark(unchecked)` on methods and mandatory on FFI imports
status: proposal
tags: [ffi, safety, syntax, stdlib]
feature-flag: unchecked_fn_extensions
created: 2026-05-14
accepted:
implemented:
spec-sections: ["9.2", "10.1"]
superseded-by:
---

# ADR-0088: `@mark(unchecked)` on methods and mandatory on FFI imports

## Status

Proposal.

## Summary

Three coordinated changes that finish the `unchecked` story started by [ADR-0028](0028-unsafe-and-raw-pointers.md) and deliberately deferred by [ADR-0085](0085-c-ffi.md):

1. **`unchecked` migrates from a hard keyword (ADR-0028) to a `@mark(unchecked)` directive (ADR-0083), and the directive becomes valid on struct methods.** Today the `unchecked` keyword is only accepted in the `unchecked fn` slot on top-level functions. The new shape is `@mark(unchecked) fn foo(self) -> ()` — usable on top-level fns, methods (regular `impl` blocks and anonymous-struct literals), and FFI imports under a single uniform spelling. The existing call-site rule (any call to an unchecked fn must sit inside a `checked { }`) applies uniformly.

2. **Every fn inside a `link_extern("…") { … }` or `static_link_extern("…") { … }` block must be written `@mark(unchecked) fn …;`.** Missing the directive is a compile error. Imported C symbols are unverified from the Gruel side by construction; making the marker mandatory and uniform at the declaration site means the FFI-vs-Gruel call discipline is visible in the source instead of hidden in a per-fn-info bit, and a reader of any call site can tell from `checked { }` brackets that a foreign function is involved. Top-level `@mark(c) fn …{ }` exports do *not* require `@mark(unchecked)` — their bodies are ordinary Gruel and the unverified boundary is the C caller's, not the Gruel caller's.

3. **The hardcoded `check_string_vec_bridge_method_gates` table and the `char::from_u32_unchecked` per-name gate retire.** The five method/associated-function escape hatches they cover (`String::from_utf8_unchecked`, `String::from_c_str_unchecked`, `String::push_byte`, `String::terminated_ptr`, `char::from_u32_unchecked`) become real `@mark(unchecked) fn` declarations in the prelude. The general unchecked-method gate from (1) replaces the by-name list. Any future stdlib escape hatch picks up the gate by adding the `@mark(unchecked)` directive to itself.

A single preview feature `unchecked_fn_extensions` gates the new surface and the FFI requirement (one breaking change on the FFI surface, behind the preview until stable). The capability-system ADR seam from ADR-0085 is preserved — `@mark(unchecked)`-on-FFI is *per-fn syntactic*, complementary to any future per-block witness mechanism, not in competition with it.

## Context

Three forces converge here:

**The deferred FFI gate.** ADR-0085 §"Call-site posture" landed C FFI without any syntactic gate at the call site (`sin(2.0)` and a local `add(2.0)` are visually identical). The ADR called this out as a scope cut: *"Earlier drafts implicitly marked extern fns `unchecked` and forced callers into `checked { … }`; v1 removes that gating because the capability ADR is the right place to decide what FFI gating should look like."* Roughly six months and two FFI ADRs later, the capability ADR still isn't on the immediate roadmap, and the ADR-0085 Negative Consequence about "a reader can't tell from the call alone that one is a foreign symbol" is the most-cited papercut in real FFI code. This ADR commits to the syntactic-gate approach — per-fn `@mark(unchecked)` on every FFI import — and leaves the capability ADR free to layer a per-block or per-fn witness on top.

**The missing method surface for `unchecked`.** ADR-0028 introduced `unchecked fn` for top-level functions as a hard keyword in a fixed grammar slot. Methods (`fn foo(self)` inside `struct { … }` bodies) were out of scope because the surface hadn't grown the affordances yet — `pub` on methods, anonymous-struct methods, ADR-0058 `Self::` paths, etc. all postdate ADR-0028. The omission is now visible at the stdlib boundary: the prelude has *five* methods/associated functions that are unverified by definition (`String::from_utf8_unchecked` and friends), but the only way to gate them today is the hardcoded by-name table `check_string_vec_bridge_method_gates` in `gruel-air/src/sema/builtins.rs` plus a separate per-name special case for `char::from_u32_unchecked` in `pointer_ops.rs`. The string.gruel prelude comment captures it directly: *"the language doesn't yet expose `unchecked fn` syntax inside struct method declarations."* The fix isn't another by-name row; it's exposing the marker uniformly.

**The marker-directive unification.** ADR-0083 replaced the `copy` / `linear` declaration-site keywords with `@mark(...)` directives, and the same motivation applies here: `unchecked` is a declaration-time marker on a fn, not a piece of its calling convention or its body. Reusing the directive mechanism instead of extending the keyword slot pays off three ways. (a) The grammar extension to methods and FFI imports is uniform — directives are already parsed in every fn-declaration position in the language, so adding `@mark(unchecked)` everywhere costs zero parser surface area. (b) The marker registry in `gruel-builtins` becomes the single source of truth for which markers exist and where each is legal — `@mark(unchecked)` joins the registry next to `copy`, `linear`, `c`, and friends. (c) The hard-keyword `unchecked` retires, freeing a reserved word and removing a parsing special-case.

The structural choice this ADR makes is to treat `@mark(unchecked)` as a *per-declaration* property uniformly across (a) top-level fns, (b) methods, and (c) FFI imports. The unifying invariant is: a fn is unchecked iff the compiler cannot verify some precondition the caller is responsible for. C imports satisfy that by definition (the compiler hasn't seen the body). Escape-hatch stdlib methods satisfy it by construction (the caller is asserting a UTF-8 invariant or borrowing through a raw pointer). Treating these the same syntactically — `@mark(unchecked) fn …` at the declaration, `checked { }` at the call — is simpler than the current mix of (i) `unchecked fn` keyword on top-level only, (ii) ungated FFI, and (iii) hardcoded by-name gates for the stdlib escape hatches.

The alternative shape that this ADR rejects is "FFI calls implicitly inside a `checked` block when surrounded by a capability witness" — too speculative without the capability ADR, and the per-call-site `checked { }` proposed here is what the capability ADR can refine, not what it has to replace.

## Decision

### `@mark(unchecked)` joins the marker registry

`@mark(unchecked)` is added to `BUILTIN_MARKERS` in `gruel-builtins`. Its legal positions are top-level fn declarations, method declarations (regular `impl` and anonymous-struct), interface method signatures (named and anonymous — see "Interface methods and conformance" below), and FFI import declarations inside `link_extern` / `static_link_extern` blocks. It is **not** legal on struct/enum declarations or destructors (`fn __drop`).

The hard keyword `unchecked` retires. Its sole current use — `unchecked fn` at top level (ADR-0028) — migrates to `@mark(unchecked) fn`. The `Unchecked` token in `gruel-lexer` is removed; the `unchecked_fn_parser` slot in `gruel-parser` is removed; both fold into the existing directive-list parsing that already runs in every fn-declaration position.

### Principle: when a fn requires `@mark(unchecked)`

The driving question — *which* fns earn the marker — needs a sharper answer than "the five stdlib escape hatches plus FFI imports." Earlier drafts of this section proposed a signature-level test ("any raw pointer in the signature ⇒ unchecked"), but that overstates the rule: returning a raw pointer whose validity is demonstrable at return time isn't itself unsafe, and the existing pointer-op gate (ADR-0028: any deref must sit inside `checked { }`) already handles the caller-side hazard. The correct principle is body-side and follows ADR-0028's definition directly:

> **A fn must be `@mark(unchecked)` iff its body relies on a precondition the caller is responsible for and the type system cannot verify.**

The categories that qualify:

1. **Body uses a caller-supplied pointer in a validity-dependent way.** The body dereferences, walks, escapes-into-state-for-later-deref, or passes-through-to-another-validity-dependent-op a `Ptr(T)` / `MutPtr(T)` parameter. The body's soundness rests on the caller's claim that the pointer is valid for the intended use. `String::from_c_str_unchecked(p: Ptr(u8))` is canonical — the body walks `p` looking for the NUL terminator and the caller is the only party who can vouch for `p`'s validity.

2. **Body trusts an invariant on caller-supplied data the type system can't encode.** The body either *uses* the invariant directly or *stores* the data in a way that future ops on the receiver will rely on it. `String::from_utf8_unchecked(v: Vec(u8))` trusts UTF-8; `char::from_u32_unchecked(n: u32)` trusts Unicode-scalar; `String::push_byte(self, b: u8)` trusts that the resulting bytes remain UTF-8 (the body doesn't *use* the invariant immediately, but every subsequent `String` method will — `chars()`, indexing, etc.).

3. **FFI imports.** Body opaque; trust always required. Covered by the dedicated FFI rule below.

What is **not** unchecked, even though it touches raw pointers:

- **Returning a raw pointer that the body itself produced and that is valid at return time.** `Vec::ptr`, `Vec::terminated_ptr`, `String::terminated_ptr`. The body has done nothing unverifiable; the pointer points to the receiver's own buffer and is sound at the moment of return. Caller-side hazards (use-after-realloc, use-after-drop, OOB read) are entirely caller-side at the *deref* site, which the existing ADR-0028 rule already gates. This is the same pattern as Rust's `Vec::as_ptr` (`*const T` return, not `unsafe fn`; deref requires `unsafe { }`).
- **Receiving a raw pointer and only doing opaque-token operations on it.** Null check, cast to integer for printing/hashing, drop on the floor, return unchanged as a field of a struct the caller already owns. None of these depend on the pointer pointing to anything valid.
- **A fn whose body does raw-pointer arithmetic on its own internal buffer, bounded by its own length field.** `Vec::push`, `Vec::get`, etc. The pointer ops live inside `checked { }` in the body, but the bounds come from `self.len`, not from caller assertions. The fn's external contract is pointer-free and the body's correctness is self-contained.

The principle applied to the prelude inventory:

| Fn | `@mark(unchecked)`? | Reason |
|---|---|---|
| `String::from_utf8_unchecked(v: Vec(u8)) -> Self` | yes | body stores `v`, future methods trust UTF-8 (category 2) |
| `String::from_c_str_unchecked(p: Ptr(u8)) -> Self` | yes | body walks `p` (category 1) |
| `String::push_byte(self, b: u8)` | yes | body stores `b`, future methods trust UTF-8 (category 2) |
| `char::from_u32_unchecked(n: u32) -> char` | yes | body trusts Unicode-scalar (category 2) |
| `String::terminated_ptr(self) -> Ptr(u8)` | **no** | body writes terminator inside `checked { }`, returns valid pointer; deref is caller-side and already gated |
| `Vec::ptr(self) -> Ptr(T)` | **no** | same shape (closes Open Question 3) |
| `Vec::terminated_ptr(self) -> Ptr(T)` | **no** | same shape (closes Open Question 3) |
| `Vec::from_raw_parts(p: Ptr(T), len: usize, cap: usize) -> Self` (future) | yes | body stores `p` for later deref (category 1) |
| `Vec::push(self, x: T)` | no | internal pointer arithmetic bounded by `self.len` |
| `String::len(self) -> usize` | no | no pointer involvement |
| `p.read()` / `p.read_volatile()` (ADR-0063) | yes | body derefs `self` (category 1) |
| `p.write(v)` / `p.write_volatile(v)` | yes | body derefs `self` (category 1) |
| `p.offset(n)` | yes | body does pointer arithmetic on a caller-supplied pointer; provenance + bounds caller-asserted (category 1) |
| `p.copy_from(src, n)` | yes | body derefs both `self` and `src` (category 1) |
| `p.is_null()` | **no** | opaque-token comparison — works on any pointer, valid or not |
| `p.to_int()` | **no** | opaque-token cast — reading the address as a number doesn't depend on the pointer being valid |
| `Ptr(T)::from(r: Ref(T))` / `MutPtr(T)::from(r: MutRef(T))` | **no** | wraps a checked reference as a raw pointer; same shape as `Vec::ptr`. Deref is caller-side and already gated |
| `Ptr(T)::null()` / `MutPtr(T)::null()` | **no** | produces a constant; no caller-asserted invariant |
| `Ptr(T)::from_int(addr)` / `MutPtr(T)::from_int(addr)` | **no** | body relabels bits as a `Ptr(T)`; every silent-trust op on the result (`read`/`write`/`offset`/`copy_from`) is itself unchecked, so the deref-time gate handles invalid `addr`. Constructing an invalid `Ptr(T)` doesn't itself cause UB |
| FFI: `@mark(unchecked) fn sin(x: f64) -> f64` | yes | category 3 |

**No signature-level lint.** Because the rule is body-side, there is nothing the compiler can mechanically check at declaration time without analysing the body. Authors decide whether a fn falls into categories 1–3; the existing ADR-0028 rule (deref outside `checked { }` ⇒ error) catches the caller-side mistakes that would actually cause UB. The marker exists to declare the contract, not to police it from the outside.

### `@mark(unchecked)` on top-level fns and methods

The grammar for fn and method declarations accepts `@mark(unchecked)` in the directive list, in the same position as `@mark(c)`, `@derive(...)`, and other directives:

```
fn-decl     := directive* `pub`? `fn` name `(` params `)` (`->` type)? block
method      := directive* `pub`? `fn` method-name `(` method-params `)` (`->` type)? block
```

The `Function` and `Method` AST nodes already carry `is_unchecked: bool` (Function from ADR-0028, Method to be added by this ADR mirroring it). The parser sets `is_unchecked = true` whenever the directive list contains `@mark(unchecked)`. RIR's method-side `MethodInfo` in `gruel-air/src/sema/info.rs` already carries the field — it's currently always `false`; this ADR makes it user-settable.

Call-site enforcement is already in place. Both `analyze_struct_method_call` (~line 3643) and `analyze_struct_function_call` (~line 4457) check `method_info.is_unchecked && ctx.checked_depth == 0` and emit the `RawPointerOutsideChecked`-shape diagnostic. Once `is_unchecked` propagates from the directive list through astgen, those gates fire as intended without further work.

Both regular `impl`-block methods and anonymous-struct-literal methods (parsed via `anon_struct_method_parser`) pick up the new directive — directive parsing is shared between them. Destructor methods (`fn __drop(self)`) are forbidden from carrying `@mark(unchecked)` (`UncheckedDestructor`), same rationale as `fn __drop` being forbidden from C-layout structs: drop glue runs implicitly at scope exit and there is no caller-side `checked { }` to gate it.

Worked example for the stdlib:

```gruel
pub struct String {
    bytes: Vec(u8),

    // ADR-0072 escape hatches — caller asserts the UTF-8 invariant.
    @mark(unchecked)
    pub fn from_utf8_unchecked(v: Vec(u8)) -> Self {
        Self { bytes: v }
    }

    @mark(unchecked)
    pub fn from_c_str_unchecked(p: Ptr(u8)) -> Self {
        Self::from_utf8_unchecked(cstr_to_vec(p))
    }
}
```

Callers continue to use `checked { String::from_utf8_unchecked(v) }` exactly as today — the wire-form of the call site is unchanged. What changes is *how* the gate is enforced: by the general `is_unchecked` flag (driven by directive presence) instead of a per-name allowlist.

### FFI imports require `@mark(unchecked)`

The grammar for fn declarations inside `link_extern` and `static_link_extern` blocks accepts `@mark(unchecked)` in the directive list, and sema requires it:

```
extern-fn-decl := directive* `fn` name `(` params `)` (`->` type)? `;`
```

The parser accepts the directive's absence (so existing source parses cleanly for good diagnostics), but sema rejects the missing-directive case with `ExternFnMissingUnchecked`. The fix is mechanical and small:

```gruel
// Old (ADR-0085, current):
link_extern("m") {
    fn sin(x: f64) -> f64;
}

// New (this ADR):
link_extern("m") {
    @mark(unchecked) fn sin(x: f64) -> f64;
}

// Call site:
fn compute(x: f64) -> f64 {
    checked { sin(x) }
}
```

Sema's `collect_extern_fn_signatures` in `gruel-air/src/sema/declarations.rs` (~line 2400) currently hardcodes `is_unchecked: false` for every `FunctionInfo` it builds from an extern fn (~line 2573). That changes to drive `is_unchecked` from the parsed directive list, and sema rejects any extern fn whose directive list lacks `@mark(unchecked)`.

Top-level `@mark(c) fn …{ }` exports do *not* require `@mark(unchecked)`. Rationale: the export's body is Gruel and the Gruel side of the call boundary is verified. The unverified party is the *C caller*, which is invisible to the Gruel call discipline. Forcing `@mark(unchecked)` on exports would mean Gruel-side callers (e.g. unit tests that exercise the exported callback) have to wrap every call in `checked { }` for no information-theoretic benefit. Imports are the asymmetric case: the foreign body is opaque.

Empty `link_extern("foo") { }` blocks remain permitted (ADR-0085: useful for indirect-symbol-access cases); the `@mark(unchecked)` requirement is vacuous when there are no fns. `static_link_extern` inherits the same rule.

### Interface methods and conformance

`@mark(unchecked)` is legal on interface method signatures, in both named and anonymous interfaces. This is the piece that lets generic code operate over interfaces whose implementations require caller-asserted preconditions, and it lifts the early draft's "no `@mark(unchecked)` interface methods in v1" restriction (formerly Open Question 4).

The motivation is specific to Gruel's generic model. Comptime-constraint generics (`fn f(comptime T: I, t: T)`, ADR-0056) are re-analyzed per specialization, with `t.method()` resolving to `C::method` after the concrete `C` is substituted. If `C::method` is `@mark(unchecked)` but the interface signature is checked, the generic body's call would suddenly require `checked { }` for *some* specializations and not others — the body would have to wrap defensively, or the gate would be undecidable at the generic site. The fix is to make the interface signature carry the unchecked-ness, and to require conforming implementations to match it exactly. Runtime fat-pointer dispatch (`Ref(I)` / `MutRef(I)`, ADR-0076) inherits the same rule trivially — the dispatch site only ever has the interface signature to consult, so the gate is decided there.

Concretely:

```gruel
// Checked interface — implementors encapsulate any internal unsafety.
interface Reader {
    fn read(self: MutRef(Self)) -> Vec(u8);
}

fn copy_all(comptime T: Reader, r: MutRef(T)) -> Vec(u8) {
    r.read()  // no checked { } — interface signature is checked
}

// Unchecked interface — caller preconditions are part of the contract.
interface UnsafeReader {
    @mark(unchecked) fn read(self: MutRef(Self)) -> Vec(u8);
}

fn copy_unchecked(comptime T: UnsafeReader, r: MutRef(T)) -> Vec(u8) {
    checked { r.read() }  // interface signature is unchecked, gate at the call
}
```

The mechanical changes are small relative to the interface machinery that already exists (ADR-0056 §"Conformance check", ADR-0060):

- **`InterfaceMethodReq` in `gruel-air/src/types.rs` gains `is_unchecked: bool`.** Parallel to `Method::is_unchecked` and `Function::is_unchecked`. `validate_interface_decls` populates it from the parsed directive list on the method signature.
- **`check_conforms` requires exact match.** For each interface method, the concrete method's `is_unchecked` must equal the interface method's `is_unchecked`. Mismatch is `InterfaceMethodUncheckedMismatch` (new diagnostic), citing both signatures the same way `InterfaceMethodSignatureMismatch` does today.
- **Call-site gate reads the interface signature.** In comptime mode, after specialization the call lowers to a direct call to `C::method`, and the existing `is_unchecked` gate (`gruel-air/src/sema/{analysis,builtins,pointer_ops}.rs`) fires on `C::method.is_unchecked` — which equals the interface's by conformance. In runtime mode (fat-pointer dispatch through `MethodCallDyn`), sema knows only the interface method's `is_unchecked` and fires on that directly. Both paths land the same diagnostic.
- **Anonymous interfaces (ADR-0057) inherit the rule unchanged.** Anonymous interface methods are built by the comptime interpreter into `InterfaceMethodReq` values via the same path; if the `@mark(unchecked)` directive is on a method signature inside `interface { … }`, the resulting `InterfaceMethodReq.is_unchecked` is `true` and structural dedup keys on it like any other signature field.
- **`Self`-substitution (ADR-0060) is orthogonal.** `is_unchecked` is a per-method flag, not a per-type substitution; it survives `Self → C` substitution unchanged.

This makes both shapes above expressible by the end of this ADR. Implementors of `Reader` must encapsulate any internal unsafety inside their body (wrapping unchecked calls in `checked { }` internally and vouching for the preconditions). Implementors of `UnsafeReader` declare their `read` as `@mark(unchecked) fn read(...)`, matching the interface; callers — generic or direct — gate at the call site.

The asymmetric design ADR-0085 reserved for capability-systems is still preserved: per-fn `@mark(unchecked)` on an interface method is the floor, and a future capability ADR can layer per-block witnesses on top of any interface-method call site without rewriting this surface.

### Capability-system seam, preserved

ADR-0085 reserved the `link_extern` block as a named lexical unit a future capability ADR could refer to. That seam is unchanged. The `@mark(unchecked)` directive proposed here is *per-declaration syntactic gating*, the same shape as `@mark(unchecked)` on top-level fns and methods; the capability ADR can layer a *per-block scope* on top — e.g. `checked using cap_libc { sin(x) }` could subsume the `checked { }` requirement for any fn imported from a block tagged `using cap_libc`. The per-fn directive doesn't conflict with that layering; it's the floor, not the ceiling.

What this ADR explicitly does not do: introduce per-block FFI gating, capability tokens, or scope-keyed `checked` brackets. Those remain the capability ADR's territory.

### Stdlib transition

Four of the five hardcoded gates retire as `@mark(unchecked)` declarations; one — `String::terminated_ptr` — retires the gate entirely (by the principle above, it is not unchecked):

| Symbol | Current gate | New form |
|---|---|---|
| `String::from_utf8_unchecked` | `check_string_vec_bridge_method_gates` by-name | `@mark(unchecked) pub fn` in `prelude/string.gruel` |
| `String::from_c_str_unchecked` | same | same |
| `String::push_byte` | same | same |
| `String::terminated_ptr` | same | **ordinary `pub fn`** — body writes terminator in `checked { }`, returns valid pointer; no caller-asserted invariant |
| `char::from_u32_unchecked` | `pointer_ops.rs` by-name | `@mark(unchecked) pub fn` in `prelude/char.gruel` |

`check_string_vec_bridge_method_gates` and the `char::from_u32_unchecked` special-case path in `pointer_ops.rs` are deleted. Existing prelude call sites of the four `@mark(unchecked)` methods already wrap in `checked { }` (string.gruel:113, char.gruel:13); the migration is purely declaration-side. Call sites of `String::terminated_ptr` lose the `checked { }` wrapper around the *call* (since the call is no longer to an unchecked fn), but keep it around any subsequent deref of the returned pointer — which is where the gate belongs.

Pointer-returning prelude methods outside the original allowlist (`Vec::ptr`, `Vec::terminated_ptr`, etc.) stay as ordinary `pub fn`. They were never on the by-name gate list, the principle above explains why no marker is needed, and Open Question 3 collapses to "no, leave them as ordinary fns."

The third hardcoded gate that retires under the principle is the `POINTER_METHODS::requires_checked` flag in `gruel-intrinsics/src/lib.rs`. Today every pointer method has `requires_checked: true` — a uniform per-class gate that's simpler than the string/char allowlist but still hardcoded and uniformly too strict (per the principle, `is_null`, `to_int`, `from`, `null`, `from_int` don't need `checked { }`; only the operations that actually dereference or do provenance-sensitive arithmetic do). The field is renamed to `is_unchecked` and rebalanced; `dispatch_pointer_method_call` stops consulting its private flag and lets the resolved method's `is_unchecked` flow into the same `UncheckedFnRequiresChecked` path that fires for user-defined `@mark(unchecked)` methods. After this ADR every "must be inside `checked { }`" enforcement in the language is driven by one mechanism — the `is_unchecked` flag on the resolved fn/method — instead of three (the string/char allowlist, the pointer-method registry's `requires_checked` field, and the general `@mark(unchecked)` directive).

### Migration of existing `unchecked fn`

The pre-existing top-level `unchecked fn` keyword surface (ADR-0028) migrates to `@mark(unchecked) fn` under the same preview gate. Every existing `unchecked fn` declaration in the codebase rewrites mechanically:

```gruel
// Old (ADR-0028):
unchecked fn deref_ptr(p: Ptr(u8)) -> u8 { … }

// New (this ADR):
@mark(unchecked)
fn deref_ptr(p: Ptr(u8)) -> u8 { … }
```

Inventory: spec tests under `cases/expressions/unchecked-code.toml` and prelude/runtime fns that currently use the keyword. The transition is one search-and-replace, gated by the preview feature so old source still parses while the migration window is open. Stabilisation removes the hard-keyword `unchecked` token from the lexer.

### Preview gating

`PreviewFeature::UncheckedFnExtensions` (CLI: `unchecked_fn_extensions`). The gate fires on:

- The `@mark(unchecked)` directive appearing on a fn or method declaration (Phase 1).
- The `@mark(unchecked)` requirement on `link_extern` / `static_link_extern` fn imports (Phase 2). Without the preview flag, the missing directive is silently accepted (existing ADR-0085 behaviour). With the preview flag, missing-directive is an error.
- The `@mark(unchecked)` directive on interface method signatures (named or anonymous), and the corresponding conformance exact-match rule (Phase 3a).
- The legacy `unchecked` keyword on top-level fns continues to work without the preview gate during the migration window, so existing source compiles unchanged. Stabilisation (Phase 6) removes the keyword.

The gate retires in Phase 5.

The preview-gating shape for the FFI rule deserves explanation: this ADR changes a stabilised surface (the `c_ffi` preview retired with ADR-0085 Phase 5), so the FFI-side change is a real source-breaking modification. The preview gate carries the breakage during incubation — existing source compiles unchanged without `--preview unchecked_fn_extensions`, and the ecosystem (which today is just the gruel repo's own scratch files and spec tests) migrates over the preview window before stabilisation.

### Diagnostics

New:

- `ExternFnMissingUnchecked { fn_name, library }` — fired in sema when an FFI import lacks `@mark(unchecked)` and the preview gate is on. Points at the `fn` keyword and suggests adding the directive. Includes the containing library name so multi-library files give actionable spans.
- `UncheckedDestructor` — fired on `@mark(unchecked) fn __drop(self)`. Drop glue runs implicitly at scope exit; there's no caller `checked { }` to gate it.
- `InterfaceMethodUncheckedMismatch { interface, method, expected_unchecked, actual_unchecked }` — fired by `check_conforms` when a candidate method's `is_unchecked` does not equal the interface method's. Renders both signatures side-by-side, same shape as `InterfaceMethodSignatureMismatch` from ADR-0056. Includes a hint: if the interface is checked and the implementation is unchecked, suggest encapsulating internally; if the interface is unchecked and the implementation isn't, suggest adding the directive.
- `UncheckedFnExtensionsPreviewRequired` — generic preview-gate error for the method-level surface, the FFI-side change, and the interface-method extension.

Existing diagnostics that no longer fire:

- The two anonymous diagnostics inside `check_string_vec_bridge_method_gates` (which reuse the `RawPointerOutsideChecked` payload) — replaced by the general `UncheckedFnRequiresChecked` path that already fires for unchecked top-level fns.

## Implementation Phases

- [x] **Phase 1: `@mark(unchecked)` directive on fns and methods** — Add `Unchecked` to `BUILTIN_MARKERS` in `gruel-builtins` with legal positions {top-level fn, method, interface method, FFI import}. Add `is_unchecked: bool` to `Method` AST (mirroring `Function`); plumb directive-list parsing through astgen so `@mark(unchecked)` sets `is_unchecked`; carry through to RIR (`RirFn` already has `is_unchecked`) → AIR (`MethodInfo::is_unchecked`); reject `@mark(unchecked) fn __drop(...)` with `UncheckedDestructor`. Keep the legacy `unchecked` keyword working on top-level fns (sets the same `is_unchecked` flag). Add `PreviewFeature::UncheckedFnExtensions`. Spec tests under `cases/items/unchecked-methods.toml`: `@mark(unchecked)` method declared, call without `checked { }` is rejected, call inside `checked { }` succeeds, `@mark(unchecked) fn __drop` rejected, preview-gating on the directive.

- [x] **Phase 2: FFI requires `@mark(unchecked)`** — Allow `@mark(unchecked)` in the directive list of extern-fn declarations (parser side); under the preview gate, sema rejects FFI imports without it (`ExternFnMissingUnchecked`); `collect_extern_fn_signatures` drives `is_unchecked` from the directive list. Update `cases/items/c-ffi.toml`, `cases/items/c-ffi-enum.toml`, `cases/items/c-ffi-static.toml`, and `cases/runtime/c-ffi.toml` extern declarations to add `@mark(unchecked)`; update call-site test cases to wrap calls in `checked { }`. New spec tests for the rejection path (missing directive under preview gate).

- [ ] **Phase 3a: Interface methods and conformance** — Allow `@mark(unchecked)` on interface method signatures (parser + RIR `InterfaceMethodSig`). Add `is_unchecked: bool` to `InterfaceMethodReq` in `gruel-air/src/types.rs`; `validate_interface_decls` populates it from the parsed directive list. Extend `check_conforms` to require exact `is_unchecked` match between interface method and concrete candidate, emitting the new `InterfaceMethodUncheckedMismatch` diagnostic on mismatch. Verify the comptime-constraint path (`comptime T: I`) gates calls via the already-substituted `C::method.is_unchecked`. Wire `MethodCallDyn` (runtime fat-pointer dispatch) to read the interface method's `is_unchecked` and apply the existing call-site gate. Anonymous-interface dedup keys on `is_unchecked` like any other signature field. Spec tests under `cases/items/unchecked-interface.toml`: `Reader` (checked) — `@mark(unchecked) fn read` implementor rejected with mismatch; `UnsafeReader` (unchecked) — checked implementor rejected with mismatch; generic `fn copy_all(comptime T: Reader, ...)` compiles without `checked { }`; generic `fn copy_unchecked(comptime T: UnsafeReader, ...)` requires `checked { }`; runtime dispatch through `Ref(UnsafeReader)` requires `checked { }` at the call site; anonymous interface `interface { @mark(unchecked) fn read(self); }` behaves identically.

- [ ] **Phase 3b: Stdlib transition** — Mark the four prelude escape hatches as `@mark(unchecked) fn` (`String::from_utf8_unchecked`, `String::from_c_str_unchecked`, `String::push_byte`, `char::from_u32_unchecked`). Drop `String::terminated_ptr` from `check_string_vec_bridge_method_gates` and leave it as ordinary `pub fn` (its call sites no longer need `checked { }` around the call itself, only around any subsequent pointer deref). Delete `check_string_vec_bridge_method_gates` from `gruel-air/src/sema/builtins.rs`. Delete the `char::from_u32_unchecked` special-case in `gruel-air/src/sema/pointer_ops.rs` (~line 716). Verify all existing prelude call sites continue to work.

- [ ] **Phase 3c: Pointer-method gate** — Rename `PointerMethod::requires_checked` to `PointerMethod::is_unchecked` in `gruel-intrinsics/src/lib.rs` and rebalance the table per the principle: `read`, `read_volatile`, `write`, `write_volatile`, `offset`, `copy_from` are `is_unchecked = true`; `is_null`, `to_int`, `from`, `null`, `from_int` are `is_unchecked = false`. `dispatch_pointer_method_call` in `gruel-air/src/sema/pointer_ops.rs` stops gating on its own private flag — the resolved method's `is_unchecked` flows into the existing `UncheckedFnRequiresChecked` path that already fires for user `@mark(unchecked)` methods. Spec-test sweep: existing tests of `Ptr(T)::null()`, `Ptr(T)::from(&x)`, `Ptr(T)::from_int(addr)`, `p.is_null()`, `p.to_int()` lose their `checked { }` wrappers around the *call* (keeping them around any deref); existing tests of the unchecked subset stay as-is. New spec tests asserting the negative case (calling `p.read()` outside `checked { }` is rejected) keyed off the directive path rather than the registry flag.

- [ ] **Phase 4: Migrate existing top-level `unchecked fn` to `@mark(unchecked)`** — Sweep the codebase (prelude, spec tests, runtime) replacing `unchecked fn` with `@mark(unchecked)\nfn`. Both syntaxes accepted during the preview window so the migration can happen in stages.

- [ ] **Phase 5: Spec + tests** — New spec section under `docs/spec/src/09-unchecked-code/` for the `@mark(unchecked)` directive surface (paragraphs 9.2:X–Y), a new paragraph in `docs/spec/src/10-c-ffi/01-c-ffi-overview.md` (or 10.1's existing extern-fn section) requiring `@mark(unchecked)` on FFI imports, an addition to §6.5 (interfaces) defining `@mark(unchecked)` on interface method signatures and its role in conformance, and updates to §9.1/§9.2 listing which `Ptr(T)`/`MutPtr(T)` methods are `@mark(unchecked)` (matching the principle table). Update ADR-0083's BUILTIN_MARKERS reference list. Add `spec = [...]` traceability to every Phase 1–4 test. UI tests for diagnostic quality on `ExternFnMissingUnchecked`, `UncheckedDestructor`, `InterfaceMethodUncheckedMismatch`, and the method-level `UncheckedFnRequiresChecked` path. Run `make test` to confirm normative coverage stays at 100%.

- [ ] **Phase 6: Stabilise** — Remove `PreviewFeature::UncheckedFnExtensions`; strip `preview = "unchecked_fn_extensions"` from spec tests. Make the FFI `@mark(unchecked)` requirement unconditional. Make the interface-method `is_unchecked` field a permanent part of `InterfaceMethodReq` (no longer gated). Make the pointer-method `is_unchecked` table the permanent disposition (delete any remaining `requires_checked` references). Remove the legacy `unchecked` hard keyword from `gruel-lexer` and the `unchecked_fn_parser` slot from `gruel-parser`. ADR status → `implemented`. Update ADR-0028, ADR-0056, ADR-0060, ADR-0063, and ADR-0085's "Open Questions"/"Future Work" sections to point at this ADR as the resolution. Sweep prelude `link_extern` blocks to add `@mark(unchecked)` on every import (the preview gate would have caught them in CI, so this should be empty by Phase 6).

## Consequences

### Positive

- **FFI/non-FFI calls are visually distinguishable at the call site.** A reader scanning `compute(x: f64) -> f64 { checked { sin(x) } }` can tell `sin` is foreign without resolving its declaration. ADR-0085's largest stated Negative consequence is closed.
- **One uniform marker story across the language.** `@mark(unchecked)` joins `@mark(copy)`, `@mark(linear)`, `@mark(c)`, etc. in the directive registry. The hard keyword `unchecked` retires alongside `copy`/`linear` did under ADR-0083 — the language has *one* mechanism for declaration-time markers, not two (directives) plus one (contextual `unchecked` keyword) plus three by-name allowlists.
- **One uniform `unchecked` story across the fn surface.** Top-level fns, methods, interface methods, FFI imports, and pointer-type methods all use the same syntactic gate (`@mark(unchecked)`) and one shared sema enforcement path. Three hardcoded gates retire — the `check_string_vec_bridge_method_gates` by-name table, the `char::from_u32_unchecked` per-name carve-out, and the `PointerMethod::requires_checked` field on the pointer-method registry. Future stdlib escape hatches pick up the gate by adding the directive — no compiler change required.
- **Pointer-method gating becomes principled rather than uniform.** Today every `Ptr(T)` / `MutPtr(T)` method requires `checked { }`; under this ADR only the methods whose bodies actually dereference or do provenance-sensitive arithmetic do (`read`, `read_volatile`, `write`, `write_volatile`, `offset`, `copy_from`). The opaque-token operations (`is_null`, `to_int`) and the constructors that don't themselves perform unverified work (`from`, `null`, `from_int`) drop their gate. This is consistent with how `Vec::ptr` / `Vec::terminated_ptr` were resolved by the principle, and it removes the asymmetry where `Ptr(T)::null()` requires `checked { }` but `Vec::ptr(self) -> Ptr(T)` doesn't.
- **The capability-system seam is preserved.** Per-fn `@mark(unchecked)` is orthogonal to the per-block capability witness ADR-0085 left open. The capability ADR can introduce `checked using cap_libc { … }` later without rewriting any of this surface.
- **Method-level `@mark(unchecked)` is small, mechanical, and self-contained.** Method AST gains one bool, parser already runs directives on every fn-like declaration site, sema gates already exist for the field.
- **The body-side principle gives a single sentence for "when is `@mark(unchecked)` required?"** A fn earns the marker iff its body relies on a precondition the caller is responsible for and the type system cannot verify. Falls out of ADR-0028's original definition. Authors apply it at declaration time; the existing pointer-op gate catches caller-side mistakes. Future stdlib escape hatches and FFI imports are covered by the same one-line rule.
- **Generic code can reach unchecked operations through interfaces.** With `@mark(unchecked)` legal on interface method signatures and made part of the conformance signature, `fn copy_unchecked(comptime T: UnsafeReader, ...)` is expressible — and so is the symmetric clean case (`fn copy_all(comptime T: Reader, ...)`). FFI-backed implementations of interfaces, allocator interfaces, raw-IO interfaces, etc. become writable as part of the regular interface surface instead of bouncing through non-generic per-type bindings. Runtime fat-pointer dispatch (`Ref(I)` / `MutRef(I)`) inherits the same rule trivially since the dispatch site reads from the interface signature.

### Negative

- **Breaking change on a stabilised FFI surface.** ADR-0085 retired the `c_ffi` preview at Phase 5; existing FFI source (the gruel repo's spec tests, real-world FFI users if any) compiles today without `@mark(unchecked)`. The preview gate carries the breakage, but stabilisation is a hard cut: every `link_extern` fn declaration in the ecosystem needs the directive added. Mitigated by scope (current FFI users = a handful of spec tests inside this repo) but still real.
- **Source-breaking migration of `unchecked fn` → `@mark(unchecked) fn`.** ADR-0028's keyword surface stabilised; every existing call site changes spelling. Mechanically a one-line-per-site edit, and the preview window keeps both syntaxes alive during the migration, but it does mean the ADR carries a second migration sweep on top of the FFI one. Mitigated by the same scope argument (gruel repo is the only ecosystem today).
- **Call-site noise on FFI-heavy code.** Code that previously read `let result = sin(2.0) + cos(2.0);` now reads `let result = checked { sin(2.0) + cos(2.0) };` (or two separate `checked { }` brackets). For numerics-heavy FFI bindings (`m`, vector math libraries) the visual weight is noticeable. The capability ADR can take the edge off by introducing block-level witnesses (`checked using cap_libm { sin(2.0) + cos(2.0) }`); until it lands, the verbosity is the price of the gate.
- **`@mark(unchecked)` becomes load-bearing in two places at once.** Today the `unchecked` keyword is a top-level-fn-only modifier most users never see. After this ADR, the directive is mandatory on every FFI import and on five stdlib methods. Anyone writing or reading FFI code touches it constantly. Whether this is good or bad depends on framing: it makes the unsafety visible (positive), but it also makes the marker feel ubiquitous in FFI code (negative if it desensitises readers to the "this is unsafe" signal).
- **The method-level surface is small today.** Five stdlib methods total. Outside the prelude, user code rarely needs unchecked methods — most user-defined unchecked operations live in top-level fns. The grammar extension is sound and consistent, but the bang-for-buck from method-level `@mark(unchecked)` is mostly *enabling the stdlib transition*, not enabling broad user-side use.
- **Directive verbosity vs. keyword.** `@mark(unchecked) fn foo()` is longer than `unchecked fn foo()`. For a marker that appears on every FFI import, the cost is non-zero. Justified by uniformity with the rest of the marker system (ADR-0083) and the disappearance of the hard keyword, but real.
- **Conformance signature widens.** `InterfaceMethodReq` gains one field (`is_unchecked: bool`) and `check_conforms` gains one comparison. Anonymous-interface structural dedup keys on it too. Mechanically small but it does add to the conformance contract surface — a type that conforms by accident today could conceivably stop conforming under the new rule if the implementation and interface disagree on unchecked-ness. In practice nothing in the current ecosystem trips this (interface methods have no precedent for `@mark(unchecked)`, since this ADR introduces it), but the precedent is worth flagging.

### Neutral

- `@mark(c) fn` exports stay non-`@mark(unchecked)`. The asymmetry between imports (always unchecked) and exports (never unchecked) is defensible — the unverified party is different in each direction — but worth flagging because future readers may find the asymmetry surprising.
- The `@mark(unchecked)` directive is purely declarative — it adds no runtime cost and emits no LLVM IR changes. All enforcement is sema-side.
- `@mark(unchecked)` and `@mark(c)` can co-occur on a single FFI import declaration (in fact every FFI import will carry both implicitly — `@mark(c)` is the binding-side convention from ADR-0085 once an extern fn participates in C ABI rules). Directive ordering is irrelevant.

## Open Questions

1. **Should `@mark(c) fn` exports also accept (or require) `@mark(unchecked)`?** This ADR says exports stay non-unchecked. Counter-argument: an exported callback is the *Gruel-side definition* of a function whose *only legitimate caller* is C code passing arguments the Gruel type system hasn't verified (e.g. raw `void*` payloads cast back to a Gruel struct). Requiring `@mark(unchecked)` on exports would make the asymmetry one-way unsafe: imports are unchecked because their bodies are foreign; exports are unchecked because their callers are foreign. Recommend leaving exports non-`@mark(unchecked)` for now (matches the Gruel-side call-site reasoning); revisit if real export-side bug reports surface.

2. **Should the FFI-must-be-`@mark(unchecked)` rule extend to FFI calls through `MutPtr(c_void)`-cast `@mark(c) fn` identifiers (ADR-0086's transport)?** Today such calls happen via raw pointer dereference inside a `checked { }` block, so the gating is already in place. But a future typed-extern-fn-pointer type (ADR-0086 Future Work) would make these calls statically typed and bypass the raw-pointer gate. Recommend: when the typed fn-pointer ADR lands, it inherits the `@mark(unchecked)` requirement from this one.

3. ~~**Should the rest of the `Vec`/`String` "raw-pointer-returning" prelude methods also be `@mark(unchecked)`?**~~ *Resolved by the body-side principle (Decision §"Principle"):* **no**. Returning a raw pointer that the body has produced and that is valid at return time does not entail a caller-asserted invariant — the caller-side hazards (use-after-realloc, use-after-drop, OOB) are gated at the deref site by ADR-0028's existing rule. `Vec::ptr`, `Vec::terminated_ptr`, and (resolved by this revision) `String::terminated_ptr` are ordinary `pub fn`. The previous draft of this ADR proposed marking them and was wrong on this point.

4. ~~**Should `@mark(unchecked)` propagate through interface implementations?**~~ *Resolved by the Decision §"Interface methods and conformance":* the directive is legal on interface method signatures, and conformance requires exact `is_unchecked` match between interface and implementor. Earlier drafts deferred this as "no `@mark(unchecked)` interface methods in v1," but that disposition left generic code unable to reach unchecked operations through an interface — making FFI-backed implementors of common interfaces inexpressible — so the v1 restriction is lifted in this ADR.

5. **Do we need a separate diagnostic for "called unchecked method without `checked { }`" vs "called unchecked fn without `checked { }`"?** Today both fire `RawPointerOutsideChecked` (overloaded). A more specific `UncheckedFnRequiresChecked` would be clearer. Recommend yes, as part of Phase 1.

6. **Could `@mark(unchecked)` ever apply to non-fn declarations (e.g. an `unchecked` block at module scope, or `@mark(unchecked) static FOO: …`)?** Out of scope for this ADR. The marker registry entry restricts legal positions to fn-like declarations only; a future extension can broaden the position set if a concrete use case appears.

## Future Work

- **Block-level capability witnesses for FFI.** The capability ADR — `checked using cap_libc { … }` or similar — layers on top of this ADR's per-fn `@mark(unchecked)` without conflict. The `link_extern` block remains the natural unit for a per-library witness.
- **Typed extern fn pointer types.** ADR-0086 Future Work; inherits the `@mark(unchecked)` requirement when it lands.
- **`@mark(unchecked)` on closures.** If/when Gruel grows closures, the same per-declaration syntactic gate applies. Anonymous-function callable structs (ADR-0055) already pick up the rule for free as soon as the directive lands on `__call` methods.
- **Lint for empty `checked { }` blocks.** A warning when a `checked { }` block's body contains no unchecked call or pointer operation (i.e. the bracket is unnecessary). Out of scope here.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0028: Unchecked Code and Raw Pointers](0028-unsafe-and-raw-pointers.md) — original `unchecked fn` keyword surface; this ADR migrates it to `@mark(unchecked)` and extends it to methods and FFI imports.
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md)
- [ADR-0072: String/Vec(u8) bridge](0072-string-vec-u8-relationship.md) — defines the five hardcoded escape hatches this ADR migrates to general `@mark(unchecked)`.
- [ADR-0083: `@mark(...)` directive](0083-mark-directive.md) — defines the marker-directive system this ADR extends with `unchecked`; same motivation (kill the contextual keyword, unify on directives).
- [ADR-0025: Compile-Time Execution](0025-comptime.md) — comptime-parameter and monomorphization model that this ADR's interface-conformance rule needs to make generic-over-interface code coherent.
- [ADR-0056: Structurally Typed Interfaces](0056-structural-interfaces.md) — defines `check_conforms` and the comptime-constraint / fat-pointer dispatch modes this ADR extends.
- [ADR-0057: Anonymous Interfaces](0057-anonymous-interfaces.md) — anonymous-interface dedup picks up `is_unchecked` as part of the conformance signature.
- [ADR-0060: Complete Interface Signatures](0060-complete-interface-signatures.md) — added `Self` and receiver modes to `InterfaceMethodReq`; this ADR extends the same struct with `is_unchecked`.
- [ADR-0063: Pointer Operations as Methods on Ptr / MutPtr](0063-pointer-method-syntax.md) — defines the `POINTER_METHODS` registry; this ADR's principle prunes the registry's uniform `requires_checked: true` to the subset of methods whose bodies do unverified work.
- [ADR-0076: Pervasive `Self` and Sole-Form References](0076-pervasive-self-and-sole-form-references.md) — `Ref(I)` / `MutRef(I)` is the dispatch shape inherited at every interface-method call site under runtime mode.
- [ADR-0085: C foreign function interface](0085-c-ffi.md) — direct parent on the FFI side; this ADR resolves the §"Call-site posture" deferral.
- [ADR-0086: C FFI extensions](0086-c-ffi-extensions.md) — adds `c_void`-and-friends; FFI-imports-are-`@mark(unchecked)` rule applies uniformly.
- [ADR-0087: Prelude fns for libc-wrapper intrinsics](0087-prelude-fns-for-libc-wrappers.md) — inherits the FFI gating shape decided here.
- [Rust Unsafe](https://doc.rust-lang.org/book/ch19-01-unsafe-rust.html) — `unsafe fn` per-declaration model is the direct precedent; Gruel diverges on spelling (directive vs. keyword) but not on semantics.
- [Rust RFC 2585](https://rust-lang.github.io/rfcs/2585-unsafe-block-in-unsafe-fn.html) — fixing the implicit-`unsafe`-body mistake; Gruel got this right in ADR-0028 by not making the unchecked-fn body an implicit `checked` block.
