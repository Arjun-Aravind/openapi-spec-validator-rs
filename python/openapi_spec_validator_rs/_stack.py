"""Running the extension's work from the caller's Python stack.

Upstream validates in Python, so how deep a document can nest before it raises
``RecursionError`` depends on how deep its caller already is and on
``sys.getrecursionlimit()``. The extension reproduces that limit from these two numbers.

Upstream also downloads ``http(s)://`` ``$ref`` targets in Python code on the calling
thread, which :func:`run` does for the extension.
"""

from __future__ import annotations

import sys
from types import FrameType
from typing import TYPE_CHECKING, Any

from openapi_spec_validator_rs import handlers

if TYPE_CHECKING:
    from openapi_spec_validator_rs._core import Task

_PACKAGE = __name__.rpartition(".")[0]

# Frames upstream's public entry points put between their caller and
# ``SpecValidator.iter_errors`` (measured on openapi-spec-validator 0.9.0).
_OFFSETS = {
    ("shortcuts", "validate"): 6,
    ("shortcuts", "validate_path"): 6,
    ("shortcuts", "validate_url"): 7,
    ("shortcuts", "validate_spec"): 6,
    ("shortcuts", "validate_spec_url"): 8,
    ("validators", "validate"): 5,
    ("validators", "is_valid"): 5,
    ("validators", "iter_errors"): 4,
    ("proxies", "validate"): 2,
    ("proxies", "is_valid"): 2,
    ("proxies", "iter_errors"): 1,
}
# `validate_spec(..., validator=...)` and `validate_spec_url(..., validator=...)` go
# through the deprecated proxy.
_THROUGH_PROXY = 3


def _depth(frame: FrameType | None) -> int:
    depth = 0
    while frame is not None:
        depth += 1
        frame = frame.f_back
    return depth


def spec_depth() -> int:
    """The depth upstream's ``SpecValidator.iter_errors`` frame would have for the call
    in progress (the outermost frame counts 1)."""
    frame = sys._getframe(1)
    depth = _depth(frame)
    entry, entry_depth = None, depth
    while frame is not None:
        module = frame.f_globals.get("__name__", "")
        if module != _PACKAGE and not module.startswith(_PACKAGE + "."):
            break
        entry, entry_depth = frame, depth
        frame = frame.f_back
        depth -= 1
    if entry is None:
        return depth + _OFFSETS[("shortcuts", "validate")]
    module = entry.f_globals.get("__name__", "").rpartition(".")[2]
    name = entry.f_code.co_name
    if name in ("validate_spec", "validate_spec_url") and entry.f_locals.get("validator") is not None:
        offset = _THROUGH_PROXY
    else:
        offset = _OFFSETS.get((module, name), _OFFSETS[("shortcuts", "validate")])
    return entry_depth - 1 + offset


def limit() -> int:
    return sys.getrecursionlimit()


def run(task: Task[Any]) -> Any:
    """The result of ``task``, downloading here the ``$ref`` targets it asks for.

    Only Python code is on this thread's stack while a download runs, as with upstream:
    an interrupt, or the interpreter ending a daemon thread at shutdown, then unwinds
    nothing of the extension's.
    """
    body = error = None
    try:
        while (url := task.step(body, error)) is not None:
            body = error = None
            try:
                body = handlers.retrieve(url)
            except Exception as exc:  # the reference is unresolvable, as upstream
                error = exc
        return task.result()
    finally:
        task.close()
