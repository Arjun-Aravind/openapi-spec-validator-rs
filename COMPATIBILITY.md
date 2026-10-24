# Compatibility with openapi-spec-validator

This project aims to return the same verdict as
[openapi-spec-validator](https://github.com/python-openapi/openapi-spec-validator) 0.9.0
for every document: valid or invalid, and if invalid, the same error class. This page
lists every check upstream performs, where it lives in this port, and every known
difference.

The reference is upstream's default installation (`pip install openapi-spec-validator`
on CPython 3.13), i.e. the pure-Python `jsonschema` backend without optional format
packages such as `rfc3987` or `fqdn`.

## Checks

Every keyword validator in upstream's `openapi_spec_validator/validation/keywords.py` is
ported as its own function, in the same evaluation order. Nothing is skipped.

| Upstream | What it checks | Port | Status |
|---|---|---|---|
| `SpecValidator.iter_errors` (meta-schema) | Whole document against the official OpenAPI/Swagger JSON Schema (formats not checked) | `meta::document_validator`, `validate` | ported |
| `SpecVersionFinder` | Version detection from `swagger` / `openapi` | `version::detect_version` | ported |
| `RootValidator` | Walk order: tags, paths, components | `Walker::root` | ported |
| `TagsValidator` | Duplicate tag names | `Walker::tags` | ported |
| `OpenAPIV32TagsValidator` | 3.2: unknown parent tags, circular tag hierarchies | `Walker::tags_v32` | ported |
| `PathsValidator` | Iterates path items | `Walker::paths` | ported |
| `PathValidator` / `OpenAPIV32PathValidator` | Path-level parameters; operations (3.2 adds `query` and `additionalOperations`) | `Walker::path` | ported |
| `OperationValidator` | Unique `operationId`s; path template parameters declared and declared path parameters used | `Walker::operation`, `Walker::path_param_names` | ported |
| `ParametersValidator` | Duplicate `(name, in)` parameters | `Walker::parameters` | ported |
| `ParameterValidator` / `OpenAPIV2ParameterValidator` | Parameter schemas; 2.0 parameter `default` against the parameter | `Walker::parameter` | ported |
| `ResponsesValidator` | Iterates responses | `Walker::responses` | ported |
| `OpenAPIV2ResponseValidator` / `OpenAPIV3ResponseValidator` | Response schema (2.0) / response content (3.x) | `Walker::response` | ported |
| `ContentValidator` / `MediaTypeValidator` | Media type schemas | `Walker::content`, `Walker::media_type` | ported |
| `ComponentsValidator` / `SchemasValidator` | `components.schemas` | `Walker::components`, `Walker::schemas` | ported |
| `SchemaValidator.__call__` | Recursion into `allOf`/`anyOf`/`oneOf`/`not`/`items`/`properties`, deduplicated by identity (first visit wins) | `Walker::schema` | ported |
| `SchemaValidator._validate_schema_meta` | Each top-level schema against its dialect's meta-schema, with format checks (3.0: Draft 4 + OAS 3.0 formats; 3.1/3.2: `$schema` or `jsonSchemaDialect`) | `Walker::validate_schema_meta`, `meta::meta_check_validator` | ported |
| `SchemaValidator._collect_properties` + required check | `required` names that no property defines (only when the schema has `allOf`) | `Walker::collect_properties`, `undefined_required` | ported |
| `ValueValidator` (`OpenAPIV30/31/32ValueValidator`) | `default` values valid against their schema, with openapi-schema-validator's `OAS30Validator` (3.0, 2.0) or `OAS31Validator`/`OAS32Validator` (3.1, 3.2) | `values::DefaultValidator`; 3.1/3.2 in `pyjsonschema`, a port of jsonschema's validators | ported |
| `jsonschema-path` `SchemaPath` + `referencing` | `$ref` resolution at every traversal step, JSON pointers, other files | `document::Documents` | ported (see below) |
| `FileHandler` (PyYAML `CSafeLoader` + JSON round trip) | Reading JSON and YAML | `loader` | ported |

### What upstream does not check (and neither does this port)

Upstream's walk only reaches schemas under `paths.*.*.parameters[].schema`,
`paths.*.*.responses.*.content.*.schema` (3.x) or `.schema` (2.0), path-level parameters,
and `components.schemas`. So the following are covered by the meta-schema only:

