"""Exceptions, with the names and hierarchy of openapi-spec-validator's.

Upstream's ``OpenAPIValidationError`` subclasses ``jsonschema.exceptions.ValidationError``.
This package does not depend on jsonschema: :class:`ValidationError` stands in for that
base class, with the same attributes and the same ``str()``.
"""

from __future__ import annotations

import re
from collections import deque
from collections.abc import Iterable, Sequence
from textwrap import dedent, indent
from typing import TYPE_CHECKING, Any, Final, cast

if TYPE_CHECKING:
    from openapi_spec_validator_rs._core import CoreError

__all__ = [
    "DuplicateOperationIDError",
    "ExtraParametersError",
    "OpenAPIError",
    "OpenAPISpecValidatorError",
    "OpenAPIValidationError",
    "OpenAPIVersionNotFound",
    "ParameterDuplicateError",
    "SpecReadError",
    "UnresolvableParameterError",
    "ValidationAbortedError",
    "ValidationError",
    "ValidatorDetectError",
]


class _Unset:
    """jsonschema's marker for error fields that do not apply to an error."""

    def __repr__(self) -> str:
        return "<unset>"


_unset: Final = _Unset()

_JSON_PATH_COMPATIBLE_PROPERTY_PATTERN = re.compile("^[a-zA-Z][a-zA-Z0-9_]*$")


class OpenAPIError(Exception):
    """Base class of the errors that are not validation errors."""


class OpenAPISpecValidatorError(OpenAPIError):
    """Base class of this package's non-validation errors."""


class ValidatorDetectError(OpenAPISpecValidatorError):
    """No supported ``swagger`` / ``openapi`` version was found in the document.

    Raised without a message, as upstream does.
    """


class OpenAPIVersionNotFound(OpenAPIError):
    """Raised by :func:`~openapi_spec_validator_rs.versions.get_spec_version`."""

    def __str__(self) -> str:
        return "Specification version not found"


class ValidationAbortedError(OpenAPISpecValidatorError):
    """Validation stopped before reaching a verdict.

    Upstream raises a non-validation exception at these points, such as
    ``referencing.exceptions.Unresolvable`` for a ``$ref`` that cannot be resolved,
    ``RecursionError`` for reference cycles, or ``ValueError`` for a malformed path
    template. The message is the text of upstream's exception (what its CLI prints after
    ``Error:``).
    """


class SpecReadError(OpenAPISpecValidatorError):
    """The document could not be parsed (upstream raises PyYAML's or ``json``'s exception).

    The message is the text of upstream's exception.
    """


