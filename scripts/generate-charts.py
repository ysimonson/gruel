#!/usr/bin/env python3
"""
Generate SVG charts from benchmark history for the performance dashboard.

This script reads benchmark history from JSON and generates SVG charts:
1. timeline.svg - Time-series chart showing total compilation time over commits
2. breakdown.svg - Stacked bar chart showing time per compiler pass
3. memory.svg - Memory usage over time
4. binary_size.svg - Binary size over time

Usage:
    # Generate charts for a single platform
    ./generate-charts.py <history.json> <output-dir> [--platform <name>]

    # Generate comparison charts from multiple platform histories
    ./generate-charts.py --comparison <output-dir> <history1.json> <history2.json> ...

Examples:
    # Single platform (legacy mode)
    ./generate-charts.py website/static/benchmarks/history.json website/static/benchmarks/

    # Per-platform generation
    ./generate-charts.py history-x86-64-linux.json platforms/x86-64-linux/ --platform x86-64-linux

    # Cross-platform comparison
    ./generate-charts.py --comparison comparison/ history-*.json
"""

import argparse
import json
import sys
from pathlib import Path
from typing import Optional

# Chart dimensions
TIMELINE_WIDTH = 800
TIMELINE_HEIGHT = 300
BREAKDOWN_WIDTH = 800
BREAKDOWN_HEIGHT = 350
MEMORY_WIDTH = 800
MEMORY_HEIGHT = 250
BINARY_WIDTH = 800
BINARY_HEIGHT = 250
RUNTIME_WIDTH = 800
RUNTIME_HEIGHT = 350
COMPARISON_WIDTH = 900
COMPARISON_HEIGHT = 400

# Colors for passes (consistent with website theme)
PASS_COLORS = {
    "lexer": "#4f6ddb",     # accent blue
    "parser": "#7c9dff",    # lighter blue
    "astgen": "#3b82f6",    # sky blue
    "sema": "#06b6d4",      # cyan
    "comptime": "#8b5cf6",  # violet
    "cfg": "#10b981",       # emerald
    "codegen": "#f59e0b",   # amber
    "linker": "#ef4444",    # red
}

# Order of passes in the stack
PASS_ORDER = ["lexer", "parser", "astgen", "sema", "comptime", "cfg", "codegen", "linker"]

# Platform display names and colors
PLATFORM_INFO = {
    "x86-64-linux": {"name": "Linux x86-64", "color": "#4f6ddb"},
    "aarch64-linux": {"name": "Linux ARM64", "color": "#10b981"},
    "aarch64-macos": {"name": "macOS ARM64", "color": "#f59e0b"},
}


def load_history(path: Path) -> dict:
    """Load benchmark history from JSON file."""
    if not path.exists():
        return {"version": 1, "runs": []}

    with open(path, "r") as f:
        data = json.load(f)

    # Handle legacy format
    if isinstance(data, list):
        return {"version": 1, "runs": data}

    return data


def get_pass_times(run: dict) -> dict[str, float]:
    """Extract pass timing from a benchmark run."""
    # Look for pass timing data in the benchmarks
    for bench in run.get("benchmarks", []):
        if "passes" in bench:
            # New format with per-pass timing
            passes = bench["passes"]
            return {
                name: passes.get(name, {}).get("mean_ms", 0)
                for name in PASS_ORDER
            }
    return {}


def get_total_time(run: dict) -> float:
    """Get total compilation time (sum across all benchmarks) from a run."""
    total = 0
    for bench in run.get("benchmarks", []):
        if "mean_ms" in bench:
            total += bench["mean_ms"]
        elif "total_ms" in bench:
            t = bench["total_ms"]
            total += t.get("mean", 0) if isinstance(t, dict) else t
    return total


def get_peak_memory(run: dict) -> float:
    """Get peak memory usage (max across all benchmarks, in MB) from a run."""
    peak = 0
    for bench in run.get("benchmarks", []):
        if "peak_memory_bytes" in bench:
            mb = bench["peak_memory_bytes"] / (1024 * 1024)
            peak = max(peak, mb)
    return peak


def get_binary_size(run: dict) -> float:
    """Get total binary size (sum across all benchmarks, in KB) from a run."""
    total = 0
    for bench in run.get("benchmarks", []):
        if "binary_size_bytes" in bench:
            total += bench["binary_size_bytes"] / 1024
    return total


def format_bytes(size_bytes: float) -> str:
    """Format bytes into human-readable form."""
    if size_bytes >= 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f}MB"
    elif size_bytes >= 1024:
        return f"{size_bytes / 1024:.1f}KB"
    else:
        return f"{size_bytes:.0f}B"


def parse_benchmark_name(name: str) -> tuple[str, str]:
    """Parse a benchmark name into (base_name, opt_level).

    E.g., 'many_functions@O3' -> ('many_functions', 'O3')
    E.g., 'many_functions' -> ('many_functions', '')
    """
    if "@" in name:
        base, opt = name.rsplit("@", 1)
        return base, opt
    return name, ""


def get_opt_levels_from_runs(runs: list[dict]) -> list[str]:
    """Get sorted list of optimization levels present in benchmark runs."""
    levels = set()
    for run in runs:
        for bench in run.get("benchmarks", []):
            _, opt = parse_benchmark_name(bench.get("name", ""))
            if opt:
                levels.add(opt)
    return sorted(levels) if levels else ["O0"]


def filter_runs_by_opt_level(runs: list[dict], opt_level: str) -> list[dict]:
    """Return runs with benchmarks filtered to only those matching opt_level.

    Benchmarks with no opt level suffix are included when opt_level is 'O0' (legacy compat).
    """
    filtered = []
    for run in runs:
        new_run = dict(run)
        new_benchmarks = []
        for bench in run.get("benchmarks", []):
            _, opt = parse_benchmark_name(bench.get("name", ""))
            if opt == opt_level or (not opt and opt_level == "O0"):
                new_benchmarks.append(bench)
        if new_benchmarks:
            new_run["benchmarks"] = new_benchmarks
            filtered.append(new_run)
    return filtered


def _find_benchmark(run: dict, benchmark_name: str) -> Optional[dict]:
    """Find a benchmark entry by name, handling @opt suffix mismatches.

    After filter_runs_by_opt_level, benchmarks may lose their @opt suffix
    (legacy data never had it). This tries exact match first, then base name.
    """
    for bench in run.get("benchmarks", []):
        if bench.get("name") == benchmark_name:
            return bench
    # Try base name (strip @opt) for legacy data
    base, _ = parse_benchmark_name(benchmark_name)
    if base != benchmark_name:
        for bench in run.get("benchmarks", []):
            if bench.get("name") == base:
                return bench
    return None


def get_benchmark_memory(run: dict, benchmark_name: str) -> float:
    """Get peak memory (MB) for a specific benchmark from a run."""
    bench = _find_benchmark(run, benchmark_name)
    if bench and "peak_memory_bytes" in bench:
        return bench["peak_memory_bytes"] / (1024 * 1024)
    return 0


