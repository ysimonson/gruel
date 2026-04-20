---
id: 0043
title: Extended Benchmark Infrastructure — Comptime, Opt Levels, and Runtime
status: proposal
tags: [tooling, performance, benchmarks, website]
feature-flag: null
created: 2026-04-20
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0043: Extended Benchmark Infrastructure — Comptime, Opt Levels, and Runtime

## Status

Proposal

## Summary

Extend the benchmarking infrastructure to cover three dimensions that are currently unmeasured: (1) comptime evaluation performance, (2) LLVM compilation + runtime performance at `-O0`, and (3) LLVM compilation + runtime performance at `-O3`. Add new benchmark programs that stress comptime execution, add a tracing span around the comptime interpreter, run each benchmark at multiple optimization levels, measure the runtime of compiled binaries, and render all of this on the website's performance dashboard.

## Context

### What the current benchmarks measure

The existing benchmark infrastructure (ADR-0019, ADR-0031) measures **compilation time** for 7 stress programs. For each program, `--benchmark-json` reports per-pass timing (lexer, parser, astgen, sema, cfg_construction, codegen, linker) and aggregate metrics (total time, peak memory, binary size). All compilation happens at the default optimization level (`-O0`).

### What's missing

**1. Comptime evaluation performance.** None of the 7 stress programs use `comptime` expressions. The comptime interpreter runs inside the `sema` pass and has no dedicated tracing span, so even if a program used comptime heavily, the interpreter's cost would be hidden inside the sema number. After ADR-0040 expanded the comptime interpreter to support nearly all pure language constructs, comptime is a substantial subsystem that deserves visibility.

**2. Optimization level coverage.** The compiler supports `-O0` through `-O3` via LLVM, but benchmarks only run at `-O0`. The LLVM mid-end pipeline (`default<O3>`) is a significant cost center at higher optimization levels. We need to track:
- How long LLVM optimization takes at each level
- Whether codegen or linking time changes across levels
- Binary size differences across optimization levels

**3. Runtime performance of compiled binaries.** The benchmarks measure how fast the compiler runs but not how fast the *compiled programs* run. This matters because:
- Optimization level changes should visibly improve runtime performance
- Runtime regressions from codegen changes would go unnoticed today
- Users care about the quality of generated code, not just compilation speed

### Design constraints

- **CI budget**: More configurations = more wall-clock time. Each new optimization-level run multiplies the benchmark matrix.
- **Determinism**: Runtime benchmarks need programs that do meaningful work and return deterministic results (not I/O-bound or allocation-heavy).
- **History compatibility**: The existing JSON schema, `append-benchmark.py`, and `generate-charts.py` need to be extended, not replaced.

## Decision

### Part 1: Comptime tracing span

Add a dedicated `info_span!("comptime")` inside the comptime interpreter entry point (`evaluate_comptime_block` in `gruel-air/src/sema/analyze_ops.rs`). This span nests under the existing `sema` span and records time spent specifically on comptime evaluation.

The `--benchmark-json` output will naturally include "comptime" as a new pass name. The chart generator already handles arbitrary pass names — no change needed in `generate-charts.py` for this to render.

Add "comptime" to `PASS_ORDER` and `PASS_COLORS` in `generate-charts.py` so it gets a consistent color and stacking position. Place it between "sema" and "cfg" in the stack, since comptime runs during sema but is conceptually its own phase.

### Part 2: Comptime stress benchmark

Add a new benchmark program `benchmarks/stress/comptime_heavy.gruel` and register it in `manifest.toml`.

The program should exercise:
- **Comptime arithmetic**: Functions with heavy `comptime { ... }` blocks doing loops and arithmetic
- **Comptime function calls**: Functions called at compile time that recurse or iterate
- **`comptime_unroll` for-loops**: Multiple `comptime_unroll` loops that expand into many iterations
- **Comptime struct/array construction**: Building composite values at compile time
- **Comptime pattern matching**: Matching on enum variants at compile time

