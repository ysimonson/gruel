#!/usr/bin/env python3
"""Enhance benchmark results with commit range metadata (version 2 schema).

Usage: enhance-benchmark.py <results_file> <output_file> <reason> <commit_range> <full_sha>
"""
import json
import sys

def main():
    if len(sys.argv) != 6:
        print(f"Usage: {sys.argv[0]} <results_file> <output_file> <reason> <commit_range> <full_sha>", file=sys.stderr)
        sys.exit(1)

    results_file, output_file, reason, commit_range_str, full_sha = sys.argv[1:]

    with open(results_file) as f:
        data = json.load(f)

    data['version'] = 2
    data['benchmark_reason'] = reason

    if commit_range_str:
        data['commit_range'] = [c.strip() for c in commit_range_str.split(',') if c.strip()]
    else:
        data['commit_range'] = [full_sha]

    with open(output_file, 'w') as f:
        json.dump(data, f, indent=2)

if __name__ == '__main__':
    main()
