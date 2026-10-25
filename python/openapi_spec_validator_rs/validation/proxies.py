"""The deprecated validator proxies, as upstream's ``validation.proxies``."""

from openapi_spec_validator_rs.proxies import (
    DetectValidatorProxy,
    SpecValidatorProxy,
    openapi_spec_validator_proxy,
    openapi_v2_spec_validator,
    openapi_v3_spec_validator,
    openapi_v30_spec_validator,
    openapi_v31_spec_validator,
    openapi_v32_spec_validator,
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