class ValidationError(Exception):
    """A value is invalid under a schema: the counterpart of jsonschema's ``ValidationError``.

    Errors produced by the validator convert ``schema``, ``instance``, the paths and
    ``context`` from the validator's data on first access.
    """

    message: str
    validator: Any
    validator_value: Any
    instance: Any
    schema: Any
    path: deque[str | int]
    relative_path: deque[str | int]
    schema_path: deque[str | int]
    relative_schema_path: deque[str | int]
    context: list[ValidationError]
    cause: BaseException | None
    parent: ValidationError | None

    def __init__(
        self,
        message: str,
        validator: Any = _unset,
        path: Iterable[str | int] = (),
        cause: BaseException | None = None,
        context: Iterable[ValidationError] = (),
        validator_value: Any = _unset,
        instance: Any = _unset,
        schema: Any = _unset,
        schema_path: Iterable[str | int] = (),
        parent: ValidationError | None = None,
    ) -> None:
        super().__init__(message)
        self._core: CoreError | None = None
        self._str: str | None = None
        self.message = message
        self.path = self.relative_path = deque(path)
        self.schema_path = self.relative_schema_path = deque(schema_path)
        self.context = list(context)
        self.cause = self.__cause__ = cause
        self.validator = validator
        self.validator_value = validator_value
        self.instance = instance
        self.schema = schema
        self.parent = parent
        for error in self.context:
            error.parent = self

    @classmethod
    def _from_core(
        cls, core: CoreError, parent: ValidationError | None = None
    ) -> ValidationError:
        error = cls.__new__(cls)
        Exception.__init__(error, core.message)
        error._core = core
        error._str = None
        error.message = core.message
        error.parent = parent
        # Set eagerly so that tracebacks show it, like jsonschema's `raise ... from cause`.
        cause = core.cause
        error.cause = error.__cause__ = None if cause is None else Exception(cause)
        return error

    def __getattr__(self, name: str) -> Any:
        # Only called for attributes not set yet: the lazily converted fields.
        core = self.__dict__.get("_core")
        if core is None:
            raise AttributeError(
                f"{type(self).__name__!r} object has no attribute {name!r}"
            )
        value: Any
        if name in ("path", "relative_path"):
            value = deque(core.instance_path)
            self.__dict__["path"] = self.__dict__["relative_path"] = value
        elif name in ("schema_path", "relative_schema_path"):
            value = deque(core.schema_path)
            self.__dict__["schema_path"] = self.__dict__["relative_schema_path"] = value
        elif name == "validator":
            value = core.keyword if core.has_details else _unset
        elif name == "validator_value":
            if not core.has_details:
                value = _unset
            elif isinstance(self.schema, dict) and self.validator in self.schema:
                value = self.schema[self.validator]
            else:
                # A `false` schema: jsonschema sets `validator` and `validator_value` to None.
                value = None
        elif name == "instance":
            value = core.instance() if core.has_details else _unset
        elif name == "schema":
            value = core.schema() if core.has_details else _unset
        elif name == "context":
            value = [ValidationError._from_core(c, parent=self) for c in core.context()]
        else:
            raise AttributeError(
                f"{type(self).__name__!r} object has no attribute {name!r}"
            )
        self.__dict__[name] = value
        return value

    def __repr__(self) -> str:
        return f"<{type(self).__name__}: {self.message!r}>"

    def __str__(self) -> str:
        # Errors from the validator render once, in Rust (and keep that text when
        # pickled); errors built in Python render from their current fields.
        if self._str is None and self._core is not None:
            self._str = self._core.render()
        return self._str if self._str is not None else self._render()

    def _render(self) -> str:
        """jsonschema's ``str()`` for errors built in Python."""
        essential_for_verbose = (
            self.validator,
            self.validator_value,
            self.instance,
            self.schema,
        )
        if any(m is _unset for m in essential_for_verbose):
            return self.message
        schema_path = _format_as_index("schema", list(self.relative_schema_path)[:-1])
        instance_path = _format_as_index("instance", self.relative_path)
        prefix = 16 * " "
        return dedent(
            f"""\
            {self.message}

            Failed validating {self.validator!r} in {schema_path}:
                {_pretty(self.schema, prefix=prefix)}

            On {instance_path}:
                {_pretty(self.instance, prefix=prefix)}
            """.rstrip(),
        )

    def __reduce__(self) -> tuple[Any, ...]:
        # Materialize the lazy fields: the validator's data cannot be pickled.
        state = {
            name: getattr(self, name)
            for name in (
                "message",
                "path",
                "relative_path",
                "schema_path",
                "relative_schema_path",
                "context",
                "cause",
                "validator",
                "validator_value",
                "instance",
                "schema",
                "parent",
            )
        }
        state["__cause__"] = self.__cause__
        state["_core"] = None
        state["_str"] = str(self) if self._core is not None else self._str
        return (_new_error, (type(self), self.message), state)

    @property
    def absolute_path(self) -> Sequence[str | int]:
        parent = self.parent
        if parent is None:
            return self.relative_path
        path = deque(self.relative_path)
        path.extendleft(reversed(parent.absolute_path))
        return path

    @property
    def absolute_schema_path(self) -> Sequence[str | int]:
        parent = self.parent
        if parent is None:
            return self.relative_schema_path
        path = deque(self.relative_schema_path)
        path.extendleft(reversed(parent.absolute_schema_path))
        return path

    @property
    def json_path(self) -> str:
        path = "$"
        for elem in self.absolute_path:
            if isinstance(elem, int):
                path += "[" + str(elem) + "]"
            elif _JSON_PATH_COMPATIBLE_PROPERTY_PATTERN.match(elem):
                path += "." + elem
            else:
                escaped_elem = elem.replace("\\", "\\\\").replace("'", r"\'")
                path += "['" + escaped_elem + "']"
        return path


class OpenAPIValidationError(ValidationError):
    """The document is invalid: meta-schema violations, invalid schemas or ``default``
    values, duplicate tags, ... Subclasses mark upstream's dedicated checks."""


class ExtraParametersError(OpenAPIValidationError):
    """A schema's ``required`` lists properties it does not define."""


class ParameterDuplicateError(OpenAPIValidationError):
    """Two parameters share the same ``name`` and ``in``."""


class UnresolvableParameterError(OpenAPIValidationError):
    """Path template parameters and declared path parameters disagree."""


class DuplicateOperationIDError(OpenAPIValidationError):
    """An ``operationId`` is used more than once."""


_ERROR_CLASSES: Final[dict[str, type[OpenAPIValidationError]]] = {
    cls.__name__: cls
    for cls in (
        OpenAPIValidationError,
        ExtraParametersError,
        ParameterDuplicateError,
        UnresolvableParameterError,
        DuplicateOperationIDError,
    )
}


def _from_core(core: CoreError) -> OpenAPIValidationError:
    """The exception for a top-level error reported by the validator."""
    return cast(OpenAPIValidationError, _ERROR_CLASSES[core.kind]._from_core(core))


def _new_error(cls: type[ValidationError], message: str) -> ValidationError:
    error = cls.__new__(cls)
    Exception.__init__(error, message)
    return error


def _format_as_index(container: str, indices: Iterable[str | int]) -> str:
    indices = list(indices)
    if not indices:
        return container
    return f"{container}[{']['.join(repr(index) for index in indices)}]"


def _pretty(thing: Any, prefix: str) -> str:
    # Imported here: pprint costs several milliseconds at start-up and is only needed
    # when an error is rendered.
    from pprint import pformat

    return indent(pformat(thing, width=72, sort_dicts=False), prefix).lstrip()
