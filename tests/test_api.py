"""The validation functions and validator classes."""

from __future__ import annotations

import json
import re
import threading
from collections.abc import Callable
from pathlib import Path
from typing import Any

import openapi_spec_validator_rs as osv
import pytest
from openapi_spec_validator_rs import (
    DuplicateOperationIDError,
    ExtraParametersError,
    OpenAPIV2SpecValidator,
    OpenAPIV3SpecValidator,
    OpenAPIV30SpecValidator,
    OpenAPIV31SpecValidator,
    OpenAPIV32SpecValidator,
    OpenAPIValidationError,
    ParameterDuplicateError,
    SpecReadError,
    UnresolvableParameterError,
    ValidationAbortedError,
    ValidatorDetectError,
)
from openapi_spec_validator_rs.exceptions import OpenAPIVersionNotFound
from openapi_spec_validator_rs.versions import (
    OPENAPIV2,
    OPENAPIV30,
    OPENAPIV31,
    OPENAPIV32,
    get_spec_version,
)

VALIDATORS = {
    "2.0": OpenAPIV2SpecValidator,
    "3.0": OpenAPIV30SpecValidator,
    "3.1": OpenAPIV31SpecValidator,
    "3.2": OpenAPIV32SpecValidator,
}

OK_RESPONSES = {"200": {"description": "ok"}}


@pytest.mark.parametrize("version", VALIDATORS)
def test_valid_specs(make_spec, version: str) -> None:
    spec = make_spec(version)
    assert osv.validate(spec) is None
    assert osv.validate(spec, cls=VALIDATORS[version]) is None
    assert osv.get_validator_cls(spec) is VALIDATORS[version]
    validator = VALIDATORS[version](spec)
    validator.validate()
    assert validator.is_valid()
    assert list(validator.iter_errors()) == []


def test_v3_alias_is_latest() -> None:
    assert OpenAPIV3SpecValidator is OpenAPIV32SpecValidator


def test_validate_raises_first_error(make_spec) -> None:
    spec = make_spec(info={"version": "1"})
    with pytest.raises(OpenAPIValidationError) as info:
        osv.validate(spec)
    assert type(info.value) is OpenAPIValidationError
    assert info.value.message == "'title' is a required property"


def test_iter_errors_lists_every_error_in_order() -> None:
    validator = OpenAPIV30SpecValidator({})
    errors = list(validator.iter_errors())
    assert [e.message for e in errors] == [
        "'openapi' is a required property",
        "'info' is a required property",
        "'paths' is a required property",
    ]
    assert not validator.is_valid()
    with pytest.raises(OpenAPIValidationError, match="'openapi' is a required"):
        validator.validate()


def test_iter_errors_validates_lazily_and_each_time(make_spec) -> None:
    spec = make_spec()
    validator = OpenAPIV30SpecValidator(spec)
    errors = validator.iter_errors()
    del spec["paths"]
    # Validation starts with iteration, and sees the document as it is then.
    assert [e.message for e in errors] == ["'paths' is a required property"]
    spec["paths"] = {}
    assert validator.is_valid()


@pytest.mark.parametrize(
    ("spec", "version"),
    [
        ({"swagger": "2.0"}, OPENAPIV2),
        ({"openapi": "3.0.0"}, OPENAPIV30),
        ({"openapi": "3.0-rc"}, OPENAPIV30),
        ({"openapi": "3.1.1"}, OPENAPIV31),
        ({"openapi": "3.2.0"}, OPENAPIV32),
    ],
)
def test_get_spec_version(spec: dict[str, Any], version: Any) -> None:
    assert get_spec_version(spec) == version


@pytest.mark.parametrize(
    "spec", [{}, {"openapi": "4.0.0"}, {"swagger": "1.2"}, {"openapi": "v3"}, []]
)
def test_unknown_version(spec: Any) -> None:
    with pytest.raises(OpenAPIVersionNotFound):
        get_spec_version(spec)
    with pytest.raises(ValidatorDetectError) as info:
        osv.validate(spec)
    # Upstream raises it without a message.
    assert str(info.value) == ""


