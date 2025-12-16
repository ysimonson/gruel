#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the rue compiler

cd "$(dirname "$0")"

# Get the path to the rue binary (this also builds it if needed)
RUE_BINARY="$(./buck2 build //crates/rue:rue --show-output | tail -1 | awk '{print $2}')"

# Run spec tests (buck2 run will build rue-spec if needed)
RUE_BINARY="$RUE_BINARY" \
RUE_SPEC_CASES="crates/rue-spec/cases" \
./buck2 run //crates/rue-spec:rue-spec -- "$@"
