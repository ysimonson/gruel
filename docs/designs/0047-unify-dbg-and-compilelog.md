---
id: 0047
title: Unify @dbg and @compileLog
status: proposal
tags: [intrinsics, comptime, diagnostics]
feature-flag: comptime_meta
created: 2026-04-21
accepted:
implemented:
spec-sections: [4.13, 4.14]
superseded-by:
---

# ADR-0047: Unify @dbg and @compileLog

## Status

Proposal

## Summary

Merge `@compileLog` into `@dbg` so there is one debug-print intrinsic whose output routing depends on the evaluation phase. When `@dbg` runs at runtime it prints to stdout (today's behavior). When it runs inside a comptime context, the sema interpreter prints each message to stderr with a `comptime dbg:` prefix *as it is evaluated* (matching `@compileLog`'s existing `eprintln!` behavior) and records the call for a post-sema "debug statement present" warning. The sema-side buffer that today records comptime `@dbg` output is preserved and exposed via a `--capture-comptime-dbg` CLI flag that suppresses the on-the-fly print, so the differential fuzzer can continue to consume structured output. `@compileLog` is removed; it is already gated behind the unstable `comptime_meta` preview feature, so the break is contained.

## Context

Today the language ships two near-duplicate compile-time debug intrinsics:

| Intrinsic | Phase | Args | Default output | Warning | Preview gate |
|-----------|-------|------|----------------|---------|--------------|
| `@dbg(x)` | runtime or comptime | single int/bool/string/struct/enum/array | runtime: stdout; comptime: silent buffer (only the fuzzer reads it) | no | none |
| `@compileLog(...)` | comptime only | variadic, any comptime type | stderr with `comptime log:` prefix | yes, per call | `comptime_meta` |

The split is accidental. `@compileLog` was introduced in ADR-0042 to give users a way to debug comptime logic. `@dbg`-in-comptime was added by ADR-0040 as infrastructure for the differential fuzzer: the interpreter needs to honor `@dbg` side effects, so it routes them into a `Vec<String>` on `Sema`. Neither surfaces comptime output through the CLI by default, so from a user's perspective today `@dbg` simply vanishes inside a `comptime { }` block. Users who want to see a comptime value must remember to reach for `@compileLog` (and enable `--preview comptime_meta`).

This is a bad mental model. The natural question "when is this `@dbg` going to print?" has the pleasing answer "whenever the code containing it executes" — but only if the compiler actually prints what it saw. Once we route comptime `@dbg` output to stderr, `@compileLog` is strictly redundant: same destination, same warning-on-successful-compile nag, same variadic formatting. Deleting it removes a concept from the language.

A real constraint is the fuzzer. `fuzz/fuzz_targets/comptime_differential.rs` compares comptime vs. runtime `@dbg` output byte-for-byte by reading the structured buffer from `SemaOutput`. That buffer needs to keep existing; a CLI flag is the clean escape hatch for suppressing the driver-side print when a tool wants the structured form instead.

## Decision

**Unify the intrinsics under `@dbg`.** Phase-specific output routing makes the print location predictable:

1. **`@dbg` becomes variadic at both phases.** Zero or more arguments, each of an acceptable type for the phase. At runtime: integer, bool, string (existing supported types). At comptime: any type `format_const_value` can render (integer, bool, unit, `comptime_str`). Arguments are space-joined for a single output line.

2. **Runtime behavior (unchanged destination, extended arity).** `@dbg(a, b, c)` at runtime prints `<a> <b> <c>\n` to stdout. A single-argument call is byte-identical to today's `@dbg(x)`.

3. **Comptime behavior (new, replacing `@compileLog`).** `@dbg(...)` inside a comptime evaluation context:
   - Evaluates each argument, formats via `format_const_value`, joins with spaces.
   - **Immediately** prints `comptime dbg: <message>` to stderr at the point of evaluation, *unless* `Sema.suppress_comptime_dbg_print` is set (which the driver flips on when `--capture-comptime-dbg` is passed). This matches how `@compileLog` is implemented today (`eprintln!` on the spot) and preserves partial output if comptime evaluation later errors or hits the step budget.
   - Appends the formatted string to `Sema.comptime_dbg_output` (existing buffer, keep the name) regardless of the suppression flag — the buffer is the fuzzer's consumption point.
   - Records `(message, span)` on `Sema.comptime_log_output` so a per-call warning is emitted after sema completes.

