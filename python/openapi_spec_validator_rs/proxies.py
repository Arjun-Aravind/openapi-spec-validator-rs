"""Upstream's deprecated validator objects (``openapi_v30_spec_validator`` etc.).

Kept so that code written against openapi-spec-validator keeps working; new code
should use the validator classes.
"""

from __future__ import annotations

import warnings
from collections.abc import Iterator, Mapping
from typing import Any

from openapi_spec_validator_rs.exceptions import (
    OpenAPIValidationError,
    ValidatorDetectError,
)
from openapi_spec_validator_rs.validators import (
    OpenAPIV2SpecValidator,
    OpenAPIV30SpecValidator,
    OpenAPIV31SpecValidator,
    OpenAPIV32SpecValidator,
    SpecValidatorType,
)

__all__ = [
    "DetectValidatorProxy",
    "SpecValidatorProxy",
    "openapi_spec_validator_proxy",
    "openapi_v2_spec_validator",
    "openapi_v3_spec_validator",
    "openapi_v30_spec_validator",
    "openapi_v31_spec_validator",
    "openapi_v32_spec_validator",
]


class SpecValidatorProxy:
    def __init__(
        self,
        cls: SpecValidatorType,
        deprecated: str = "SpecValidator",
        use: str | None = None,
    ) -> None:
        self.cls = cls
        self.deprecated = deprecated
        self.use = use or self.cls.__name__

    def validate(
        self, schema: Any, base_uri: str = "", spec_url: str | None = None
    ) -> None:
        for err in self.iter_errors(schema, base_uri=base_uri, spec_url=spec_url):
            raise err

    def is_valid(self, schema: Any) -> bool:
        error = next(self.iter_errors(schema), None)
        return error is None

    def iter_errors(
        self, schema: Any, base_uri: str = "", spec_url: str | None = None
    ) -> Iterator[OpenAPIValidationError]:
        warnings.warn(
            f"{self.deprecated} is deprecated. Use {self.use} instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        validator = self.cls(schema, base_uri=base_uri, spec_url=spec_url)
        return validator.iter_errors()


class DetectValidatorProxy:
    def __init__(self, choices: Mapping[tuple[str, str], SpecValidatorProxy]) -> None:
        self.choices = choices

    def detect(self, instance: Any) -> SpecValidatorProxy:
        for (key, value), validator in self.choices.items():
            if key in instance and instance[key].startswith(value):
                return validator
        raise ValidatorDetectError("Spec schema version not detected")

    def validate(
        self, instance: Any, base_uri: str = "", spec_url: str | None = None
    ) -> None:
        validator = self.detect(instance)
        for err in validator.iter_errors(
            instance, base_uri=base_uri, spec_url=spec_url
        ):
            raise err

    def is_valid(self, instance: Any) -> bool:
        validator = self.detect(instance)
        error = next(validator.iter_errors(instance), None)
        return error is None

    def iter_errors(
        self, instance: Any, base_uri: str = "", spec_url: str | None = None
    ) -> Iterator[OpenAPIValidationError]:
        warnings.warn(
            "openapi_spec_validator_proxy is deprecated.",
            DeprecationWarning,
            stacklevel=2,
        )
        validator = self.detect(instance)
        yield from validator.iter_errors(instance, base_uri=base_uri, spec_url=spec_url)


openapi_v2_spec_validator = SpecValidatorProxy(
    OpenAPIV2SpecValidator,
    deprecated="openapi_v2_spec_validator",
    use="OpenAPIV2SpecValidator",
)
openapi_v30_spec_validator = SpecValidatorProxy(
    OpenAPIV30SpecValidator,
    deprecated="openapi_v30_spec_validator",
    use="OpenAPIV30SpecValidator",
)
openapi_v31_spec_validator = SpecValidatorProxy(
    OpenAPIV31SpecValidator,
    deprecated="openapi_v31_spec_validator",
    use="OpenAPIV31SpecValidator",
)
openapi_v32_spec_validator = SpecValidatorProxy(
    OpenAPIV32SpecValidator,
    deprecated="openapi_v32_spec_validator",
    use="OpenAPIV32SpecValidator",
)
# Alias to the latest 3.x version, as upstream.
openapi_v3_spec_validator = openapi_v32_spec_validator

openapi_spec_validator_proxy = DetectValidatorProxy(
    {
        ("swagger", "2.0"): openapi_v2_spec_validator,
        ("openapi", "3.0"): openapi_v30_spec_validator,
        ("openapi", "3.1"): openapi_v31_spec_validator,
        ("openapi", "3.2"): openapi_v32_spec_validator,
    },
)
