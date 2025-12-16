#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the rue compiler

cd "$(dirname "$0")"

# Run unit tests for all crates
echo "Running unit tests..."
./buck2 test \
    //crates/rue-span:rue-span-test \
    //crates/rue-intern:rue-intern-test \
    //crates/rue-lexer:rue-lexer-test \
    //crates/rue-parser:rue-parser-test \
    //crates/rue-rir:rue-rir-test \
    //crates/rue-air:rue-air-test \
    //crates/rue-codegen:rue-codegen-test \
    //crates/rue-elf:rue-elf-test \
    //crates/rue-compiler:rue-compiler-test

# Get the path to the rue binary (this also builds it if needed)
RUE_BINARY="$(./buck2 build //crates/rue:rue --show-output | tail -1 | awk '{print $2}')"

# Run spec tests (buck2 run will build rue-spec if needed)
echo "Running spec tests..."
RUE_BINARY="$RUE_BINARY" \
RUE_SPEC_CASES="crates/rue-spec/cases" \
./buck2 run //crates/rue-spec:rue-spec -- "$@"
