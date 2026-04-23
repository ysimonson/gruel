#!/usr/bin/env bash
set -euo pipefail

# Build the Gruel website
# Usage: ./build.sh [serve|deploy]
#   (no args) - local build with root-relative URLs
#   serve     - dev server at http://127.0.0.1:1111
#   deploy    - production build with absolute URLs (https://gruel.yusufsimonson.com)

cd "$(dirname "$0")/.."
ROOT="$PWD"

# Platforms to generate charts for
PLATFORMS=("x86-64-linux" "aarch64-linux" "aarch64-macos")

# Copy spec content into website/content/spec
# We use a copy rather than a symlink for Windows compatibility (symlinks
# require elevated privileges on Windows). The spec source lives in
# docs/spec/src/ to keep it near the compiler code.
echo "Copying spec content..."
rm -rf website/content/spec
cp -r docs/spec/src website/content/spec

# Rewrite internal links: spec files use @/XX-... but once copied to
# website/content/spec/, the links need to be @/spec/XX-...
echo "Rewriting spec internal links..."
if [[ "$OSTYPE" == "darwin"* ]]; then
    find website/content/spec -name "*.md" -exec sed -i '' 's|@/\([0-9]\)|@/spec/\1|g' {} \;
else
    find website/content/spec -name "*.md" -exec sed -i 's|@/\([0-9]\)|@/spec/\1|g' {} \;
fi

# Copy the generated intrinsics reference into the website. It's generated
# from the gruel-intrinsics registry (see ADR-0050) and committed under
# docs/generated/; `make check-intrinsic-docs` enforces it stays in sync.
echo "Copying intrinsics reference..."
mkdir -p website/content/learn/references
INTRINSICS_DST=website/content/learn/references/intrinsics.md
{
    cat <<'EOF'
+++
title = "Intrinsics"
weight = 1
template = "learn/page.html"
+++

EOF
    # Skip the auto-generated HTML comment and rewrite the ADR link to point
    # at the ADR page we render under references/adrs/.
    tail -n +2 docs/generated/intrinsics-reference.md \
        | sed -e 's|(\.\./designs/\([0-9][0-9][0-9][0-9]\)-\([^)]*\)\.md)|(@/learn/references/adrs/\1-\2.md)|g'
} > "$INTRINSICS_DST"

# Copy ADRs. Each ADR becomes a page under references/adrs/. The section
# index is authored in-tree (see content/learn/references/adrs/_index.md);
# we just copy the individual ADR files.
echo "Copying ADRs..."
mkdir -p website/content/learn/references/adrs
# Clear any previously-copied ADR pages (keep _index.md).
find website/content/learn/references/adrs -maxdepth 1 -name '*.md' \
    ! -name '_index.md' -delete
