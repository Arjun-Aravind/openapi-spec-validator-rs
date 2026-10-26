"""The meta-schemas documents are validated against, as upstream's ``schemas`` module
offers them (``schema_v2``, ``schema_v30``, ``schema_v31``, ``schema_v32`` and
``schema_v3``, the latest 3.x).

Each is loaded on first use. Upstream's ``jsonschema`` validator objects in that module
(``openapi_v30_schema_validator`` and the like) are not offered: this package does not
depend on ``jsonschema``.
"""

from __future__ import annotations

import json
from typing import Any

from openapi_spec_validator_rs import _core

__all__ = ["schema_v2", "schema_v3", "schema_v30", "schema_v31", "schema_v32"]

_VERSIONS = {
    "schema_v2": "2.0",
    "schema_v30": "3.0",
    "schema_v31": "3.1",
    "schema_v32": "3.2",
    "schema_v3": "3.2",
}


def __getattr__(name: str) -> Any:
    version = _VERSIONS.get(name)
    if version is None:
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    # `schema_v3` is the very object `schema_v32` is.
    loaded = {"2.0": "schema_v2", "3.0": "schema_v30", "3.1": "schema_v31", "3.2": "schema_v32"}
    canonical = loaded[version]
    schema = globals().get(canonical)
    if schema is None:
        schema = json.loads(_core._meta_schema(version))
        globals()[canonical] = schema
    globals()[name] = schema
    return schema


def __dir__() -> list[str]:
    return sorted({*globals(), *_VERSIONS})
