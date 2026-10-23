"""Command-line entry point: ``openapi-spec-validator-rs`` / ``python -m openapi_spec_validator_rs``."""

import sys
from collections.abc import Sequence

from openapi_spec_validator_rs._core import cli_main


def main(args: Sequence[str] | None = None) -> None:
    """Run the validator CLI, with upstream's exit behaviour.

    Returns normally when every file is valid. Otherwise raises ``SystemExit`` with
    status 1 (read or validation error) or 2 (any other error); argument errors,
    ``--help`` and ``--version`` also exit, as with argparse.
    """
    if args is None:
        args = sys.argv[1:]
    code, out, err, ran = cli_main(list(args))
    if out:
        sys.stdout.write(out)
    if err:
        sys.stderr.write(err)
    if code != 0 or not ran:
        sys.exit(code)


if __name__ == "__main__":
    main()