def native_type_error(action: Callable[[], Any]) -> str:
    """The interpreter's own wording for the TypeError ``action`` raises (it differs
    between Python versions)."""
    try:
        action()
    except TypeError as error:
        return str(error)
    raise AssertionError("expected a TypeError")


@pytest.mark.parametrize(
    ("spec", "same_error"),
    [
        ({"openapi": 3.0}, lambda: re.match("", 3.0)),  # type: ignore[call-overload]
        (None, lambda: "openapi" in None),  # type: ignore[operator]
        ("swagger: 2.0", lambda: (lambda text: text["swagger"])("swagger: 2.0")),
    ],
    ids=["float", "None", "str"],
)
def test_malformed_version_raises_type_error(
    spec: Any, same_error: Callable[[], Any], write: Callable[[str, str], Path]
) -> None:
    message = re.escape(native_type_error(same_error))
    with pytest.raises(TypeError, match=message):
        osv.validate(spec)
    # Read from a file, the version is detected by the core; the error is the same.
    path = write("malformed.json", json.dumps(spec))
    with pytest.raises(TypeError, match=message):
        osv.validate_path(path)


def test_cls_overrides_detection(make_spec) -> None:
    with pytest.raises(OpenAPIValidationError, match="'openapi' is a required"):
        osv.validate(make_spec("2.0"), cls=OpenAPIV30SpecValidator)


# One document per error class, with upstream's message.
ERROR_CASES = [
    pytest.param(
        "3.0",
        {
            "paths": {
                "/a": {
                    "get": {"operationId": "op", "responses": OK_RESPONSES},
                    "post": {"operationId": "op", "responses": OK_RESPONSES},
                }
            }
        },
        DuplicateOperationIDError,
        "Operation ID 'op' for 'post' in '/a' is not unique",
        id="duplicate-operation-id",
    ),
    pytest.param(
        "3.0",
        {
            "paths": {
                "/a": {
                    "get": {
                        "parameters": [
                            {"name": "q", "in": "query", "schema": {"type": "string"}},
                            {"name": "q", "in": "query", "schema": {"type": "integer"}},
                        ],
                        "responses": OK_RESPONSES,
                    }
                }
            }
        },
        ParameterDuplicateError,
        "Duplicate parameter 'q'",
        id="duplicate-parameter",
    ),
    pytest.param(
        "3.0",
        {"paths": {"/a/{id}": {"get": {"responses": OK_RESPONSES}}}},
        UnresolvableParameterError,
        "Path parameter 'id' for 'get' operation in '/a/{id}' was not resolved",
        id="unresolvable-parameter",
    ),
    pytest.param(
        "3.0",
        {
            "components": {
                "schemas": {
                    "A": {
                        "allOf": [{"type": "object"}],
                        "properties": {"a": {}},
                        "required": ["a", "b"],
                    }
                }
            }
        },
        ExtraParametersError,
        "Required list has not defined properties: ['b']",
        id="extra-parameters",
    ),
    pytest.param(
        "3.0",
        {"tags": [{"name": "t"}, {"name": "t", "description": "again"}]},
        OpenAPIValidationError,
        "Duplicate tag name 't'",
        id="duplicate-tag",
    ),
    pytest.param(
        "3.1",
        {"components": {"schemas": {"A": {"type": "integer", "default": "x"}}}},
        OpenAPIValidationError,
        "'x' is not of type 'integer'",
        id="default-value",
    ),
    pytest.param(
        "3.2",
        {"tags": [{"name": "a", "parent": "b"}]},
        OpenAPIValidationError,
        "Tag 'a' references unknown parent tag 'b'",
        id="unknown-parent-tag",
    ),
]


