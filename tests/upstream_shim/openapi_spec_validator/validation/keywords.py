"""Upstream's keyword validators: no counterpart, the walk is implemented in Rust.

``validator_for`` exists so that tests can monkeypatch it.
"""

from typing import Any


def validator_for(*args: Any, **kwargs: Any) -> Any:
    raise NotImplementedError("the keyword validators are implemented in Rust")
