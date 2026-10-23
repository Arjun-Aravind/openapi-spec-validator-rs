"""Upstream's pydantic settings: no counterpart in this package.

The resolver cache size and the schema backend choice do not apply to the Rust
validator. The constant exists so that test modules importing it can be collected.
"""

from typing import Any

RESOLVED_CACHE_MAXSIZE_DEFAULT = 128


class OpenAPISpecValidatorSettings:
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        raise NotImplementedError("openapi-spec-validator-rs has no settings")