@pytest.mark.parametrize(("version", "fields", "cls", "message"), ERROR_CASES)
def test_error_classes(
    make_spec, version: str, fields: dict[str, Any], cls: type, message: str
) -> None:
    spec = make_spec(version, **fields)
    with pytest.raises(cls) as info:
        osv.validate(spec)
    assert type(info.value) is cls
    assert info.value.message == message
    [error] = VALIDATORS[version](spec).iter_errors()
    assert type(error) is cls
    assert error.message == message


@pytest.mark.parametrize(
    ("fields", "message"),
    [
        pytest.param(
            {"paths": {"/a": {"$ref": "#/components/pathItems/missing"}}},
            "'/components/pathItems/missing' does not exist within",
            id="pointer-to-nowhere",
        ),
        pytest.param(
            {"paths": {"/a": {"get": {"responses": {"200": {"$ref": "other.yaml"}}}}}},
            "other.yaml",
            id="unresolvable",
        ),
        pytest.param(
            {"paths": {"/a/{b}}": {"get": {"responses": OK_RESPONSES}}}},
            "Single '}' encountered in format string",
            id="path-template",
        ),
        pytest.param(
            {
                "components": {
                    "schemas": {
                        "A": {"$ref": "#/components/schemas/B"},
                        "B": {"$ref": "#/components/schemas/A"},
                    }
                }
            },
            "maximum recursion depth exceeded",
            id="reference-cycle",
        ),
    ],
)
def test_validation_aborted(make_spec, fields: dict[str, Any], message: str) -> None:
    spec = make_spec(**fields)
    with pytest.raises(ValidationAbortedError, match="^" + re.escape(message)):
        osv.validate(spec)
    with pytest.raises(ValidationAbortedError):
        list(OpenAPIV30SpecValidator(spec).iter_errors())
    with pytest.raises(ValidationAbortedError):
        OpenAPIV30SpecValidator(spec).is_valid()


def test_validation_stops_at_first_error_before_aborting(make_spec) -> None:
    # Like upstream, `validate()` and `is_valid()` stop at the meta-schema error and
    # never reach the unresolvable reference.
    spec = make_spec(
        info={"title": "t"},
        paths={"/a": {"get": {"responses": {"200": {"$ref": "#/nowhere"}}}}},
    )
    with pytest.raises(OpenAPIValidationError, match="'version' is a required"):
        osv.validate(spec)
    assert not OpenAPIV30SpecValidator(spec).is_valid()


EXTERNAL_MAIN = """\
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /pets:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema: {$ref: "schemas/pet.yaml#/Pet"}
"""


def test_base_uri_resolves_relative_refs(write) -> None:
    main = write("openapi.yaml", EXTERNAL_MAIN)
    write(
        "schemas/pet.yaml", "Pet: {type: object, properties: {id: {type: integer}}}\n"
    )
    spec = _load(main)

    osv.validate(spec, base_uri=main.as_uri())
    assert OpenAPIV30SpecValidator(spec, base_uri=main.as_uri()).is_valid()
    osv.validate_path(main)
    osv.validate_url(main.as_uri())
    # Without a base URI the reference cannot be resolved, as upstream.
    with pytest.raises(ValidationAbortedError, match="schemas/pet.yaml"):
        osv.validate(spec)


def test_errors_inside_external_documents(write) -> None:
    main = write("openapi.yaml", EXTERNAL_MAIN)
    write("schemas/pet.yaml", "Pet: {type: integer, default: x}\n")
    with pytest.raises(OpenAPIValidationError) as info:
        osv.validate_path(main)
    assert info.value.message == "'x' is not of type 'integer'"


