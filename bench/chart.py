"""Renders bench/benchmarks.svg from hyperfine's JSON results (no dependencies).

Usage: python bench/chart.py RESULTS_DIR OUTPUT.svg
"""

import json
import os
import sys
from html import escape

PANELS = [
    ("github-json.json", "GitHub REST API", "OpenAPI 3.0 · JSON · 13.5 MB"),
    ("github-yaml.json", "GitHub REST API", "OpenAPI 3.0 · YAML · 10.3 MB"),
    ("stripe-json.json", "Stripe API", "OpenAPI 3.0 · JSON · 8.3 MB"),
    ("kubernetes-json.json", "Kubernetes API", "Swagger 2.0 · JSON · 4.3 MB"),
]
TOOLS = [
    ("openapi-spec-validator-rs", "openapi-spec-validator-rs"),
    ("openapi-spec-validator (jsonschema-rs backend)", "openapi-spec-validator, jsonschema-rs backend"),
    ("openapi-spec-validator (default backend)", "openapi-spec-validator, default backend"),
    ("vacuum (validation rules only)", "vacuum, validation rules only"),
]
OURS = TOOLS[0][0]

WIDTH = 760


def stats(result):
    """`(mean, stddev, peak memory)` from hyperfine 1.x or 2.x JSON."""
    summary = result.get("summary")
    if summary:
        wall = summary["time_wall_clock"]
        memory = summary.get("memory_peak_resident", {}).get("mean")
        return wall["mean"], wall["stddev"], memory
    return result["mean"], result["stddev"], None


def by_name(results):
    return {r.get("name", r["command"]): r for r in results}

LABEL_W = 300
# Leaves room for the longest value label to the right of the longest bar.
BAR_W = 260
BAR_H = 14
ROW_H = 26
PANEL_GAP = 28
TOP = 76


def fmt(seconds):
    if seconds < 1:
        return f"{seconds * 1000:.0f} ms"
    return f"{seconds:.2f} s"


def main():
    results_dir, output = sys.argv[1:]
    panels = []
    for name, title, subtitle in PANELS:
        path = os.path.join(results_dir, name)
        if not os.path.exists(path):
            continue
        with open(path) as fh:
            results = {name: stats(r)[0] for name, r in by_name(json.load(fh)["results"]).items()}
        rows = [(label, results[cmd]) for cmd, label in TOOLS if cmd in results]
        ours = results.get(OURS)
        panels.append((title, subtitle, rows, ours))

    height = TOP + sum(30 + len(rows) * ROW_H + PANEL_GAP for _, _, rows, _ in panels)
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
        f'viewBox="0 0 {WIDTH} {height}" role="img" aria-labelledby="t d" '
        'font-family="-apple-system, BlinkMacSystemFont, Segoe UI, Helvetica, Arial, sans-serif">',
        '<title id="t">Validation time per spec, lower is better</title>',
        '<desc id="d">Mean wall time of each tool validating each spec, measured with hyperfine.</desc>',
        "<style>",
        ".surface{fill:#fcfcfb}.ink{fill:#0b0b0b}.muted{fill:#52514e}.axis{stroke:#d9d8d4}"
        ".ours{fill:#2a78d6}.other{fill:#a8a7a1}",
        "@media (prefers-color-scheme: dark){.surface{fill:#1a1a19}.ink{fill:#f0efec}"
        ".muted{fill:#c3c2b7}.axis{stroke:#3a3a37}.ours{fill:#3987e5}.other{fill:#6d6c66}}",
        "</style>",
        f'<rect class="surface" width="{WIDTH}" height="{height}" rx="8"/>',
        '<text class="ink" x="24" y="34" font-size="17" font-weight="600">'
        "Validation time per spec (lower is better)</text>",
        '<text class="muted" x="24" y="56" font-size="12.5">'
        "Mean of 10 runs with hyperfine; each panel has its own scale.</text>",
    ]
    y = TOP
    for title, subtitle, rows, ours in panels:
        out.append(f'<text class="ink" x="24" y="{y + 14}" font-size="14" font-weight="600">{escape(title)}</text>')
        out.append(f'<text class="muted" x="{24 + 8 + len(title) * 8}" y="{y + 14}" font-size="12">{escape(subtitle)}</text>')
        y += 30
        top = y
        longest = max(value for _, value in rows)
        for label, value in rows:
            is_ours = label == TOOLS[0][1]
            width = max(2.0, BAR_W * value / longest)
            x0 = 24 + LABEL_W
            cls = "ours" if is_ours else "other"
            weight = ' font-weight="600"' if is_ours else ""
            out.append(f'<text class="ink" x="{x0 - 10}" y="{y + BAR_H - 2}" font-size="12.5" text-anchor="end"{weight}>{escape(label)}</text>')
            # Square at the baseline, 4px rounding at the data end.
            r = min(4.0, width / 2)
            out.append(
                f'<path class="{cls}" d="M{x0},{y} h{width - r:.1f} a{r},{r} 0 0 1 {r},{r} '
                f'v{BAR_H - 2 * r} a{r},{r} 0 0 1 {-r},{r} h{-(width - r):.1f} z"/>'
            )
            note = fmt(value)
            if ours and not is_ours:
                note += f"  ({value / ours:.0f}× slower)"
            out.append(f'<text class="{"ink" if is_ours else "muted"}" x="{x0 + width + 6:.1f}" y="{y + BAR_H - 2}" font-size="12"{weight}>{note}</text>')
            y += ROW_H
        out.append(f'<line class="axis" x1="{24 + LABEL_W}" y1="{top - 4}" x2="{24 + LABEL_W}" y2="{y - ROW_H + BAR_H + 4}" stroke-width="1"/>')
        y += PANEL_GAP
    out.append("</svg>")
    with open(output, "w") as fh:
        fh.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
