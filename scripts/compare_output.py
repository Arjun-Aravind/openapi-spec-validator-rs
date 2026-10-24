"""Check that the CLI prints exactly what the reference CLI prints.

Runs both CLIs with the same arguments (from the CLI crate's directory, so the
paths match the snapshot tests) and diffs stdout, stderr and the exit status.

Usage:
    python scripts/compare_output.py --reference PATH_TO_REFERENCE_CLI --candidate PATH_TO_CLI
"""

import argparse
import difflib
import os
import shlex
import subprocess
import sys

CASES = [
    "valid.yaml",
    "swagger.yaml",
    "missing-response-description.yaml",
    "--subschema-errors all missing-response-description.yaml",
    "many-errors.yaml",
    "--validation-errors all many-errors.yaml",
    "bad-regex.yaml",
    "crash-path-template.yaml",
    "crash-version.yaml",
    "broken.yaml",
    "split/openapi.yaml",
    "valid.yaml swagger.yaml many-errors.yaml valid.yaml",
    "--schema 3.1 valid.yaml",
    "does-not-exist.yaml",
    "--errors all valid.yaml",
    "--validation-errors all oas31-defaults.yaml",
    "oas31-malformed-ref-target.yaml",
    "--validation-errors all oas31-malformed-ref-target.yaml",
    "swagger-parameter.yaml",
]

CLI_DIR = os.path.join(os.path.dirname(__file__), "..", "crates", "openapi-spec-validator-rs-cli")


def run(command, args, env):
    argv = [a if not a.endswith(".yaml") else f"tests/fixtures/{a}" for a in shlex.split(args)]
    result = subprocess.run([*command, *argv], capture_output=True, text=True, cwd=CLI_DIR, env=env)
    return f"exit code: {result.returncode}\n--- stdout\n{result.stdout}--- stderr\n{result.stderr}"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", required=True, type=lambda s: [os.path.abspath(p) if os.sep in p else p for p in shlex.split(s)])
    parser.add_argument("--candidate", required=True, type=lambda s: [os.path.abspath(p) if os.sep in p else p for p in shlex.split(s)])
    args = parser.parse_args()
    env = dict(os.environ, OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND="jsonschema")
    failures = 0
    for case in CASES:
        expected = run(args.reference, case, env)
        actual = run(args.candidate, case, env)
        if expected == actual:
            print(f"same  {case}")
        else:
            failures += 1
            print(f"DIFF  {case}")
            sys.stdout.writelines(difflib.unified_diff(
                expected.splitlines(keepends=True), actual.splitlines(keepends=True),
                "reference", "candidate",
            ))
    print(f"\n{len(CASES) - failures}/{len(CASES)} identical")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
