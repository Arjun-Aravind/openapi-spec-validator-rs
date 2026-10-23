"""The Python API against openapi-spec-validator itself, on the same documents.

Needs openapi-spec-validator installed (a dev dependency). Compares outcomes, error
classes, messages, ``validator``, instance and schema paths, ``context`` and ``str()``.
"""

from __future__ import annotations

import os
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
        return ("raises", name, str(error))
    return ("errors", [describe(error) for error in errors])


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
        # The order of subschema errors may differ (see COMPATIBILITY.md).
        sorted(
            repr((e.message, list(e.path), list(e.schema_path))) for e in error.context
        ),
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


# Differences found through the Python API whose fix belongs in the Rust crates.
CORE_DIFFERENCES = [
    pytest.param(
        _spec("3.0.3", info={"title": 1, "version": "1"}),
        id="schema-path-through-ref",
        marks=pytest.mark.xfail(
            strict=True,
            reason="meta-schema errors under a $ref report the referenced definition's "
            "path (['definitions', 'Info', ...]) instead of jsonschema's "
            "(['properties', 'info', ...])",
        ),
    ),
    pytest.param(
        _spec("3.0.3", components={"schemas": {"A": False}}),
        id="context-schema-path-through-ref",
        marks=pytest.mark.xfail(
            strict=True,
            reason="oneOf context errors under a $ref have schema_path "
            "['definitions', 'Schema', 'type'] instead of [0, 'type']",
        ),
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
        marks=pytest.mark.xfail(
            strict=True,
            reason="a false subschema in a default check gets the parent's keyword and "
            "path (validator 'x', path ['x']) instead of validator None and no path",
        ),
    ),
    pytest.param(
        _spec(
            "3.1.0",
            components={
                "schemas": {"A": {"type": "array", "items": False, "default": [1]}}
            },
        ),
        id="default-items-false",
        marks=pytest.mark.xfail(
            strict=True,
            reason="2020-12 'items: false' in a default check reports 'False schema does "
            "not allow 1' instead of 'Expected at most 0 items but found 1 extra: 1'",
        ),
    ),
]


@pytest.mark.parametrize("spec", CORE_DIFFERENCES)
def test_core_differences(spec: dict[str, Any]) -> None:
    expected = outcome(first_error, upstream, spec, "")
    actual = outcome(first_error, osv, spec, "")
    assert actual == expected