* `example` / `examples` values are never validated against schemas; only `default` is.
* Request bodies, response headers, callbacks, links and `components.parameters`,
  `responses`, `requestBodies`, `headers` are not walked, and neither are Swagger 2.0
  `definitions` unless reached through `$ref`.
* The "required vs defined properties" check only fires for schemas that have `allOf`.
* Keywords next to a `$ref` (for example a sibling `default`) are invisible, because
  traversal replaces the reference with its target.

These are faithful to upstream. Adding them would change verdicts on real-world specs.

## Matching upstream's quirks

These behaviours look surprising but are reproduced on purpose:

| Behaviour | Example |
|---|---|
| JSON files are parsed as YAML 1.1 and round-tripped through `json.dumps` | `yes` → `true`, `017` → `15`, unquoted `200:` keys become `"200"` |
| JSON that libyaml cannot read is a read error, even if it is valid JSON | `"\ud83d\ude00"` (how Python's `json.dump` writes emoji), tabs outside brackets, keys over 1022 characters |
| Read errors name the stream `<urllib response>` (upstream reads files through `urlopen`) | `while parsing a block mapping\n  in "<urllib response>", line 1, column 1` |
| A non-string version is an error (exit 2), not a validation error | `swagger: 2.0` → `Error: expected string or bytes-like object, got 'float'` |
| An unknown version is an error with an empty message | `openapi: 4.0.0` → `Error: ` |
| `$ref` cycles, and cycles through `allOf`/`items` in `required` checks, hit Python's recursion limit | `Error: maximum recursion depth exceeded` |
| Path templates are parsed with Python's `str.format` grammar | `/a/{b}}` → `Error: Single '}' encountered in format string` |
| Schema `pattern`s must be valid **Python** regular expressions | `(?<name>x)` is rejected (Python needs `(?P<name>x)`) |
| `default: null` without `nullable: true` reports two errors | `None for not nullable`, `None is not of type 'string'` |
| `required` properties marked `readOnly`/`writeOnly` are not enforced in defaults | |
| A `default` can reach a schema through `$ref` before that schema's own meta-schema check; a malformed keyword there fails when it is applied, with Python's exception | a `default: {"x": 5}` whose property `x` refers to a later `{"type": "string", "maximum": "z"}` → with `--validation-errors all`, `Error: '>' not supported between instances of 'int' and 'str'` |
| Validation stops at the first error unless all errors are requested, so an exception further on is never raised | the same document without `--validation-errors all` → `5 is not of type 'string'` |
| `multipleOf` with a float divisor divides in floating point | `multipleOf: 0.1`, `default: 0.3` → `0.3 is not a multiple of 0.1` |
| In 3.1/3.2 defaults, a subschema's `$schema` switches to that draft's keywords, and `$id` moves the base URI of the `$ref`s below it (3.1 only: 3.2's validator ignores `$id`) | a Draft 4 subschema treats `exclusiveMaximum: true` as a flag |
| `$ref`s to `$id`s or `$anchor`s do not resolve: upstream's registry does not index them | `$ref: "#foo"` → `Error: 'foo' does not exist within {...}` |
| In OpenAPI 3.0 and Swagger 2.0, `1.0` is not an `integer` (Draft 4 semantics) | |
| Format checks depend on what a default install supports | `date-time`, `email`, `ipv4`, `uuid`, ... are checked; `uri`, `hostname` are not |

## Known differences

| Area | Difference | Impact |
|---|---|---|
| Remote references | `http(s)://` `$ref`s are not fetched; they are reported as unresolvable (exit 2). Upstream fetches them. | Specs with remote refs |
| `\N{NAME}` in patterns | No Unicode name table is bundled: impossible names are rejected exactly, plausible ones are accepted. Matching `default`s against such patterns, or against conditionals inside their own group, is not supported (reported as an error). | Rare |
| Draft 3 schemas in 3.1+ | A schema declaring `"$schema": "http://json-schema.org/draft-03/schema#"` is reported as an unknown dialect, and inside a `default`'s schema such a subschema keeps the surrounding dialect. Upstream validates it as Draft 3. | None seen in practice |
| Extremely deep nesting | Upstream hits Python's recursion limit (`Error: maximum recursion depth exceeded`, exit 2) on schemas nested 122 or more levels deep through `properties` or `allOf` (163 through `items`). This port validates them. None of the 4,138 APIs.guru specs comes close. | Pathological documents |
| Recursion limit elsewhere | `$ref` chains and recursion while validating a `default` hit Python's limit as upstream does. The thresholds are measured for references and `default`s at a component (a chain of 978 `$ref`s, a `default` 490 `$ref`s or 246 levels of a recursive schema deep); upstream's limit counts the frames above, so elsewhere in a document it can be a few levels off. | Pathological documents |
| NaN / Infinity, huge integers | YAML `.nan`/`.inf` and numbers too large for a float (`1e400`) load as `null`, and integers beyond 64 bits as floats. So a `default: .nan` fails with `None for not nullable` where upstream accepts it, and `format: int64` accepts `-9223372036854775809` (a float here) where upstream rejects it. An explicit `!!int` with non-ASCII digits (`!!int '١٢'`) is a read error here; upstream reads 12. | Rare |
| References into strings | `$ref: "#/info/title/0"` points into a string. Upstream resolves it to that one-character string; here it does not exist (`Error: PointerToNowhere: ...`, exit 2). | Pathological documents |
| Other files | Every file named by a `$ref` (or a discriminator mapping) anywhere in the document is read up front, even in examples and extensions; upstream reads only those it follows. Verdicts are the same: a file that cannot be read only matters where a reference to it is followed. | None on verdicts |
| Error output | Messages, "Failed validating ..." blocks, best-match hints and error order follow upstream; on the APIs.guru corpus the complete output is byte-identical for every rejected spec except in the two cases in this table. For errors at the top level of the document, upstream prints its meta-schema as `<Proxy at 0x... wrapping {...}>`, with a memory address, which cannot be reproduced. A 3.1/3.2 schema error reached through the dialect's `$dynamicRef` can name an equivalent branch of the meta-schema (`['$dynamicRef']['allOf'][1]` for upstream's `['$dynamicRef']['allOf'][0]['allOf'][0]`). Python's `FutureWarning: Possible nested set` for patterns like `[[:alpha:]]` is not printed. Upstream crashes while printing an error about a value nested about 9,000 levels deep; this port prints it. | Output text only |
| Set ordering | Where upstream iterates a Python set, its output changes from run to run with the hash seed: the list of undefined `required` properties, unexpected additional properties in OpenAPI 3.0 and Swagger 2.0 (3.1 sorts them), and which property is checked first under `additionalProperties`. This port uses document order. | Output text only |
| Upstream with `jsonschema-rs` installed | Upstream switches to its optional `jsonschema-rs` backend automatically when that package is installed. That backend also checks `format` across the whole document, with ECMAScript regex rules, so for example `pattern: "(?P<n>a)"` fails there but passes the default backend. This port follows the default backend. | Specs with `format` violations outside schemas |

