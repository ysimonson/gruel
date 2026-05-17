#!/usr/bin/env bash
# ADR-0089 Phase 6: regenerate docs/generated/stdlib/ and
# docs/generated/prelude/ from std/*.gruel and prelude/*.gruel.
#
# Usage: scripts/gen-stdlib-docs.sh [OUT_DIR]
#
# OUT_DIR defaults to docs/generated/. Pass a custom dir (target/...)
# from the check target.

set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

OUT_BASE="${1:-docs/generated}"
STDLIB_OUT="$OUT_BASE/stdlib"
PRELUDE_OUT="$OUT_BASE/prelude"

GRUEL=target/debug/gruel
if [[ ! -x "$GRUEL" ]]; then
    cargo build -q -p gruel
fi

# We pass `std/math.gruel` and the doc-bearing prelude files
# individually. Each becomes its own subdirectory under the output, so
# `std/math.gruel` → `<OUT>/stdlib/math/{index,fn.*.md}`. Files without
# any docs are still rendered (empty pages) so the user can see what's
# in the surface area.

mkdir -p "$STDLIB_OUT" "$PRELUDE_OUT"

# Wipe previous output so removed items don't linger in the tree.
rm -rf "$STDLIB_OUT" "$PRELUDE_OUT"
mkdir -p "$STDLIB_OUT" "$PRELUDE_OUT"

# stdlib: just `std/math.gruel` for now. Adding more std modules is a
# matter of listing them here.
"$GRUEL" --preview docs --doc markdown --doc-output-dir "$STDLIB_OUT" \
    std/math.gruel

# prelude: every `prelude/*.gruel` that isn't the root or auto-private.
PRELUDE_SOURCES=(
    prelude/cmp.gruel
    prelude/option.gruel
    prelude/result.gruel
    prelude/string.gruel
    prelude/vec.gruel
)

"$GRUEL" --preview docs --doc markdown --doc-output-dir "$PRELUDE_OUT" \
    "${PRELUDE_SOURCES[@]}"

echo "wrote stdlib docs to $STDLIB_OUT"
echo "wrote prelude docs to $PRELUDE_OUT"
