"""Fetching the documents that ``http(s)://`` ``$ref``s point to.

Upstream resolves such references through jsonschema-path's default handlers, which use
``requests`` when that package is importable and ``urllib`` otherwise. This module does
the same, in the same interpreter, so proxies, certificates and decoding behave exactly
as they do for upstream. The validator calls :func:`retrieve` lazily, when it first
reaches a reference to a document, and once per document per validation.
"""

from __future__ import annotations

from contextlib import closing
from types import ModuleType
from typing import Final
from urllib.request import urlopen

__all__ = ["retrieve"]

#: The timeout jsonschema-path's handlers pass to ``urlopen`` and ``requests.get``.
TIMEOUT: Final = 10

_UNRESOLVED: Final = ModuleType("unresolved")
_requests: ModuleType | None = _UNRESOLVED


def _requests_module() -> ModuleType | None:
    """``requests`` when it is installed, looked up once like jsonschema-path does."""
    global _requests
    if _requests is _UNRESOLVED:
        try:
            import requests
        except ImportError:
            _requests = None
        else:
            _requests = requests
    return _requests


def retrieve(url: str) -> bytes:
    """The body of ``url``, fetched as jsonschema-path's default handlers fetch it.

    With ``requests`` installed the response is decoded as ``response.text`` (so a
    ``text/*`` body without a charset is read as ISO-8859-1, as upstream reads it);
    otherwise the raw ``urllib`` body is returned, to be decoded like a file. Any
    exception makes the reference unresolvable.
    """
    requests = _requests_module()
    if requests is not None:
        response = requests.get(url, timeout=TIMEOUT, verify=True)
        response.raise_for_status()
        return response.text.encode("utf-8")
    with closing(urlopen(url, timeout=TIMEOUT)) as response:
        return response.read()
