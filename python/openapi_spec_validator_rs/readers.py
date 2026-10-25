"""Reading spec files, as upstream's ``readers`` module.

Each function returns the parsed document and the base URI to resolve its ``$ref``s
against, ready for :func:`~openapi_spec_validator_rs.validate`::

    from openapi_spec_validator_rs import validate
    from openapi_spec_validator_rs.readers import read_from_filename

    spec_dict, base_uri = read_from_filename("openapi.yaml")
    validate(spec_dict, base_uri=base_uri)

The file is parsed exactly as the command line parses it (PyYAML's rules, timestamps
left as strings), so the result holds only ``dict``, ``list``, ``str``, ``int``,
``float``, ``bool`` and ``None``.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any

from openapi_spec_validator_rs import _core, handlers

__all__ = ["read_from_filename", "read_from_stdin"]


def read_from_filename(filename: str | os.PathLike[str]) -> tuple[Any, str]:
    """The document in ``filename`` and its ``file://`` URI.

    Raises ``OSError`` with upstream's message (``No such file: ...``) when ``filename``
    is not a file, and :class:`~openapi_spec_validator_rs.SpecReadError` when it cannot
    be parsed.
    """
    filename = os.fspath(filename)
    if not os.path.isfile(filename):
        raise OSError(f"No such file: {filename}")
    uri = Path(os.path.abspath(filename)).as_uri()
    with open(filename, "rb") as stream:
        data = handlers.read_stream(stream)
    return _core.load(data, "<urllib response>"), uri


def read_from_stdin(filename: str = "") -> tuple[Any, str]:
    """The document on standard input and an empty base URI (``filename`` is ignored,
    as upstream ignores it)."""
    del filename
    stream = getattr(sys.stdin, "buffer", sys.stdin)
    data = handlers.read_stream(stream)
    if isinstance(data, str):
        data = data.encode("utf-8", "surrogateescape")
    return _core.load(data, "<stdin>"), ""
