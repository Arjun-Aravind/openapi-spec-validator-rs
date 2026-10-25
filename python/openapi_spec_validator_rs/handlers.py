"""Fetching the documents that ``http(s)://`` ``$ref``s point to.

Upstream resolves such references through jsonschema-path's default handlers, which use
``requests`` when that package is importable and ``urllib`` otherwise. This module does
the same, in the same interpreter, so proxies, certificates and decoding behave exactly
as they do for upstream. The validator calls :func:`retrieve` lazily, when it first
reaches a reference to a document, and once per document per validation.
"""

from __future__ import annotations

import codecs
import re
from contextlib import closing
from types import ModuleType
from typing import IO, Final

__all__ = ["read_stream", "retrieve"]

#: The timeout jsonschema-path's handlers pass to ``urlopen`` and ``requests.get``.
TIMEOUT: Final = 10

_CHUNK_SIZE: Final = 16 * 1024
# What libyaml refuses to decode ("control characters are not allowed"): everything but
# tab, line feed, carriage return, printable ASCII, NEL and the non-control Unicode ranges.
_UNREADABLE: Final = re.compile("[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x84\x86-\x9f\ufffe\uffff]")


def read_stream(stream: IO[bytes], text: bool = False) -> bytes:
    """Reads a binary stream only as far as PyYAML's reader would get.

    PyYAML parses a stream as it reads it, so a document libyaml cannot even decode fails
    at the first offending bytes, however much follows them: upstream gives up on
    ``/dev/zero`` after one buffer. Reading stops at the first bytes libyaml would refuse
    (a sequence that is not UTF-8 or UTF-16, or a character YAML does not allow), which
    the parser then meets and reports as upstream does; everything else is read whole.
    The encoding is detected as libyaml detects it (UTF-16 after a byte-order mark),
    except for the bytes behind a text stream (``text=True``, ``sys.stdin``), which Python
    decodes as UTF-8 before PyYAML sees them.
    """
    data = bytearray()
    decoder = None if not text else codecs.getincrementaldecoder("utf-8")()
    checked = 0
    while chunk := stream.read(_CHUNK_SIZE):
        data += chunk
        if decoder is None:
            if len(data) < 2:
                continue
            if data[:2] == b"\xff\xfe":
                decoder = codecs.getincrementaldecoder("utf-16-le")()
                checked = 2
            elif data[:2] == b"\xfe\xff":
                decoder = codecs.getincrementaldecoder("utf-16-be")()
                checked = 2
            else:
                decoder = codecs.getincrementaldecoder("utf-8")()
        try:
            # The decoder keeps an incomplete character for the next chunk.
            decoded = decoder.decode(data[checked:])
        except UnicodeDecodeError:
            break
        checked = len(data)
        if _UNREADABLE.search(decoded):
            break
    return bytes(data)


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
    # Imported here: urllib.request costs tens of milliseconds at start-up and most runs
    # never fetch anything.
    from urllib.request import urlopen

    requests = _requests_module()
    if requests is not None:
        response = requests.get(url, timeout=TIMEOUT, verify=True)
        response.raise_for_status()
        return response.text.encode("utf-8")
    with closing(urlopen(url, timeout=TIMEOUT)) as response:
        # Read in chunks as PyYAML reads a stream: a body cut short by the server is
        # used as far as it goes, where `read()` would raise `IncompleteRead`.
        return read_stream(response)
