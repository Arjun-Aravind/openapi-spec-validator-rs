"""The Python API against openapi-spec-validator itself, on the same documents.

Needs openapi-spec-validator installed (a dev dependency). Compares outcomes, error
classes, messages, ``validator``, instance and schema paths, ``context`` and ``str()``.
"""

from __future__ import annotations

import datetime
import os
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

# The reference is upstream's default backend, even if jsonschema-rs is installed.
os.environ["OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND"] = "jsonschema"

upstream = pytest.importorskip("openapi_spec_validator")
upstream_readers = pytest.importorskip("openapi_spec_validator.readers")
upstream_shortcuts = pytest.importorskip("openapi_spec_validator.shortcuts")

import openapi_spec_validator_rs as osv  # noqa: E402
from openapi_spec_validator_rs import shortcuts  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
DOCUMENTS = sorted(
    [
        *(ROOT / "crates/openapi-spec-validator-rs/tests/fixtures/checks").glob(
            "*.json"
        ),
        *(ROOT / "upstream/tests/integration/data").rglob("*.yaml"),
    ]
)

# Non-validation exceptions keep upstream's class for version detection; everything
# else is a ValidationAbortedError with upstream's message.
DETECTION_ERRORS = {"TypeError", "ValidatorDetectError"}


def first_error(mod: Any, spec: Any, base_uri: str) -> list[Any]:
    mod.shortcuts.get_validator_cls(spec)(spec, base_uri=base_uri).validate()
    return []


def all_errors(mod: Any, spec: Any, base_uri: str) -> list[Any]:
    return list(
        mod.shortcuts.get_validator_cls(spec)(spec, base_uri=base_uri).iter_errors()
    )


def from_url(mod: Any, spec: Any, base_uri: str) -> list[Any]:
    mod.validate_url(base_uri)
    return []


def outcome(run: Callable[..., list[Any]], mod: Any, spec: Any, base_uri: str) -> Any:
    try:
        errors = run(mod, spec, base_uri)
    except (
        osv.OpenAPIValidationError,
        upstream.validation.exceptions.OpenAPIValidationError,
    ) as error:
        errors = [error]
    except Exception as error:  # noqa: BLE001 - the class is what is compared
        name = type(error).__name__
        if mod is upstream and name not in DETECTION_ERRORS:
            name = "ValidationAbortedError"
        return ("raises", name, modern_wording(str(error)))
    return ("errors", [describe(error) for error in errors])


def modern_wording(message: str) -> str:
    """Python's own exception messages as CPython 3.12+ words them, which is what this
    package reproduces; older interpreters word upstream's differently (see
    COMPATIBILITY.md)."""
    if sys.version_info < (3, 12) and message.startswith("maximum recursion depth exceeded"):
        return "maximum recursion depth exceeded"
    return message


def describe(error: Any) -> tuple[Any, ...]:
    text: str | None = str(error)
    # Upstream prints its meta-schema as `<Proxy at 0x...>` for top-level errors.
    if "<Proxy at 0x" in text:
        text = None
    validator = (
        error.validator if isinstance(error.validator, (str, type(None))) else "<unset>"
    )
    return (
        type(error).__name__,
        error.message,
        list(error.path),
        validator,
        list(error.schema_path),
        [(e.message, list(e.path), list(e.schema_path)) for e in error.context],
        text,
    )


@pytest.mark.parametrize(
    "run", [first_error, all_errors, from_url], ids=lambda f: f.__name__
)
@pytest.mark.parametrize(
    "path", DOCUMENTS, ids=lambda p: p.relative_to(ROOT).as_posix()
)
def test_same_as_upstream(path: Path, run: Callable[..., list[Any]]) -> None:
    spec, base_uri = upstream_readers.read_from_filename(str(path))
    expected = outcome(run, upstream, spec, base_uri)
    actual = outcome(run, osv, spec, base_uri)
    if expected[0] == actual[0] == "errors":
        for mine, theirs in zip(actual[1], expected[1], strict=True):
            if theirs[-1] is None:
                mine = (*mine[:-1], None)
            assert mine == theirs
    else:
        assert actual == expected


def test_corpus() -> None:
    assert len(DOCUMENTS) >= 40

    def versions(mapping: Any) -> list[tuple[str, str, str, str]]:
        return [
            (v.keyword, v.major, v.minor, cls.__name__) for v, cls in mapping.items()
        ]

    assert versions(shortcuts.SPEC2VALIDATOR) == versions(
        upstream_shortcuts.SPEC2VALIDATOR
    )


def _spec(version: str, **fields: Any) -> dict[str, Any]:
    spec = {"openapi": version, "info": {"title": "t", "version": "1"}, "paths": {}}
    spec.update(fields)
    return spec


# Cases that once differed through the Python API; kept as regression tests.
CORE_DIFFERENCES = [
    pytest.param(
        _spec("3.0.3", info={"title": 1, "version": "1"}),
        id="schema-path-through-ref",
    ),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": False}}),
        id="context-schema-path-through-ref",
    ),
    pytest.param(
        _spec(
            "3.1.0",
            components={
                "schemas": {
                    "A": {
                        "type": "object",
                        "properties": {"x": False},
                        "default": {"x": 1},
                    }
                }
            },
        ),
        id="default-false-subschema",
    ),
    pytest.param(
        _spec(
            "3.1.0",
            components={
                "schemas": {"A": {"type": "array", "items": False, "default": [1]}}
            },
        ),
        id="default-items-false",
    ),
    pytest.param(
        _spec(
            "3.0.3",
            components={
                "schemas": {
                    "A": {
                        "type": "object",
                        "properties": {"x": False},
                        "default": {"x": 1},
                    }
                }
            },
        ),
        id="default-false-subschema-oas30",
    ),
]


