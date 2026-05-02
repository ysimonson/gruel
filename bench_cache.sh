#!/bin/bash
# ADR-0074 Phase 7: cold-vs-hot cache benchmarks.
#
# Runs each benchmark program in three modes:
#
#   1. cold-no-cache:   --preview off, fresh build (baseline)
#   2. cold-cache-on:   --preview on, empty cache (measures cache-write overhead)
#   3. warm-cache-on:   --preview on, populated cache (measures hit-rate win)
#
# Reports wall-clock times for each. Designed to plug into the perf
# dashboard alongside the existing bench.sh once results stabilize over
# several iterations (per ADR-0074 Phase 7's "must run for several
# iterations before flipping defaults" requirement).
#
# Usage:
#   ./bench_cache.sh              # Run all scenarios, default 3 iterations
#   ./bench_cache.sh -i 10        # 10 iterations per scenario
#   ./bench_cache.sh --json       # Emit JSON for downstream tooling
#
# This script is intentionally separate from bench.sh: that script's
# manifest-driven loop doesn't model the warm/cold distinction the
# cache needs, and refactoring it to do so is its own change. Once the
# format here proves itself, integration into bench.sh is mechanical.

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH_DIR="$SCRIPT_DIR/benchmarks/cache"
BUILD_MODE="release"
ITERATIONS=3
EMIT_JSON=false

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

ADR-0074 Phase 7 cold-vs-hot cache benchmarks.

Options:
  -i, --iterations N    Iterations per scenario (default: 3)
  --debug               Use debug build of gruel (default: release)
  --json                Emit machine-readable JSON instead of human-readable text
  -h, --help            Show this help

Each program in benchmarks/cache/ is run in three scenarios:
  cold-no-cache    Baseline, --preview off, fresh build
  cold-cache-on    --preview on, empty cache (measures write overhead)
  warm-cache-on    --preview on, populated cache (measures hit win)
EOF
}

while [[ $# -gt 0 ]]; do
    case $1 in
    -i | --iterations)
        ITERATIONS="$2"
        shift 2
        ;;
    --debug) BUILD_MODE="debug"; shift ;;
    --json) EMIT_JSON=true; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage; exit 1 ;;
    esac
done

# Build the compiler.
if [[ "$BUILD_MODE" == "release" ]]; then
    cargo build -p gruel --release >/dev/null 2>&1
    GRUEL="$SCRIPT_DIR/target/release/gruel"
else
    cargo build -p gruel >/dev/null 2>&1
    GRUEL="$SCRIPT_DIR/target/debug/gruel"
fi

if [[ ! -x "$GRUEL" ]]; then
    echo "could not find gruel binary at $GRUEL" >&2
    exit 1
fi

# Time a single command, return seconds (float). Uses /usr/bin/time -p
# for portable parsing. Writes time output to a temp file so we don't
# tangle with the inner command's stderr.
time_one() {
    local tmp
    tmp=$(mktemp)
    /usr/bin/time -p "$@" >/dev/null 2>"$tmp"
    awk '/^real /{print $2}' "$tmp"
    rm -f "$tmp"
}

# Run N iterations, take the median.
median() {
    local arr=( "$@" )
    local sorted
    IFS=$'\n' sorted=( $(printf "%s\n" "${arr[@]}" | sort -n) )
    unset IFS
    echo "${sorted[$(( ${#sorted[@]} / 2 ))]}"
}

run_cold_no_cache() {
    local prog="$1" cache_dir="$2"
    local times=()
    for ((i = 0; i < ITERATIONS; i++)); do
        local t
        t=$(time_one "$GRUEL" "$prog" "$cache_dir/.benchout")
        times+=( "$t" )
    done
    median "${times[@]}"
}

results=()
for prog in "$BENCH_DIR"/*.gruel; do
    name=$(basename "$prog" .gruel)
    cache_dir=$(mktemp -d)

    # Warm up OS file cache + dynamic loader to remove first-invocation
    # bias. Without this, the first scenario's first iteration is
    # systematically slower than subsequent ones.
    "$GRUEL" "$prog" "$cache_dir/.warmup" >/dev/null 2>&1 || true

    # Scenario 1: cold no cache.
    rm -rf "$cache_dir"; mkdir -p "$cache_dir"
    cold_no_cache=$(run_cold_no_cache "$prog" "$cache_dir")

    # Scenario 2: cold cache on (cache empty before this iteration).
    # We rm -rf between iterations so each is truly cold.
    rm -rf "$cache_dir"; mkdir -p "$cache_dir"
    cold_cache_on=$(
        ts=()
        for ((i = 0; i < ITERATIONS; i++)); do
            rm -rf "$cache_dir"; mkdir -p "$cache_dir"
            t=$(time_one "$GRUEL" --preview incremental_compilation \
                --cache-dir "$cache_dir" "$prog" "$cache_dir/.out")
            ts+=( "$t" )
        done
        median "${ts[@]}"
    )

    # Scenario 3: warm cache on. First populate, then time the next runs.
    rm -rf "$cache_dir"; mkdir -p "$cache_dir"
    "$GRUEL" --preview incremental_compilation --cache-dir "$cache_dir" \
        "$prog" "$cache_dir/.out" >/dev/null 2>&1
    warm_cache_on=$(
        ts=()
        for ((i = 0; i < ITERATIONS; i++)); do
            t=$(time_one "$GRUEL" --preview incremental_compilation \
                --cache-dir "$cache_dir" "$prog" "$cache_dir/.out")
            ts+=( "$t" )
        done
        median "${ts[@]}"
    )

    rm -rf "$cache_dir"

    if $EMIT_JSON; then
        results+=( "{\"name\":\"$name\",\"cold_no_cache\":$cold_no_cache,\"cold_cache_on\":$cold_cache_on,\"warm_cache_on\":$warm_cache_on}" )
    else
        printf "%-32s  cold-nc=%5.3fs  cold-c=%5.3fs  warm-c=%5.3fs  speedup=%5.2fx\n" \
            "$name" "$cold_no_cache" "$cold_cache_on" "$warm_cache_on" \
            "$(echo "$cold_no_cache / $warm_cache_on" | bc -l)"
    fi
done

if $EMIT_JSON; then
    printf '{"iterations":%d,"results":[' "$ITERATIONS"
    printf '%s' "$(IFS=,; echo "${results[*]}")"
    printf ']}\n'
fi
