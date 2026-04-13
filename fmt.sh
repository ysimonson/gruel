#!/bin/bash
set -euo pipefail

# Format or check Rust code formatting
#
# Usage:
#   ./fmt.sh        # Format all Rust files
#   ./fmt.sh check  # Check formatting (for CI)

MODE="${1:-format}"

if [ "$MODE" = "check" ]; then
    echo "Checking Rust formatting..."
    cargo fmt --all -- --check
    echo "All files formatted correctly!"
else
    echo "Formatting Rust files..."
    cargo fmt --all
    echo "Done!"
fi