4. **`@compileLog` is removed.** Calls to `@compileLog` produce an error diagnostic suggesting `@dbg`. The `compile_log` entry in `known_symbols.rs` is removed, as is `analyze_compile_log_intrinsic`. The `comptime_meta` preview feature continues to gate the remaining metaprogramming intrinsics (`@typeName`, `@typeInfo`, `@field`, `@compileError`, `comptime_unroll for`) — `@dbg` itself is not gated, since it is already stable.

5. **Warning kind is kept but renamed.** `WarningKind::ComptimeLogPresent` becomes `WarningKind::ComptimeDbgPresent` with the same text ("debug statement present — remove before release").

### Phase detection

There is no new phase-detection machinery. Sema already has two distinct `@dbg` handlers — `analyze_dbg_intrinsic` (runtime path, produces an AIR intrinsic) and the `evaluate_comptime_inst` branch for `known.dbg` (comptime path, populates the buffer). This ADR widens both to accept variadic args and, in the comptime path, records for warning emission too.

### CLI surface

```
--capture-comptime-dbg    Suppress the on-the-fly stderr print of @dbg output
                          from comptime evaluation. The buffer is still populated
                          and accessible through the compilation state. Intended
                          for tools and fuzz harnesses that consume structured output.
```

The driver sets `Sema.suppress_comptime_dbg_print = true` when this flag is present; otherwise the intrinsic prints inline as it evaluates. No post-sema "replay" step — the CLI does not walk `comptime_dbg_output` for printing.

### Migration

Within the repo: update `crates/gruel-spec/cases/expressions/comptime_meta.toml` (compile_log tests become dbg tests or are folded into existing dbg tests), `fuzz/src/lib.rs` (only uses `@dbg`, no change needed), and `fuzz/fuzz_targets/comptime_differential.rs` (add `--capture-comptime-dbg` when invoking the compiler). Spec prose in `docs/spec/src/04-expressions/13-intrinsics.md` and `docs/spec/src/04-expressions/14-comptime.md` is rewritten to describe the unified intrinsic. Tutorial at `website/content/tutorial/14-comptime.md` is updated.

External users are unlikely: `@compileLog` required `--preview comptime_meta`, which is explicitly unstable and documented as subject to breaking changes.

## Implementation Phases

- [x] **Phase 1: Spec rewrite.** Update `docs/spec/src/04-expressions/13-intrinsics.md` and `docs/spec/src/04-expressions/14-comptime.md`: describe variadic `@dbg`, phase-dependent output routing, warning behavior. Remove `@compileLog` paragraphs (4.14:52, 4.14:53, 4.14:54); renumber or retire the IDs per spec conventions. Add new paragraphs covering the unified behavior and the `--capture-comptime-dbg` flag.

- [x] **Phase 2: Variadic runtime `@dbg`.** Relax `analyze_dbg_intrinsic` in `gruel-air/src/sema/analysis.rs` to accept zero or more args. Update codegen in `gruel-codegen-llvm/src/codegen.rs` to emit a sequence of type-dispatched `__gruel_dbg_*` calls interleaved with a space-writing call (add `__gruel_dbg_space` in `gruel-runtime/src/debug.rs`) and a final newline. Update spec tests covering `@dbg` runtime behavior.

- [x] **Phase 3: Variadic comptime `@dbg` + on-the-fly print + warning.** Widen the `known.dbg` branch in `evaluate_comptime_inst` to accept variadic args and use `format_const_value` on each, space-joining. At the point of evaluation: `eprintln!("comptime dbg: {msg}")` unless `self.suppress_comptime_dbg_print` is set. Always append to `comptime_dbg_output`. Push a `(msg, span)` pair onto `comptime_log_output` so the existing warning-emission pass fires. Rename `WarningKind::ComptimeLogPresent` → `WarningKind::ComptimeDbgPresent` (and the text).

- [x] **Phase 4: `--capture-comptime-dbg` flag wiring.** Add `suppress_comptime_dbg_print: bool` to `Sema` (default false). Add the CLI flag in `crates/gruel/src/main.rs`, thread through `gruel-compiler` into sema construction. No driver-side "print the buffer" step — the printing already happened in Phase 3.

