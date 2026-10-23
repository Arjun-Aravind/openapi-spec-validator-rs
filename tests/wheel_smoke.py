"""Smoke test for an installed wheel: ``python tests/wheel_smoke.py``.

Needs only the package itself. Run it from outside the source tree so that the
installed copy is the one imported.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

import openapi_spec_validator_rs as osv

SPEC = """\
openapi: 3.1.0
info: {title: Pets, version: "1.0"}
paths:
  /pets/{id}:
    get:
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer}}
      responses:
        "200": {description: ok}
"""


def main() -> None:
    print(f"openapi-spec-validator-rs {osv.__version__} from {osv.__file__}")
    osv.validate(
        {"openapi": "3.0.3", "info": {"title": "t", "version": "1"}, "paths": {}}
    )
    try:
        osv.validate({"swagger": "2.0", "info": {"title": "t"}, "paths": {}})
    except osv.OpenAPIValidationError as error:
        assert error.message == "'version' is a required property", error.message
    else:
        raise AssertionError("an invalid spec passed")

    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "pets.yaml"
        path.write_text(SPEC, encoding="utf-8")
        osv.validate_path(path)
        osv.validate_url(path.as_uri())
        for command in (
            [sys.executable, "-m", "openapi_spec_validator_rs", str(path)],
            ["openapi-spec-validator-rs", str(path)],
        ):
            result = subprocess.run(command, capture_output=True, text=True, check=True)
            assert result.stdout == f"{path}: OK\n", result.stdout
    print("ok")


if __name__ == "__main__":
    main()
