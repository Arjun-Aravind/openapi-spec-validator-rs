"""Exception classes and their jsonschema-style attributes."""

from __future__ import annotations

import copy
import pickle
from collections import deque
from typing import Any

import openapi_spec_validator_rs as osv
import pytest
from openapi_spec_validator_rs import exceptions
from openapi_spec_validator_rs.exceptions import (
    DuplicateOperationIDError,
    ExtraParametersError,
    OpenAPIError,
    OpenAPISpecValidatorError,
    OpenAPIValidationError,
    OpenAPIVersionNotFound,
    ParameterDuplicateError,
    SpecReadError,
    UnresolvableParameterError,
    ValidationAbortedError,
    ValidationError,
    ValidatorDetectError,
)


def first_error(spec: dict[str, Any]) -> OpenAPIValidationError:
    with pytest.raises(OpenAPIValidationError) as info:
        osv.validate(spec)
    return info.value


def test_hierarchy() -> None:
    for cls in (
        ExtraParametersError,
        ParameterDuplicateError,
        UnresolvableParameterError,
        DuplicateOperationIDError,
    ):
        assert issubclass(cls, OpenAPIValidationError)
    assert issubclass(OpenAPIValidationError, ValidationError)
    assert issubclass(ValidationError, Exception)
    for cls in (ValidatorDetectError, ValidationAbortedError, SpecReadError):
        assert issubclass(cls, OpenAPISpecValidatorError)
    assert issubclass(OpenAPISpecValidatorError, OpenAPIError)
    assert issubclass(OpenAPIVersionNotFound, OpenAPIError)
    assert str(OpenAPIVersionNotFound()) == "Specification version not found"


def test_schema_error_attributes(make_spec) -> None:
    error = first_error(make_spec(info={"title": 1, "version": "1"}))

    assert error.message == "1 is not of type 'string'"
    assert error.validator == "type"
    assert error.validator_value == "string"
    assert error.instance == 1
    assert error.schema == {"type": "string"}
    assert error.path == deque(["info", "title"])
    assert error.relative_path is error.path
    assert error.absolute_path == deque(["info", "title"])
    assert error.schema_path[-1] == "type"
    assert error.relative_schema_path is error.schema_path
    assert error.json_path == "$.info.title"
    assert error.cause is None
    assert error.context == []
    assert error.parent is None
    assert repr(error) == "<OpenAPIValidationError: \"1 is not of type 'string'\">"
    assert str(error).startswith(
        "1 is not of type 'string'\n\nFailed validating 'type' in schema"
    )
    assert str(error).endswith("On instance['info']['title']:\n    1")


def test_semantic_error_attributes(make_spec) -> None:
    responses = {"200": {"description": "ok"}}
    error = first_error(
        make_spec(
            paths={
                "/a": {"get": {"operationId": "x", "responses": responses}},
                "/b": {"get": {"operationId": "x", "responses": responses}},
            }
        )
    )

    assert type(error) is DuplicateOperationIDError
    assert (
        str(error)
        == error.message
        == "Operation ID 'x' for 'get' in '/b' is not unique"
    )
    # Like jsonschema's errors built from a message only.
    for name in ("validator", "validator_value", "instance", "schema"):
        assert repr(getattr(error, name)) == "<unset>"
    assert error.path == deque()
    assert error.schema_path == deque()
    assert error.context == []


def test_context_errors(make_spec) -> None:
    error = first_error(make_spec(paths={"/a": {"get": {"responses": {"200": {}}}}}))

    assert error.message == "{} is not valid under any of the given schemas"
    assert error.validator == "oneOf"
    assert error.path == deque(["paths", "/a", "get", "responses", "200"])
    assert [type(e) for e in error.context] == [ValidationError, ValidationError]
    assert [e.message for e in error.context] == [
        "'description' is a required property",
        "'$ref' is a required property",
    ]
    first = error.context[0]
    assert first.parent is error
    assert first.schema_path == deque([0, "required"])
    assert list(first.absolute_path) == list(error.path)
    assert list(first.absolute_schema_path) == [*error.schema_path, 0, "required"]
    assert first.json_path == "$.paths['/a'].get.responses['200']"


