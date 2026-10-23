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
| `ValueValidator` (`OpenAPIV30/31/32ValueValidator`) | `default` values valid against their schema | `values::DefaultValidator` | ported |
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
| JSON that libyaml cannot read is a read error, even if it is valid JSON | `"😀"` (how Python's `json.dump` writes emoji), tabs outside brackets, keys over 1022 characters |
| Read errors name the stream `<urllib response>` (upstream reads files through `urlopen`) | `while parsing a block mapping\n  in "<urllib response>", line 1, column 1` |
| A non-string version is an error (exit 2), not a validation error | `swagger: 2.0` → `Error: expected string or bytes-like object, got 'float'` |
| An unknown version is an error with an empty message | `openapi: 4.0.0` → `Error: ` |
| `$ref` cycles, and cycles through `allOf`/`items` in `required` checks, hit Python's recursion limit | `Error: maximum recursion depth exceeded` |
| Path templates are parsed with Python's `str.format` grammar | `/a/{b}}` → `Error: Single '}' encountered in format string` |
| Schema `pattern`s must be valid **Python** regular expressions | `(?<name>x)` is rejected (Python needs `(?P<name>x)`) |
| `default: null` without `nullable: true` reports two errors | `None for not nullable`, `None is not of type 'string'` |
| `required` properties marked `readOnly`/`writeOnly` are not enforced in defaults | |
| In OpenAPI 3.0 and Swagger 2.0, `1.0` is not an `integer` (Draft 4 semantics) | |
| Format checks depend on what a default install supports | `date-time`, `email`, `ipv4`, `uuid`, ... are checked; `uri`, `hostname` are not |

## Known differences

| Area | Difference | Impact |
|---|---|---|
| Remote references | `http(s)://` `$ref`s are not fetched; they are reported as unresolvable (exit 2). Upstream fetches them. | Specs with remote refs |
| `\N{NAME}` in patterns | No Unicode name table is bundled: impossible names are rejected exactly, plausible ones are accepted. Matching `default`s against such patterns, or against conditionals inside their own group, is not supported (reported as an error). | Rare |
| Draft 3 schemas in 3.1+ | A schema declaring `"$schema": "http://json-schema.org/draft-03/schema#"` is reported as an unknown dialect. | None seen in practice |
| `$id` / `$anchor` | References to `$id`-identified resources or anchors are not resolved by the walker. | Rare in OpenAPI documents |
| NaN / Infinity, huge integers | YAML `.nan`/`.inf` load as `null`; integers beyond 64 bits load as floats. | Rare |
| Error order | When the document, or a single schema, has several meta-schema violations, the one reported first may differ from upstream's, which follows Python's keyword iteration order. The verdict and error class are the same. On the APIs.guru corpus this affects the full `--validation-errors all` output of 3 of 4,138 specs and no first lines. | Output text only |
| Error messages | Messages follow upstream's wording; the "Failed validating ..." detail block and best-match subschema hints may differ in places. For errors at the top level of the document, upstream prints its meta-schema as `<Proxy at 0x... wrapping {...}>`, including a memory address, which cannot be reproduced. | Output text only |
| Set ordering | Upstream prints multiple undefined `required` properties, and unexpected additional properties, in Python set order (which varies between runs). This port uses document order. | Output text only |
| Upstream with `jsonschema-rs` installed | Upstream switches to its optional `jsonschema-rs` backend automatically when that package is installed. That backend also checks `format` across the whole document, with ECMAScript regex rules, so for example `pattern: "(?P<n>a)"` fails there but passes the default backend. This port follows the default backend. | Specs with `format` violations outside schemas |
