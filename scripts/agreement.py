"""Measure verdict agreement with the reference openapi-spec-validator on a corpus.

Runs the reference CLI and this project's CLI on every spec file under the given
directories (in parallel), and reports how often they agree on:

* the verdict: exit status plus error class (OK, a specific validation error
  class, a non-validation error, or a read error);
* the exact first line of output.

Reference results are cached in a JSON Lines file, so re-running after a change
only re-runs the candidate.

Usage:
    python scripts/agreement.py --reference "path/to/openapi-spec-validator" \
        --candidate target/release/openapi-spec-validator-rs \
        --cache bench/results/reference.jsonl --report bench/results/agreement.md \
        bench/data/openapi-directory/APIs
"""

import argparse
import collections
import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

ERROR_CLASSES = [
    ("DuplicateOperationIDError", re.compile(r"^Operation ID '.*' for '.*' in '.*' is not unique")),
    ("ParameterDuplicateError", re.compile(r"^Duplicate parameter '")),
    ("UnresolvableParameterError", re.compile(r"^Path parameter '.*' for '.*' operation in '.*' was not resolved")),
    ("ExtraParametersError", re.compile(r"^Required list has not defined properties: ")),
]


def classify(returncode, stdout, path):
    """Reduce one CLI run to a comparable verdict and its first output line."""
    lines = stdout.splitlines()
    first = lines[0] if lines else ""
    # Normalise the file name so both tools' lines compare equal.
    first = first.replace(path, "<file>", 1)
    if returncode == 0:
        return "OK", first
    if returncode in (124, -9):
        return "Timeout", first
    if ": Validation Error: " in first:
        message = first.split(": Validation Error: ", 1)[1]
        for name, pattern in ERROR_CLASSES:
            if pattern.search(message):
                return name, first
        return "OpenAPIValidationError", first
    if ": Error: " in first:
        return "Error", first
    if returncode == 1:
        return "ReadError", first
    return f"exit {returncode}", first


def run(command, path, env, timeout):
    start = time.perf_counter()
    try:
        result = subprocess.run(
            [*command, path], capture_output=True, text=True, env=env,
            timeout=timeout, errors="replace",
        )
        verdict, first = classify(result.returncode, result.stdout, path)
    except subprocess.TimeoutExpired:
        verdict, first = "Timeout", ""
    return {"verdict": verdict, "first": first, "seconds": time.perf_counter() - start}


def file_key(path):
    stat = os.stat(path)
    return hashlib.sha1(f"{path}:{stat.st_size}:{stat.st_mtime_ns}".encode()).hexdigest()


def collect(roots):
    files = []
    for root in roots:
        if os.path.isfile(root):
            files.append(root)
            continue
        for directory, _, names in os.walk(root):
            for name in names:
                if name.endswith((".yaml", ".yml", ".json")):
                    files.append(os.path.join(directory, name))
    return sorted(files)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--reference", required=True, type=shlex.split)
    parser.add_argument("--candidate", required=True, type=shlex.split)
    parser.add_argument("--backend", default="jsonschema",
                        help="reference schema validator backend (default: jsonschema)")
    parser.add_argument("--cache", required=True, help="JSON Lines cache of reference results")
    parser.add_argument("--report", help="write a Markdown report here")
    parser.add_argument("--jobs", type=int, default=os.cpu_count())
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("roots", nargs="+")
    args = parser.parse_args()

    files = collect(args.roots)
    env = dict(os.environ, OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND=args.backend)

    cache = {}
    if os.path.exists(args.cache):
        with open(args.cache) as fh:
            for line in fh:
                entry = json.loads(line)
                cache[(entry["key"], entry["backend"])] = entry
    os.makedirs(os.path.dirname(os.path.abspath(args.cache)), exist_ok=True)

    missing = [f for f in files if (file_key(f), args.backend) not in cache]
    print(f"{len(files)} specs; running the reference on {len(missing)} uncached", file=sys.stderr)
    with open(args.cache, "a") as out, ThreadPoolExecutor(args.jobs) as pool:
        done = 0
        for path, result in zip(missing, pool.map(lambda f: run(args.reference, f, env, args.timeout), missing)):
            entry = {"key": file_key(path), "backend": args.backend, "path": path, **result}
            cache[(entry["key"], args.backend)] = entry
            out.write(json.dumps(entry) + "\n")
            done += 1
            if done % 200 == 0:
                out.flush()
                print(f"  reference: {done}/{len(missing)}", file=sys.stderr)

    with ThreadPoolExecutor(args.jobs) as pool:
        candidate = dict(zip(files, pool.map(lambda f: run(args.candidate, f, env, args.timeout), files)))

    verdict_agree = line_agree = 0
    mismatches = collections.defaultdict(list)
    line_mismatches = []
    by_verdict = collections.Counter()
    ref_seconds = cand_seconds = 0.0
    for path in files:
        ref = cache[(file_key(path), args.backend)]
        cand = candidate[path]
        ref_seconds += ref["seconds"]
        cand_seconds += cand["seconds"]
        by_verdict[ref["verdict"]] += 1
        if ref["verdict"] == cand["verdict"]:
            verdict_agree += 1
            if ref["first"] == cand["first"]:
                line_agree += 1
            else:
                line_mismatches.append((path, ref["first"], cand["first"]))
        else:
            mismatches[(ref["verdict"], cand["verdict"])].append((path, ref["first"], cand["first"]))

    total = len(files)
    lines = [
        f"# Agreement with openapi-spec-validator ({args.backend} backend)",
        "",
        f"* Specs: {total}",
        f"* Same verdict (exit status and error class): **{verdict_agree}/{total} "
        f"({100.0 * verdict_agree / total:.2f}%)**",
        f"* Same first output line: {line_agree}/{total} ({100.0 * line_agree / total:.2f}%)",
        f"* Total time per file (sum): reference {ref_seconds:.0f} s, candidate {cand_seconds:.1f} s",
        "",
        "## Reference verdicts",
        "",
        "| Verdict | Specs |",
        "|---|---:|",
        *(f"| {v} | {n} |" for v, n in by_verdict.most_common()),
        "",
        "## Mismatches",
        "",
    ]
    if not mismatches:
        lines.append("None.")
    for (ref_v, cand_v), items in sorted(mismatches.items(), key=lambda kv: -len(kv[1])):
        lines.append(f"### reference {ref_v} / candidate {cand_v} ({len(items)})")
        lines.append("")
        for path, ref_first, cand_first in items:
            lines.append(f"* `{path}`")
            lines.append(f"  * reference: `{ref_first[:300]}`")
            lines.append(f"  * candidate: `{cand_first[:300]}`")
        lines.append("")
    lines += [
        "## Same verdict, different first line",
        "",
        f"{len(line_mismatches)} specs. Messages differ only in wording or in which of several errors is reported first.",
        "",
    ]
    for path, ref_first, cand_first in line_mismatches:
        lines.append(f"* `{path}`")
        lines.append(f"  * reference: `{ref_first[:300]}`")
        lines.append(f"  * candidate: `{cand_first[:300]}`")
    report = "\n".join(lines)
    if args.report:
        os.makedirs(os.path.dirname(os.path.abspath(args.report)), exist_ok=True)
        with open(args.report, "w") as fh:
            fh.write(report)
    print("\n".join(lines[:6]))
    for (ref_v, cand_v), items in sorted(mismatches.items(), key=lambda kv: -len(kv[1])):
        print(f"  {len(items):5d}  reference {ref_v} / candidate {cand_v}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
