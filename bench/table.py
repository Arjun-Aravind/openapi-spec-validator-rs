"""Prints the benchmark results as Markdown tables.

Usage: python bench/table.py RESULTS_DIR [--summary]
"""

import json
import os
import sys

SPECS = [
    ("github-json.json", "GitHub (OpenAPI 3.0, JSON, 13.5 MB)"),
    ("github-yaml.json", "GitHub (OpenAPI 3.0, YAML, 10.3 MB)"),
    ("stripe-json.json", "Stripe (OpenAPI 3.0, JSON, 8.3 MB)"),
    ("kubernetes-json.json", "Kubernetes (Swagger 2.0, JSON, 4.3 MB)"),
]
OURS = "openapi-spec-validator-rs"
UP_RS = "openapi-spec-validator (jsonschema-rs backend)"
UP_PY = "openapi-spec-validator (default backend)"
VACUUM = "vacuum (validation rules only)"


def stats(result):
    """`(mean, stddev, peak memory in bytes or None)` from hyperfine 1.x or 2.x JSON."""
    summary = result.get("summary")
    if summary:
        wall = summary["time_wall_clock"]
        memory = summary.get("memory_peak_resident", {}).get("mean")
        return wall["mean"], wall["stddev"], memory
    return result["mean"], result["stddev"], None


def by_name(results):
    return {r.get("name", r["command"]): r for r in results}


def fmt(seconds, stddev=None):
    if seconds < 1:
        text = f"{seconds * 1000:.0f} ms"
        return text if stddev is None else f"{text} ± {stddev * 1000:.0f}"
    text = f"{seconds:.2f} s"
    return text if stddev is None else f"{text} ± {stddev:.2f}"


def load(results_dir):
    rows = []
    for name, label in SPECS:
        path = os.path.join(results_dir, name)
        if os.path.exists(path):
            with open(path) as fh:
                results = by_name(json.load(fh)["results"])
            rows.append((label, results))
    return rows


def main():
    results_dir = sys.argv[1]
    summary = "--summary" in sys.argv
    rows = load(results_dir)
    if summary:
        print("| Spec | openapi-spec-validator-rs | openapi-spec-validator | Speed-up |")
        print("|---|---:|---:|---:|")
        for label, r in rows:
            ours = stats(r[OURS])[0]
            up_rs, up_py = stats(r[UP_RS])[0], stats(r[UP_PY])[0]
            print(f"| {label} | **{fmt(ours)}** | {fmt(up_rs)} – {fmt(up_py)} "
                  f"| **{up_rs / ours:.0f}–{up_py / ours:.0f}×** |")
        return
    tools = (OURS, UP_RS, UP_PY, VACUUM)
    print("| Spec | openapi-spec-validator-rs | openapi-spec-validator, jsonschema-rs backend "
          "| openapi-spec-validator, default backend | vacuum (validation rules) |")
    print("|---|---:|---:|---:|---:|")
    for label, r in rows:
        ours = stats(r[OURS])[0]
        cells = []
        for key in tools:
            if key not in r:
                cells.append("–")
                continue
            mean, stddev, _ = stats(r[key])
            cell = fmt(mean, stddev)
            cells.append(f"**{cell}**" if key == OURS else f"{cell} ({mean / ours:.0f}× slower)")
        print(f"| {label} | " + " | ".join(cells) + " |")
    print()
    print("Peak memory (resident set):")
    print()
    print("| Spec | openapi-spec-validator-rs | openapi-spec-validator, jsonschema-rs backend "
          "| openapi-spec-validator, default backend | vacuum (validation rules) |")
    print("|---|---:|---:|---:|---:|")
    for label, r in rows:
        cells = []
        for key in tools:
            memory = stats(r[key])[2] if key in r else None
            cells.append(f"{memory / 2**20:.0f} MiB" if memory else "–")
        print(f"| {label} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main()
