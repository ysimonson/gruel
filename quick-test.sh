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

echo ""
echo "Unit tests passed! Run ./test.sh for full verification before committing."