def test_validate_path(write, make_spec) -> None:
    valid = write(
        "valid.yaml", "swagger: '2.0'\ninfo: {title: t, version: '1'}\npaths: {}\n"
    )
    osv.validate_path(valid)
    osv.validate_path(str(valid), cls=OpenAPIV2SpecValidator)
    with pytest.raises(OpenAPIValidationError, match="'openapi' is a required"):
        osv.validate_path(valid, cls=OpenAPIV30SpecValidator)

    invalid = write("invalid.json", '{"openapi": "3.0.3", "info": {}, "paths": {}}')
    with pytest.raises(OpenAPIValidationError, match="'title' is a required"):
        osv.validate_path(invalid)

    no_version = write("no-version.yaml", "{}")
    with pytest.raises(ValidatorDetectError):
        osv.validate_path(no_version)

    # An empty document loads as None, which upstream cannot look a version up in.
    empty = write("empty.yaml", "")
    # Python 3.14 words it "is not a container or iterable".
    with pytest.raises(TypeError, match="argument of type 'NoneType' is not (a container or )?iterable"):
        osv.validate_path(empty)

    float_version = write("float.yaml", "swagger: 2.0\n")
    expected = re.escape(native_type_error(lambda: re.match("", 2.0)))  # type: ignore[call-overload]
    with pytest.raises(TypeError, match=expected):
        osv.validate_path(float_version)


def test_validate_path_missing_file(tmp_path: Path) -> None:
    missing = tmp_path / "missing.yaml"
    with pytest.raises(FileNotFoundError, match="^No such file: ") as info:
        osv.validate_path(missing)
    assert str(info.value) == f"No such file: {missing}"
    with pytest.raises(FileNotFoundError):
        osv.validate_path(tmp_path)


def test_read_errors_match_upstream_text(write) -> None:
    broken = write("broken.yaml", "openapi: 3.0.3\ninfo: [\n")
    with pytest.raises(SpecReadError) as info:
        osv.validate_path(broken)
    # Upstream reads files through `urlopen`, so PyYAML names the stream that way.
    assert 'in "<urllib response>", line 3, column 1' in str(info.value)
    with pytest.raises(SpecReadError):
        osv.validate_url(broken.as_uri())


def test_validate_url(write) -> None:
    spec = write(
        "spec.json",
        '{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {}}',
    )
    osv.validate_url(spec.as_uri())
    osv.validate_url(spec.as_uri(), cls=OpenAPIV31SpecValidator)
    with pytest.raises(OpenAPIValidationError):
        osv.validate_url(spec.as_uri(), cls=OpenAPIV2SpecValidator)
    with pytest.raises(ValueError, match="^Scheme ftp not allowed$"):
        osv.validate_url("ftp://example.com/spec.yaml")
    with pytest.raises(OSError):
        osv.validate_url(spec.with_name("missing.json").as_uri())


def test_deprecated_shortcuts(write, make_spec) -> None:
    spec = make_spec()
    with pytest.deprecated_call():
        osv.validate_spec(spec)
    with pytest.deprecated_call():
        osv.validate_spec(spec, validator=osv.openapi_v30_spec_validator)
    with pytest.deprecated_call(), pytest.raises(OpenAPIValidationError):
        osv.validate_spec({"openapi": "3.0.3"})

    path = write(
        "spec.yaml", "openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths: {}\n"
    )
    with pytest.deprecated_call():
        osv.validate_spec_url(path.as_uri())
    with pytest.deprecated_call():
        osv.validate_spec_url(path.as_uri(), validator=osv.openapi_v30_spec_validator)


def test_deprecated_proxies(make_spec) -> None:
    from openapi_spec_validator_rs.proxies import openapi_spec_validator_proxy

    with pytest.deprecated_call():
        assert osv.openapi_v2_spec_validator.is_valid(make_spec("2.0"))
    with pytest.deprecated_call():
        assert not osv.openapi_v31_spec_validator.is_valid({})
    with pytest.deprecated_call(), pytest.raises(OpenAPIValidationError):
        openapi_spec_validator_proxy.validate({"openapi": "3.1.0"})
    with pytest.raises(ValidatorDetectError):
        openapi_spec_validator_proxy.validate({})


