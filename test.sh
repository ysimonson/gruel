#!/usr/bin/env bash
set -euo pipefail

# Run all tests for the gruel compiler

cd "$(dirname "$0")"

# Run unit tests for all crates (excluding gruel-runtime: no_std, no test harness)
echo "Running unit tests..."
cargo test --workspace --exclude gruel-runtime

# Build the gruel binary
GRUEL_BINARY="$(cargo build -p gruel --message-format=json 2>/dev/null \
    | grep '"executable"' \
    | tail -1 \
    | sed 's/.*"executable":"\([^"]*\)".*/\1/')"

# Run spec tests
echo "Running spec tests..."
GRUEL_BINARY="$GRUEL_BINARY" \
GRUEL_SPEC_CASES="crates/gruel-spec/cases" \
cargo run -p gruel-spec -- --quiet "$@"

# Run traceability check (fails if coverage < 100% or orphan references exist)
echo "Running spec traceability check..."
GRUEL_SPEC_DIR="docs/spec/src" \
GRUEL_SPEC_CASES="crates/gruel-spec/cases" \
cargo run -p gruel-spec -- --traceability

# Run UI tests
echo "Running UI tests..."
GRUEL_BINARY="$GRUEL_BINARY" \
GRUEL_UI_CASES="crates/gruel-ui-tests/cases" \
cargo run -p gruel-ui-tests -- --quiet "$@"