## Python API

The `openapi_spec_validator_rs` package mirrors upstream's public Python API, and
upstream's own test suite runs against it unmodified through a shim package
(`python tests/run_upstream_suite.py`). The tests that cannot pass are listed with their
reasons in `tests/upstream_shim/upstream_suite.py`; they are the differences below.

| Upstream | This package |
|---|---|
| `validate(spec, base_uri="", cls=None)` | Same |
| `validate_url(spec_url, cls=None)` | Same: fetched with `urllib.request.urlopen` (`http`, `https`, `file`) and parsed exactly as upstream parses it. `cls` only selects the version. |
| | `validate_path(path, cls=None)`: reads a file like the command line, resolving relative `$ref`s against it |
| `validate_spec`, `validate_spec_url` (deprecated) | Same, with the same warnings (and, like upstream, `validate_spec` ignores `base_uri` unless `validator` is given) |
| `OpenAPIV2SpecValidator`, `OpenAPIV30SpecValidator`, `OpenAPIV31SpecValidator`, `OpenAPIV32SpecValidator`, `OpenAPIV3SpecValidator` | Same constructor `(schema, base_uri="")` and `validate()`, `is_valid()`, `iter_errors()`. A jsonschema-path `SchemaPath` is accepted too. |
| `openapi_v2_spec_validator`, `openapi_v30_spec_validator`, ... (deprecated) | Same |
| `get_validator_cls`, `versions.get_spec_version`, `SpecVersion`, `OPENAPIV2` ... `OPENAPIV32` | Same |
| `OpenAPISpecValidatorError`, `ValidatorDetectError`, `OpenAPIValidationError`, `ExtraParametersError`, `ParameterDuplicateError`, `UnresolvableParameterError`, `DuplicateOperationIDError` | Same names and hierarchy, in `openapi_spec_validator_rs.exceptions` and at the top level |

