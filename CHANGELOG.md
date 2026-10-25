# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- Documents nested deeper than upstream's recursion allows are reported as upstream
  reports them, `Error: maximum recursion depth exceeded`, instead of being validated:
  around 122 levels of nested schemas in OpenAPI 3.0, 162 in Swagger 2.0, 61 to 81 in
  3.1 and 3.2. A model of the Python frames jsonschema's traversal uses, fitted to
  measurements of upstream, finds where upstream gives up (counting the caller's frames
  and `sys.getrecursionlimit()` in the Python API), and an error upstream finds before
  getting that deep is still reported first. This also ends the quadratic growth of
  error output on such documents: an invalid 3.0 schema 1,000 levels deep (41 KB) took
  1.7 GB and printed 21 MB; it now fails at once, as upstream does.
- Ctrl-C, or any exception a signal handler raises, now cancels the validation under
  way instead of leaving it running unobserved on its thread (three interrupted runs
  kept 2.5 CPU cores busy until they finished).
- Streams that start with a UTF-16 byte-order mark are read only as far as libyaml's
  UTF-16 decoder gets, like other streams since 0.2.2: an endless one no longer exhausts
  memory. Standard input is checked as UTF-8, which is how Python decodes it.
- A file that claims more bytes than memory allows (a sparse file) no longer aborts the
  process: the size is only a hint for the buffer.
- Swagger 2.0 (and OpenAPI 3.0) `type: 'null'` with a `null` default reports
  `None for not nullable` again when only the first error is wanted, as upstream does;
  0.2.2 raised `Unknown type 'null'` there, which upstream only does when collecting
  every error.
- A `__repr__` that changes the document no longer escapes as `PanicException`, and an
  object whose `repr()` raises only fails validation where upstream would print it:
  objects are described after the document has been read, and a dict changed while it
  is read raises `RuntimeError` like Python's own iteration.
- Importing the package in a second interpreter of the same process (a subinterpreter
  after the main interpreter, as mod_wsgi does with several applications) raises
  `ImportError` instead of handing it the first interpreter's objects, which broke on
  Python 3.12 and could crash.

### Added

- `openapi_spec_validator_rs.validation.types`, which openapi-core imports from upstream.

### Changed

- The wheels are stripped of debugging symbols (about a quarter smaller), and the
  crates.io token is only visible to the publishing step of the release workflow.

## [0.2.2] - 2026-10-25

### Added

- `openapi_spec_validator_rs.readers` (`read_from_filename`, `read_from_stdin`) and the
  `validation` and `versions` sub-packages with upstream's module names
  (`validation.validators`, `validation.exceptions`, `validation.proxies`,
  `versions.consts`, `versions.datatypes`, `versions.exceptions`, `versions.shortcuts`), so
  that upstream's documented imports work with the package name changed.
- The Python API accepts documents holding objects of types JSON has no counterpart for,
  as `yaml.safe_load` produces them: a `datetime.date` or `datetime.datetime` from an
  unquoted timestamp, `bytes`, a `set`, any other object. They are handled as jsonschema
  handles them, of no type: accepted in `example`, extensions and untyped `default`s,
  rejected with their `repr()` wherever a type is required
  (`datetime.date(2020, 1, 1) is not of type 'string'`), equal to themselves in `enum`,
  and returned as the error's `instance`. Such documents raised `TypeError` before; about
  one valid YAML spec in ten holds a date once loaded with `yaml.safe_load`.
- `read_until_unreadable` in the Rust library: reads a stream only as far as PyYAML's
  reader would get.

### Fixed

- Documents and `$ref` targets are read only as far as PyYAML would read them: a file or
  body that libyaml cannot decode (`/dev/zero`, a binary) is given up on at its first bad
  bytes, with upstream's message, instead of being read until memory runs out.
- Ctrl-C (`KeyboardInterrupt`) takes effect at once in the Python API and the
  `pip`-installed command line, as it does upstream, rather than when the validation under
  way returns; the signal was ignored for the whole run, 10 seconds for a stalled download.
