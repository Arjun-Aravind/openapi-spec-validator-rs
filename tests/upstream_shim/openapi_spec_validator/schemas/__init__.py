"""Upstream's schema backend selection.

This package reproduces the default ``jsonschema`` backend only.
"""


def get_validator_backend() -> str:
    return "jsonschema"
