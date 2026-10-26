"""Version detection, as upstream's ``versions.finders``."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from openapi_spec_validator_rs.exceptions import OpenAPIVersionNotFound
from openapi_spec_validator_rs.versions import _PATTERN, SpecVersion

__all__ = ["SpecVersionFinder"]


class SpecVersionFinder:
    """Finds which of ``versions`` a document declares."""

    pattern = _PATTERN

    def __init__(self, versions: list[SpecVersion]) -> None:
        self.versions = versions

    def find(self, spec: Mapping[str, Any]) -> SpecVersion:
        for version in self.versions:
            if version.keyword in spec:
                match = self.pattern.match(spec[version.keyword])
                if match:
                    found = SpecVersion(**match.groupdict(), keyword=version.keyword)
                    if found == version:
                        return version
        raise OpenAPIVersionNotFound