### Exceptions

`OpenAPIValidationError` derives from `openapi_spec_validator_rs.exceptions.ValidationError`
instead of `jsonschema.exceptions.ValidationError` (there is no jsonschema dependency). It
has jsonschema's attributes — `message`, `validator`, `validator_value`, `instance`,
`schema`, `path` / `relative_path`, `schema_path` / `relative_schema_path`,
`absolute_path`, `absolute_schema_path`, `json_path`, `context`, `cause`, `parent` — and the
same `str()`. `context` holds plain `ValidationError`s, as upstream's does. `cause` is an
`Exception` carrying the text of the exception the format check raised. `instance` and
`schema` are copies of the document's values, not the objects passed in.

Where upstream raises something other than a validation error, this package raises:

| Upstream raises | This package raises |
|---|---|
| `ValidatorDetectError` (no supported version) | `ValidatorDetectError` |
| `TypeError` from version detection (non-string version, document not a mapping) | `TypeError`, same message |
| Any other exception while validating: `referencing.exceptions.Unresolvable` (and its `PointerToNowhere`, `InvalidAnchor`, ...), `RecursionError`, `ValueError` for malformed path templates, `KeyError`, ... | `ValidationAbortedError` (an `OpenAPISpecValidatorError`), same message. This is what the CLI prints as `Error: ...` with exit status 2. |
| PyYAML's or `json`'s exceptions while reading (`validate_url`, `validate_path`) | `SpecReadError` (an `OpenAPISpecValidatorError`), same message |
| `OSError("No such file: ...")` from the CLI's reader | `validate_path`: `FileNotFoundError`, same message |

### Documents passed as Python objects

Documents are read the way `json.loads` output looks: `dict` with `str` keys, `list`,
`str`, `int`, `float`, `bool` and `None`, including subclasses (`OrderedDict`, `StrEnum`,
`IntEnum`, ...). Beyond that:

| Input | This package | Upstream |
|---|---|---|
| `tuple` | An array | Not an array to jsonschema, so a type error wherever an array is expected |
| NaN, ±infinity | `null`, like the file reader (see above) | Kept as floats |
| Integers beyond 64 bits | The nearest float, like the file reader | Kept as integers |
| Non-`str` keys (e.g. `200:` from `yaml.safe_load`) | `TypeError` naming the location | A `TypeError` from jsonschema in most positions, accepted where no schema applies |
| Other types (`set`, `bytes`, `datetime`, `Decimal`, custom objects) | `TypeError` naming the location | Accepted where no schema applies (e.g. in extensions), type errors elsewhere |
| Circular structures | `ValueError` | Accepted where no schema applies |
| `IntEnum` / `StrEnum` members | Shown in messages as their value (`1`) | Shown as the member (`<Num.ONE: 1>`) |
| More than 9,989 nested containers | `RecursionError` | Depends on where the nesting is |

### Other differences

* Upstream's settings (`OPENAPI_SPEC_VALIDATOR_RESOLVED_CACHE_MAXSIZE`,
  `OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND`) do not apply: validation always
  follows the default `jsonschema` backend.
* Custom formats registered with openapi-schema-validator's format checkers
  (`oas30_format_checker.checks(...)`) are not seen.
* `iter_errors()` validates when iteration starts. If validation aborts, the
  `ValidationAbortedError` is raised before any error is yielded; upstream first yields
  the errors it found before the failure.
* Results are not cached per validator: every call validates the document as it is at
  that moment (upstream caches `iter_errors()` per instance).
* `openapi_spec_validator_rs.__main__.main()` returns normally when every file is valid
  and raises `SystemExit` otherwise, like upstream's, and reads `-` from `sys.stdin`.
  Program name, version and argument error messages are this package's own (clap's
  instead of argparse's).
* The GIL is released while documents are loaded, validated and errors rendered.
