"""Compare this tool's verdicts with the reference openapi-spec-validator.

For each spec file, runs both CLIs and compares the exit status and the error
category/class of the first line of output.

Usage:
    python scripts/compare.py --reference PATH_TO_REFERENCE_CLI --candidate PATH_TO_CLI FILES...
"""

import argparse
import os
import re
import shlex
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

ERROR_CLASS_PATTERNS = [
    ("DuplicateOperationIDError", re.compile(r"Operation ID '.*' for '.*' in '.*' is not unique")),
    ("ParameterDuplicateError", re.compile(r"Duplicate parameter '")),
    ("UnresolvableParameterError", re.compile(r"Path parameter '.*' for '.*' operation in '.*' was not resolved")),
    ("ExtraParametersError", re.compile(r"Required list has not defined properties: ")),
]


def classify(exit_code, output):
    """Reduce a CLI run to (exit code, category)."""
    first = output.splitlines()[0] if output else ""
    if exit_code == 0:
        return exit_code, "OK"
    if ": Validation Error: " in first:
        message = first.split(": Validation Error: ", 1)[1]
        for name, pattern in ERROR_CLASS_PATTERNS:
            if pattern.search(message):
                return exit_code, name
        return exit_code, "OpenAPIValidationError"
    if ": Error: " in first:
        return exit_code, "Error"
    return exit_code, "ReadError"


def run(cli, path, env):
    result = subprocess.run(
        [*cli, path], capture_output=True, text=True, env=env, timeout=600
    )
    return classify(result.returncode, result.stdout), result.stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", required=True, type=shlex.split, help="reference CLI command")
    parser.add_argument("--candidate", required=True, type=shlex.split, help="candidate CLI command")
    parser.add_argument("--jobs", type=int, default=os.cpu_count())
    parser.add_argument("--show-output", action="store_true")
    parser.add_argument("files", nargs="+")
    args = parser.parse_args()

    env = dict(os.environ, OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND="jsonschema")

    def compare(path):
        ref, ref_out = run(args.reference, path, env)
        cand, cand_out = run(args.candidate, path, env)
        return path, ref, cand, ref_out, cand_out

    agree = 0
    with ThreadPoolExecutor(args.jobs) as pool:
        for path, ref, cand, ref_out, cand_out in pool.map(compare, args.files):
            same = ref == cand
            agree += same
            mark = "same" if same else "DIFF"
            print(f"{mark}  ref={ref[0]}:{ref[1]:<28} cand={cand[0]}:{cand[1]:<28} {path}")
            if not same or args.show_output:
                for label, text in (("reference", ref_out), ("candidate", cand_out)):
                    head = "\n".join(text.splitlines()[:3])
                    print(f"    {label}: {head[:400]}")
    total = len(args.files)
    print(f"\n{agree}/{total} agree ({100.0 * agree / total:.1f}%)")
    return 0 if agree == total else 1


if __name__ == "__main__":
    sys.exit(main())