def get_benchmark_binary_size(run: dict, benchmark_name: str) -> float:
    """Get binary size (KB) for a specific benchmark from a run."""
    bench = _find_benchmark(run, benchmark_name)
    if bench and "binary_size_bytes" in bench:
        return bench["binary_size_bytes"] / 1024
    return 0


def get_benchmark_runtime(run: dict, benchmark_name: str) -> float:
    """Get runtime (in ms) for a specific benchmark from a run."""
    bench = _find_benchmark(run, benchmark_name)
    if bench:
        return bench.get("runtime_ms", 0)
    return 0


def get_benchmark_cold_ms(run: dict, benchmark_name: str) -> float:
    """Cold compile time for a benchmark (iter 1, page caches cold)."""
    bench = _find_benchmark(run, benchmark_name)
    if bench:
        return bench.get("cold_ms", 0) or 0
    return 0


def get_benchmark_hot_ms(run: dict, benchmark_name: str) -> float:
    """Hot compile time for a benchmark (mean of iters 2..N, caches warm)."""
    bench = _find_benchmark(run, benchmark_name)
    if bench:
        return bench.get("hot_ms", 0) or 0
    return 0


def get_total_cold_ms(run: dict) -> float:
    """Sum of cold compile times across all benchmarks in a run."""
    return sum((b.get("cold_ms") or 0) for b in run.get("benchmarks", []))


def get_total_hot_ms(run: dict) -> float:
    """Sum of hot compile times across all benchmarks in a run."""
    return sum((b.get("hot_ms") or 0) for b in run.get("benchmarks", []))


def calculate_delta(current: float, previous: float) -> tuple[float, str]:
    """Calculate delta and format as string with arrow indicator."""
    if previous == 0:
        return 0, ""
    delta = current - previous
    pct = (delta / previous) * 100
    if abs(pct) < 0.1:
        return pct, "→ 0%"
    arrow = "↑" if pct > 0 else "↓"
    return pct, f"{arrow} {abs(pct):.1f}%"


def escape_xml(s: str) -> str:
    """Escape special XML characters."""
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def short_commit(commit: str) -> str:
    """Get short commit hash."""
    if commit and len(commit) >= 7:
        return commit[:7]
    return commit or "?"


