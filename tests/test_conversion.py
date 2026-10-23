"""How Python objects are read as a spec document."""

from __future__ import annotations

import enum
import math
from collections import OrderedDict
from typing import Any

import openapi_spec_validator_rs as osv
import pytest
from openapi_spec_validator_rs import OpenAPIValidationError


class Version(str, enum.Enum):
    V30 = "3.0.3"


class Code(enum.IntEnum):
    LIMIT = 10


def title_error(make_spec, title: Any) -> OpenAPIValidationError:
    with pytest.raises(OpenAPIValidationError) as info:
        osv.validate(make_spec(info={"title": title, "version": "1"}))
    return info.value


def test_subclasses_and_tuples(make_spec) -> None:
    spec = OrderedDict(make_spec())
    spec["openapi"] = Version.V30
    spec["tags"] = ({"name": "a"}, {"name": "b"})
    spec["components"] = {"schemas": {"A": {"type": "string", "maxLength": Code.LIMIT}}}
    osv.validate(spec)


@pytest.mark.parametrize(
    ("title", "message"),
    [
        (True, "True is not of type 'string'"),
        (1.0, "1.0 is not of type 'string'"),
        (None, "None is not of type 'string'"),
        ([1, (2, 3)], "[1, [2, 3]] is not of type 'string'"),
        # Like the file reader: NaN and infinities load as null, integers beyond 64
        # bits as floats.
        (math.nan, "None is not of type 'string'"),
        (-math.inf, "None is not of type 'string'"),
        (2**64, "1.8446744073709552e+19 is not of type 'string'"),
        (10**400, "None is not of type 'string'"),
        (2**63 - 1, "9223372036854775807 is not of type 'string'"),
        (2**64 - 1, "18446744073709551615 is not of type 'string'"),
    ],
)
def test_scalar_values(make_spec, title: Any, message: str) -> None:
    assert title_error(make_spec, title).message == message


def test_values_are_copied(make_spec) -> None:
    title = ["a"]
    error = title_error(make_spec, title)
    assert error.instance == title
    assert error.instance is not title


def test_non_string_keys_are_rejected(make_spec) -> None:
    # YAML loaders produce integer keys for unquoted status codes; upstream fails on
    # them with a TypeError too.
    spec = make_spec(paths={"/a": {"get": {"responses": {200: {"description": "ok"}}}}})
    with pytest.raises(TypeError) as info:
        osv.validate(spec)
    assert str(info.value) == (
        "keys must be str, not 'int': 200 in spec['paths']['/a']['get']['responses']"
    )


@pytest.mark.parametrize(
    ("value", "type_name"),
    [({1, 2}, "set"), (b"x", "bytes"), (object(), "object"), (1j, "complex")],
)
def test_unsupported_types_are_rejected(make_spec, value: Any, type_name: str) -> None:
    with pytest.raises(
        TypeError, match=f"^unsupported type '{type_name}' at spec\\['x-a'\\]\\[1\\]"
    ):
        osv.validate(make_spec(**{"x-a": [0, value]}))


def test_circular_references_are_rejected(make_spec) -> None:
    spec = make_spec()
    spec["x-self"] = {"again": spec}
    with pytest.raises(
        ValueError,
        match=r"^Circular reference detected at spec\['x-self'\]\['again'\]$",
    ):
        osv.validate(spec)
    shared = {"type": "string"}
    # Shared (but not circular) values are fine.
    osv.validate(make_spec(components={"schemas": {"A": shared, "B": shared}}))


def nested(depth: int) -> list[Any]:
    outer: list[Any] = []
    inner = outer
    for _ in range(depth - 1):
        inner.append([])
        inner = inner[0]
    return outer


def test_nesting_limit(make_spec) -> None:
    osv.validate(make_spec(**{"x-deep": nested(9000)}))
    with pytest.raises(RecursionError, match="more than 9989 containers"):
        osv.validate(make_spec(**{"x-deep": nested(20_000)}))


def test_deep_values_in_errors(make_spec) -> None:
    error = title_error(make_spec, nested(5000))
    assert str(error).startswith("[[[[")
    depth = 0
    value = error.instance
    while value:
        value = value[0]
        depth += 1
    assert depth == 4999
