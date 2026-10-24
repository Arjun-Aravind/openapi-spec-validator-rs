"""pytest plugin that runs openapi-spec-validator's own tests against this package.

Loaded by ``tests/run_upstream_suite.py``, with the ``openapi_spec_validator`` shim
package next to this file first on ``sys.path``. Every test that cannot pass is listed
below with the reason and marked as an expected failure: it is reported, never skipped,
and (unless it is known to pass by accident) an unexpected pass fails the run so that
the lists stay accurate.
"""

from __future__ import annotations

from pathlib import Path

import pytest

SHIM = Path(__file__).resolve().parent
ROOT = SHIM.parents[1]
UPSTREAM = ROOT / "upstream"
INTEGRATION = UPSTREAM / "tests" / "integration"

_NO_SETTINGS = (
    "upstream's pydantic settings (resolver cache size, schema backend) have no "
    "counterpart: there is one backend and no resolver cache to size"
)
_RUST_CLI = (
    "the command line runs in Rust, so patching a Python validator method has no effect"
)
_UNRESOLVABLE = (
    "unresolvable references raise ValidationAbortedError (same message), not "
    "referencing.exceptions.Unresolvable: this package does not depend on referencing"
)
_CACHE_INTERNALS = (
    "counts calls to upstream's internal jsonschema-path SchemaPath.from_dict; "
    "references are resolved in Rust"
)

# Differences that are intentional or cannot be removed. Keys are test IDs relative to
# upstream's `tests/integration`; a key without `[...]` covers every parametrization.
KNOWN_DIFFERENCES: dict[str, str] = {
    "test_main.py::test_schema_unknown": (
        "argument errors are worded by clap, not argparse "
        "(\"invalid value 'x.x' for '--schema ...'\")"
    ),
    "test_main.py::test_unknown_error": _RUST_CLI,
    "test_main.py::test_subschema_details_gated_for_jsonschema_rs_backend": (
        "patches upstream's jsonschema-rs backend switch; this package reproduces the "
        "default jsonschema backend only"
    ),
    "test_main.py::test_version": (
        "the program is named openapi-spec-validator-rs and has its own version"
    ),
    "test_shortcuts.py::test_validate_uses_resolved_cache_maxsize_env": _NO_SETTINGS,
    "test_shortcuts.py::test_validate_uses_default_resolved_cache_on_invalid_env": (
        _NO_SETTINGS
    ),
    "test_shortcuts.py::test_schema_validator_backend_env_valid_values": _NO_SETTINGS,
    "test_shortcuts.py::test_schema_validator_backend_env_invalid_value_warns": (
        _NO_SETTINGS
    ),
    "validation/test_dialect.py::test_meta_schema_checker_cache_reuses_known_dialect": (
        "counts calls to upstream's internal keywords.validator_for; meta-schema "
        "validators are compiled once in Rust"
    ),
    "validation/test_dialect.py::test_meta_schema_checker_cache_reuses_unknown_dialect": (
        "counts calls to upstream's internal keywords.validator_for; meta-schema "
        "validators are compiled once in Rust"
    ),
    "validation/test_exceptions.py::TestSpecValidatorIterErrors"
    "::test_parameter_default_value_custom_format_invalid": (
        "registers a Python format checker with openapi-schema-validator; formats are "
        "checked in Rust and custom formats cannot be plugged in"
    ),
    "validation/test_validators.py::TestLocalOpenAPIv2Validator::test_ref_failed": (
        _UNRESOLVABLE
    ),
    "validation/test_validators.py::TestLocalOpenAPIv30Validator::test_ref_failed": (
        _UNRESOLVABLE
    ),
    "validation/test_validators.py::test_spec_validator_uses_resolved_cache_maxsize_env": (
        _CACHE_INTERNALS
    ),
    "validation/test_validators.py"
    "::test_spec_validator_uses_default_resolved_cache_on_invalid_env": _CACHE_INTERNALS,
}



def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers", "network: marks tests which do need network-enabled environment"
    )
    import openapi_spec_validator

    module = Path(openapi_spec_validator.__file__).resolve()
    if SHIM not in module.parents:
        raise pytest.UsageError(
            f"openapi_spec_validator resolves to {module}, not to the shim in {SHIM}"
        )


def _key(item: pytest.Item) -> str:
    relative = Path(item.path).resolve().relative_to(INTEGRATION).as_posix()
    return relative + "::" + item.nodeid.split("::", 1)[1]


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    for item in items:
        key = _key(item)
        reason = KNOWN_DIFFERENCES.get(key) or KNOWN_DIFFERENCES.get(key.split("[", 1)[0])
        if reason:
            item.add_marker(pytest.mark.xfail(reason=reason, strict=True))


@pytest.fixture(autouse=True)
def _upstream_cwd(monkeypatch: pytest.MonkeyPatch) -> None:
    """Upstream's CLI tests use file paths relative to its checkout."""
    monkeypatch.chdir(UPSTREAM)
