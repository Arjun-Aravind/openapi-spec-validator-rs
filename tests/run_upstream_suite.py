"""Runs openapi-spec-validator's own test suite against this package.

    python tests/run_upstream_suite.py [pytest options]

Needs upstream's sources in ./upstream (tag 0.9.0) and its test dependencies, which
``pip install openapi-spec-validator==0.9.0`` provides. A shim package named
``openapi_spec_validator`` (``tests/upstream_shim``) serves upstream's import paths from
this package, shadowing the installed upstream. Tests that need the network carry
upstream's ``network`` marker: deselect them with ``-m "not network"``. Expected
failures and their reasons are listed in ``tests/upstream_shim/upstream_suite.py``.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHIM = ROOT / "tests" / "upstream_shim"
INTEGRATION = ROOT / "upstream" / "tests" / "integration"


def main(args: list[str]) -> int:
    if not INTEGRATION.is_dir():
        print(
            f"upstream's test suite was not found in {INTEGRATION}; clone it with\n"
            "  git clone --depth 1 --branch 0.9.0 "
            "https://github.com/python-openapi/openapi-spec-validator upstream",
            file=sys.stderr,
        )
        return 4
    sys.path.insert(0, str(SHIM))
    import pytest
    import upstream_suite

    return pytest.main(
        [
            "--rootdir",
            str(ROOT),
            "-c",
            str(ROOT / "pyproject.toml"),
            "-p",
            "no:cacheprovider",
            str(INTEGRATION),
            *args,
        ],
        plugins=[upstream_suite],
    )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