- [x] **Phase 5: Remove `@compileLog`.** Delete `compile_log` from `known_symbols.rs`, delete `analyze_compile_log_intrinsic`, delete the `@compileLog` branch in `evaluate_comptime_inst`. Any remaining use produces "unknown intrinsic `compileLog`"; add a targeted diagnostic that suggests `@dbg` when the name is exactly `compileLog`. Remove the `comptime_meta` gate on the now-deleted code paths.

- [x] **Phase 6: Migrate tests and fuzz.** Convert `@compileLog` usages in `crates/gruel-spec/cases/expressions/comptime_meta.toml` to `@dbg` (or delete as redundant with existing dbg tests). Add UI tests in `crates/gruel-ui-tests/cases/` for: (a) default comptime `@dbg` prints to stderr with prefix, (b) warning emitted, (c) `--capture-comptime-dbg` suppresses the print, (d) helpful diagnostic for `@compileLog` misuse. Update `fuzz/fuzz_targets/comptime_differential.rs` to pass `--capture-comptime-dbg`. Update the tutorial at `website/content/tutorial/14-comptime.md`.

- [x] **Phase 7: Traceability and `make test`.** Run the traceability check; ensure every new spec paragraph has a test and no removed paragraph is still referenced. Run `make test` green.

## Consequences

### Positive

- One debug-print intrinsic instead of two. Mental model: "`@dbg` prints when the code runs."
- Comptime `@dbg` finally does something observable from the CLI by default — today it silently vanishes into a buffer.
- On-the-fly printing preserves output when comptime evaluation later fails (step budget exceeded, `@compileError`, type error in a subsequent instruction). A buffer-then-replay design would lose everything on error.
- The fuzz harness keeps working via an explicit, documented flag instead of an implicit "the CLI doesn't surface this" behavior.
- `comptime_meta` preview shrinks by one intrinsic, reducing the surface area remaining to stabilize.

### Negative

- Breaking change for anyone using `@compileLog` (mitigated: it was unstable and preview-gated).
- New subtle behavior: a regular function containing `@dbg` that gets called from both comptime and runtime code prints in both phases. This is a consequence of phase-consistent execution semantics, not a bug, but it may surprise users the first time.
- Runtime variadic `@dbg` requires a small codegen change and one new runtime function (`__gruel_dbg_space`) — minor but non-zero complexity.
- The sema interpreter now unconditionally prints comptime `@dbg` output (unless suppressed), which changes compiler stderr behavior for any program using comptime `@dbg`. Existing spec/fuzz tests that call comptime `@dbg` without `--capture-comptime-dbg` will start emitting stderr content; they'll need the flag or test updates.
- Output ordering interleaves with any other stderr the compiler emits during sema (e.g. warnings emitted mid-analysis). In practice sema runs single-threaded per module and most other diagnostics are emitted at pass boundaries, so interleaving is not a meaningful concern.

## Open Questions

- **Prefix format.** `comptime dbg:` vs `comptime log:` vs something else? `@compileLog` uses `comptime log:` today, but "log" is no longer the name of anything after this change. Proposal: `comptime dbg:`. Consistent with the intrinsic name.
- **Runtime variadic separator policy.** Space-join matches `@compileLog` precedent. Alternative: newline-join at runtime ("print each arg on its own line") for ergonomics when printing large values. Proposal: space-join for symmetry with comptime; users can call `@dbg` multiple times for per-line output.
- **Should the warning fire for every call or once per `fn`?** `@compileLog` fires once per call site. Keep that.

## Future Work

- Once `comptime_meta` stabilizes more broadly (ADR-0042 completion), reconsider whether `@dbg` at comptime should require any gate at all. Current proposal: no gate, since `@dbg` itself is stable and phase-dependent routing is a natural extension.
- A structured `@trace(event_name, fields...)` intrinsic for higher-fidelity comptime debugging is a separate, larger design and is explicitly out of scope here.

## References

- [ADR-0040: Comptime Expansion](0040-comptime-expansion.md) — introduced the comptime `@dbg` buffer
- [ADR-0042: Comptime Metaprogramming](0042-comptime-metaprogramming.md) — introduced `@compileLog`
- Spec §4.13 (intrinsics), §4.14 (compile-time expressions)
- `crates/gruel-air/src/sema/analysis.rs:3438` (runtime `@dbg`), `:7431` (comptime `@dbg`), `:7467` (`@compileLog`)
