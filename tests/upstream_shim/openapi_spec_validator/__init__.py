"""openapi-spec-validator's import paths, served by openapi-spec-validator-rs.

Lets upstream's test suite run unmodified against this package (see
``tests/run_upstream_suite.py``). Each module re-exports this package's implementation;
names with no counterpart are placeholders that raise ``NotImplementedError``, and the
tests that use them are listed as expected failures in ``upstream_suite.py``.
"""

from openapi_spec_validator_rs import (
    OpenAPIV2SpecValidator,
    OpenAPIV3SpecValidator,
    OpenAPIV30SpecValidator,
    OpenAPIV31SpecValidator,
    OpenAPIV32SpecValidator,
    __version__,
    openapi_v2_spec_validator,
    openapi_v3_spec_validator,
    openapi_v30_spec_validator,
    openapi_v31_spec_validator,
    openapi_v32_spec_validator,
    validate,
    validate_spec,
    validate_spec_url,
    validate_url,
)

__all__ = [
    "OpenAPIV2SpecValidator",
    "OpenAPIV3SpecValidator",
    "OpenAPIV30SpecValidator",
    "OpenAPIV31SpecValidator",
    "OpenAPIV32SpecValidator",
    "__version__",
    "openapi_v2_spec_validator",
    "openapi_v3_spec_validator",
    "openapi_v30_spec_validator",
    "openapi_v31_spec_validator",
    "openapi_v32_spec_validator",
    "validate",
    "validate_spec",
    "validate_spec_url",
    "validate_url",
]
