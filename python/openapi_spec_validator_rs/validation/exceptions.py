"""The validation exceptions, as upstream's ``validation.exceptions``."""

from openapi_spec_validator_rs.exceptions import (
    DuplicateOperationIDError,
    ExtraParametersError,
    OpenAPIValidationError,
    ParameterDuplicateError,
    UnresolvableParameterError,
    ValidationAbortedError,
    ValidatorDetectError,
)

__all__ = [
    "DuplicateOperationIDError",
    "ExtraParametersError",
    "OpenAPIValidationError",
    "ParameterDuplicateError",
    "UnresolvableParameterError",
    "ValidationAbortedError",
    "ValidatorDetectError",
]
