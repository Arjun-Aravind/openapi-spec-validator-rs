from __future__ import annotations

import copy
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

INFO = {"title": "Test API", "version": "1.0"}

MINIMAL_SPECS: dict[str, dict[str, Any]] = {
    "2.0": {"swagger": "2.0", "info": INFO, "paths": {}},
    "3.0": {"openapi": "3.0.3", "info": INFO, "paths": {}},
    "3.1": {"openapi": "3.1.0", "info": INFO, "paths": {}},
    "3.2": {"openapi": "3.2.0", "info": INFO, "paths": {}},
}


@pytest.fixture
def make_spec() -> Callable[..., dict[str, Any]]:
    """A fresh minimal valid spec of the given version, with top-level fields replaced."""

    def make(version: str = "3.0", **fields: Any) -> dict[str, Any]:
        spec = copy.deepcopy(MINIMAL_SPECS[version])
        spec.update(fields)
        return spec

    return make


@pytest.fixture
def write(tmp_path: Path) -> Callable[[str, str], Path]:
    """Writes a file under a temporary directory and returns its path."""

    def write(name: str, text: str) -> Path:
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    return write
