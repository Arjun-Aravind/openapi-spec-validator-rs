"""Specification versions and version detection, as upstream's ``versions`` package."""

from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final

from openapi_spec_validator_rs.exceptions import OpenAPIVersionNotFound

__all__ = [
    "OPENAPIV2",
    "OPENAPIV30",
    "OPENAPIV31",
    "OPENAPIV32",
    "VERSIONS",
    "SpecVersion",
    "get_spec_version",
]


@dataclass(frozen=True)
class SpecVersion:
    """Spec version designates the OAS feature set."""

    keyword: str
    major: str
    minor: str

    def __str__(self) -> str:
        return f"OpenAPIV{self.major}.{self.minor}"


OPENAPIV2: Final = SpecVersion(keyword="swagger", major="2", minor="0")
OPENAPIV30: Final = SpecVersion(keyword="openapi", major="3", minor="0")
OPENAPIV31: Final = SpecVersion(keyword="openapi", major="3", minor="1")
OPENAPIV32: Final = SpecVersion(keyword="openapi", major="3", minor="2")

VERSIONS: Final[list[SpecVersion]] = [OPENAPIV2, OPENAPIV30, OPENAPIV31, OPENAPIV32]

_PATTERN: Final = re.compile(r"(?P<major>\d+)\.(?P<minor>\d+)(\..*)?")


def get_spec_version(spec: Mapping[str, Any]) -> SpecVersion:
    """Detects the version from the ``swagger`` / ``openapi`` field.

    Same algorithm as upstream, including its exceptions: :class:`OpenAPIVersionNotFound`
    when no known version matches, and ``TypeError`` for non-string versions or
    documents that are not mappings.
    """
    for version in VERSIONS:
        if version.keyword in spec:
            match = _PATTERN.match(spec[version.keyword])
            if match:
                found = SpecVersion(**match.groupdict(), keyword=version.keyword)
                if found == version:
                    return version
    raise OpenAPIVersionNotFound
