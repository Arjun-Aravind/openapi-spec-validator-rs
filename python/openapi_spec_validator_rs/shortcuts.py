"""One-call validation functions, as upstream's ``shortcuts`` module."""

from __future__ import annotations

import os
import warnings
from collections.abc import Mapping
from contextlib import closing
from pathlib import Path
from typing import Any, Final
from urllib.parse import urlparse

from openapi_spec_validator_rs import _core, _stack, handlers
from openapi_spec_validator_rs.exceptions import (
    OpenAPIVersionNotFound,
    ValidatorDetectError,
    _from_core,
)
from openapi_spec_validator_rs.proxies import SpecValidatorProxy
from openapi_spec_validator_rs.validators import (
    OpenAPIV2SpecValidator,
    OpenAPIV30SpecValidator,
    OpenAPIV31SpecValidator,
    OpenAPIV32SpecValidator,
    SpecValidatorType,
)
from openapi_spec_validator_rs.versions import (
    OPENAPIV2,
    OPENAPIV30,
    OPENAPIV31,
    OPENAPIV32,
    SpecVersion,
    get_spec_version,
)

__all__ = [
    "SPEC2VALIDATOR",
    "get_validator_cls",
    "validate",
    "validate_path",
    "validate_spec",
    "validate_spec_url",
    "validate_url",
]

SPEC2VALIDATOR: Final[Mapping[SpecVersion, SpecValidatorType]] = {
    OPENAPIV2: OpenAPIV2SpecValidator,
    OPENAPIV30: OpenAPIV30SpecValidator,
    OPENAPIV31: OpenAPIV31SpecValidator,
    OPENAPIV32: OpenAPIV32SpecValidator,
}

# Upstream reads URLs with jsonschema-path's `UrllibHandler("http", "https", "file")`.
_URL_SCHEMES: Final = ("http", "https", "file")
_URL_TIMEOUT: Final = 10
# The name PyYAML reports for a `urlopen("file://...")` response, as upstream's CLI does.
_FILE_STREAM_NAME: Final = "<urllib response>"


def get_validator_cls(spec: Any) -> SpecValidatorType:
    """The validator class for the document's ``swagger`` / ``openapi`` version.

    Raises :class:`ValidatorDetectError` (without a message, as upstream) when no
    supported version is found.
    """
    try:
        spec_version = get_spec_version(spec)
    except OpenAPIVersionNotFound:
        raise ValidatorDetectError from None
    return SPEC2VALIDATOR[spec_version]


def validate(
    spec: Any,
    base_uri: str = "",
    cls: SpecValidatorType | None = None,
) -> None:
    """Validates a parsed spec document and raises its first error.

    ``base_uri`` is the document's location, used to resolve relative ``$ref``s to
    other files. ``cls`` picks the version instead of detecting it.
    """
    if cls is None:
        cls = get_validator_cls(spec)
    validator = cls(spec, base_uri=base_uri)
    # Upstream reads the document through a `SchemaPath`, which follows a root `$ref`.
    validator._follow_root_ref = True
    validator.validate()


def validate_url(spec_url: str, cls: SpecValidatorType | None = None) -> None:
    """Fetches a spec (``http``, ``https`` or ``file`` URL), validates it and raises its
    first error.

    The response is parsed exactly as upstream parses it; ``cls`` only selects the
    specification version.
    """
    data, stream_name = _read_url(spec_url)
    _validate_bytes(data, stream_name, spec_url, cls)


def validate_path(
    path: str | os.PathLike[str], cls: SpecValidatorType | None = None
) -> None:
    """Reads a spec file, validates it and raises its first error.

    Parses the file exactly as upstream's command line does, and resolves relative
    ``$ref``s against the file's location. Raises ``FileNotFoundError`` with upstream's
    message (``No such file: ...``) when ``path`` is not a file.
    """
    filename = os.fspath(path)
    if not os.path.isfile(filename):
        raise FileNotFoundError(f"No such file: {filename}")
    uri = Path(os.path.abspath(filename)).as_uri()
    with open(filename, "rb") as stream:
        data = handlers.read_stream(stream)
    _validate_bytes(data, _FILE_STREAM_NAME, uri, cls)


def validate_spec(
    spec: Any,
    base_uri: str = "",
    validator: SpecValidatorProxy | None = None,
    cls: SpecValidatorType | None = None,
    spec_url: str | None = None,
) -> None:
    """Deprecated: use :func:`validate`."""
    warnings.warn(
        "validate_spec shortcut is deprecated. Use validate instead.",
        DeprecationWarning,
        stacklevel=2,
    )
    if validator is not None:
        warnings.warn(
            "validator parameter is deprecated. Use cls instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return validator.validate(spec, base_uri=base_uri, spec_url=spec_url)
    if cls is None:
        cls = get_validator_cls(spec)
    # Upstream ignores `base_uri` and `spec_url` here; so does this port.
    return cls(spec).validate()


def validate_spec_url(
    spec_url: str,
    validator: SpecValidatorProxy | None = None,
    cls: SpecValidatorType | None = None,
) -> None:
    """Deprecated: use :func:`validate_url`."""
    warnings.warn(
        "validate_spec_url shortcut is deprecated. Use validate_url instead.",
        DeprecationWarning,
        stacklevel=2,
    )
    if validator is not None:
        warnings.warn(
            "validator parameter is deprecated. Use cls instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        data, stream_name = _read_url(spec_url)
        return validator.validate(_core.load(data, stream_name), base_uri=spec_url)
    return validate_url(spec_url, cls=cls)


def _read_url(spec_url: str) -> tuple[bytes, str]:
    """The body of ``spec_url`` and the stream name PyYAML would report for it."""
    scheme = urlparse(spec_url).scheme
    if scheme not in _URL_SCHEMES:
        raise ValueError(f"Scheme {scheme} not allowed")
    # Imported here: urllib.request costs tens of milliseconds at start-up.
    from urllib.request import urlopen

    with closing(urlopen(spec_url, timeout=_URL_TIMEOUT)) as response:
        # PyYAML names a stream after its `name` attribute, else `<file>`.
        stream_name = str(getattr(response, "name", "<file>"))
        return handlers.read_stream(response), stream_name


def _validate_bytes(
    data: bytes, stream_name: str, base_uri: str, cls: SpecValidatorType | None
) -> None:
    version = None
    if cls is not None:
        version = f"{cls.spec_version.major}.{cls.spec_version.minor}"
    try:
        errors = _core.validate_bytes(
            data,
            stream_name,
            base_uri,
            version,
            False,
            handlers.retrieve,
            _stack.spec_depth(),
            _stack.limit(),
        )
    except TypeError:
        # Version detection failed on a malformed document. Detect again in Python so
        # that the TypeError is the interpreter's own, worded as upstream's would be.
        get_spec_version(_core.load(data, stream_name))
        raise
    if errors:
        raise _from_core(errors[0])
