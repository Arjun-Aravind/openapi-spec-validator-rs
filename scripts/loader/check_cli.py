"""Replay every expected-error fixture through the real CLI to validate the in-process harness.

Usage: python -I scripts/loader/check_cli.py
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from upstream import run_cli  # noqa: E402

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "crates", "openapi-spec-validator-rs", "tests", "fixtures", "loader", "corpus.json")
corpus = json.load(open(path))
checked = mismatches = 0
for r in corpus:
    if "err" not in r:
        continue
    got = run_cli(bytes.fromhex(r["input"]), r["mode"])
    checked += 1
    if got != r["err"]:
        mismatches += 1
        print("MISMATCH", r["name"], r["mode"], repr(got)[:300], repr(r["err"])[:300])
print(f"{checked} error fixtures replayed through the CLI, {mismatches} mismatches")
sys.exit(1 if mismatches else 0)
