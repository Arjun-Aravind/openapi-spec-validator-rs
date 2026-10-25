"""Compare the frame model's predictions with measurements of upstream.

Generates documents that nest in many ways (or takes files), measures upstream's
deepest stack per phase with ``measure.py`` (in the upstream interpreter), asks the
model (``cargo run --example frames``) for the same, and reports every difference.

Usage:
    python scripts/frames/compare.py --python PATH_TO_UPSTREAM_PYTHON [FILE...]

With ``CHAINS=1`` the measurements carry upstream's deepest call chain, and
``FRAMES_DEBUG=1`` makes the example print where the model's deepest point lies.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))

HEADS = {
    "2.0": "swagger: '2.0'\ninfo: {title: t, version: '1'}\npaths: {}\n",
    "3.0": "openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths: {}\n",
    "3.1": "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths: {}\n",
    "3.2": "openapi: 3.2.0\ninfo: {title: t, version: '1'}\npaths: {}\n",
}

WRAPS = {
    "properties": "{{type: object, properties: {{p: {inner}}}}}",
    "allOf": "{{allOf: [{inner}]}}",
    "anyOf": "{{anyOf: [{inner}, {{type: string}}]}}",
    "oneOf": "{{oneOf: [{{type: integer}}, {inner}]}}",
    "items": "{{type: array, items: {inner}}}",
    "additionalProperties": "{{type: object, additionalProperties: {inner}}}",
    "not": "{{not: {inner}}}",
    "mixed": "{{type: object, required: [p], properties: {{p: {{allOf: [{{description: x}}, {inner}]}}}}}}",
}

LEAVES = {
    "string": "{type: string}",
    "bad-type": "{type: 5}",
    "enum": "{type: string, enum: [a, b], default: a}",
    "ref": "{$ref: '#/x-target'}",
    "pattern": "{type: string, pattern: '^a+$'}",
    "format": "{type: string, format: date}",
}


def generated(directory):
    files = []
    for version, head in HEADS.items():
        for wrap, template in WRAPS.items():
            if version == "2.0" and wrap in ("anyOf", "oneOf", "not"):
                continue
            for leaf, inner in LEAVES.items():
                for depth in (6, 7):
                    schema = inner
                    for _ in range(depth):
                        schema = template.format(inner=schema)
                    if version == "2.0":
                        body = head + "definitions:\n  A: " + schema + "\nx-target: {type: string}\n"
                    else:
                        body = head + "components:\n  schemas:\n    A: " + schema + "\nx-target: {type: string}\n"
                    path = os.path.join(directory, f"{version}-{wrap}-{leaf}-{depth}.yaml")
                    with open(path, "w") as f:
                        f.write(body)
                    files.append(path)
    return files


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--python", required=True)
    parser.add_argument("files", nargs="*")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        files = args.files or generated(directory)
        measured = {}
        out = subprocess.run(
            [args.python, os.path.join(HERE, "measure.py"), *files],
            capture_output=True, text=True, check=True,
        ).stdout
        for line in out.splitlines():
            record = json.loads(line)
            measured[record["file"]] = record["phases"]
        requests = []
        for path in files:
            phases = measured[path]
            at = [p["at"] for p in phases if p["phase"] == "meta" and p["max"] > p["root"]]
            requests.append(json.dumps({"file": path, "at": list(dict.fromkeys(at))}))
        predicted = subprocess.run(
            ["cargo", "run", "-q", "--release", "--example", "frames", "-p", "openapi-spec-validator-rs"],
            input="\n".join(requests) + "\n", capture_output=True, text=True, check=True, cwd=ROOT,
        ).stdout
        total = differences = 0
        for line in predicted.splitlines():
            record = json.loads(line)
            name = record["file"]
            if record["doc"] is None:
                continue
            for phase in measured[name]:
                # A generator closed by the garbage collector looks like a phase that
                # made no calls.
                if phase["phase"] == "crash" or phase["max"] == phase["root"]:
                    continue
                actual = phase["max"] - phase["root"]
                if phase["phase"] == "doc":
                    model = record["doc"]
                else:
                    model = record["meta"].get(phase["at"])
                total += 1
                if model != actual:
                    differences += 1
                    print(f"{name} {phase['phase']} {phase.get('at', '')}: upstream {actual}, model {model}")
        print(f"{total - differences}/{total} phases predicted exactly", file=sys.stderr)


if __name__ == "__main__":
    main()
