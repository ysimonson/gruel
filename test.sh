#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the gruel compiler

cd "$(dirname "$0")"

# Run unit tests for all crates
echo "Running unit tests..."
./buck2 test \
    //crates/gruel-span:gruel-span-test \
    //crates/gruel-error:gruel-error-test \
    //crates/gruel-target:gruel-target-test \
    //crates/gruel-lexer:gruel-lexer-test \
    //crates/gruel-parser:gruel-parser-test \
    //crates/gruel-rir:gruel-rir-test \
    //crates/gruel-cfg:gruel-cfg-test \
    //crates/gruel-air:gruel-air-test \
    //crates/gruel-codegen:gruel-codegen-test \
    //crates/gruel-linker:gruel-linker-test \
    //crates/gruel-compiler:gruel-compiler-test

# Get the path to the gruel binary (this also builds it if needed)
GRUEL_BINARY="$(./buck2 build //crates/gruel:gruel --show-output | tail -1 | awk '{print $2}')"

# Run spec tests (buck2 run will build gruel-spec if needed)
echo "Running spec tests..."
GRUEL_BINARY="$GRUEL_BINARY" \
GRUEL_SPEC_CASES="crates/gruel-spec/cases" \
./buck2 run //crates/gruel-spec:gruel-spec -- --quiet "$@"

# Run traceability check (fails if coverage < 100% or orphan references exist)
echo "Running spec traceability check..."
GRUEL_SPEC_DIR="docs/spec/src" \
GRUEL_SPEC_CASES="crates/gruel-spec/cases" \
./buck2 run //crates/gruel-spec:gruel-spec -- --traceability

# Run UI tests (compiler-specific tests like warnings)
echo "Running UI tests..."
GRUEL_BINARY="$GRUEL_BINARY" \
GRUEL_UI_CASES="crates/gruel-ui-tests/cases" \
./buck2 run //crates/gruel-ui-tests:gruel-ui-tests -- --quiet "$@"
