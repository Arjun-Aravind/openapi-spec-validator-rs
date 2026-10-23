"""Runs openapi-spec-validator's own test suite against this package.

See ``run_upstream_suite.py``; tests that need the network are left out here.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent


@pytest.mark.skipif(
    not (ROOT / "upstream" / "tests" / "integration").is_dir(),
    reason="upstream's sources are not checked out in ./upstream",
)
def test_upstream_suite() -> None:
    pytest.importorskip(
        "jsonschema_path", reason="upstream's test dependencies are missing"
    )
    result = subprocess.run(
        [
            sys.executable,
            str(ROOT / "tests" / "run_upstream_suite.py"),
            "-q",
            "-rfE",
            "-m",
            "not network",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stdout + result.stderr
