#!/usr/bin/env bash
set -euo pipefail

# Quick test script for fast development iteration
# Runs only unit tests (no spec tests, no UI tests)
#
# Use this for:
# - Fast feedback during development (~2-5 seconds)
# - Iterating on code changes before full verification
#
# Before committing, run ./test.sh for full verification.

cd "$(dirname "$0")"

echo "Running unit tests (quick mode)..."
cargo test --workspace --exclude gruel-runtime

echo ""
echo "Unit tests passed! Run ./test.sh for full verification before committing."
