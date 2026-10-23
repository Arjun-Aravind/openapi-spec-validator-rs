"""The command line: ``python -m openapi_spec_validator_rs`` and ``main()``."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import openapi_spec_validator_rs
import pytest
from openapi_spec_validator_rs.__main__ import main

VALID = "openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths: {}\n"
INVALID = "openapi: 3.0.3\ninfo: {title: t}\npaths: {}\n"


def run(*args: str, stdin: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "openapi_spec_validator_rs", *args],
        input=stdin,
        capture_output=True,
        text=True,
        check=False,
    )


def test_module_valid_and_invalid(write) -> None:
    valid = write("valid.yaml", VALID)
    result = run(str(valid))
    assert (result.returncode, result.stdout, result.stderr) == (
        0,
        f"{valid}: OK\n",
        "",
    )

    invalid = write("invalid.yaml", INVALID)
    result = run(str(valid), str(invalid))
    assert result.returncode == 1
    assert result.stdout.startswith(
        f"{valid}: OK\n{invalid}: Validation Error: 'version' is a required property\n"
    )


def test_module_errors(tmp_path: Path, write) -> None:
    result = run(str(tmp_path / "missing.yaml"))
    assert (result.returncode, result.stdout) == (
        1,
        f"No such file: {tmp_path / 'missing.yaml'}\n",
    )

    unresolvable = write(
        "ref.yaml",
        VALID.replace("paths: {}", "paths: {/a: {$ref: 'other.yaml'}}"),
    )
    result = run(str(unresolvable))
    assert result.returncode == 2
    assert result.stdout.startswith(f"{unresolvable}: Error: ")


def test_module_stdin() -> None:
    result = run("-", stdin=VALID)
    assert (result.returncode, result.stdout) == (0, "stdin: OK\n")
    result = run("--validation-errors", "all", "-", stdin="openapi: 3.0.0\n")
    assert result.returncode == 1
    assert result.stdout.endswith("stdin: 2 validation errors found\n")


def test_module_usage() -> None:
    result = run("--version")
    assert (result.returncode, result.stdout) == (
        0,
        f"openapi-spec-validator-rs {openapi_spec_validator_rs.__version__}\n",
    )
    result = run()
    assert result.returncode == 2
    assert "Usage:" in result.stderr


def test_main_returns_on_success(write, capsys: pytest.CaptureFixture[str]) -> None:
    valid = write("valid.yaml", VALID)
    assert main([str(valid)]) is None
    assert capsys.readouterr().out == f"{valid}: OK\n"


@pytest.mark.parametrize(
    ("args", "code"),
    [
        (["{invalid}"], 1),
        (["--version"], 0),
        (["--help"], 0),
        (["--schema", "9"], 2),
        ([], 2),
    ],
)
def test_main_exits(write, args: list[str], code: int) -> None:
    invalid = write("invalid.yaml", INVALID)
    with pytest.raises(SystemExit) as info:
        main([arg.format(invalid=invalid) for arg in args])
    assert info.value.code == code


def test_console_script_is_declared() -> None:
    from importlib.metadata import entry_points

    [script] = entry_points(group="console_scripts", name="openapi-spec-validator-rs")
    assert script.value == "openapi_spec_validator_rs.__main__:main"