@pytest.mark.parametrize("spec", CORE_DIFFERENCES)
def test_core_differences(spec: dict[str, Any]) -> None:
    expected = outcome(first_error, upstream, spec, "")
    actual = outcome(first_error, osv, spec, "")
    assert actual == expected


DATE = datetime.date(2020, 1, 1)
DATETIME = datetime.datetime(2020, 1, 1, 10, 0, tzinfo=datetime.timezone.utc)


def _swagger(**fields: Any) -> dict[str, Any]:
    spec = {"swagger": "2.0", "info": {"title": "t", "version": "1"}, "paths": {}}
    spec.update(fields)
    return spec


# Python objects of types JSON has no counterpart for, as `yaml.safe_load` produces them
# for unquoted timestamps: of no type to jsonschema, printed as their repr.
PYTHON_OBJECTS = [
    pytest.param(_spec("3.0.3", **{"x-when": DATE}), id="extension"),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": {"type": "string", "example": DATE}}}),
        id="example",
    ),
    pytest.param(
        _spec("3.1.0", components={"schemas": {"A": {"type": "string", "examples": [DATE, DATETIME]}}}),
        id="examples-31",
    ),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": {"type": "string", "default": DATE}}}),
        id="default-30",
    ),
    pytest.param(
        _spec("3.1.0", components={"schemas": {"A": {"type": "string", "default": DATETIME}}}),
        id="default-31",
    ),
    pytest.param(
        _swagger(definitions={"A": {"type": "string", "default": DATE}}), id="default-20"
    ),
    pytest.param(
        _spec(
            "3.0.3",
            components={
                "schemas": {
                    "A": {"properties": {"a": {"type": "string"}}, "required": ["a"], "default": DATE}
                }
            },
        ),
        id="default-untyped-30",
    ),
    pytest.param(
        _spec(
            "3.1.0",
            components={
                "schemas": {
                    "A": {"properties": {"a": {"type": "string"}}, "required": ["a"], "default": DATE}
                }
            },
        ),
        id="default-untyped-31",
    ),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": {"enum": [DATETIME], "default": DATE}}}),
        id="default-enum",
    ),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": {"type": "string", "format": "binary", "default": b"ab"}}}),
        id="default-binary-bytes",
    ),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": {"type": "array", "items": {"type": "string"}, "default": [DATE]}}}),
        id="default-list",
    ),
    pytest.param(_spec("3.0.3", info={"title": "t", "version": DATE}), id="info-version"),
    pytest.param(_spec("3.0.3", info={"title": b"t", "version": "1"}), id="bytes-title"),
    pytest.param(_spec("3.0.3", info={"title": [DATE], "version": "1"}), id="list-title"),
    pytest.param(_spec("3.0.3", info=DATE), id="info-30"),
    pytest.param(_spec("3.1.0", info=DATE), id="info-31"),
    pytest.param(_swagger(info=DATE), id="info-20"),
    pytest.param(
        _spec("3.0.3", servers=[{"url": "http://a/{v}", "variables": {"v": {"default": DATE}}}]),
        id="server-variable",
    ),
    pytest.param(_spec("3.0.3", components={"schemas": {"A": DATE}}), id="schema-30"),
    pytest.param(_spec("3.1.0", components={"schemas": {"A": DATE}}), id="schema-31"),
    pytest.param(_spec("3.0.3", **{"x-set": {1, 2}}), id="set"),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": {"type": "object", "required": DATE}}}),
        id="keyword-value",
    ),
    # Not listed: a date where the walk reads a string (`tags: [{name: <date>}]`), whose
    # `TypeError` this package raises as `ValidationAbortedError`, as for any other value.
    pytest.param({"openapi": DATE, "info": {"title": "t", "version": "1"}, "paths": {}}, id="version"),
    pytest.param(DATE, id="root"),
]


# `type: 'null'` is unknown to OpenAPI 3.0's type checker, which Swagger 2.0 uses too; a
# `null` default reports "None for not nullable" before the type is looked up.
NULL_TYPE = [
    pytest.param(
        _swagger(paths={"/a": {"get": {"parameters": [{"name": "b", "in": "body", "schema": {"type": "null", "default": None}}], "responses": {"200": {"description": "ok"}}}}}),
        id="body-20",
    ),
    pytest.param(
        _swagger(paths={"/a": {"get": {"responses": {"200": {"description": "ok", "schema": {"type": "null", "default": None}}}}}}),
        id="response-20",
    ),
    pytest.param(
        _swagger(paths={"/a": {"get": {"parameters": [{"name": "q", "in": "query", "type": "null", "default": None}], "responses": {"200": {"description": "ok"}}}}}),
        id="parameter-20",
    ),
    pytest.param(
        _swagger(paths={"/a": {"get": {"responses": {"200": {"description": "ok", "schema": {"type": "null", "default": 1}}}}}}),
        id="non-null-default-20",
    ),
    pytest.param(_spec("3.0.3", components={"schemas": {"A": {"type": "null", "default": None}}}), id="schema-30"),
]


@pytest.mark.parametrize("run", [first_error, all_errors], ids=lambda f: f.__name__)
@pytest.mark.parametrize("spec", NULL_TYPE)
def test_null_type(spec: Any, run: Callable[..., list[Any]]) -> None:
    assert outcome(run, osv, spec, "") == outcome(run, upstream, spec, "")


@pytest.mark.parametrize("run", [first_error, all_errors], ids=lambda f: f.__name__)
@pytest.mark.parametrize("spec", PYTHON_OBJECTS)
def test_python_objects(spec: Any, run: Callable[..., list[Any]]) -> None:
    expected = outcome(run, upstream, spec, "")
    actual = outcome(run, osv, spec, "")
    assert actual == expected
