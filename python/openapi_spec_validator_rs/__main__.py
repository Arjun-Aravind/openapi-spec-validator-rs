"""Command-line entry point: ``openapi-spec-validator-rs`` / ``python -m openapi_spec_validator_rs``."""

import sys
from collections.abc import Sequence

from openapi_spec_validator_rs._core import cli_main


def main(args: Sequence[str] | None = None) -> None:
    """Run the validator CLI; exits with upstream-compatible status codes."""
    if args is None:
        args = sys.argv[1:]
    code, out, err = cli_main(list(args))
    if out:
        sys.stdout.write(out)
    if err:
        sys.stderr.write(err)
    sys.exit(code)


if __name__ == "__main__":
    main()
