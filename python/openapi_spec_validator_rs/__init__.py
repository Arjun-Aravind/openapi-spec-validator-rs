"""A fast, drop-in Rust companion to openapi-spec-validator.

The API mirrors openapi-spec-validator's: :func:`validate`, :func:`validate_url`, the
``OpenAPIV*SpecValidator`` classes and the same exception classes, plus
:func:`validate_path`.
"""

from openapi_spec_validator_rs._core import __version__
from openapi_spec_validator_rs.exceptions import (
    DuplicateOperationIDError,
    ExtraParametersError,
    OpenAPISpecValidatorError,
    OpenAPIValidationError,
    ParameterDuplicateError,
    SpecReadError,
    UnresolvableParameterError,
    ValidationAbortedError,
    ValidatorDetectError,
)
from openapi_spec_validator_rs.proxies import (
    openapi_v2_spec_validator,
    openapi_v3_spec_validator,
    openapi_v30_spec_validator,
    openapi_v31_spec_validator,
    openapi_v32_spec_validator,
)
from openapi_spec_validator_rs.shortcuts import (
    get_validator_cls,
    validate,
    validate_path,
    validate_spec,
    validate_spec_url,
    validate_url,
)
from openapi_spec_validator_rs.validators import (
    OpenAPIV2SpecValidator,
    OpenAPIV3SpecValidator,
    OpenAPIV30SpecValidator,
    OpenAPIV31SpecValidator,
    OpenAPIV32SpecValidator,
    SpecValidator,
)

__all__ = [
    "DuplicateOperationIDError",
    "ExtraParametersError",
    "OpenAPISpecValidatorError",
    "OpenAPIV2SpecValidator",
    "OpenAPIV3SpecValidator",
    "OpenAPIV30SpecValidator",
    "OpenAPIV31SpecValidator",
    "OpenAPIV32SpecValidator",
    "OpenAPIValidationError",
    "ParameterDuplicateError",
    "SpecReadError",
    "SpecValidator",
    "UnresolvableParameterError",
    "ValidationAbortedError",
    "ValidatorDetectError",
    "__version__",
    "get_validator_cls",
    "openapi_v2_spec_validator",
    "openapi_v3_spec_validator",
    "openapi_v30_spec_validator",
    "openapi_v31_spec_validator",
    "openapi_v32_spec_validator",
    "validate",
    "validate_path",
    "validate_spec",
    "validate_spec_url",
    "validate_url",
]