def test_format_error_cause(make_spec) -> None:
    schema = {"type": "string", "format": "date", "default": "2020-13-01"}
    error = first_error(make_spec(components={"schemas": {"A": schema}}))

    assert error.message == "'2020-13-01' is not a 'date'"
    assert isinstance(error.cause, Exception)
    assert str(error.cause) == "month must be in 1..12"
    assert error.__cause__ is error.cause


def test_values_keep_python_types(make_spec) -> None:
    # OpenAPI 3.0 schemas cannot be booleans.
    error = first_error(make_spec(components={"schemas": {"A": False}}))
    assert error.instance is False
    assert [(e.message, e.instance) for e in error.context] == [
        ("False is not of type 'object'", False)
    ] * 2

    error = first_error(make_spec(info={"title": 1.5, "version": "1"}))
    assert type(error.instance) is float


def test_pickle_round_trip(make_spec) -> None:
    error = first_error(make_spec(paths={"/a": {"get": {"responses": {"200": {}}}}}))
    copied = pickle.loads(pickle.dumps(error))

    assert type(copied) is OpenAPIValidationError
    assert str(copied) == str(error)
    assert copied.message == error.message
    assert copied.path == error.path
    assert copied.relative_path is copied.path
    assert copied.schema == error.schema
    assert copied.instance == error.instance
    assert [str(e) for e in copied.context] == [str(e) for e in error.context]
    assert copied.context[0].parent is copied
    assert str(copy.deepcopy(error)) == str(error)


def test_errors_built_in_python() -> None:
    error = ExtraParametersError("Required list has not defined properties: ['b']")
    assert str(error) == error.message
    assert error.path == deque()
    assert repr(error.validator) == "<unset>"

    error = ValidationError(
        "1 is not of type 'string'",
        validator="type",
        validator_value="string",
        instance=1,
        schema={"type": "string"},
        path=["info", "title"],
        schema_path=["properties", "info", "properties", "title", "type"],
    )
    assert str(error) == (
        "1 is not of type 'string'\n\n"
        "Failed validating 'type' in schema['properties']['info']['properties']['title']:\n"
        "    {'type': 'string'}\n\n"
        "On instance['info']['title']:\n"
        "    1"
    )


def test_python_rendering_matches_jsonschema() -> None:
    jsonschema_exceptions = pytest.importorskip("jsonschema.exceptions")
    schema = {
        "type": "object",
        "properties": {f"p{i}": {"type": "string"} for i in range(9)},
    }
    fields: dict[str, Any] = {
        "validator": "required",
        "validator_value": ["a"],
        "instance": {"x": list(range(30))},
        "schema": schema,
        "path": ["a", 0],
        "schema_path": ["items", "required"],
    }
    ours = ValidationError("msg", **fields)
    theirs = jsonschema_exceptions.ValidationError("msg", **fields)
    assert str(ours) == str(theirs)
    assert ours.json_path == theirs.json_path


def test_aborted_and_read_errors_carry_upstream_text(write) -> None:
    with pytest.raises(ValidationAbortedError) as aborted:
        osv.validate(
            {
                "openapi": "3.0.3",
                "info": {"title": "t", "version": "1"},
                "paths": {
                    "/a/{b}}": {"get": {"responses": {"200": {"description": "ok"}}}}
                },
            }
        )
    assert str(aborted.value) == "Single '}' encountered in format string"

    path = write("bad.yaml", "a: b: c\n")
    with pytest.raises(SpecReadError) as read:
        osv.validate_path(path)
    assert str(read.value) == (
        'mapping values are not allowed in this context\n  in "<urllib response>", line 1, column 5'
    )


def test_module_exports() -> None:
    for name in exceptions.__all__:
        assert hasattr(exceptions, name)
    for name in osv.__all__:
        assert hasattr(osv, name)