def generate_empty_chart(width: int, height: int, message: str) -> str:
    """Generate an SVG chart showing a message when no data is available."""
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" class="benchmark-chart">
  <style>
    .chart-bg {{ fill: var(--chart-bg, #ffffff); }}
    .chart-text {{ fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }}
    @media (prefers-color-scheme: dark) {{
      .chart-bg {{ fill: #1a1a1a; }}
      .chart-text {{ fill: #9ca3af; }}
    }}
  </style>
  <rect class="chart-bg" width="{width}" height="{height}" rx="8"/>
  <text class="chart-text" x="{width/2}" y="{height/2}" text-anchor="middle" font-size="14">{escape_xml(message)}</text>
</svg>'''


def generate_timeline_chart(runs: list[dict], platform: Optional[str] = None, benchmark_name: Optional[str] = None) -> str:
    """Generate time-series SVG chart of total compilation time.

    If benchmark_name is provided, shows data for that specific benchmark only.
    """
    if not runs:
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    for run in runs[-100:]:  # Show last 100 commits
        total = get_benchmark_time(run, benchmark_name) if benchmark_name else get_total_time(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "time": total})

    if not points or all(p["time"] == 0 for p in points):
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No timing data in benchmarks")

    # Chart layout
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = TIMELINE_WIDTH - margin["left"] - margin["right"]
    chart_height = TIMELINE_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_time = max(p["time"] for p in points) * 1.1  # 10% padding
    if max_time == 0:
        max_time = 1  # Avoid division by zero

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_time) * chart_height

    # Title with optional platform and benchmark
    title = "Compilation Time Over Recent Commits"
    if benchmark_name:
        base, _ = parse_benchmark_name(benchmark_name)
        title = f"Compilation Time - {base}"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {TIMELINE_WIDTH} {TIMELINE_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-accent, #4f6ddb); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-accent, #4f6ddb); }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{TIMELINE_WIDTH}" height="{TIMELINE_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{TIMELINE_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_time * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}ms</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{TIMELINE_HEIGHT - margin["bottom"]}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw line connecting points
    if len(points) > 1:
        path_d = "M " + " L ".join(
            f"{scale_x(i)},{scale_y(p['time'])}"
            for i, p in enumerate(points)
        )
        svg_parts.append(f'  <path class="chart-line" d="{path_d}"/>')

    # Draw points and x-axis labels
    for i, p in enumerate(points):
        x = scale_x(i)
        y = scale_y(p["time"])
        svg_parts.append(f'  <circle class="chart-point" cx="{x}" cy="{y}" r="4"/>')

        # X-axis label (rotated for readability)
        label_y = TIMELINE_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_hot_vs_cold_chart(
    runs: list[dict],
    platform: Optional[str] = None,
    benchmark_name: Optional[str] = None,
) -> str:
    """Time-series SVG charting cold (iter 1) vs hot (mean of iters 2..N) compile time.

    If benchmark_name is set, the chart is for that benchmark only; otherwise it
    aggregates across all benchmarks in each run.
    """
    if not runs:
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No benchmark data available yet")

    points = []
    for run in runs[-100:]:
        if benchmark_name:
            cold = get_benchmark_cold_ms(run, benchmark_name)
            hot = get_benchmark_hot_ms(run, benchmark_name)
        else:
            cold = get_total_cold_ms(run)
            hot = get_total_hot_ms(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "cold": cold, "hot": hot})

    if not points or all(p["cold"] == 0 and p["hot"] == 0 for p in points):
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No hot/cold timing data yet")

    margin = {"top": 50, "right": 30, "bottom": 60, "left": 70}
    chart_width = TIMELINE_WIDTH - margin["left"] - margin["right"]
    chart_height = TIMELINE_HEIGHT - margin["top"] - margin["bottom"]

    max_time = max(max(p["cold"], p["hot"]) for p in points) * 1.1
    if max_time == 0:
        max_time = 1

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_time) * chart_height

    title = "Hot vs Cold Compilation"
    if benchmark_name:
        base, _ = parse_benchmark_name(benchmark_name)
        title = f"Hot vs Cold Compilation - {base}"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {TIMELINE_WIDTH} {TIMELINE_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    .cold-line { stroke: #ef4444; fill: none; stroke-width: 2; }
    .cold-point { fill: #ef4444; }
    .hot-line { stroke: #4f6ddb; fill: none; stroke-width: 2; }
    .hot-point { fill: #4f6ddb; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{TIMELINE_WIDTH}" height="{TIMELINE_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{TIMELINE_WIDTH/2}" y="22" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    legend_y = 40
    legend_x = margin["left"]
    svg_parts.append(f'  <rect x="{legend_x}" y="{legend_y - 8}" width="12" height="3" fill="#ef4444"/>')
    svg_parts.append(f'  <text class="chart-text" x="{legend_x + 18}" y="{legend_y - 4}" font-size="11">Cold (iter 1)</text>')
    svg_parts.append(f'  <rect x="{legend_x + 110}" y="{legend_y - 8}" width="12" height="3" fill="#4f6ddb"/>')
    svg_parts.append(f'  <text class="chart-text" x="{legend_x + 128}" y="{legend_y - 4}" font-size="11">Hot (mean of remaining iters)</text>')

    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_time * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}ms</text>'
        )

    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{TIMELINE_HEIGHT - margin["bottom"]}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )

    for series, line_class, point_class in (("cold", "cold-line", "cold-point"), ("hot", "hot-line", "hot-point")):
        visible = [(i, p) for i, p in enumerate(points) if p[series] > 0]
        if len(visible) > 1:
            path_d = "M " + " L ".join(f"{scale_x(i)},{scale_y(p[series])}" for i, p in visible)
            svg_parts.append(f'  <path class="{line_class}" d="{path_d}"/>')
        for i, p in visible:
            svg_parts.append(
                f'  <circle class="{point_class}" cx="{scale_x(i)}" cy="{scale_y(p[series])}" r="3"/>'
            )

    for i, p in enumerate(points):
        x = scale_x(i)
        label_y = TIMELINE_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def get_benchmark_names(runs: list[dict]) -> list[str]:
    """Get list of all benchmark names from runs."""
    names = set()
    for run in runs:
        for bench in run.get("benchmarks", []):
            if "name" in bench:
                names.add(bench["name"])
    return sorted(names)


# Colors for different benchmark programs
BENCHMARK_COLORS = [
    "#4f6ddb",  # blue
    "#10b981",  # emerald
    "#f59e0b",  # amber
    "#ef4444",  # red
    "#8b5cf6",  # violet
    "#06b6d4",  # cyan
    "#ec4899",  # pink
]


def get_benchmark_time(run: dict, benchmark_name: str) -> float:
    """Get timing for a specific benchmark from a run."""
    bench = _find_benchmark(run, benchmark_name)
    if bench:
        if "mean_ms" in bench:
            return bench["mean_ms"]
        if "total_ms" in bench:
            total = bench["total_ms"]
            if isinstance(total, dict):
                return total.get("mean", 0)
            return total
    return 0




def get_pass_times_for_benchmark(run: dict, benchmark_name: str) -> dict[str, float]:
    """Extract pass timing for a specific benchmark from a run."""
    bench = _find_benchmark(run, benchmark_name)
    if bench and "passes" in bench:
        passes = bench["passes"]
        return {
            name: passes.get(name, {}).get("mean_ms", 0)
            for name in PASS_ORDER
        }
    return {}


def generate_breakdown_chart(runs: list[dict], benchmark_name: Optional[str] = None, platform: Optional[str] = None) -> str:
    """Generate stacked bar chart showing time per compiler pass.

    If benchmark_name is provided, shows data for that specific benchmark.
    Otherwise, shows aggregate data across all benchmarks.
    """
    if not runs:
        return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No benchmark data available yet")

    # Get the most recent run with pass data
    pass_times: Optional[dict[str, float]] = None
    commit = ""
    for run in reversed(runs):
        if benchmark_name:
            pt = get_pass_times_for_benchmark(run, benchmark_name)
        else:
            pt = get_pass_times(run)
        if pt and any(v > 0 for v in pt.values()):
            pass_times = pt
            commit = short_commit(run.get("commit", ""))
            break

    if not pass_times or all(v == 0 for v in pass_times.values()):
        return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No pass timing data available")

    # Chart layout
    margin = {"top": 50, "right": 150, "bottom": 40, "left": 70}
    chart_width = BREAKDOWN_WIDTH - margin["left"] - margin["right"]
    chart_height = BREAKDOWN_HEIGHT - margin["top"] - margin["bottom"]

    total = sum(pass_times.values())
    if total == 0:
        total = 1

    # Build title
    title_parts = ["Compilation Time by Pass"]
    if benchmark_name:
        title_parts.append(f" - {benchmark_name}")
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title_parts.append(f" ({platform_name})")
    title = "".join(title_parts)

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BREAKDOWN_WIDTH} {BREAKDOWN_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BREAKDOWN_WIDTH}" height="{BREAKDOWN_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BREAKDOWN_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
        f'  <text class="chart-text" x="{BREAKDOWN_WIDTH/2}" y="42" text-anchor="middle" font-size="11">(commit: {escape_xml(commit)})</text>',
    ]

    # Horizontal stacked bar
    bar_height = 40
    bar_y = margin["top"] + (chart_height - bar_height) / 2
    x_offset = margin["left"]

    for pass_name in PASS_ORDER:
        time = pass_times.get(pass_name, 0)
        width = (time / total) * chart_width if time > 0 else 0
        color = PASS_COLORS.get(pass_name, "#888888")

        if width > 0:
            svg_parts.append(
                f'  <rect x="{x_offset}" y="{bar_y}" width="{width}" height="{bar_height}" fill="{color}" rx="2"/>'
            )
            # Add time label if bar is wide enough
            if width > 30:
                svg_parts.append(
                    f'  <text x="{x_offset + width/2}" y="{bar_y + bar_height/2 + 4}" text-anchor="middle" font-size="10" fill="white">{time:.1f}ms</text>'
                )
            x_offset += width

    # Legend
    legend_x = BREAKDOWN_WIDTH - margin["right"] + 20
    legend_y = margin["top"] + 20

    for i, pass_name in enumerate(PASS_ORDER):
        y = legend_y + i * 22
        color = PASS_COLORS.get(pass_name, "#888888")
        time = pass_times.get(pass_name, 0)
        pct = (time / total) * 100

        svg_parts.append(f'  <rect x="{legend_x}" y="{y}" width="12" height="12" fill="{color}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{legend_x + 18}" y="{y + 10}" font-size="11">{pass_name} ({pct:.0f}%)</text>'
        )

    # Total time annotation
    svg_parts.append(
        f'  <text class="chart-text" x="{margin["left"]}" y="{bar_y + bar_height + 25}" font-size="12">Total: {total:.1f}ms</text>'
    )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_memory_chart(runs: list[dict], platform: Optional[str] = None, benchmark_name: Optional[str] = None) -> str:
    """Generate time-series SVG chart of peak memory usage.

    If benchmark_name is provided, shows data for that specific benchmark only.
    """
    if not runs:
        return generate_empty_chart(MEMORY_WIDTH, MEMORY_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    for run in runs[-100:]:  # Show last 100 commits
        memory = get_benchmark_memory(run, benchmark_name) if benchmark_name else get_peak_memory(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "memory": memory})

    if not points or all(p["memory"] == 0 for p in points):
        return generate_empty_chart(MEMORY_WIDTH, MEMORY_HEIGHT, "No memory data in benchmarks")

    # Chart layout
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = MEMORY_WIDTH - margin["left"] - margin["right"]
    chart_height = MEMORY_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_memory = max(p["memory"] for p in points) * 1.1  # 10% padding
    if max_memory == 0:
        max_memory = 1  # Avoid division by zero

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_memory) * chart_height

    # Title with optional platform and benchmark
    title = "Peak Memory Usage Over Recent Commits"
    if benchmark_name:
        base, _ = parse_benchmark_name(benchmark_name)
        title = f"Peak Memory - {base}"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {MEMORY_WIDTH} {MEMORY_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-memory, #10b981); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-memory, #10b981); }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{MEMORY_WIDTH}" height="{MEMORY_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{MEMORY_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 4
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_memory * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{MEMORY_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}MB</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{MEMORY_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{MEMORY_HEIGHT - margin["bottom"]}" x2="{MEMORY_WIDTH - margin["right"]}" y2="{MEMORY_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw line connecting points
    valid_points = [(i, p) for i, p in enumerate(points) if p["memory"] > 0]
    if len(valid_points) > 1:
        path_d = "M " + " L ".join(
            f"{scale_x(i)},{scale_y(p['memory'])}"
            for i, p in valid_points
        )
        svg_parts.append(f'  <path class="chart-line" d="{path_d}"/>')

    # Draw points and x-axis labels
    for i, p in enumerate(points):
        x = scale_x(i)
        if p["memory"] > 0:
            y = scale_y(p["memory"])
            svg_parts.append(f'  <circle class="chart-point" cx="{x}" cy="{y}" r="4"/>')

        # X-axis label (rotated for readability)
        label_y = MEMORY_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_binary_size_chart(runs: list[dict], platform: Optional[str] = None, benchmark_name: Optional[str] = None) -> str:
    """Generate time-series SVG chart of binary size.

    If benchmark_name is provided, shows data for that specific benchmark only.
    """
    if not runs:
        return generate_empty_chart(BINARY_WIDTH, BINARY_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    for run in runs[-100:]:  # Show last 100 commits
        size = get_benchmark_binary_size(run, benchmark_name) if benchmark_name else get_binary_size(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "size": size})

    if not points or all(p["size"] == 0 for p in points):
        return generate_empty_chart(BINARY_WIDTH, BINARY_HEIGHT, "No binary size data in benchmarks")

    # Chart layout
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = BINARY_WIDTH - margin["left"] - margin["right"]
    chart_height = BINARY_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_size = max(p["size"] for p in points) * 1.1  # 10% padding
    if max_size == 0:
        max_size = 1  # Avoid division by zero

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_size) * chart_height

    # Title with optional platform and benchmark
    title = "Binary Size Over Recent Commits"
    if benchmark_name:
        base, _ = parse_benchmark_name(benchmark_name)
        title = f"Binary Size - {base}"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BINARY_WIDTH} {BINARY_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-binary, #f59e0b); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-binary, #f59e0b); }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BINARY_WIDTH}" height="{BINARY_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BINARY_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 4
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_size * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{BINARY_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}KB</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{BINARY_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{BINARY_HEIGHT - margin["bottom"]}" x2="{BINARY_WIDTH - margin["right"]}" y2="{BINARY_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw line connecting points
    valid_points = [(i, p) for i, p in enumerate(points) if p["size"] > 0]
    if len(valid_points) > 1:
        path_d = "M " + " L ".join(
            f"{scale_x(i)},{scale_y(p['size'])}"
            for i, p in valid_points
        )
        svg_parts.append(f'  <path class="chart-line" d="{path_d}"/>')

    # Draw points and x-axis labels
    for i, p in enumerate(points):
        x = scale_x(i)
        if p["size"] > 0:
            y = scale_y(p["size"])
            svg_parts.append(f'  <circle class="chart-point" cx="{x}" cy="{y}" r="4"/>')

        # X-axis label (rotated for readability)
        label_y = BINARY_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_runtime_chart(runs: list[dict], benchmark_names: list[str], platform: Optional[str] = None) -> str:
    """Generate time-series SVG chart of runtime performance for compiled binaries."""
    if not runs or not benchmark_names:
        return generate_empty_chart(RUNTIME_WIDTH, RUNTIME_HEIGHT, "No runtime data available yet")

    # Filter to benchmarks that have runtime data
    names_with_runtime = []
    for name in benchmark_names:
        for run in runs:
            if get_benchmark_runtime(run, name) > 0:
                names_with_runtime.append(name)
                break

    if not names_with_runtime:
        return generate_empty_chart(RUNTIME_WIDTH, RUNTIME_HEIGHT, "No runtime data in benchmarks")

    # Extract data points for each benchmark
    commits = [short_commit(run.get("commit", "")) for run in runs[-100:]]
    benchmark_data = {}

    for name in names_with_runtime:
        points = []
        for run in runs[-100:]:
            runtime = get_benchmark_runtime(run, name)
            points.append(runtime)
        benchmark_data[name] = points

    all_runtimes = [t for pts in benchmark_data.values() for t in pts if t > 0]
    if not all_runtimes:
        return generate_empty_chart(RUNTIME_WIDTH, RUNTIME_HEIGHT, "No runtime data in benchmarks")

    # Chart layout (taller to accommodate legend)
    height = RUNTIME_HEIGHT
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = RUNTIME_WIDTH - margin["left"] - margin["right"]
    chart_height = RUNTIME_HEIGHT - margin["top"] - margin["bottom"] - 80  # Room for legend

    # Scale calculations
    max_runtime = max(all_runtimes) * 1.1
    if max_runtime == 0:
        max_runtime = 1

    def scale_x(i: int) -> float:
        if len(commits) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(commits) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_runtime) * chart_height

    # Title
    title = "Runtime Performance Over Recent Commits"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {RUNTIME_WIDTH} {height}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{RUNTIME_WIDTH}" height="{height}" rx="8"/>',
        f'  <text class="chart-title" x="{RUNTIME_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_runtime * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{RUNTIME_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.2f}ms</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{margin["top"] + chart_height}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"] + chart_height}" x2="{RUNTIME_WIDTH - margin["right"]}" y2="{margin["top"] + chart_height}"/>'
    )

    # Draw lines and points for each benchmark
    for idx, name in enumerate(names_with_runtime):
        color = BENCHMARK_COLORS[idx % len(BENCHMARK_COLORS)]
        points = benchmark_data[name]

        # Draw connecting line
        if len(points) > 1:
            line_points = []
            for i, runtime in enumerate(points):
                if runtime > 0:
                    line_points.append(f"{scale_x(i)},{scale_y(runtime)}")
            if line_points:
                path_d = "M " + " L ".join(line_points)
                svg_parts.append(f'  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2"/>')

        # Draw points
        for i, runtime in enumerate(points):
            if runtime > 0:
                x = scale_x(i)
                y = scale_y(runtime)
                svg_parts.append(f'  <circle cx="{x}" cy="{y}" r="3" fill="{color}"/>')

    # X-axis labels (commits)
    for i, commit in enumerate(commits):
        x = scale_x(i)
        label_y = margin["top"] + chart_height + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(commit)}</text>'
        )

    # Legend at bottom
    legend_y = height - 60
    legend_x_start = margin["left"]
    for idx, name in enumerate(names_with_runtime):
        color = BENCHMARK_COLORS[idx % len(BENCHMARK_COLORS)]
        # Strip @opt_level for display in legend
        display_name, _ = parse_benchmark_name(name)
        x = legend_x_start + (idx % 3) * 220
        y = legend_y + (idx // 3) * 20
        svg_parts.append(f'  <rect x="{x}" y="{y}" width="12" height="12" fill="{color}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{x + 18}" y="{y + 10}" font-size="11">{escape_xml(display_name)}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)




def _generate_comparison_line_chart(
    platform_data: dict[str, list[dict]],
    metric_fn,
    title: str,
    y_format_fn,
    empty_msg: str = "No data available",
) -> str:
    """Generic comparison line chart showing a metric across all platforms."""
    if not platform_data or all(not runs for runs in platform_data.values()):
        return generate_empty_chart(COMPARISON_WIDTH, COMPARISON_HEIGHT, empty_msg)

    # Build unified commit timeline
    commit_to_values: dict[str, dict[str, float]] = {}
    for platform, runs in platform_data.items():
        for run in runs[-100:]:
            commit = short_commit(run.get("commit", ""))
            value = metric_fn(run)
            if commit not in commit_to_values:
                commit_to_values[commit] = {}
            commit_to_values[commit][platform] = value

    if not commit_to_values:
        return generate_empty_chart(COMPARISON_WIDTH, COMPARISON_HEIGHT, empty_msg)

    commits = list(commit_to_values.keys())[-20:]
    platforms = list(platform_data.keys())

    all_values = [v for values in commit_to_values.values() for v in values.values() if v > 0]
    if not all_values:
        return generate_empty_chart(COMPARISON_WIDTH, COMPARISON_HEIGHT, empty_msg)
    max_value = max(all_values) * 1.1

    # Chart layout
    margin = {"top": 50, "right": 150, "bottom": 70, "left": 70}
    chart_width = COMPARISON_WIDTH - margin["left"] - margin["right"]
    chart_height = COMPARISON_HEIGHT - margin["top"] - margin["bottom"] - 50

    def scale_x(i: int) -> float:
        if len(commits) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(commits) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_value) * chart_height

    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {COMPARISON_WIDTH} {COMPARISON_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{COMPARISON_WIDTH}" height="{COMPARISON_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{COMPARISON_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_value * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{COMPARISON_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{y_format_fn(value)}</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{margin["top"] + chart_height}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"] + chart_height}" x2="{COMPARISON_WIDTH - margin["right"]}" y2="{margin["top"] + chart_height}"/>'
    )

    # Draw lines for each platform
    for platform in platforms:
        info = PLATFORM_INFO.get(platform, {"name": platform, "color": "#888888"})
        color = info["color"]

        line_points = []
        for i, commit in enumerate(commits):
            value = commit_to_values.get(commit, {}).get(platform, 0)
            if value > 0:
                line_points.append((i, value))

        if len(line_points) > 1:
            path_d = "M " + " L ".join(
                f"{scale_x(i)},{scale_y(v)}" for i, v in line_points
            )
            svg_parts.append(f'  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2"/>')

        for i, v in line_points:
            svg_parts.append(f'  <circle cx="{scale_x(i)}" cy="{scale_y(v)}" r="4" fill="{color}"/>')

    # X-axis labels
    for i, commit in enumerate(commits):
        x = scale_x(i)
        label_y = margin["top"] + chart_height + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="9" transform="rotate(-45 {x} {label_y})">{escape_xml(commit)}</text>'
        )

    # Legend
    legend_y = COMPARISON_HEIGHT - 35
    legend_x_start = margin["left"]
    for idx, platform in enumerate(platforms):
        info = PLATFORM_INFO.get(platform, {"name": platform, "color": "#888888"})
        x = legend_x_start + idx * 200
        svg_parts.append(f'  <rect x="{x}" y="{legend_y}" width="14" height="14" fill="{info["color"]}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{x + 20}" y="{legend_y + 11}" font-size="12">{escape_xml(info["name"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_comparison_timeline_chart(platform_data: dict[str, list[dict]]) -> str:
    """Generate a comparison chart showing compilation time across all platforms."""
    return _generate_comparison_line_chart(
        platform_data, get_total_time,
        "Compilation Time - All Platforms",
        lambda v: f"{v:.1f}ms",
        "No timing data available",
    )


def generate_comparison_memory_chart(platform_data: dict[str, list[dict]]) -> str:
    """Generate a comparison chart showing peak memory across all platforms."""
    return _generate_comparison_line_chart(
        platform_data, get_peak_memory,
        "Peak Memory - All Platforms",
        lambda v: f"{v:.1f}MB",
        "No memory data available",
    )


def generate_comparison_binary_chart(platform_data: dict[str, list[dict]]) -> str:
    """Generate a comparison chart showing binary size across all platforms."""
    return _generate_comparison_line_chart(
        platform_data, get_binary_size,
        "Binary Size - All Platforms",
        lambda v: f"{v:.1f}KB",
        "No binary size data available",
    )


def generate_comparison_runtime_chart(platform_data: dict[str, list[dict]]) -> str:
    """Generate a comparison chart showing total runtime across all platforms."""
    def get_total_runtime(run: dict) -> float:
        total = 0
        for bench in run.get("benchmarks", []):
            total += bench.get("runtime_ms", 0)
        return total

    return _generate_comparison_line_chart(
        platform_data, get_total_runtime,
        "Runtime Performance - All Platforms",
        lambda v: f"{v:.2f}ms",
        "No runtime data available",
    )


def generate_comparison_breakdown_chart(platform_data: dict[str, list[dict]]) -> str:
    """Generate a comparison breakdown chart showing pass times per platform."""
    if not platform_data:
        return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No data available")

    # Get latest pass times for each platform
    platform_passes: dict[str, dict[str, float]] = {}
    for platform, runs in platform_data.items():
        for run in reversed(runs):
            pt = get_pass_times(run)
            if pt and any(v > 0 for v in pt.values()):
                platform_passes[platform] = pt
                break

    if not platform_passes:
        return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No pass timing data available")

    platforms = list(platform_passes.keys())

    # Chart layout
    margin = {"top": 50, "right": 150, "bottom": 40, "left": 120}
    chart_width = BREAKDOWN_WIDTH - margin["left"] - margin["right"]
    chart_height = BREAKDOWN_HEIGHT - margin["top"] - margin["bottom"]

    # Max total for scaling
    max_total = max(sum(pt.values()) for pt in platform_passes.values())
    if max_total == 0:
        max_total = 1

    bar_height = min(40, chart_height / (len(platforms) + 1))
    bar_gap = bar_height * 0.5
    total_bars_height = len(platforms) * (bar_height + bar_gap) - bar_gap
    start_y = margin["top"] + (chart_height - total_bars_height) / 2

    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BREAKDOWN_WIDTH} {BREAKDOWN_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BREAKDOWN_WIDTH}" height="{BREAKDOWN_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BREAKDOWN_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Compilation Time by Pass - All Platforms</text>',
    ]

    for pi, platform in enumerate(platforms):
        pass_times = platform_passes[platform]
        total = sum(pass_times.values())
        bar_y = start_y + pi * (bar_height + bar_gap)

        # Platform label
        info = PLATFORM_INFO.get(platform, {"name": platform})
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{bar_y + bar_height / 2 + 4}" text-anchor="end" font-size="11">{escape_xml(info.get("name", platform))}</text>'
        )

        # Stacked bars
        x_offset = margin["left"]
        for pass_name in PASS_ORDER:
            time = pass_times.get(pass_name, 0)
            width = (time / max_total) * chart_width if time > 0 else 0
            color = PASS_COLORS.get(pass_name, "#888888")
            if width > 0:
                svg_parts.append(
                    f'  <rect x="{x_offset}" y="{bar_y}" width="{width}" height="{bar_height}" fill="{color}" rx="2"/>'
                )
                if width > 30:
                    svg_parts.append(
                        f'  <text x="{x_offset + width / 2}" y="{bar_y + bar_height / 2 + 4}" text-anchor="middle" font-size="10" fill="white">{time:.1f}ms</text>'
                    )
                x_offset += width

        # Total label
        svg_parts.append(
            f'  <text class="chart-text" x="{x_offset + 5}" y="{bar_y + bar_height / 2 + 4}" font-size="10">{total:.1f}ms</text>'
        )

    # Legend
    legend_x = BREAKDOWN_WIDTH - margin["right"] + 20
    legend_y = margin["top"] + 20
    for i, pass_name in enumerate(PASS_ORDER):
        y = legend_y + i * 22
        color = PASS_COLORS.get(pass_name, "#888888")
        svg_parts.append(f'  <rect x="{legend_x}" y="{y}" width="12" height="12" fill="{color}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{legend_x + 18}" y="{y + 10}" font-size="11">{pass_name}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def calculate_coverage_metrics(runs: list[dict]) -> dict:
    """Calculate benchmark coverage metrics.

    Returns coverage information including:
    - How many distinct commits have been benchmarked
    - The commit ranges covered by each benchmark run
    - Data gaps (periods without benchmarks)
    """
    if not runs:
        return {
            "total_commits_covered": 0,
            "run_count": 0,
            "coverage_pct": 0,
            "gaps": []
        }

    # Collect all commits covered by benchmark runs
    covered_commits = set()
    run_info = []

    for run in runs:
        # Version 2 schema has commit_range field
        commit_range = run.get("commit_range", [])
        if not commit_range:
            # Version 1 schema - single commit only
            commit = run.get("commit", "")
            if commit:
                covered_commits.add(commit)
                commit_range = [commit]
        else:
            # Add all commits in the range
            for c in commit_range:
                if c:
                    covered_commits.add(c)

        run_info.append({
            "commit": short_commit(run.get("commit", "")),
            "timestamp": run.get("timestamp", ""),
            "commit_count": len(commit_range),
            "reason": run.get("benchmark_reason", "unknown")
        })

    # Calculate coverage percentage
    # Note: We can't calculate true coverage without knowing the total number of commits
    # in the repository, so we report the number of distinct commits benchmarked
    total_commits_covered = len(covered_commits)

    return {
        "total_commits_covered": total_commits_covered,
        "run_count": len(runs),
        "avg_commits_per_run": round(total_commits_covered / len(runs), 1) if runs else 0,
        "runs": run_info[-20:]  # Last 20 runs for display
    }


def generate_summary_data(runs: list[dict], platform: Optional[str] = None) -> dict:
    """Generate summary statistics for the performance dashboard."""
    if not runs:
        return {}

    latest = runs[-1] if runs else None
    previous = runs[-2] if len(runs) >= 2 else None

    # Get latest values
    latest_time = get_total_time(latest) if latest else 0
    latest_memory = get_peak_memory(latest) if latest else 0
    latest_binary = get_binary_size(latest) if latest else 0
    latest_commit = short_commit(latest.get("commit", "")) if latest else ""

    # Calculate deltas
    prev_time = get_total_time(previous) if previous else 0
    prev_memory = get_peak_memory(previous) if previous else 0
    prev_binary = get_binary_size(previous) if previous else 0

    time_delta_pct, time_delta_str = calculate_delta(latest_time, prev_time)
    memory_delta_pct, memory_delta_str = calculate_delta(latest_memory, prev_memory)
    binary_delta_pct, binary_delta_str = calculate_delta(latest_binary, prev_binary)

    # Calculate 7-run average (or whatever we have)
    recent_runs = runs[-7:] if len(runs) >= 7 else runs
    avg_time = sum(get_total_time(r) for r in recent_runs) / len(recent_runs) if recent_runs else 0
    avg_memory = sum(get_peak_memory(r) for r in recent_runs) / len(recent_runs) if recent_runs else 0

    # Find best ever
    all_times = [get_total_time(r) for r in runs if get_total_time(r) > 0]
    best_time = min(all_times) if all_times else 0

    result = {
        "latest_commit": latest_commit,
        "latest_time_ms": round(latest_time, 2),
        "latest_memory_mb": round(latest_memory, 2),
        "latest_binary_kb": round(latest_binary, 2),
        "time_delta_pct": round(time_delta_pct, 2),
        "time_delta_str": time_delta_str,
        "memory_delta_pct": round(memory_delta_pct, 2),
        "memory_delta_str": memory_delta_str,
        "binary_delta_pct": round(binary_delta_pct, 2),
        "binary_delta_str": binary_delta_str,
        "avg_time_ms": round(avg_time, 2),
        "avg_memory_mb": round(avg_memory, 2),
        "best_time_ms": round(best_time, 2),
        "run_count": len(runs),
    }

    if platform:
        result["platform"] = platform
        info = PLATFORM_INFO.get(platform, {})
        result["platform_name"] = info.get("name", platform)

    return result


def generate_platform_charts(history_path: Path, output_dir: Path, platform: Optional[str] = None):
    """Generate all charts for a single platform."""
    # Load history
    history = load_history(history_path)
    runs = history.get("runs", [])

    print(f"Loaded {len(runs)} benchmark runs from {history_path}")
    if platform:
        print(f"Generating charts for platform: {platform}")

    # Ensure output directory exists
    output_dir.mkdir(parents=True, exist_ok=True)

    # Detect optimization levels present in the data
    opt_levels = get_opt_levels_from_runs(runs)
    print(f"Optimization levels found: {', '.join(opt_levels)}")

    # Get all benchmark names (includes @opt suffixes)
    all_benchmark_names = get_benchmark_names(runs)
    print(f"Found {len(all_benchmark_names)} benchmarks: {', '.join(all_benchmark_names)}")

    # Generate per-opt-level chart variants
    for opt in opt_levels:
        opt_runs = filter_runs_by_opt_level(runs, opt)
        opt_names = get_benchmark_names(opt_runs)

        if not opt_runs:
            print(f"  No data for {opt}, skipping")
            continue

        print(f"  Generating charts for -{opt} ({len(opt_names)} benchmarks)")

        # Timeline (aggregate)
        svg = generate_timeline_chart(opt_runs, platform)
        path = output_dir / f"timeline_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

        # Breakdown (aggregate)
        svg = generate_breakdown_chart(opt_runs, platform=platform)
        path = output_dir / f"breakdown_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

        # Memory
        svg = generate_memory_chart(opt_runs, platform)
        path = output_dir / f"memory_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

        # Binary size
        svg = generate_binary_size_chart(opt_runs, platform)
        path = output_dir / f"binary_size_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

        # Runtime chart (aggregate - all benchmarks)
        svg = generate_runtime_chart(opt_runs, opt_names, platform)
        path = output_dir / f"runtime_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

        # Hot vs cold compilation (aggregate)
        svg = generate_hot_vs_cold_chart(opt_runs, platform)
        path = output_dir / f"hot_vs_cold_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

        # Per-benchmark charts for all chart types
        # Always use {base_name}@{opt} naming for consistency with HTML loading
        for bench_name in opt_names:
            base_name, _ = parse_benchmark_name(bench_name)
            canonical_name = f"{base_name}@{opt}"
            safe_name = canonical_name.replace(" ", "_").replace("/", "_")

            # Breakdown
            svg = generate_breakdown_chart(opt_runs, bench_name, platform)
            path = output_dir / f"breakdown_{safe_name}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Timeline
            svg = generate_timeline_chart(opt_runs, platform, benchmark_name=bench_name)
            path = output_dir / f"timeline_{safe_name}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Memory
            svg = generate_memory_chart(opt_runs, platform, benchmark_name=bench_name)
            path = output_dir / f"memory_{safe_name}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Binary size
            svg = generate_binary_size_chart(opt_runs, platform, benchmark_name=bench_name)
            path = output_dir / f"binary_size_{safe_name}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Runtime (single benchmark)
            svg = generate_runtime_chart(opt_runs, [bench_name], platform)
            path = output_dir / f"runtime_{safe_name}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Hot vs cold (single benchmark)
            svg = generate_hot_vs_cold_chart(opt_runs, platform, benchmark_name=bench_name)
            path = output_dir / f"hot_vs_cold_{safe_name}.svg"
            with open(path, "w") as f:
                f.write(svg)

        print(f"  Generated per-benchmark charts for {len(opt_names)} benchmarks")

    # Generate backwards-compatible default charts (using first opt level, typically O0)
    default_opt = opt_levels[0] if opt_levels else "O0"
    default_runs = filter_runs_by_opt_level(runs, default_opt)

    timeline_svg = generate_timeline_chart(default_runs, platform)
    with open(output_dir / "timeline.svg", "w") as f:
        f.write(timeline_svg)

    breakdown_svg = generate_breakdown_chart(default_runs, platform=platform)
    with open(output_dir / "breakdown.svg", "w") as f:
        f.write(breakdown_svg)

    memory_svg = generate_memory_chart(default_runs, platform)
    with open(output_dir / "memory.svg", "w") as f:
        f.write(memory_svg)

    binary_svg = generate_binary_size_chart(default_runs, platform)
    with open(output_dir / "binary_size.svg", "w") as f:
        f.write(binary_svg)

    # Generate summary statistics (using default opt level)
    summary = generate_summary_data(default_runs, platform)

    # Include latest run's metrics for display (all opt levels)
    latest_benchmarks = []
    if runs:
        latest_run = runs[-1]
        for bench in latest_run.get("benchmarks", []):
            bench_info = {
                "name": bench.get("name", ""),
                "mean_ms": bench.get("mean_ms", 0),
            }
            if "source_metrics" in bench:
                sm = bench["source_metrics"]
                bench_info["source_metrics"] = sm
                # Calculate throughput metrics
                if bench_info["mean_ms"] > 0:
                    seconds = bench_info["mean_ms"] / 1000
                    bench_info["lines_per_sec"] = int(sm.get("lines", 0) / seconds)
                    bench_info["tokens_per_sec"] = int(sm.get("tokens", 0) / seconds)
            if "peak_memory_bytes" in bench:
                bench_info["peak_memory_mb"] = round(bench["peak_memory_bytes"] / (1024 * 1024), 2)
            if "binary_size_bytes" in bench:
                bench_info["binary_size_kb"] = round(bench["binary_size_bytes"] / 1024, 2)
            if "runtime_ms" in bench:
                bench_info["runtime_ms"] = round(bench["runtime_ms"], 3)
            if "runtime_std_ms" in bench:
                bench_info["runtime_std_ms"] = round(bench["runtime_std_ms"], 3)
            # Include opt level info
            _, opt = parse_benchmark_name(bench.get("name", ""))
            if opt:
                bench_info["opt_level"] = opt
            latest_benchmarks.append(bench_info)

    # Calculate coverage metrics
    coverage = calculate_coverage_metrics(runs)

    # Write metadata JSON for the website to consume (includes summary and detailed metrics)
    metadata = {
        "benchmarks": all_benchmark_names,
        "opt_levels": opt_levels,
        "run_count": len(runs),
        "latest_commit": short_commit(runs[-1].get("commit", "")) if runs else None,
        "summary": summary,
        "latest_benchmarks": latest_benchmarks,
        "coverage": coverage,
    }
    if platform:
        metadata["platform"] = platform
        info = PLATFORM_INFO.get(platform, {})
        metadata["platform_name"] = info.get("name", platform)

    metadata_path = output_dir / "metadata.json"
    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)
    print(f"Generated {metadata_path}")


def generate_comparison_charts(history_files: list[Path], output_dir: Path):
    """Generate comparison charts from multiple platform history files."""
    print(f"Generating comparison charts from {len(history_files)} history files")

    # Load all histories
    platform_data: dict[str, list[dict]] = {}
    platform_info_list = []

    for path in history_files:
        # Extract platform from filename (e.g., history-x86-64-linux.json -> x86-64-linux)
        name = path.stem
        if name.startswith("history-"):
            platform = name[8:]  # Remove "history-" prefix
        elif name == "history":
            platform = "unknown"
        else:
            platform = name

        history = load_history(path)
        runs = history.get("runs", [])

        if runs:
            platform_data[platform] = runs
            info = PLATFORM_INFO.get(platform, {"name": platform, "color": "#888888"})
            platform_info_list.append({
                "id": platform,
                "name": info["name"],
                "color": info["color"],
                "run_count": len(runs),
                "latest_commit": short_commit(runs[-1].get("commit", "")) if runs else None,
                "has_data": True
            })
            print(f"  Loaded {len(runs)} runs for {platform}")
        else:
            print(f"  No data for {platform}")

    if not platform_data:
        print("No data available for comparison charts")
        return

    # Ensure output directory exists
    output_dir.mkdir(parents=True, exist_ok=True)

    # Determine all opt levels and benchmark names across platforms
    all_opt_levels: set[str] = set()
    all_benchmark_base_names: set[str] = set()
    for runs in platform_data.values():
        all_opt_levels.update(get_opt_levels_from_runs(runs))
        for name in get_benchmark_names(runs):
            base, _ = parse_benchmark_name(name)
            all_benchmark_base_names.add(base)
    opt_levels = sorted(all_opt_levels) if all_opt_levels else ["O0"]
    benchmark_names = sorted(all_benchmark_base_names)

    print(f"  Opt levels across platforms: {', '.join(opt_levels)}")
    print(f"  Benchmark programs: {', '.join(benchmark_names)}")

    # Chart generators: (name_prefix, generator_function)
    chart_generators = [
        ("timeline", generate_comparison_timeline_chart),
        ("memory", generate_comparison_memory_chart),
        ("binary_size", generate_comparison_binary_chart),
        ("runtime", generate_comparison_runtime_chart),
        ("breakdown", generate_comparison_breakdown_chart),
    ]

    # Generate per-opt-level comparison charts
    for opt in opt_levels:
        filtered_data = {
            p: filter_runs_by_opt_level(runs, opt)
            for p, runs in platform_data.items()
        }
        # Remove platforms with no data for this opt level
        filtered_data = {p: runs for p, runs in filtered_data.items() if runs}

        if not filtered_data:
            print(f"  No data for {opt} across platforms, skipping")
            continue

        print(f"  Generating comparison charts for -{opt}")
        for prefix, gen_fn in chart_generators:
            svg = gen_fn(filtered_data)
            path = output_dir / f"{prefix}_{opt}.svg"
            with open(path, "w") as f:
                f.write(svg)
            print(f"    Generated {path}")

        # Hot vs cold (aggregate across platforms): merge runs, sort by timestamp
        merged_runs = []
        for runs in filtered_data.values():
            merged_runs.extend(runs)
        merged_runs.sort(key=lambda r: r.get("timestamp", ""))
        svg = generate_hot_vs_cold_chart(merged_runs)
        path = output_dir / f"hot_vs_cold_{opt}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"    Generated {path}")

        # Per-benchmark comparison charts
        for bench_base in benchmark_names:
            bench_full = f"{bench_base}@{opt}"

            # Timeline per-benchmark
            def make_bench_time_fn(name):
                return lambda run: get_benchmark_time(run, name)
            svg = _generate_comparison_line_chart(
                filtered_data, make_bench_time_fn(bench_full),
                f"Compilation Time - {bench_base} (All Platforms)",
                lambda v: f"{v:.1f}ms",
                "No timing data available",
            )
            path = output_dir / f"timeline_{bench_full}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Memory per-benchmark
            def make_bench_mem_fn(name):
                return lambda run: get_benchmark_memory(run, name)
            svg = _generate_comparison_line_chart(
                filtered_data, make_bench_mem_fn(bench_full),
                f"Peak Memory - {bench_base} (All Platforms)",
                lambda v: f"{v:.1f}MB",
                "No memory data available",
            )
            path = output_dir / f"memory_{bench_full}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Binary size per-benchmark
            def make_bench_bin_fn(name):
                return lambda run: get_benchmark_binary_size(run, name)
            svg = _generate_comparison_line_chart(
                filtered_data, make_bench_bin_fn(bench_full),
                f"Binary Size - {bench_base} (All Platforms)",
                lambda v: f"{v:.1f}KB",
                "No binary size data available",
            )
            path = output_dir / f"binary_size_{bench_full}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Runtime per-benchmark
            def make_bench_rt_fn(name):
                return lambda run: get_benchmark_runtime(run, name)
            svg = _generate_comparison_line_chart(
                filtered_data, make_bench_rt_fn(bench_full),
                f"Runtime - {bench_base} (All Platforms)",
                lambda v: f"{v:.2f}ms",
                "No runtime data available",
            )
            path = output_dir / f"runtime_{bench_full}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Breakdown per-benchmark
            def make_bench_breakdown(data, name):
                """Generate comparison breakdown for a specific benchmark."""
                platform_passes = {}
                for plat, runs in data.items():
                    for run in reversed(runs):
                        pt = get_pass_times_for_benchmark(run, name)
                        if pt and any(v > 0 for v in pt.values()):
                            platform_passes[plat] = pt
                            break
                if not platform_passes:
                    return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No pass timing data available")
                # Reuse comparison breakdown but with per-benchmark data
                fake_data = {}
                for plat, pt in platform_passes.items():
                    fake_data[plat] = [{"benchmarks": [{"passes": {k: {"mean_ms": v} for k, v in pt.items()}}]}]
                return generate_comparison_breakdown_chart(fake_data)

            svg = make_bench_breakdown(filtered_data, bench_full)
            path = output_dir / f"breakdown_{bench_full}.svg"
            with open(path, "w") as f:
                f.write(svg)

            # Hot vs cold per-benchmark (merged across platforms)
            merged_bench_runs = []
            for runs in filtered_data.values():
                merged_bench_runs.extend(runs)
            merged_bench_runs.sort(key=lambda r: r.get("timestamp", ""))
            svg = generate_hot_vs_cold_chart(merged_bench_runs, benchmark_name=bench_full)
            path = output_dir / f"hot_vs_cold_{bench_full}.svg"
            with open(path, "w") as f:
                f.write(svg)

        print(f"  Generated per-benchmark comparison charts for {len(benchmark_names)} programs")

    # Generate default (no opt suffix) comparison charts using first opt level
    default_opt = opt_levels[0] if opt_levels else "O0"
    default_data = {
        p: filter_runs_by_opt_level(runs, default_opt)
        for p, runs in platform_data.items()
    }
    default_data = {p: runs for p, runs in default_data.items() if runs}

    for prefix, gen_fn in chart_generators:
        svg = gen_fn(default_data)
        path = output_dir / f"{prefix}.svg"
        with open(path, "w") as f:
            f.write(svg)
        print(f"  Generated {path}")

    # Generate comparison metadata
    metadata = {
        "platforms": platform_info_list,
        "default_platform": platform_info_list[0]["id"] if platform_info_list else None,
        "opt_levels": opt_levels,
        "benchmarks": benchmark_names,
    }
    metadata_path = output_dir / "metadata.json"
    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)
    print(f"Generated {metadata_path}")


def main():
    parser = argparse.ArgumentParser(
        description="Generate SVG charts from benchmark history for the performance dashboard."
    )
    parser.add_argument(
        "--comparison",
        action="store_true",
        help="Generate comparison charts from multiple platform histories"
    )
    parser.add_argument(
        "--platform",
        type=str,
        help="Platform identifier for chart titles (e.g., x86-64-linux)"
    )
    parser.add_argument(
        "paths",
        nargs="+",
        help="History file(s) and output directory. For single platform: <history.json> <output-dir>. "
             "For comparison: <output-dir> <history1.json> <history2.json> ..."
    )

    args = parser.parse_args()

    if args.comparison:
        # Comparison mode: first arg is output dir, rest are history files
        if len(args.paths) < 2:
            print("Error: Comparison mode requires output directory and at least one history file", file=sys.stderr)
            sys.exit(1)
        output_dir = Path(args.paths[0])
        history_files = [Path(p) for p in args.paths[1:]]
        generate_comparison_charts(history_files, output_dir)
    else:
        # Single platform mode
        if len(args.paths) != 2:
            print("Error: Single platform mode requires exactly <history.json> <output-dir>", file=sys.stderr)
            sys.exit(1)
        history_path = Path(args.paths[0])
        output_dir = Path(args.paths[1])
        generate_platform_charts(history_path, output_dir, args.platform)


if __name__ == "__main__":
    main()
