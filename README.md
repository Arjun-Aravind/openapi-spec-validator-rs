# openapi-spec-validator-rs

**A fast, drop-in Rust companion to [openapi-spec-validator](https://github.com/python-openapi/openapi-spec-validator).**

It validates Swagger 2.0 and OpenAPI 3.0, 3.1 and 3.2 documents with the same rules as
openapi-spec-validator, returns the same verdicts, and runs 40 to 145 times faster on large
specs. It ships as a command-line tool, a Python package with the same API, and a pre-commit
hook. It is an independent project, not affiliated with or endorsed by the maintainers of
openapi-spec-validator.

Command-line run time, including start-up and parsing, on an Apple M4
(mean of 10 runs; the range is openapi-spec-validator with its jsonschema-rs backend and with
its default backend):

| Spec | openapi-spec-validator-rs | openapi-spec-validator | Speed-up |
|---|---:|---:|---:|
| GitHub (OpenAPI 3.0, JSON, 13.5 MB) | **57 ms** | 6.41 s – 8.18 s | **113–144×** |
| GitHub (OpenAPI 3.0, YAML, 10.3 MB) | **152 ms** | 6.45 s – 8.20 s | **42–54×** |
| Stripe (OpenAPI 3.0, JSON, 8.3 MB) | **31 ms** | 1.80 s – 2.82 s | **59–92×** |
| Kubernetes (Swagger 2.0, JSON, 4.3 MB) | **20 ms** | 762 ms – 2.13 s | **39–109×** |

## Why

openapi-spec-validator is the standard OpenAPI validator in the Python ecosystem, but on
large documents it takes seconds: most of the time goes into walking the document in
Python, and its optional jsonschema-rs backend only speeds up a small part of that. This
project ports the whole validator to Rust, rule for rule, so CI pipelines, pre-commit
hooks and test suites get the same answers without the wait.

"Same answers" is checked, not assumed:

- **Every check is ported.** Each keyword validator in openapi-spec-validator's
  `keywords.py` has a counterpart; [COMPATIBILITY.md][compatibility] lists them, along
  with every known difference.
- **Real-world agreement.** On all 4,138 specs in the [APIs.guru](https://apis.guru)
  directory, the verdict (valid, or which error class) and the first line of output match
  openapi-spec-validator 0.9.0 **100%** of the time, including the 482 specs it rejects.
- **Upstream's own test suite** runs against the Python package, unmodified; the 17 of its
  136 tests that cannot pass (argparse's wording, upstream's internal settings and caches)
  are listed with their reasons.
- **The quirks too.** JSON and YAML are parsed exactly as PyYAML parses them, `pattern`s
  follow Python's `re` rules, and errors are reported with the same messages, in the same
  order.

## Install

```bash
pip install openapi-spec-validator-rs
```

This installs the `openapi-spec-validator-rs` command and the `openapi_spec_validator_rs`
Python package (wheels for Linux, macOS and Windows; Python 3.10+). To build the command
from source instead:

```bash
cargo install --path crates/openapi-spec-validator-rs-cli
```

## Command line

Use it exactly like `openapi-spec-validator`:

```bash
openapi-spec-validator-rs openapi.yaml
```

```text
openapi.yaml: OK
```

It accepts the same arguments (`--schema {detect,2.0,3.0,3.1,3.2}`,
`--validation-errors {first,all}`, `--subschema-errors {best-match,all}`, `-` for standard
input), prints the same messages and exits with the same status: 0 when valid, 1 for a
validation or read error, 2 for any other error. Several files are validated in parallel
and reported in order. `$ref`s to other files and to `http(s)://` URLs are followed as
upstream follows them: the `pip`-installed command fetches with Python's `urllib` (or
`requests`, when installed) like upstream, the Rust binary with a built-in client that
uses the operating system's certificate store and the `HTTPS_PROXY`/`NO_PROXY` variables.

## Python

The package mirrors openapi-spec-validator's API:

```python
from openapi_spec_validator_rs import validate, validate_path, validate_url
from openapi_spec_validator_rs import OpenAPIV31SpecValidator, OpenAPIValidationError

validate(spec_dict)                     # raises the first error
validate_path("openapi.yaml")           # reads the file like the command line
validate_url("https://example.com/openapi.json")

errors = list(OpenAPIV31SpecValidator(spec_dict).iter_errors())
```

Errors are `OpenAPIValidationError` (or one of its subclasses: `DuplicateOperationIDError`,
`ParameterDuplicateError`, `UnresolvableParameterError`, `ExtraParametersError`) with the
same attributes and messages as upstream's. Moving over is usually a matter of changing the
import from `openapi_spec_validator` to `openapi_spec_validator_rs`.

## Validate your FastAPI schema in CI

FastAPI builds its OpenAPI document at runtime, so check it in your test suite:

```python
# tests/test_openapi.py
from openapi_spec_validator_rs import validate

from myapp.main import app


def test_openapi_schema_is_valid():
    validate(app.openapi())
```

Run it in CI like any other test:

```yaml
# .github/workflows/ci.yml
- run: pip install -e . pytest openapi-spec-validator-rs
- run: pytest tests/test_openapi.py
```

## pre-commit

```yaml
# .pre-commit-config.yaml
repos:
  - repo: local
    hooks:
      - id: openapi-spec-validator-rs
        name: openapi-spec-validator-rs
        entry: openapi-spec-validator-rs
        language: python
        additional_dependencies: [openapi-spec-validator-rs==0.1.0]
        files: .*(openapi|swagger).*\.(json|yaml|yml)$
```

Adjust `files:` to match your spec files. The repository also ships a
`.pre-commit-hooks.yaml`, so the hook can be referenced as
`repo: https://github.com/Arjun-Aravind/openapi-spec-validator-rs` with a `rev` instead,
but pre-commit then builds the package from source, which needs a Rust toolchain on every
machine that runs it; the form above installs the prebuilt wheel.

## Benchmarks

![Validation time per spec][chart]

Calling `validate(spec_dict)` from Python on an already-loaded document (what a test that
validates FastAPI's `app.openapi()` does), median of 5 runs:

| Spec | openapi-spec-validator-rs | openapi-spec-validator, jsonschema-rs backend | openapi-spec-validator, default backend |
|---|---:|---:|---:|
| GitHub (13.5 MB) | **33 ms** | 4.67 s (143× slower) | 6.38 s (196× slower) |
| Stripe (8.3 MB) | **16 ms** | 0.89 s (55× slower) | 1.91 s (119× slower) |
| Kubernetes (4.3 MB) | **11 ms** | 0.33 s (29× slower) | 1.70 s (149× slower) |

It also needs about a quarter of the memory on JSON specs: 81 MiB instead of 290 MiB for
GitHub's.

Validating every spec in the APIs.guru directory that openapi-spec-validator accepts (3,646
files, 535 MB of YAML) takes 1.6 s instead of 202–332 s.

See [BENCHMARKS.md][benchmarks] for the method, every measurement and how to reproduce
them.

## How it works

The validator is a Rust library (`crates/openapi-spec-validator-rs`) with a CLI and
PyO3 bindings on top. It follows the reference implementation step by step:

1. **Load**: JSON goes through serde_json when libyaml would read it the same way; anything
   else goes through libyaml (the parser inside PyYAML) with PyYAML's YAML 1.1 rules, then
   through the same JSON round trip.
2. **Meta-schema**: the whole document is validated against the official schema for its
   version with the [`jsonschema`](https://crates.io/crates/jsonschema) crate.
3. **Walk**: the document is walked like `keywords.py`, following `$ref`s (into other
   files and over HTTP, fetched when first reached), checking every reachable schema
   against its dialect, `default` values against their schemas, and operation IDs,
   parameters, path templates and tags.

Python-specific behaviour (`re` syntax, `str.format` parsing of path templates, `repr` in
messages, format checks) is reproduced by small modules tested against CPython.

## Compatibility

See [COMPATIBILITY.md][compatibility]. In short: verdicts match openapi-spec-validator
0.9.0's default installation, `$ref`s to other files and to `http(s)://` URLs included; a
handful of pathological inputs (schemas nested over 120 levels deep, NaN literals) behave
differently.

## License

Apache-2.0, like openapi-spec-validator, which this project ports. See [LICENSE][license]
and [NOTICE][notice].

[benchmarks]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/blob/main/BENCHMARKS.md
[chart]: https://raw.githubusercontent.com/Arjun-Aravind/openapi-spec-validator-rs/main/bench/benchmarks.svg?sanitize=true
[compatibility]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/blob/main/COMPATIBILITY.md
[license]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/blob/main/LICENSE
[notice]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/blob/main/NOTICE