for adr in docs/designs/[0-9][0-9][0-9][0-9]-*.md; do
    [ -f "$adr" ] || continue
    base=$(basename "$adr" .md)
    num=${base%%-*}
    # Skip the ADR template.
    [ "$num" = "0000" ] && continue
    # Pull title from the YAML frontmatter's `title:` field; fall back to the
    # first H1 or the filename.
    title=$(awk '/^---$/{n++; next} n==1 && /^title:/{sub(/^title:[[:space:]]*/, ""); print; exit}' "$adr")
    if [ -z "$title" ]; then
        title=$(grep -m1 '^# ' "$adr" | sed -e 's/^# *//' -e 's/^ADR-[0-9]*: *//')
    fi
    if [ -z "$title" ]; then
        title="$base"
    fi
    # Escape double quotes for TOML.
    title_esc=${title//\"/\\\"}
    dst="website/content/learn/references/adrs/${base}.md"
    {
        printf '+++\ntitle = "ADR-%s: %s"\nweight = %s\ntemplate = "learn/page.html"\n+++\n\n' \
            "$num" "$title_esc" "$((10#$num))"
        # Strip the YAML frontmatter block; the remaining markdown renders as
        # the page body.
        awk 'BEGIN{n=0} /^---$/ && n<2 {n++; next} n>=2' "$adr"
    } > "$dst"
done

# Generate benchmark charts for each platform
echo "Generating benchmark charts..."
BENCHMARKS_DIR="$ROOT/website/static/benchmarks"
mkdir -p "$BENCHMARKS_DIR/platforms"
mkdir -p "$BENCHMARKS_DIR/comparison"

# Track which platforms have data for the root metadata.json
PLATFORMS_WITH_DATA=()

for platform in "${PLATFORMS[@]}"; do
    history_file="$BENCHMARKS_DIR/history-${platform}.json"
    platform_dir="$BENCHMARKS_DIR/platforms/${platform}"

    if [[ -f "$history_file" ]]; then
        echo "  Generating charts for ${platform}..."
        mkdir -p "$platform_dir"
        python3 "$ROOT/scripts/generate-charts.py" \
            "$history_file" \
            "$platform_dir" \
            --platform "$platform"
        PLATFORMS_WITH_DATA+=("$platform")
    else
        echo "  No history file for ${platform} (skipping)"
    fi
done

# Generate comparison charts if we have multiple platforms
if [[ ${#PLATFORMS_WITH_DATA[@]} -gt 0 ]]; then
    echo "  Generating comparison charts..."
    history_files=()
    for platform in "${PLATFORMS_WITH_DATA[@]}"; do
        history_files+=("$BENCHMARKS_DIR/history-${platform}.json")
    done
    python3 "$ROOT/scripts/generate-charts.py" \
        --comparison \
        "$BENCHMARKS_DIR/comparison" \
        "${history_files[@]}"
fi

# Generate root metadata.json listing all platforms
echo "  Generating root metadata.json..."
# Build a JSON array of platform IDs to pass to Python
PLATFORMS_JSON="["
first=true
for platform in "${PLATFORMS_WITH_DATA[@]+"${PLATFORMS_WITH_DATA[@]}"}"; do
    if [ "$first" = true ]; then
        first=false
    else
        PLATFORMS_JSON+=","
    fi
    PLATFORMS_JSON+="\"$platform\""
done
PLATFORMS_JSON+="]"

python3 -c "
import json
import sys
from pathlib import Path

benchmarks_dir = Path('$BENCHMARKS_DIR')
platforms_with_data = json.loads('$PLATFORMS_JSON')
platforms = []

for platform in platforms_with_data:
    platform = platform.strip()
    if not platform:
        continue
    metadata_file = benchmarks_dir / 'platforms' / platform / 'metadata.json'
    if metadata_file.exists():
        with open(metadata_file) as f:
            data = json.load(f)
        platforms.append({
            'id': platform,
            'name': data.get('platform_name', platform),
            'has_data': True,
            'run_count': data.get('run_count', 0),
            'latest_commit': data.get('latest_commit')
        })

# Add platforms without data
all_platforms = ['x86-64-linux', 'aarch64-linux', 'aarch64-macos']
platform_ids = [p['id'] for p in platforms]
for platform in all_platforms:
    if platform not in platform_ids:
        platforms.append({
            'id': platform,
            'name': {'x86-64-linux': 'Linux x86-64', 'aarch64-linux': 'Linux ARM64', 'aarch64-macos': 'macOS ARM64'}.get(platform, platform),
            'has_data': False
        })

# Sort by platform id for consistency
platforms.sort(key=lambda p: p['id'])

metadata = {
    'platforms': platforms,
    'default_platform': platforms[0]['id'] if platforms else None
}

# Merge opt_levels and benchmarks from comparison metadata if available
comparison_meta = benchmarks_dir / 'comparison' / 'metadata.json'
if comparison_meta.exists():
    with open(comparison_meta) as f:
        comp_data = json.load(f)
    if 'opt_levels' in comp_data:
        metadata['opt_levels'] = comp_data['opt_levels']
    if 'benchmarks' in comp_data:
        metadata['benchmarks'] = comp_data['benchmarks']

with open(benchmarks_dir / 'metadata.json', 'w') as f:
    json.dump(metadata, f, indent=2)
print(f'  Generated {benchmarks_dir}/metadata.json with {len([p for p in platforms if p[\"has_data\"]])} platforms with data')
"

# Backwards compatibility: Generate charts from legacy history.json if it exists
# and no per-platform history exists yet
if [[ -s "$BENCHMARKS_DIR/history.json" && ${#PLATFORMS_WITH_DATA[@]} -eq 0 ]]; then
    echo "  Generating legacy charts from history.json..."
    python3 "$ROOT/scripts/generate-charts.py" \
        "$BENCHMARKS_DIR/history.json" \
        "$BENCHMARKS_DIR/"
fi

# Build Tailwind CSS
echo "Building Tailwind CSS..."
cd website
"$ROOT/tailwindcss" -i css/input.css -o static/style.css --minify

# Build or serve
if [[ "${1:-}" == "serve" ]]; then
    echo "Starting dev server at http://127.0.0.1:1111"
    echo "Note: CSS changes require rebuilding Tailwind manually"
    "$ROOT/zola" serve
elif [[ "${1:-}" == "deploy" ]]; then
    echo "Building website for production..."
    "$ROOT/zola" build --base-url https://gruel.yusufsimonson.com
    echo "Done! Output in website/public/"
else
    echo "Building website..."
    "$ROOT/zola" build
    echo "Done! Output in website/public/"
fi