- A `dependencies` or `dependentRequired` failure is worded as Python's jsonschema words
  it (`'minimum' is a dependency of 'exclusiveMinimum'`); it was reported as a missing
  required property.
- Validating a `default` against an OpenAPI 3.0 (or Swagger 2.0) schema with
  `type: 'null'` fails like upstream (`Unknown type 'null' for validator with schema`,
  exit status 2) instead of accepting the type.
- `int()` and `float()` emulations accept every script's decimal digits and whitespace as
  Python's do: a `$ref` pointer segment such as `/parameters/٠`, and `!!int` / `!!float`
  tagged scalars, no longer fail where upstream reads them.
- The command line no longer panics when no thread can be started (a process limit
  reached): it validates the files one after another on its own thread.
- The libyaml wrapper owns the parser through a raw pointer, as libyaml keeps a pointer to
  the parser inside it: moving the box that held it broke Rust's aliasing rules (reported
  by Miri; no misbehaviour was observed).
- GitHub Actions in the workflows are pinned to commits, kept current by Dependabot.

## [0.2.1] - 2026-10-25

### Fixed

- Reading a document whose aliases nest deeper than the limit no longer builds the
  expanded value first: a 240 KB chain of 10,000 aliases took 3.8 GB before failing with
  the recursion error (upstream holds 160 MB at that point); the error is now found
  before anything is built.
- A `$ref` whose pointer reaches into a string resolves as upstream resolves it: a
  numeric segment yields that character (so `#/info/title/0` is validated as the schema
  `'t'`), and any other segment fails with Python's `invalid literal for int()` message.
  Both were reported as the pointer not existing.
- The remote-reference tests no longer fail when a proxy is configured in the
  environment; `HttpRetriever::without_proxies` ignores the proxy variables.

## [0.2.0] - 2026-10-24

### Added

- `$ref`s to `http://` and `https://` documents are followed, as openapi-spec-validator
  follows them: the document is downloaded when validation first reaches a reference to
  it, once per validation, parsed like a file, and any failure (network or TLS error,
  error status, 10 second timeout, unparsable body) makes the reference unresolvable
  (`Error: <ref>`, exit status 2). The Python package downloads with Python's `urllib`,
  or `requests` when that package is installed, exactly as upstream's handlers do; the
  Rust command line and library use a built-in client (the `http` feature of the
  library crate, on by default) that requests the URL as `urljoin` forms it, follows
  redirects with `urllib`'s rules, checks certificates against the operating system's
  store and reads `http_proxy`, `https_proxy` and `no_proxy` as `urllib` does. See
  COMPATIBILITY.md for the details.
- The `Retrieve` trait and `Options::retriever` in the library crate: how remote
  documents are fetched, or `None` to leave such references unresolvable.
- Wheels for musl-based Linux (Alpine), x86_64 and aarch64.

### Fixed

- A malformed version in a document read from a file or URL raised a `TypeError` worded
  as CPython 3.12 words it on every interpreter; it is now the running interpreter's
  own wording, as for documents passed as objects and as upstream's.

### Changed

- The Python package imports `urllib.request` and `pprint` only when they are needed,
  which takes about 20 ms off every run of the `pip`-installed command. README.md and
  BENCHMARKS.md now say which command their figures are for, and give the
  `pip`-installed command's.
- Documents named by `$ref`s are read when a reference to them is first followed, like
  upstream, instead of all being read before validation starts; references inside
  examples and extensions are no longer read at all.
- `openapi_spec_validator_rs_cli::run_with_stdin` takes the retriever to use.
- Reading stops as soon as a document nests deeper than the 9,989 containers upstream's
  JSON round trip accepts, with the same error as before, instead of scanning the whole
  file first: a file of 100,000 nested mappings took 25 seconds (libyaml's scanner slows
  quadratically with nesting) and now fails in well under a second.

## [0.1.0] - 2026-10-24

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

[0.2.2]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/releases/tag/v0.2.2
[0.2.1]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/releases/tag/v0.2.1
[0.2.0]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/releases/tag/v0.2.0
[0.1.0]: https://github.com/Arjun-Aravind/openapi-spec-validator-rs/releases/tag/v0.1.0
