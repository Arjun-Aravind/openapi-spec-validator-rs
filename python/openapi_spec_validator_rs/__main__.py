"""Command-line entry point: ``openapi-spec-validator-rs`` / ``python -m openapi_spec_validator_rs``."""

import sys
from collections.abc import Sequence

from openapi_spec_validator_rs import _stack, handlers
from openapi_spec_validator_rs._core import cli_main


def main(args: Sequence[str] | None = None) -> None:
    """Run the validator CLI, with upstream's exit behaviour.

    Returns normally when every file is valid. Otherwise raises ``SystemExit`` with
    status 1 (read or validation error) or 2 (any other error); argument errors,
    ``--help`` and ``--version`` also exit, as with argparse.
    """
    if args is None:
        args = sys.argv[1:]
    args = list(args)
    code, out, err, ran = _stack.run(cli_main(args, _replaced_stdin(args), handlers.retrieve))
    if out:
        sys.stdout.write(out)
    if err:
        sys.stderr.write(err)
    if code != 0 or not ran:
        sys.exit(code)


def _replaced_stdin(args: list[str]) -> bytes | None:
    """The content of ``sys.stdin`` when it is not the process's standard input.

    The command line reads ``-`` from file descriptor 0; when ``sys.stdin`` has been
    replaced (as in tests), its content is passed along instead, as upstream reads
    ``sys.stdin`` itself.
    """
    if not any(arg in ("-", "/-") for arg in args) or sys.stdin is sys.__stdin__:
        return None
    buffer = getattr(sys.stdin, "buffer", None)
    if buffer is not None:
        return handlers.read_stream(buffer, text=True)
    data = sys.stdin.read()
    if isinstance(data, bytes):
        return data
    return data.encode("utf-8", "surrogateescape")


if __name__ == "__main__":
    main()
