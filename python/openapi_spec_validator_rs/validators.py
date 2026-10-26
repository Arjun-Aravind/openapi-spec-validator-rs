"""Spec validator classes, one per specification version, as upstream's."""

from __future__ import annotations

import warnings
from collections.abc import Iterator, Mapping
from typing import Any, ClassVar

from openapi_spec_validator_rs import _core, _stack, handlers
from openapi_spec_validator_rs.exceptions import OpenAPIValidationError, _from_core
from openapi_spec_validator_rs.versions import (
    OPENAPIV2,
    OPENAPIV30,
    OPENAPIV31,
    OPENAPIV32,
    SpecVersion,
)

__all__ = [
    "OpenAPIV2SpecValidator",
    "OpenAPIV30SpecValidator",
    "OpenAPIV31SpecValidator",
    "OpenAPIV32SpecValidator",
    "OpenAPIV3SpecValidator",
    "SpecValidator",
    "SpecValidatorType",
]


class SpecValidator:
    """Validates one spec document as the version fixed by the subclass.

    ``schema`` is the parsed document (``dict``, ``list``, ``str``, ``int``, ``float``,
    ``bool``, ``None``; tuples count as lists) or a jsonschema-path ``SchemaPath``.
    ``base_uri`` is the document's location, used to resolve relative ``$ref``s to
    other documents (e.g. ``file:///path/to/openapi.yaml``). ``http(s)://`` documents
    are downloaded as upstream downloads them (see :mod:`~openapi_spec_validator_rs.handlers`).

    Every call validates the document as it is at that moment.
    """

    spec_version: ClassVar[SpecVersion]
    # Upstream's classes check a plain mapping as given; its `validate()` shortcut wraps
    # it in a `SchemaPath`, which follows a `$ref` at the root.
    _follow_root_ref: bool = False

    def __init__(
        self,
        schema: Any,
        base_uri: str = "",
        spec_url: str | None = None,
    ) -> None:
        if spec_url is not None:
            warnings.warn(
                "spec_url parameter is deprecated. Use base_uri instead.",
                DeprecationWarning,
                stacklevel=2,
            )
            base_uri = spec_url
        read_value = getattr(schema, "read_value", None)
        if callable(read_value) and not isinstance(schema, Mapping):
            # A jsonschema-path `SchemaPath`: like upstream, resolve references
            # against its own base URI.
            base_uri = schema.base_uri
            root = getattr(getattr(schema, "accessor", None), "node", None)
            if not getattr(schema, "parts", ()) and root is not None:
                # Its value follows a `$ref` at the root, which upstream checks against
                # the meta-schema, while its walk resolves references from the root
                # document itself.
                schema = root
                self._follow_root_ref = True
            else:
                schema = read_value()
        self.schema = schema
        self.base_uri = base_uri

    def validate(self) -> None:
        """Raises the first error, if any."""
        for error in self._errors(all_errors=False):
            raise error

    def is_valid(self) -> bool:
        return not self._errors(all_errors=False)

    def iter_errors(self) -> Iterator[OpenAPIValidationError]:
        """Yields every error, in upstream's order.

        Validation runs when iteration starts. A failure that stops validation
        (:class:`~openapi_spec_validator_rs.exceptions.ValidationAbortedError`) is raised
        before any error is yielded.
        """
        yield from self._errors(all_errors=True)

    def _errors(self, all_errors: bool) -> list[OpenAPIValidationError]:
        version = self.spec_version
        errors = _core.validate(
            self.schema,
            f"{version.major}.{version.minor}",
            self.base_uri,
            all_errors,
            handlers.retrieve,
            _stack.spec_depth(),
            _stack.limit(),
            follow_root_ref=self._follow_root_ref,
        )
        return [_from_core(error) for error in errors]


class OpenAPIV2SpecValidator(SpecValidator):
    """Swagger 2.0."""

    spec_version = OPENAPIV2


class OpenAPIV30SpecValidator(SpecValidator):
    """OpenAPI 3.0.x."""

    spec_version = OPENAPIV30


class OpenAPIV31SpecValidator(SpecValidator):
    """OpenAPI 3.1.x."""

    spec_version = OPENAPIV31


class OpenAPIV32SpecValidator(SpecValidator):
    """OpenAPI 3.2.x."""

    spec_version = OPENAPIV32


# Alias to the latest 3.x version, as upstream.
OpenAPIV3SpecValidator = OpenAPIV32SpecValidator

SpecValidatorType = type[SpecValidator]
