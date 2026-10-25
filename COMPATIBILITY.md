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
| Documents named by `$ref`s, local files and `http(s)://` URLs alike, are read when a reference to them is first followed, once per validation; a `$ref` in an example or an extension is never followed, and nothing is fetched after the error a first-error run stops at | A spec whose only remote `$ref` sits in `components` fetches nothing when an earlier `default` is invalid |
| Any failure to retrieve a document, whether a missing file, an HTTP error status, a network or TLS error, a 10 second timeout or an unparsable body, is reported as the reference being unresolvable | `Error: https://example.com/schemas/user.json` (exit 2) |
| The Python package fetches `http(s)://` documents with `requests` when that package is importable (as jsonschema-path does), decoding them as `response.text`, and with `urllib` otherwise | With `requests`, UTF-8 served as `text/plain` without a charset is read as ISO-8859-1: `'café' is not of type 'integer'` becomes `'cafÃ©' is not of type 'integer'` |

## Known differences

| Area | Difference | Impact |
|---|---|---|
| Remote references from the Rust command line and library | `http(s)://` `$ref` targets are downloaded by a built-in client that behaves like upstream's `urllib` handler, not by Python's network stack: the URL is joined with a port of `urljoin` and requested as written; what `urllib` refuses (spaces or control characters, non-ASCII, user information before the host) is refused without a request; plain `GET`, 10 second socket timeout, redirects followed with `urllib`'s rules (10 at most, the same URL 4 times at most, `http(s)` targets only), any status outside 200-299 a failure, a body cut short used as far as it arrived. Remaining differences: certificates come from the operating system's store (Python's default SSL context does the same on Linux and Windows; python.org builds on macOS use their own bundle); proxies come only from `http_proxy`, `https_proxy` and `no_proxy` (upper-case too), read as `urllib` reads them, not from macOS system settings or `.netrc`, and plain `http` targets are tunnelled through the proxy with `CONNECT` like `https` ones, where `urllib` sends them to the proxy as ordinary requests; non-ASCII host names are not IDNA-encoded; the `User-Agent` is `openapi-spec-validator-rs/<version>`; with `requests` installed upstream decodes bodies as text (see above), the Rust tools read the raw bytes like `urllib`. The `pip`-installed command and the Python API use Python's own `urllib` or `requests` and so behave exactly as upstream in the same interpreter. | Network environment only |
| `\N{NAME}` in patterns | No Unicode name table is bundled: impossible names are rejected exactly, plausible ones are accepted. Matching `default`s against such patterns, or against conditionals inside their own group, is not supported (reported as an error). | Rare |
| Draft 3 schemas in 3.1+ | A schema declaring `"$schema": "http://json-schema.org/draft-03/schema#"` is reported as an unknown dialect, and inside a `default`'s schema such a subschema keeps the surrounding dialect. Upstream validates it as Draft 3. | None seen in practice |
| Extremely deep nesting | Upstream hits Python's recursion limit (`Error: maximum recursion depth exceeded`, exit 2) on schemas nested 122 or more levels deep through `properties` or `allOf` (163 through `items`). This port validates them. None of the 4,138 APIs.guru specs comes close. | Pathological documents |
| Documents nested beyond the reading limit | Reading stops as soon as containers nest deeper than the 9,989 levels upstream's JSON round trip accepts (mappings flattened by merge keys not counted, as the round trip does not see them), with its `maximum recursion depth exceeded while encoding a JSON object` (exit 1), without scanning the rest of the file: libyaml's scanner slows quadratically with the nesting of mappings (100,000 levels took 25 s before; PyYAML scans at the same speed but crashes at about 50,000 levels, after about 5 s). Upstream reads the whole file first, so it would instead report what it meets elsewhere: a YAML syntax error or a second document further on, an unknown tag, an unhashable mapping key or an unserialisable value above the deep part, or nothing at all when the deep part sits under a duplicated key it discards. The limit itself mirrors `python -m openapi_spec_validator` on CPython 3.13 (9,989 for files, 9,992 for standard input); upstream's console script runs three frames shallower and accepts three levels more, and other CPython versions differ by a few levels. | Pathological documents |
| Exponential schemas | Some recursive schemas make jsonschema's algorithm revisit the same subschemas at every level, for example two `allOf` members that each refer back to the schema: validating a `default` nested 20 levels deep then takes a second here (over 20 s upstream), and each further level doubles it. This port runs the same algorithm, so both tools slow down on the same documents. | Pathological documents |
| Recursion limit elsewhere | `$ref` chains and recursion while validating a `default` hit Python's limit as upstream does. The thresholds are measured for references and `default`s at a component (a chain of 978 `$ref`s, a `default` 490 `$ref`s or 246 levels of a recursive schema deep); upstream's limit counts the frames above, so elsewhere in a document it can be a few levels off. | Pathological documents |
| NaN / Infinity, huge integers | YAML `.nan`/`.inf` and numbers too large for a float (`1e400`) load as `null`, and integers beyond 64 bits as floats. So a `default: .nan` fails with `None for not nullable` where upstream accepts it, and `format: int64` accepts `-9223372036854775809` (a float here) where upstream rejects it. | Rare |
| Error output | Messages, "Failed validating ..." blocks, best-match hints and error order follow upstream; on the APIs.guru corpus the complete output is byte-identical for every rejected spec except in the two cases in this table. For errors at the top level of the document, upstream prints its meta-schema as `<Proxy at 0x... wrapping {...}>`, with a memory address, which cannot be reproduced. Python's `FutureWarning: Possible nested set` for patterns like `[[:alpha:]]` is not printed. Upstream crashes while printing an error about a value nested about 9,000 levels deep; this port prints it. | Output text only |
| Set ordering | Where upstream iterates a Python set, its output changes from run to run with the hash seed: the list of undefined `required` properties, unexpected additional properties in OpenAPI 3.0 and Swagger 2.0 (3.1 sorts them), and which property is checked first under `additionalProperties`. This port uses document order. | Output text only |
| Windows | Upstream decodes standard input (`-`) with the console or ANSI code page unless Python's UTF-8 mode is on, and writes `\r\n` line endings; this port decodes standard input as UTF-8 and writes `\n`. Files are read the same way on every platform. Not tested on Windows yet. | Non-ASCII specs piped through `-`; line endings |
| Upstream with `jsonschema-rs` installed | Upstream switches to its optional `jsonschema-rs` backend automatically when that package is installed. That backend also checks `format` across the whole document, with ECMAScript regex rules, so for example `pattern: "(?P<n>a)"` fails there but passes the default backend. This port follows the default backend. | Specs with `format` violations outside schemas |

## Python API

The `openapi_spec_validator_rs` package mirrors upstream's public Python API, and
upstream's own test suite runs against it unmodified through a shim package
(`python tests/run_upstream_suite.py`). The tests that cannot pass are listed with their
reasons in `tests/upstream_shim/upstream_suite.py`; they are the differences below.

| Upstream | This package |
|---|---|
| `validate(spec, base_uri="", cls=None)` | Same |
| `validate_url(spec_url, cls=None)` | Same: fetched with `urllib.request.urlopen` (`http`, `https`, `file`) and parsed exactly as upstream parses it, reading only as far as PyYAML would (a stream libyaml cannot decode, `/dev/zero` say, is given up on at its first bad bytes rather than read to the end). `cls` only selects the version. |
| `$ref`s to other documents | Same: files are read from disk and `http(s)://` documents are fetched, lazily, through `openapi_spec_validator_rs.handlers`, which uses `requests` when it is importable and `urllib` otherwise, exactly as jsonschema-path's default handlers, with the URL joined as `urljoin` joins it. A `SchemaPath` passed in is read through its own base URI; custom handlers attached to it are not used. Validation runs on a thread of its own while the calling thread handles signals, so Ctrl-C (`KeyboardInterrupt`, or whatever a signal handler raises) takes effect at once, as it does upstream, and the validation under way is cancelled: it stops within milliseconds, except for a download in progress, which ends or times out (10 seconds) first. |
| | `validate_path(path, cls=None)`: reads a file like the command line, resolving relative `$ref`s against it |
| `validate_spec`, `validate_spec_url` (deprecated) | Same, with the same warnings (and, like upstream, `validate_spec` ignores `base_uri` unless `validator` is given) |
| `OpenAPIV2SpecValidator`, `OpenAPIV30SpecValidator`, `OpenAPIV31SpecValidator`, `OpenAPIV32SpecValidator`, `OpenAPIV3SpecValidator` | Same constructor `(schema, base_uri="")` and `validate()`, `is_valid()`, `iter_errors()`. A jsonschema-path `SchemaPath` is accepted too. |
| `openapi_v2_spec_validator`, `openapi_v30_spec_validator`, ... (deprecated) | Same |
| `get_validator_cls`, `versions.get_spec_version`, `SpecVersion`, `OPENAPIV2` ... `OPENAPIV32` | Same |
| `readers.read_from_filename`, `readers.read_from_stdin` | Same: the document (parsed like the command line's files) and its base URI |
| Module layout: `validation.validators`, `validation.exceptions`, `validation.proxies`, `validation.types`, `versions.consts`, `versions.datatypes`, `versions.exceptions`, `versions.shortcuts` | Same names, so an import path only changes its first segment. `validation.keywords` (the keyword validator classes), `validation.caches`, `validation.registries`, `versions.finders` and `settings` have no counterpart: the walk is implemented in Rust |
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
| `RecursionError` (`$ref` cycles, deep defaults) | `ValidationAbortedError("maximum recursion depth exceeded")`, worded as CPython 3.12 and later word it whichever interpreter runs this package (3.10 and 3.11 add `in comparison` or `while calling a Python object`). The `TypeError` from version detection is raised by the interpreter itself, so its wording is always the running interpreter's, as upstream's is. |
| `OSError("No such file: ...")` from the CLI's reader | `validate_path`: `FileNotFoundError`, same message |

### Documents passed as Python objects

Documents are read the way `json.loads` output looks: `dict` with `str` keys, `list`,
`str`, `int`, `float`, `bool` and `None`, including subclasses (`OrderedDict`, `StrEnum`,
`IntEnum`, ...). An object of any other type that is not a number, such as the
`datetime.date` that `yaml.safe_load` makes of an unquoted `2020-01-01`, `bytes` or a
`set`, is handled as jsonschema handles it: it is of no type, so it passes wherever no
type is required (`example`, `x-` extensions, an untyped `default`) and fails wherever one
is, with its `repr()` in the message (`datetime.date(2020, 1, 1) is not of type 'string'`);
equal objects are equal to `enum`; and the error's `instance` is the object itself. The
`repr()` and `str()` of such objects are taken once the whole document has been read; one
that raises fails validation (with its own exception) only where upstream would build a
message or rendering that includes it. The
`TypeError` upstream raises when such an object sits where its walk expects a mapping or
a string (`'datetime.date' object is not iterable`) is raised with the same message, as
`ValidationAbortedError` like every other failure. Beyond that:

| Input | This package | Upstream |
|---|---|---|
| `tuple` | An array | Not an array to jsonschema, so a type error wherever an array is expected |
| NaN, ±infinity | `null`, like the file reader (see above) | Kept as floats |
| Integers beyond 64 bits | The nearest float, like the file reader | Kept as integers |
| Non-`str` keys (e.g. `200:` from `yaml.safe_load`) | `TypeError` naming the location | A `TypeError` from jsonschema in most positions, accepted where no schema applies |
| Other numbers (`Decimal`, `Fraction`, `complex`, NumPy scalars) | `TypeError` naming the location | Numbers to jsonschema, compared and printed as such |
| An object of another type as a schema keyword's value (`required: <date>`, `type: <date>`) | The meta-schema error; where upstream's walk then fails on it, usually the same message, not always | Depends on the keyword |
| An empty `bytes` or `set` where upstream tests truthiness (`exclusiveMinimum: b''`), a `set` where it hashes (`enum: [{1}]`) | Treated as true and hashable | False, unhashable |
| Circular structures | `ValueError` | Accepted where no schema applies |
| A document changed while it is read (by a property or `__instancecheck__` it triggers) | `RuntimeError: dictionary changed size during iteration` | Depends on when |
| `IntEnum` / `StrEnum` members | Shown in messages as their value (`1`) | Shown as the member (`<Num.ONE: 1>`) |
| More than 9,989 nested containers | `RecursionError` | Depends on where the nesting is |

### Other differences

* The package can be used in one interpreter per process: the first to import it
  (the main interpreter or a subinterpreter, as under mod_wsgi). Another interpreter's
  import raises `ImportError`, because CPython would hand it the first interpreter's
  objects. Upstream, in pure Python, works in every subinterpreter.
* Each validation reserves address space for a large thread stack (64 MiB, plus 256 MiB
  per pass), and the allocator reserves about 1 GiB when the extension loads. Under a
  virtual memory limit (`ulimit -v`) much below 2 GiB, validation raises `RuntimeError:
  cannot start the validation thread`.
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
