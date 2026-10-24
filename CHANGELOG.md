# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## 0.1.0 - 2026-10-24

First release: a Rust port of [openapi-spec-validator](https://github.com/python-openapi/openapi-spec-validator)
0.9.0 that returns the same verdicts.

### Added

- Validation of Swagger 2.0 and OpenAPI 3.0, 3.1 and 3.2 documents with every check of
  openapi-spec-validator: the official meta-schemas; per-schema dialect checks; `default`
  values; `required` versus defined properties; duplicate parameters; unique operation
  IDs; path template parameters; duplicate tags and, for 3.2, tag hierarchies.
- `default` values validated by ports of openapi-schema-validator's `OAS30Validator`
  (Swagger 2.0, OpenAPI 3.0) and of jsonschema's validators (OpenAPI 3.1 and 3.2, including
  subschemas in other drafts), keyword by keyword: the same messages, error paths and
  exceptions, and Python's recursion limit.
- A document loader that reads JSON and YAML exactly like the reference: libyaml
  parsing with PyYAML's YAML 1.1 rules, then a JSON round trip.
- Python `re` semantics for `pattern` and `format: regex`, and openapi-schema-validator's
  format checks.
- `$ref` resolution within a document and across local files.
- The `openapi-spec-validator-rs` command, with the reference CLI's arguments, output and
  exit codes; files are validated in parallel.
- The `openapi-spec-validator-rs` Rust library (`validate`, `validate_path`, `load`), which
  validates on threads of its own, so any document can be validated from any thread.
- The `openapi_spec_validator_rs` Python package, mirroring the reference's API
  (`validate`, `validate_url`, the `OpenAPIV*SpecValidator` classes, the exception classes)
  plus `validate_path`.
- A pre-commit hook.

### Compatibility

- Same verdict and same first line of output as openapi-spec-validator 0.9.0 on all 4,138
  specs of the APIs.guru directory; see [COMPATIBILITY.md](COMPATIBILITY.md) for the
  remaining, documented differences.

