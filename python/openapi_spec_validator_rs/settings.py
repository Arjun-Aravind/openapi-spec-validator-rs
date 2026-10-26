"""Upstream's settings, for code that imports them.

openapi-spec-validator reads two environment variables with pydantic-settings:
``OPENAPI_SPEC_VALIDATOR_RESOLVED_CACHE_MAXSIZE`` and
``OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND``. This package validates the same way
whatever they say (always as upstream's default ``jsonschema`` backend); this module only
offers upstream's names, reading the variables as upstream does.
"""

from __future__ import annotations

import os
import warnings
from typing import Any

__all__ = [
    "ENV_PREFIX",
    "RESOLVED_CACHE_MAXSIZE_DEFAULT",
    "SCHEMA_VALIDATOR_BACKEND_ALLOWED",
    "SCHEMA_VALIDATOR_BACKEND_DEFAULT",
    "OpenAPISpecValidatorSettings",
    "get_resolved_cache_maxsize",
    "get_schema_validator_backend",
]

ENV_PREFIX = "OPENAPI_SPEC_VALIDATOR_"
RESOLVED_CACHE_MAXSIZE_DEFAULT = 128
SCHEMA_VALIDATOR_BACKEND_DEFAULT = "auto"
SCHEMA_VALIDATOR_BACKEND_ALLOWED = {
    "auto",
    "jsonschema",
    "jsonschema-rs",
}

_MISSING: Any = object()


def _environ(name: str) -> Any:
    # pydantic-settings matches variable names case-insensitively by default.
    wanted = (ENV_PREFIX + name).lower()
    for key, value in os.environ.items():
        if key.lower() == wanted:
            return value
    return _MISSING


def _cache_maxsize(value: Any) -> int:
    # `bool` is an `int` here, as for upstream's validator.
    if not isinstance(value, (int, str)):
        return RESOLVED_CACHE_MAXSIZE_DEFAULT
    try:
        parsed = int(value)
    except ValueError:
        return RESOLVED_CACHE_MAXSIZE_DEFAULT
    return RESOLVED_CACHE_MAXSIZE_DEFAULT if parsed < 0 else parsed


def _backend(value: Any) -> str:
    if value is None:
        return SCHEMA_VALIDATOR_BACKEND_DEFAULT
    if isinstance(value, str) and value.strip().lower() in SCHEMA_VALIDATOR_BACKEND_ALLOWED:
        return value.strip().lower()
    warnings.warn(
        "Invalid value for "
        "OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND. "
        "Expected one of: auto, jsonschema, jsonschema-rs. "
        "Falling back to auto.",
        UserWarning,
    )
    return SCHEMA_VALIDATOR_BACKEND_DEFAULT


class OpenAPISpecValidatorSettings:
    """Upstream's settings: keyword arguments, else the environment, else defaults."""

    resolved_cache_maxsize: int
    schema_validator_backend: str

    def __init__(
        self,
        *,
        resolved_cache_maxsize: Any = _MISSING,
        schema_validator_backend: Any = _MISSING,
        **_ignored: Any,
    ) -> None:
        # pydantic-settings takes keyword arguments only and ignores unknown ones.
        if resolved_cache_maxsize is _MISSING:
            resolved_cache_maxsize = _environ("resolved_cache_maxsize")
        if schema_validator_backend is _MISSING:
            schema_validator_backend = _environ("schema_validator_backend")
        self.resolved_cache_maxsize = (
            RESOLVED_CACHE_MAXSIZE_DEFAULT
            if resolved_cache_maxsize is _MISSING or resolved_cache_maxsize is None
            else _cache_maxsize(resolved_cache_maxsize)
        )
        self.schema_validator_backend = (
            SCHEMA_VALIDATOR_BACKEND_DEFAULT
            if schema_validator_backend is _MISSING
            else _backend(schema_validator_backend)
        )

    def __repr__(self) -> str:
        return (
            f"OpenAPISpecValidatorSettings(resolved_cache_maxsize="
            f"{self.resolved_cache_maxsize!r}, schema_validator_backend="
            f"{self.schema_validator_backend!r})"
        )


def get_resolved_cache_maxsize() -> int:
    return OpenAPISpecValidatorSettings().resolved_cache_maxsize


def get_schema_validator_backend() -> str:
    return OpenAPISpecValidatorSettings().schema_validator_backend