Target ~1-2 seconds of compile time (same as other stress tests) with the majority spent in the comptime interpreter. The program must also *run* successfully (for Part 4's runtime measurement).

### Part 3: Multi-opt-level benchmarking

Extend `bench.sh` to run each benchmark at multiple optimization levels. Currently `bench.sh` compiles each program once (at `-O0` default). Change it to compile at `-O0` and `-O3`.

**Manifest extension:**
```toml
# Global benchmark config
[config]
opt_levels = ["O0", "O3"]  # Levels to benchmark

[[benchmark]]
name = "many_functions"
path = "stress/many_functions.gruel"
description = "1000 functions to stress function handling"
```

If the `[config]` section or `opt_levels` key is absent, default to `["O0"]` for backwards compatibility.

**Result naming:** Each benchmark result is tagged with its optimization level. The benchmark name becomes `"{name}@{opt_level}"` in the JSON output (e.g., `"many_functions@O0"`, `"many_functions@O3"`).

**bench.sh changes:**
1. Parse `opt_levels` from the `[config]` section of `manifest.toml`
2. For each benchmark × opt level combination, run `$GRUEL_BIN --benchmark-json -{opt_level} "$full_path" "$output_binary"`
3. Tag the result with the opt level
4. Keep the compiled binary for runtime measurement (Part 4)

**JSON schema:** The existing per-benchmark object gains an `"opt_level"` field:
```json
{
  "name": "many_functions@O0",
  "opt_level": "O0",
  "iterations": 5,
  "mean_ms": 14.2,
  ...
}
```

### Part 4: Runtime benchmarking

After compiling each benchmark program, run the resulting binary and measure its execution time. This captures the quality of generated code.

**bench.sh changes:**
1. After compiling, run the binary with `/usr/bin/time` to capture wall-clock time and peak memory
2. Run multiple iterations (same count as compilation iterations)
3. Record mean and stddev of execution time
4. Store in the benchmark result as `"runtime_ms"` and `"runtime_std_ms"`

**Program requirements for runtime benchmarks:**
- Programs must do deterministic computation (no I/O, no randomness)
- Programs must take long enough to measure reliably (at least ~10ms runtime)
- Programs must return a deterministic exit code for verification

Some existing benchmarks (e.g., `many_functions` where most functions are never called from `main`) may produce trivially fast binaries. That's fine — the runtime will just be near-zero. The interesting runtime data comes from programs that actually compute (e.g., `arithmetic_heavy`, `control_flow`, and the new `comptime_heavy` which should produce non-trivial runtime code from unrolled loops).

**JSON extension:**
```json
{
  "name": "arithmetic_heavy@O3",
  "opt_level": "O3",
  "mean_ms": 22.5,
  "runtime_ms": 0.8,
  "runtime_std_ms": 0.02,
  ...
}
```

### Part 5: Website visualization

Extend the performance dashboard to display the new data:

**1. Opt-level selector.** Add a dropdown next to the existing "Program" dropdown that lets the user filter by optimization level. Default to "O0" to match current behavior.

**2. Compilation time by opt level.** A new chart type showing compilation time for the same program at different optimization levels side by side (grouped bar chart or overlaid lines). This makes it easy to see the LLVM optimization overhead.

**3. Runtime performance chart.** A new time-series chart in its own section showing runtime of compiled binaries over commits, similar to the existing compilation time trend. Separate lines per opt level.

**4. Binary size by opt level.** The existing binary size chart already works per-program; extend it to show O0 vs O3 side by side.

**5. Comptime breakdown.** No special chart needed — the comptime pass will appear in the existing stacked pass breakdown as its own color band.

**Chart generator changes (`generate-charts.py`):**
- Add `"comptime"` to `PASS_COLORS` and `PASS_ORDER`
- Add runtime chart generation function
- Modify the per-program timeline to support opt-level grouping
- Add binary size comparison across opt levels
- Generate new SVG files: `runtime.svg`, `binary_size_by_opt.svg`
- Update `metadata.json` to include runtime stats and opt-level info

**Website template changes (`performance.html`):**
- Add opt-level selector dropdown
- Add "Runtime Performance" section with chart container
- Add "Binary Size by Optimization Level" section
- Update methodology text to describe new measurements
- Update benchmark suite list to include `comptime_heavy`

### Part 6: CI integration

The existing CI workflow (`benchmarks.yml`) runs `bench.sh`. Since `bench.sh` will now run multiple opt levels, the CI time increases roughly 2× (O0 + O3). To keep this manageable:

- Keep the 5-iteration count (reliable enough for trends)
- The parallelism across platforms remains unchanged
- History files on the perf branch naturally grow to include the new data points

No changes to the workflow YAML itself — the expansion is entirely within `bench.sh` and `manifest.toml`.

## Implementation Phases

- [x] **Phase 1: Comptime tracing span** — Add `info_span!("comptime")` around `evaluate_comptime_block` in `gruel-air/src/sema/analyze_ops.rs`. Verify it appears in `--time-passes` and `--benchmark-json` output. Add "comptime" to chart generator's `PASS_ORDER`/`PASS_COLORS`.

- [x] **Phase 2: Comptime stress benchmark** — Write `benchmarks/stress/comptime_heavy.gruel` exercising comptime arithmetic, function calls, `comptime_unroll`, struct/array construction, and pattern matching. Register in `manifest.toml`. Verify the program compiles and runs, and that the comptime span shows significant time.

- [x] **Phase 3: Multi-opt-level benchmarking** — Extend `bench.sh` to parse `opt_levels` from `manifest.toml` `[config]` section. Run each benchmark at each opt level. Tag results with `"opt_level"` field and `"@{level}"` suffix in benchmark name. Update `manifest.toml` with `opt_levels = ["O0", "O3"]`. Update `append-benchmark.py` if needed to handle the new fields.

- [x] **Phase 4: Runtime benchmarking** — After compiling each benchmark in `bench.sh`, run the binary with `/usr/bin/time` for multiple iterations. Compute mean/stddev of wall-clock execution time. Add `"runtime_ms"` and `"runtime_std_ms"` to the per-benchmark JSON output.

- [ ] **Phase 5: Website visualization** — Update `generate-charts.py` to generate runtime charts and opt-level comparison charts. Update `performance.html` to add opt-level dropdown, runtime section, and binary size comparison section. Update `metadata.json` generation to include runtime stats.

- [ ] **Phase 6: Polish and documentation** — Update `benchmarks/README.md` with new benchmark descriptions and opt-level configuration. Update the website's methodology section. Update CLAUDE.md if the benchmark workflow instructions change.

## Consequences

### Positive

- **Comptime visibility**: Comptime interpreter performance is tracked independently from sema, enabling focused optimization
- **Optimization level insight**: Can see how much compilation time LLVM optimization adds and whether it improves runtime
- **Runtime quality tracking**: Codegen regressions that produce slower binaries are now detectable
- **Richer dashboard**: The website gives a more complete picture of compiler performance

### Negative

- **~2× CI benchmark time**: Running at both O0 and O3 roughly doubles the benchmark wall clock per commit
- **More chart complexity**: Dashboard gains new sections and dropdowns; could be overwhelming if not laid out well
- **Larger history files**: Roughly double the data points per run stored in perf branch

### Neutral

- **No schema break**: Existing history data remains valid — new fields are additive
- **Comptime tracing span is zero-cost**: When `--time-passes`/`--benchmark-json` is not active, the span has no overhead

## Resolved Questions

1. **Should we also benchmark `-O1` and `-O2`?** Starting with just O0 and O3 captures the extremes. O1/O2 can be added later by editing `manifest.toml` if the data is valuable.

2. **Are all existing benchmarks worth running at O3?** Programs like `many_functions` (1000 trivial functions) may have negligible runtime. We could add a per-benchmark `skip_runtime = true` flag in the manifest, but it may not be worth the complexity — near-zero runtimes are still valid data.

3. **Should runtime be measured in the same iteration loop as compilation?** Running the binary immediately after compiling means the binary is hot in the disk cache. For consistency we should always do it this way, not mix.

## Future Work

- **Microbenchmarks for specific codegen patterns**: e.g., how efficiently the LLVM backend handles struct passing, array copies
- **Comptime step count tracking**: Report how many interpreter steps each comptime block takes (already tracked internally via `COMPTIME_MAX_STEPS`)
- **Regression alerting**: Automatically flag commits where runtime performance degrades significantly
- **Comparison with other compilers**: Track compile time and runtime performance against equivalent Rust/Zig/C programs

## References

- [ADR-0019: Compiler Performance Dashboard](0019-performance-dashboard.md) — Original benchmark infrastructure
- [ADR-0031: Robust Performance Testing Infrastructure](0031-robust-performance-testing.md) — Parallel execution and batching
- [ADR-0033: LLVM Backend and Comptime Interpreter](0033-llvm-backend-and-comptime-interpreter.md) — LLVM backend with opt levels
- [ADR-0040: Comptime Interpreter Expansion](0040-comptime-expansion.md) — Latest comptime interpreter capabilities
