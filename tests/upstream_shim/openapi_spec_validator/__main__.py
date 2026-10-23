"""``openapi_spec_validator.__main__``: this package's command line.

The other names mirror upstream's module namespace so that the tests' ``mock.patch``
targets (``sys.stdin``, ``os.environ``, ``schemas``, the validator classes) exist.
"""

import os
import sys

from openapi_spec_validator_rs.__main__ import main

from openapi_spec_validator import __version__, schemas
from openapi_spec_validator.shortcuts import get_validator_cls, validate
from openapi_spec_validator.validation import (
    OpenAPIV2SpecValidator,
    OpenAPIV30SpecValidator,
    OpenAPIV31SpecValidator,
    OpenAPIV32SpecValidator,
    SpecValidator,
)

__all__ = [
    "OpenAPIV2SpecValidator",
    "OpenAPIV30SpecValidator",
    "OpenAPIV31SpecValidator",
    "OpenAPIV32SpecValidator",
    "SpecValidator",
    "__version__",
    "get_validator_cls",
    "main",
    "os",
    "schemas",
    "sys",
    "validate",
]

if __name__ == "__main__":
    main()