def test_spec_url_argument_is_deprecated(make_spec) -> None:
    with pytest.deprecated_call():
        validator = OpenAPIV30SpecValidator(make_spec(), spec_url="file:///spec.yaml")
    assert validator.base_uri == "file:///spec.yaml"


def test_schema_path_input(write) -> None:
    jsonschema_path = pytest.importorskip("jsonschema_path")
    main = write("openapi.yaml", EXTERNAL_MAIN)
    write("schemas/pet.yaml", "Pet: {type: object}\n")
    schema_path = jsonschema_path.SchemaPath.from_dict(
        _load(main), base_uri=main.as_uri()
    )

    validator = OpenAPIV30SpecValidator(schema_path)
    assert validator.base_uri == main.as_uri()
    validator.validate()


def test_concurrent_validation(make_spec) -> None:
    spec = make_spec(
        paths={f"/p{i}/{{id}}": {"get": {"responses": OK_RESPONSES}} for i in range(50)}
    )
    results: list[int] = []

    def work() -> None:
        results.append(len(list(OpenAPIV30SpecValidator(spec).iter_errors())))

    threads = [threading.Thread(target=work) for _ in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    assert results == [50] * 8


def _load(path: Path) -> Any:
    from openapi_spec_validator_rs import _core

    return _core.load(path.read_bytes(), "<urllib response>")


def test_upstream_module_layout(write, make_spec) -> None:
    # Upstream's modules, so that `openapi_spec_validator.x` becomes `openapi_spec_validator_rs.x`.
    from openapi_spec_validator_rs import readers, validation, versions
    from openapi_spec_validator_rs.validation import exceptions, proxies, types, validators
    from openapi_spec_validator_rs.versions import consts, datatypes, shortcuts

    assert validation.OpenAPIV31SpecValidator is osv.OpenAPIV31SpecValidator
    assert validators.OpenAPIV30SpecValidator is osv.OpenAPIV30SpecValidator
    assert exceptions.OpenAPIValidationError is osv.OpenAPIValidationError
    assert proxies.openapi_v31_spec_validator is osv.openapi_v31_spec_validator
    assert types.SpecValidatorType is osv.validators.SpecValidatorType
    assert consts.OPENAPIV30 is OPENAPIV30
    assert datatypes.SpecVersion is versions.SpecVersion
    assert shortcuts.get_spec_version is versions.get_spec_version

    path = write("openapi.json", json.dumps(make_spec(**{"x-when": "2020-01-01"})))
    spec, base_uri = readers.read_from_filename(path)
    assert spec["x-when"] == "2020-01-01"
    assert base_uri == path.resolve().as_uri()
    osv.validate(spec, base_uri=base_uri)
    with pytest.raises(OSError, match="^No such file: missing.yaml$"):
        readers.read_from_filename("missing.yaml")


def test_keyboard_interrupt_takes_effect_at_once(make_spec, monkeypatch: pytest.MonkeyPatch) -> None:
    # Validation runs on its own thread; the calling thread handles signals meanwhile,
    # as upstream's pure-Python validation can be interrupted anywhere.
    import os
    import signal
    import time

    from openapi_spec_validator_rs import handlers

    download_ended = threading.Event()

    def slow(url: str) -> bytes:
        time.sleep(4)
        download_ended.set()
        return b"{}"

    monkeypatch.setattr(handlers, "retrieve", slow)
    spec = make_spec(components={"schemas": {"A": {"$ref": "http://127.0.0.1:9/a.json"}}})
    if threading.current_thread() is not threading.main_thread():
        pytest.skip("signal handlers run on the main thread only")
    timer = threading.Timer(0.1, lambda: os.kill(os.getpid(), signal.SIGINT))
    timer.start()
    try:
        with pytest.raises(KeyboardInterrupt):
            osv.validate(spec)
    finally:
        timer.cancel()
    # Raised while the download was still under way, not once it had returned.
    assert not download_ended.is_set()
