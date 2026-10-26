//! Bundled meta-schemas, compiled once, and conversion of `jsonschema` errors.
//!
//! Two kinds of validators mirror upstream:
//! * whole-document validators (`validator_for(schema)(schema)`): no format checking;
//! * schema meta-checks (`OAS30Validator.check_schema`, and for 3.1+ the dialect's
//!   `check_schema`): format checking with openapi-schema-validator's format sets.
//!
//! Draft 4 based validators use Python's Draft 4 `type` semantics (`1.0` is not an
//! integer), and `format` always follows the Python checkers via [`formats`].

use std::sync::{LazyLock, OnceLock};

use jsonschema::error::{TypeKind, ValidationErrorKind};
use jsonschema::paths::{Location, LocationSegment};
use jsonschema::{Keyword, Registry, Validator};
use serde_json::{Map, Value};

use crate::combinators;
use crate::error::{SchemaErrorDetails, ValidationError};
use crate::formats::{self, FormatOutcome, FormatSet};
use crate::pyregex::{self, PyRegex};
use crate::pyrepr::{self, PathItem, repr, repr_str};
use crate::schema_path::{Resource, Search, python_order};
use crate::version::SpecVersion;

fn bundled(json: &str) -> Value {
    serde_json::from_str(json).expect("bundled meta-schemas are valid JSON")
}

// The Swagger 2.0 meta-schema reuses Draft 4 definitions; point them at Python's copy.
static SCHEMA_V20: LazyLock<Value> = LazyLock::new(|| {
    let mut schema = bundled(include_str!("../resources/schemas/v2.0/schema.json"));
    redirect_refs(
        &mut schema,
        "http://json-schema.org/draft-04/schema#",
        DRAFT4_URN,
    );
    schema
});
static SCHEMA_V30: LazyLock<Value> =
    LazyLock::new(|| bundled(include_str!("../resources/schemas/v3.0/schema.json")));

/// The meta-schema of `version` as upstream ships it (`resources/schemas/v*/schema.json`).
pub(crate) fn bundled_document_schema(version: SpecVersion) -> &'static str {
    match version {
        SpecVersion::V2_0 => include_str!("../resources/schemas/v2.0/schema.json"),
        SpecVersion::V3_0 => include_str!("../resources/schemas/v3.0/schema.json"),
        SpecVersion::V3_1 => include_str!("../resources/schemas/v3.1/schema.json"),
        SpecVersion::V3_2 => include_str!("../resources/schemas/v3.2/schema.json"),
    }
}
static SCHEMA_V31: LazyLock<Value> =
    LazyLock::new(|| bundled(include_str!("../resources/schemas/v3.1/schema.json")));
static SCHEMA_V32: LazyLock<Value> =
    LazyLock::new(|| bundled(include_str!("../resources/schemas/v3.2/schema.json")));

static OAS31_DIALECT: LazyLock<Value> =
    LazyLock::new(|| bundled(include_str!("../resources/dialects/oas3.1/metaschema.json")));
static OAS31_VOCAB: LazyLock<Value> = LazyLock::new(|| {
    bundled(include_str!(
        "../resources/dialects/oas3.1/vocabularies/base.json"
    ))
});
static OAS32_DIALECT: LazyLock<Value> = LazyLock::new(|| {
    bundled(include_str!(
        "../resources/dialects/oas3.2/dialect/2025-09-17.json"
    ))
});
static OAS32_VOCAB: LazyLock<Value> = LazyLock::new(|| {
    bundled(include_str!(
        "../resources/dialects/oas3.2/meta/2025-09-17.json"
    ))
});

/// Default `jsonSchemaDialect` of OpenAPI 3.1 documents.
pub(crate) const OAS31_DIALECT_ID: &str = "https://spec.openapis.org/oas/3.1/dialect/base";
/// Default `jsonSchemaDialect` of OpenAPI 3.2 documents.
pub(crate) const OAS32_DIALECT_ID: &str = "https://spec.openapis.org/oas/3.2/dialect/2025-09-17";
const OAS31_VOCAB_URI: &str = "https://spec.openapis.org/oas/3.1/vocab/base";
const OAS32_VOCAB_URI: &str = "https://spec.openapis.org/oas/3.2/vocab/base";

/// Private URIs for the Draft 4 and Draft 7 meta-schemas exactly as Python's
/// `jsonschema-specifications` ships them. The copies built into the `jsonschema` crate
/// differ (Draft 4's `enum` lacks `minItems`/`uniqueItems`, Draft 7 adds `writeOnly`) and
/// cannot be overridden under their official URIs.
const DRAFT4_URN: &str = "urn:openapi-spec-validator-rs:draft-04";
const DRAFT7_URN: &str = "urn:openapi-spec-validator-rs:draft-07";

static PY_DRAFT4: LazyLock<Value> = LazyLock::new(|| {
    let mut schema = bundled(include_str!("../resources/metaschemas/draft-04.json"));
    schema["id"] = Value::String(format!("{DRAFT4_URN}#"));
    schema
});
static PY_DRAFT7: LazyLock<Value> = LazyLock::new(|| {
    let mut schema = bundled(include_str!("../resources/metaschemas/draft-07.json"));
    schema["$id"] = Value::String(DRAFT7_URN.to_owned());
    schema
});

/// A copy of `schema` for display, with the private meta-schema URIs given back their
/// official values (the Swagger 2.0 meta-schema's references to Draft 4 included).
fn official_ids(schema: &Value) -> Value {
    let mut schema = schema.clone();
    redirect_refs(
        &mut schema,
        &format!("{DRAFT4_URN}#"),
        "http://json-schema.org/draft-04/schema",
    );
    if let Value::Object(map) = &mut schema {
        for key in ["id", "$id"] {
            if let Some(Value::String(id)) = map.get_mut(key) {
                if id.as_str() == format!("{DRAFT4_URN}#") {
                    *id = "http://json-schema.org/draft-04/schema#".to_owned();
                } else if id == DRAFT7_URN {
                    *id = "http://json-schema.org/draft-07/schema#".to_owned();
                }
            }
        }
    }
    schema
}

/// Rewrites `$ref`s that start with `from` to start with `to` instead.
fn redirect_refs(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get_mut("$ref") {
                if let Some(rest) = reference.strip_prefix(from) {
                    *reference = format!("{to}#{rest}");
                }
            }
            for child in map.values_mut() {
                redirect_refs(child, from, to);
            }
        }
        Value::Array(items) => {
            for child in items {
                redirect_refs(child, from, to);
            }
        }
        _ => {}
    }
}

static OAS_REGISTRY: LazyLock<Registry<'static>> = LazyLock::new(|| {
    Registry::new()
        .add(DRAFT4_URN, &*PY_DRAFT4)
        .and_then(|r| r.add(DRAFT7_URN, &*PY_DRAFT7))
        .and_then(|r| r.add(OAS31_DIALECT_ID, &*OAS31_DIALECT))
        .and_then(|r| r.add("https://spec.openapis.org/oas/3.1/meta/base", &*OAS31_VOCAB))
        .and_then(|r| r.add(OAS32_DIALECT_ID, &*OAS32_DIALECT))
        .and_then(|r| {
            r.add(
                "https://spec.openapis.org/oas/3.2/meta/2025-09-17",
                &*OAS32_VOCAB,
            )
        })
        .and_then(|r| r.prepare())
        .expect("bundled dialect meta-schemas form a valid registry")
});

/// The meta-schema a whole document is validated against.
pub(crate) fn document_schema(version: SpecVersion) -> &'static Value {
    match version {
        SpecVersion::V2_0 => &SCHEMA_V20,
        SpecVersion::V3_0 => &SCHEMA_V30,
        SpecVersion::V3_1 => &SCHEMA_V31,
        SpecVersion::V3_2 => &SCHEMA_V32,
    }
}

/// Validator for whole documents of `version` (formats are not checked upstream).
pub(crate) fn document_validator(version: SpecVersion) -> &'static Validator {
    static VALIDATORS: [OnceLock<Validator>; 4] = [const { OnceLock::new() }; 4];
    let index = SpecVersion::ALL
        .iter()
        .position(|v| *v == version)
        .unwrap_or_default();
    VALIDATORS[index].get_or_init(|| {
        let type_keyword = if matches!(version, SpecVersion::V2_0 | SpecVersion::V3_0) {
            py_type_draft4
        } else {
            py_type_modern
        };
        jsonschema::options()
            .with_registry(&OAS_REGISTRY)
            .should_validate_formats(false)
            .with_keyword("pattern", py_pattern)
            .with_keyword("type", type_keyword)
            .build(document_schema(version))
            .expect("bundled document meta-schemas compile")
    })
}

/// Private URI of the Swagger 2.0 meta-schema, which has no `id` of its own.
const SWAGGER20_URN: &str = "urn:openapi-spec-validator-rs:swagger-2.0";
const OAS30_SCHEMA_ID: &str = "https://spec.openapis.org/oas/3.0/schema/2021-09-28";

/// The bundled schemas holding the `oneOf` and `anyOf` of each version's document check.
static V20_RESOURCES: [(&str, &LazyLock<Value>); 2] =
    [(SWAGGER20_URN, &SCHEMA_V20), (DRAFT4_URN, &PY_DRAFT4)];
static V30_RESOURCES: [(&str, &LazyLock<Value>); 1] = [(OAS30_SCHEMA_ID, &SCHEMA_V30)];

/// The Draft 4 document meta-schemas, by URI, for the branch validators of
/// [`combinators`].
static DOCUMENT_REGISTRY: LazyLock<Registry<'static>> = LazyLock::new(|| {
    Registry::new()
        .add(DRAFT4_URN, &*PY_DRAFT4)
        .and_then(|r| r.add(SWAGGER20_URN, &*SCHEMA_V20))
        .and_then(|r| r.add(OAS30_SCHEMA_ID, &*SCHEMA_V30))
        .and_then(|r| r.prepare())
        .expect("bundled document meta-schemas form a valid registry")
});

fn lazy_options(
    resources: &'static [(&'static str, &'static LazyLock<Value>)],
    build: combinators::Build,
) -> jsonschema::ValidationOptions<'static> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft4)
        .with_registry(&DOCUMENT_REGISTRY)
        .should_validate_formats(false)
        .with_keyword("pattern", py_pattern)
        .with_keyword("type", py_type_draft4)
        .with_keyword("oneOf", combinators::factory(resources, true, build))
        .with_keyword("anyOf", combinators::factory(resources, false, build))
}

fn build_lazy_v20(schema: &Value) -> Validator {
    lazy_options(&V20_RESOURCES, build_lazy_v20)
        .build(schema)
        .expect("bundled document meta-schemas compile")
}

fn build_lazy_v30(schema: &Value) -> Validator {
    lazy_options(&V30_RESOURCES, build_lazy_v30)
        .build(schema)
        .expect("bundled document meta-schemas compile")
}

/// [`document_validator`] with `oneOf` and `anyOf` that leave the context of their errors
/// to be computed when the error is converted (see [`combinators`]), for the versions
/// whose meta-schemas allow it: Draft 4 has no keywords that depend on what those
/// branches evaluated.
pub(crate) fn lazy_document_validator(version: SpecVersion) -> Option<&'static Validator> {
    static V20: OnceLock<Validator> = OnceLock::new();
    static V30: OnceLock<Validator> = OnceLock::new();
    match version {
        SpecVersion::V2_0 => Some(V20.get_or_init(|| build_lazy_v20(document_schema(version)))),
        SpecVersion::V3_0 => Some(V30.get_or_init(|| build_lazy_v30(document_schema(version)))),
        _ => None,
    }
}

/// A JSON Schema dialect a schema can be meta-checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Draft4,
    Draft6,
    Draft7,
    Draft201909,
    Draft202012,
    Oas31,
    Oas32,
}

impl Dialect {
    const ALL: [Dialect; 7] = [
        Self::Draft4,
        Self::Draft6,
        Self::Draft7,
        Self::Draft201909,
        Self::Draft202012,
        Self::Oas31,
        Self::Oas32,
    ];

    /// `jsonschema.validators.validator_for({"$schema": uri}, default=None)` for the
    /// dialects this port supports. URIs are normalized like `URIDict`
    /// (case-insensitive scheme, empty fragment dropped).
    pub(crate) fn from_uri(uri: &str) -> Option<Self> {
        let uri = uri.strip_suffix('#').unwrap_or(uri);
        let (scheme, rest) = uri.split_once("://")?;
        let normalized = format!("{}://{rest}", scheme.to_ascii_lowercase());
        Some(match normalized.as_str() {
            "http://json-schema.org/draft-04/schema" => Self::Draft4,
            "http://json-schema.org/draft-06/schema" => Self::Draft6,
            "http://json-schema.org/draft-07/schema" => Self::Draft7,
            "https://json-schema.org/draft/2019-09/schema" => Self::Draft201909,
            "https://json-schema.org/draft/2020-12/schema" => Self::Draft202012,
            OAS31_DIALECT_ID => Self::Oas31,
            OAS32_DIALECT_ID => Self::Oas32,
            _ => return None,
        })
    }

    fn meta_schema_uri(self) -> &'static str {
        match self {
            Self::Draft4 => DRAFT4_URN,
            Self::Draft6 => "http://json-schema.org/draft-06/schema#",
            Self::Draft7 => DRAFT7_URN,
            Self::Draft201909 => "https://json-schema.org/draft/2019-09/schema",
            Self::Draft202012 => "https://json-schema.org/draft/2020-12/schema",
            Self::Oas31 => OAS31_DIALECT_ID,
            Self::Oas32 => OAS32_DIALECT_ID,
        }
    }
}

/// The validator implementing `check_schema` for `dialect` with `formats`.
///
/// Upstream validates each schema against its dialect's meta-schema with a format
/// checker: OpenAPI 3.0 (and Swagger 2.0) schemas use `OAS30Validator.check_schema`,
/// i.e. the Draft 4 meta-schema with the OAS 3.0 formats.
pub(crate) fn meta_check_validator(dialect: Dialect, formats: FormatSet) -> &'static Validator {
    static VALIDATORS: [[OnceLock<Validator>; 3]; 7] =
        [const { [const { OnceLock::new() }; 3] }; 7];
    let dialect_index = Dialect::ALL
        .iter()
        .position(|d| *d == dialect)
        .unwrap_or_default();
    let format_index = match formats {
        FormatSet::Oas30 => 0,
        FormatSet::Oas31 => 1,
        FormatSet::Oas32 => 2,
    };
    VALIDATORS[dialect_index][format_index].get_or_init(|| {
        let reference = serde_json::json!({ "$ref": dialect.meta_schema_uri() });
        let mut options = jsonschema::options()
            .with_registry(&OAS_REGISTRY)
            .with_vocabulary(OAS31_VOCAB_URI)
            .with_vocabulary(OAS32_VOCAB_URI)
            .should_validate_formats(true)
            .with_keyword("format", python_format(formats))
            .with_keyword("pattern", py_pattern);
        options = match dialect {
            Dialect::Draft4 => options
                .with_draft(jsonschema::Draft::Draft4)
                .with_keyword("type", py_type_draft4),
            Dialect::Draft6 => options.with_draft(jsonschema::Draft::Draft6),
            Dialect::Draft7 => options.with_draft(jsonschema::Draft::Draft7),
            Dialect::Draft201909 => options.with_draft(jsonschema::Draft::Draft201909),
            _ => options.with_draft(jsonschema::Draft::Draft202012),
        };
        if dialect != Dialect::Draft4 {
            options = options.with_keyword("type", py_type_modern);
        }
        options
            .build(&reference)
            .expect("bundled dialect meta-schemas compile")
    })
}

/// Python's `type` keyword: booleans are not numbers, a Python object of a type JSON has
/// no counterpart for (see [`pyrepr::foreign`]) is of no type, and under Draft 4 floats
/// are never integers.
struct PyType {
    /// The accepted types, as [`type_bits`] bits.
    accepted: u8,
    /// The rendered `type` value, as upstream prints it.
    reprs: String,
}

const NULL: u8 = 1;
const BOOLEAN: u8 = 1 << 1;
const INTEGER: u8 = 1 << 2;
const NUMBER: u8 = 1 << 3;
const STRING: u8 = 1 << 4;
const ARRAY: u8 = 1 << 5;
const OBJECT: u8 = 1 << 6;

fn type_bit(name: &str) -> Option<u8> {
    Some(match name {
        "array" => ARRAY,
        "boolean" => BOOLEAN,
        "integer" => INTEGER,
        "null" => NULL,
        "number" => NUMBER,
        "object" => OBJECT,
        "string" => STRING,
        _ => return None,
    })
}

/// The types `instance` is to Python's type checkers. Draft 6 and later count floats with
/// an integral value as integers.
fn type_bits(instance: &Value, draft4: bool) -> u8 {
    match instance {
        Value::Null => NULL,
        Value::Bool(_) => BOOLEAN,
        Value::Number(n) => {
            let integral = n.is_i64()
                || n.is_u64()
                || (!draft4 && n.as_f64().is_some_and(|f| f.fract() == 0.0));
            if integral { NUMBER | INTEGER } else { NUMBER }
        }
        Value::String(_) => STRING,
        Value::Array(_) => ARRAY,
        Value::Object(_) => {
            if pyrepr::foreign(instance).is_some() {
                0
            } else {
                OBJECT
            }
        }
    }
}

fn py_type<'a>(
    value: &'a Value,
    draft4: bool,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>> {
    let types: Vec<&str> = match value {
        Value::String(t) => vec![t.as_str()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let reprs = types
        .iter()
        .map(|t| repr_str(t))
        .collect::<Vec<_>>()
        .join(", ");
    let accepted = types
        .iter()
        .filter_map(|t| type_bit(t))
        .fold(0, |bits, bit| bits | bit);
    Ok(Box::new(PyType {
        accepted: if draft4 { accepted | DRAFT4 } else { accepted },
        reprs,
    }))
}

/// Marks a [`PyType`] built for Draft 4 (floats are never integers).
const DRAFT4: u8 = 1 << 7;

fn py_type_draft4<'a>(
    _: &'a Map<String, Value>,
    value: &'a Value,
    _: Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>> {
    py_type(value, true)
}

fn py_type_modern<'a>(
    _: &'a Map<String, Value>,
    value: &'a Value,
    _: Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>> {
    py_type(value, false)
}

/// `TypeChecker.is_type` for Python's Draft 4 type checker (and openapi-schema-validator's
/// OAS 3.0 checker, which is identical for JSON values).
pub(crate) fn is_draft4_type(instance: &Value, name: &str) -> Option<bool> {
    is_python_type(instance, name, true)
}

/// `TypeChecker.is_type` for Python's type checkers; `None` for a type they do not define.
pub(crate) fn is_python_type(instance: &Value, name: &str, draft4: bool) -> Option<bool> {
    Some(type_bits(instance, draft4) & type_bit(name)? != 0)
}

impl<'i> Keyword<'i> for PyType {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(format!(
                "{} is not of type {}",
                repr(instance),
                self.reprs
            )))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        // The crate's validation offers no other hook, and `type` is everywhere.
        crate::context::check();
        type_bits(instance, self.accepted & DRAFT4 != 0) & self.accepted != 0
    }
}

/// `pattern` with Python's `re.search` (the meta-schemas' `\\d` and `$` differ from the
/// Rust engine's). The bundled patterns all compile.
struct PyPattern {
    pattern: String,
    regex: Option<PyRegex>,
}

fn py_pattern<'a>(
    _: &'a Map<String, Value>,
    value: &'a Value,
    _: Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>> {
    let pattern = value.as_str().unwrap_or_default().to_owned();
    let regex = PyRegex::compile(&pattern).ok();
    Ok(Box::new(PyPattern { pattern, regex }))
}

impl<'i> Keyword<'i> for PyPattern {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(format!(
                "{} does not match {}",
                repr(instance),
                repr_str(&self.pattern)
            )))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        let Value::String(text) = instance else {
            return true;
        };
        self.regex
            .as_ref()
            .is_none_or(|regex| regex.search(text).unwrap_or(true))
    }
}

/// Factory for the `format` keyword checked with `set`.
#[allow(clippy::type_complexity)]
pub(crate) fn python_format(
    set: FormatSet,
) -> impl for<'a> Fn(
    &'a Map<String, Value>,
    &'a Value,
    Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>>
+ Send
+ Sync
+ 'static {
    move |_, value, _| {
        Ok(Box::new(PythonFormat {
            format: value.as_str().map(str::to_owned),
            set,
        }))
    }
}

/// Prefix marking a `jsonschema` custom-keyword error that stands for an exception
/// upstream would raise (custom keywords can only report validation errors).
const CRASH_MARKER: &str = "\u{0}openapi-spec-validator-rs crash\u{0}";

/// A validation error carrying an upstream exception message.
pub(crate) fn crash_error(message: &str) -> jsonschema::ValidationError<'static> {
    jsonschema::ValidationError::custom(format!("{CRASH_MARKER}{message}"))
}

/// The exception message, if `error` was produced by [`crash_error`].
pub(crate) fn crash_message(error: &ValidationError) -> Option<&str> {
    error.message.strip_prefix(CRASH_MARKER)
}

/// The `format` keyword with openapi-schema-validator's format checker.
struct PythonFormat {
    format: Option<String>,
    set: FormatSet,
}

impl PythonFormat {
    fn check(&self, instance: &Value) -> FormatOutcome {
        match &self.format {
            Some(format) => {
                formats::check_format_with_cause(
                    self.set,
                    format,
                    instance,
                    &pyregex::check_regex_format,
                )
                .0
            }
            None => FormatOutcome::Unknown,
        }
    }
}

impl<'i> Keyword<'i> for PythonFormat {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        match self.check(instance) {
            FormatOutcome::Unknown | FormatOutcome::Valid => Ok(()),
            FormatOutcome::Invalid(message) => Err(jsonschema::ValidationError::custom(message)),
            FormatOutcome::Crash(message) => Err(crash_error(&message)),
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        matches!(
            self.check(instance),
            FormatOutcome::Unknown | FormatOutcome::Valid
        )
    }
}

/// Converts a `jsonschema` error into upstream's representation, with Python-style
/// messages for the keywords that appear in the bundled meta-schemas.
///
/// `roots` are the schema resources the failing keyword may live in; the first one
/// containing the error's absolute keyword location provides the subschema shown in
/// the verbose "Failed validating ..." rendering.
///
/// `instance_root` is the validated value; it disambiguates path segments such as
/// `200`, which may be a mapping key or a list index.
pub(crate) fn convert_error(
    error: &jsonschema::ValidationError<'_>,
    schemas: &Schemas<'_>,
    instance_root: &Value,
) -> ValidationError {
    let base = Base {
        at: &[],
        instance: instance_root,
    };
    convert_relative(error, schemas, instance_root, base, &[], &[], Detail::Full)
}

/// The `python_order` key of a converted error with absolute paths.
fn order_key(schemas: &Schemas<'_>, error: &ValidationError, instance_root: &Value) -> Vec<usize> {
    match (&error.schema_error, schemas.resources.first()) {
        (Some(details), Some(root)) => python_order(
            &schemas.resources,
            root,
            &details.schema_path,
            instance_root,
            &details.instance_path,
        ),
        _ => Vec::new(),
    }
}

/// The error Python's lazy iteration would yield first, skipping those `skip` rejects
/// (given with their paths only), converted. Only that error is converted in full, with
/// its context: what Python never gets to is never built.
pub(crate) fn first_error<'e>(
    errors: impl Iterator<Item = jsonschema::ValidationError<'e>>,
    schemas: &Schemas<'_>,
    instance_root: &Value,
    skip: impl Fn(&ValidationError) -> bool,
) -> Option<ValidationError> {
    let base = Base {
        at: &[],
        instance: instance_root,
    };
    let mut first: Option<(Vec<usize>, jsonschema::ValidationError<'e>)> = None;
    for error in errors.filter(|error| !about_python_object(error, instance_root)) {
        let located = convert_relative(
            &error,
            schemas,
            instance_root,
            base,
            &[],
            &[],
            Detail::Location,
        );
        if skip(&located) {
            continue;
        }
        let key = order_key(schemas, &located, instance_root);
        // Ties keep the earlier error, as the stable sort of `convert_errors` does.
        if first.as_ref().is_none_or(|(first, _)| key < *first) {
            first = Some((key, error));
        }
    }
    first.map(|(_, error)| convert_error(&error, schemas, instance_root))
}

/// Converts errors and puts them in the order Python's jsonschema would yield them
/// (upstream reports the first one, or lists them all in that order).
pub(crate) fn convert_errors<'e>(
    errors: impl Iterator<Item = jsonschema::ValidationError<'e>>,
    schemas: &Schemas<'_>,
    instance_root: &Value,
) -> Vec<ValidationError> {
    let mut keyed: Vec<(Vec<usize>, ValidationError)> = errors
        .filter(|error| !about_python_object(error, instance_root))
        .map(|error| {
            let converted = convert_error(&error, schemas, instance_root);
            (order_key(schemas, &converted, instance_root), converted)
        })
        .collect();
    // Stable: errors Python would yield from the same keyword keep their order.
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    // jsonschema reports `items: false` once per extra item; Python once per array.
    keyed.dedup_by(|a, b| a.1 == b.1);
    keyed.into_iter().map(|(_, error)| error).collect()
}

/// Whether Python's jsonschema would never produce `error`, because it concerns a Python
/// object of a type JSON has no counterpart for (see [`pyrepr::foreign`]): the crate sees
/// the mapping standing in for it and applies the keywords that only concern mappings,
/// which jsonschema skips for anything that is not a `dict`; `type` (and `enum`, `const`)
/// are what it reports for such an object, and those pass through.
fn about_python_object(error: &jsonschema::ValidationError<'_>, instance_root: &Value) -> bool {
    const MAPPING_KEYWORDS: [&str; 11] = [
        "required",
        "properties",
        "patternProperties",
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "minProperties",
        "maxProperties",
        "dependencies",
        "dependentRequired",
        "dependentSchemas",
    ];
    let instance_path = instance_path_items(error.instance_path(), instance_root);
    // An error inside the stand-in (a subschema applied to its entry).
    if instance_path
        .iter()
        .any(|item| matches!(item, PathItem::Key(key) if key == pyrepr::FOREIGN_KEY))
    {
        return true;
    }
    if pyrepr::foreign(error.instance()).is_none() {
        return false;
    }
    let keyword = path_items(error.schema_path());
    matches!(keyword.last(), Some(PathItem::Key(keyword)) if MAPPING_KEYWORDS.contains(&keyword.as_str()))
}

/// The schema resources a validator was built from, for interpreting its errors.
pub(crate) struct Schemas<'a> {
    /// The validation root first, then every resource it references.
    pub resources: Vec<Resource<'a>>,
    /// Whether `$ref` hides its sibling keywords (Draft 4 to 7).
    pub ref_overrides: bool,
}

impl<'a> Schemas<'a> {
    fn new(roots: impl IntoIterator<Item = &'a Value>, ref_overrides: bool) -> Self {
        let resources = roots
            .into_iter()
            .map(|contents| Resource {
                uri: contents
                    .get("$id")
                    .or_else(|| contents.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                contents,
            })
            .collect();
        Self {
            resources,
            ref_overrides,
        }
    }
}

/// The instance a crate error's instance path starts from: the validation root, or for
/// the errors of a [`combinators`] branch, the instance of the `oneOf` / `anyOf` error.
#[derive(Clone, Copy)]
struct Base<'a> {
    /// Its absolute path.
    at: &'a [PathItem],
    instance: &'a Value,
}

/// How much of an error [`convert_relative`] builds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// Everything upstream's error holds.
    Full,
    /// Only the keyword and the paths, which give its place in Python's order.
    Location,
}

/// Converts `error` with paths relative to its parent error (an `anyOf`/`oneOf` failure),
/// as jsonschema's `relative_path` / `relative_schema_path` are for errors in `context`.
/// The parent paths are absolute (from the validation root).
fn convert_relative(
    error: &jsonschema::ValidationError<'_>,
    schemas: &Schemas<'_>,
    instance_root: &Value,
    base: Base<'_>,
    parent_instance_path: &[PathItem],
    parent_schema_path: &[PathItem],
    detail: Detail,
) -> ValidationError {
    // Converting a large error tree takes a while: a cancelled validation stops here.
    crate::context::check();
    // jsonschema validates each key against `propertyNames` as an instance of its own,
    // at the object's path; the crate wraps that error, whose paths are the ones to use.
    let paths = error;
    let error = match error.kind() {
        ValidationErrorKind::PropertyNames { error } => error.as_ref(),
        _ => error,
    };
    let mut instance = error.instance().as_ref();
    let mut location = path_items(paths.schema_path());
    let mut instance_path = [
        base.at,
        &instance_path_items(paths.instance_path(), base.instance),
    ]
    .concat();
    // jsonschema reports a `false` subschema at its own location; Python's `descend`
    // returns before recording that step, so the error belongs to the keyword that
    // descended, without the member it descended into.
    let false_schema = matches!(error.kind(), ValidationErrorKind::FalseSchema)
        .then(|| false_schema_step(&mut location, &mut instance_path))
        .flatten();
    let mut keyword = match location.last() {
        Some(PathItem::Key(keyword)) => keyword.clone(),
        _ => String::new(),
    };
    let parent = &location[..location.len().saturating_sub(1)];
    // Several meta-schemas can have the same keyword at the same pointer (every 2020-12
    // vocabulary has a root `type`): the error's absolute location names the right one.
    let resource_uri = paths.absolute_keyword_location().map(|uri| {
        uri.as_str()
            .split_once('#')
            .map_or(uri.as_str(), |(base, _)| base)
    });
    let candidates = || {
        schemas.resources.iter().filter_map(|resource| {
            subschema(resource.contents, parent)
                .filter(|schema| schema.get(&keyword).is_some())
                .map(|schema| (resource.uri, schema))
        })
    };
    let schema = candidates()
        .find(|(uri, _)| resource_uri.is_none_or(|wanted| uri.trim_end_matches('#') == wanted))
        .or_else(|| candidates().next())
        .map(|(_, schema)| schema);
    // Draft 4's boolean `exclusiveMinimum`/`exclusiveMaximum` only modify `minimum` /
    // `maximum`, which is the keyword Python reports.
    if let Some(bound) = keyword.strip_prefix("exclusive") {
        if schema.is_some_and(|s| s.get(&keyword).is_some_and(Value::is_boolean)) {
            keyword = bound.to_ascii_lowercase();
        }
    }
    // 2020-12 `items: false` fails once for the whole array, with its own message.
    let items_false = false_schema.is_some_and(|consumed| consumed)
        && keyword == "items"
        && schema.is_some_and(|s| s.get("items") == Some(&Value::Bool(false)));
    if items_false {
        instance = instance_at(instance_root, &instance_path).unwrap_or(instance);
    }
    let schema_path = schema
        .zip(schemas.resources.first())
        .and_then(|(target, root)| {
            Search::new(
                &schemas.resources,
                schemas.ref_overrides,
                &instance_path,
                target,
                &keyword,
            )
            .run(root.contents, root.uri)
        })
        .unwrap_or_else(|| {
            // Upstream's schema path skips `$ref` hops.
            path_items(paths.evaluation_path())
                .into_iter()
                .filter(|item| !matches!(item, PathItem::Key(key) if key == "$ref"))
                .collect()
        });
    if detail == Detail::Location {
        return ValidationError::from_schema(
            String::new(),
            SchemaErrorDetails {
                keyword,
                schema_path: strip_prefix(&schema_path, parent_schema_path).to_vec(),
                schema: Value::Null,
                instance_path: strip_prefix(&instance_path, parent_instance_path).to_vec(),
                instance: Value::Null,
                cause: None,
                context: Vec::new(),
            },
        );
    }
    // A `oneOf` / `anyOf` of `combinators`: its branches, and whether several are valid.
    let lazy = match error.kind() {
        ValidationErrorKind::Custom { message, .. }
            if message == combinators::NONE_VALID || message == combinators::SEVERAL_VALID =>
        {
            schema
                .and_then(|schema| schema.get(&keyword))
                .and_then(combinators::branches)
                .map(|branches| (branches, message == combinators::SEVERAL_VALID))
        }
        _ => None,
    };
    // Each subschema's errors, in the order Python's iteration yields them.
    let in_order = |branch: &mut dyn Iterator<Item = &jsonschema::ValidationError<'_>>,
                    base: Base<'_>| {
        let mut keyed: Vec<(Vec<usize>, ValidationError)> = branch
            .filter(|child| !about_python_object(child, base.instance))
            .map(|child| {
                let converted = convert_relative(
                    child,
                    schemas,
                    instance_root,
                    base,
                    &instance_path,
                    &schema_path,
                    Detail::Full,
                );
                let key = context_order(
                    schemas,
                    &converted,
                    instance_root,
                    &instance_path,
                    &schema_path,
                );
                (key, converted)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        keyed.into_iter().map(|(_, error)| error)
    };
    let context = match (error.kind(), &lazy) {
        (_, Some((branches, false))) => {
            // Computed now, from each branch's validator, as Python's `oneOf` / `anyOf`
            // does: the errors of every branch, borrowing the instance.
            let base = Base {
                at: &instance_path,
                instance,
            };
            (0..branches.len())
                .flat_map(|index| {
                    let errors: Vec<_> = branches.validator(index).iter_errors(instance).collect();
                    in_order(&mut errors.iter(), base).collect::<Vec<_>>()
                })
                .collect()
        }
        (
            ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context },
            _,
        ) => context
            .iter()
            .flat_map(|branch| in_order(&mut branch.iter(), base).collect::<Vec<_>>())
            .collect(),
        _ => Vec::new(),
    };
    let schema = schema.map_or(Value::Null, official_ids);
    let (message, keyword, schema) = if let Some((branches, several)) = &lazy {
        let message = if *several {
            several_valid_message(instance, &schema, &keyword, |index| {
                branches.validator(index).is_valid(instance)
            })
        } else {
            format!(
                "{} is not valid under any of the given schemas",
                repr(instance)
            )
        };
        (message, keyword, schema)
    } else if items_false {
        (items_false_message(&schema, instance), keyword, schema)
    } else if false_schema.is_some() {
        let message = format!("False schema does not allow {}", repr(instance));
        (message, String::new(), Value::Bool(false))
    } else {
        (
            python_message(error, &keyword, instance, &schema),
            keyword,
            schema,
        )
    };
    ValidationError::from_schema(
        message,
        SchemaErrorDetails {
            keyword,
            schema_path: strip_prefix(&schema_path, parent_schema_path).to_vec(),
            schema,
            instance_path: strip_prefix(&instance_path, parent_instance_path).to_vec(),
            instance: instance.clone(),
            cause: None,
            context,
        },
    )
}

/// The `python_order` key of a context error whose paths are relative to its parent's.
fn context_order(
    schemas: &Schemas<'_>,
    error: &ValidationError,
    instance_root: &Value,
    parent_instance_path: &[PathItem],
    parent_schema_path: &[PathItem],
) -> Vec<usize> {
    let (Some(details), Some(root)) = (&error.schema_error, schemas.resources.first()) else {
        return Vec::new();
    };
    let schema_path = [parent_schema_path, &details.schema_path].concat();
    let instance_path = [parent_instance_path, &details.instance_path].concat();
    python_order(
        &schemas.resources,
        root,
        &schema_path,
        instance_root,
        &instance_path,
    )
}

/// Rewrites the location of a `false` subschema to the keyword that descended into it,
/// dropping the member (property, index) that keyword descended into from
/// `instance_path`. Returns whether a member was dropped, or `None` if the location has
/// no recognisable parent keyword.
fn false_schema_step(
    location: &mut Vec<PathItem>,
    instance_path: &mut Vec<PathItem>,
) -> Option<bool> {
    // Keywords whose subschemas are named (by property, pattern or index).
    const NAMED: [&str; 8] = [
        "properties",
        "patternProperties",
        "prefixItems",
        "items",
        "allOf",
        "anyOf",
        "oneOf",
        "dependentSchemas",
    ];
    // Keywords that descend into a member of the instance.
    const CONSUMING: [&str; 8] = [
        "properties",
        "patternProperties",
        "prefixItems",
        "items",
        "additionalProperties",
        "additionalItems",
        "unevaluatedProperties",
        "unevaluatedItems",
    ];
    let n = location.len();
    let parent = n.checked_sub(2).and_then(|i| location.get(i));
    let keyword = match (parent, location.last()?) {
        (Some(PathItem::Key(parent)), _) if NAMED.contains(&parent.as_str()) => {
            let parent = parent.clone();
            location.pop();
            parent
        }
        (_, PathItem::Key(keyword)) => keyword.clone(),
        _ => return None,
    };
    let consumed = CONSUMING.contains(&keyword.as_str());
    if consumed {
        instance_path.pop();
    }
    Some(consumed)
}

/// jsonschema's 2020-12 `items` message when `items` is `false`.
fn items_false_message(schema: &Value, array: &Value) -> String {
    let prefix = schema
        .get("prefixItems")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let items = array.as_array().map(Vec::as_slice).unwrap_or_default();
    let extra = items.len().saturating_sub(prefix);
    let rest = if extra == 1 {
        repr(&items[prefix])
    } else {
        repr(&Value::Array(
            items.get(prefix..).unwrap_or_default().to_vec(),
        ))
    };
    let item = if prefix == 1 { "item" } else { "items" };
    format!("Expected at most {prefix} {item} but found {extra} extra: {rest}")
}

fn instance_at<'v>(root: &'v Value, path: &[PathItem]) -> Option<&'v Value> {
    path.iter()
        .try_fold(root, |value, item| match (value, item) {
            (Value::Object(map), PathItem::Key(key)) => map.get(key),
            (Value::Array(items), PathItem::Index(index)) => items.get(*index),
            _ => None,
        })
}

/// Instance path segments typed by the instance itself: `jsonschema` reports any
/// numeric segment as an index, but under a mapping it is a key.
fn instance_path_items(location: &Location, root: &Value) -> Vec<PathItem> {
    let mut current = Some(root);
    location
        .iter()
        .map(|segment| {
            let key = match &segment {
                LocationSegment::Property(key) => key.to_string(),
                LocationSegment::Index(index) => index.to_string(),
            };
            let item = match current {
                Some(Value::Array(_)) => match key.parse() {
                    Ok(index) => PathItem::Index(index),
                    Err(_) => PathItem::Key(key.clone()),
                },
                _ => PathItem::Key(key.clone()),
            };
            current = match (current, &item) {
                (Some(Value::Object(map)), PathItem::Key(key)) => map.get(key),
                (Some(Value::Array(items)), PathItem::Index(index)) => items.get(*index),
                _ => None,
            };
            item
        })
        .collect()
}

fn strip_prefix<'p>(path: &'p [PathItem], prefix: &[PathItem]) -> &'p [PathItem] {
    path.strip_prefix(prefix).unwrap_or(path)
}

/// The schemas whole-document validation of `version` uses.
pub(crate) fn document_schemas(version: SpecVersion) -> Schemas<'static> {
    let draft4 = matches!(version, SpecVersion::V2_0 | SpecVersion::V3_0);
    Schemas::new([document_schema(version), &*PY_DRAFT4], draft4)
}

/// The schemas `dialect`'s meta-check uses.
pub(crate) fn meta_check_schemas(dialect: Dialect) -> Schemas<'static> {
    let ref_overrides = matches!(dialect, Dialect::Draft4 | Dialect::Draft6 | Dialect::Draft7);
    Schemas::new(meta_check_roots(dialect), ref_overrides)
}

/// Schema resources an error from `dialect`'s meta-check can point into, root first.
fn meta_check_roots(dialect: Dialect) -> Vec<&'static Value> {
    use referencing::meta;
    let draft202012 = || {
        [
            &**meta::DRAFT202012,
            &**meta::DRAFT202012_CORE,
            &**meta::DRAFT202012_APPLICATOR,
            &**meta::DRAFT202012_UNEVALUATED,
            &**meta::DRAFT202012_VALIDATION,
            &**meta::DRAFT202012_META_DATA,
            &**meta::DRAFT202012_FORMAT_ANNOTATION,
            &**meta::DRAFT202012_CONTENT,
        ]
    };
    match dialect {
        Dialect::Draft4 => vec![&*PY_DRAFT4],
        Dialect::Draft6 => vec![&**meta::DRAFT6],
        Dialect::Draft7 => vec![&*PY_DRAFT7],
        Dialect::Draft201909 => vec![
            &**meta::DRAFT201909,
            &**meta::DRAFT201909_CORE,
            &**meta::DRAFT201909_APPLICATOR,
            &**meta::DRAFT201909_VALIDATION,
            &**meta::DRAFT201909_META_DATA,
            &**meta::DRAFT201909_FORMAT,
            &**meta::DRAFT201909_CONTENT,
        ],
        Dialect::Draft202012 => draft202012().to_vec(),
        Dialect::Oas31 => [&*OAS31_DIALECT, &*OAS31_VOCAB]
            .into_iter()
            .chain(draft202012())
            .collect(),
        Dialect::Oas32 => [&*OAS32_DIALECT, &*OAS32_VOCAB]
            .into_iter()
            .chain(draft202012())
            .collect(),
    }
}

fn path_items(location: &Location) -> Vec<PathItem> {
    location
        .iter()
        .map(|segment| match segment {
            LocationSegment::Property(key) => PathItem::Key(key.into_owned()),
            LocationSegment::Index(index) => PathItem::Index(index),
        })
        .collect()
}

/// Follows a schema path inside `root` (no `$ref` traversal).
fn subschema<'v>(root: &'v Value, path: &[PathItem]) -> Option<&'v Value> {
    path.iter()
        .try_fold(root, |value, item| match (item, value) {
            (PathItem::Key(key), Value::Object(map)) => map.get(key),
            (PathItem::Index(index), Value::Array(items)) => items.get(*index),
            _ => None,
        })
}

fn python_message(
    error: &jsonschema::ValidationError<'_>,
    keyword: &str,
    instance: &Value,
    schema: &Value,
) -> String {
    let inst = repr(instance);
    let value = |key: &str| schema.get(key).map(repr).unwrap_or_default();
    match error.kind() {
        ValidationErrorKind::Custom { message, .. } => message.clone(),
        // The crate checks an array-valued dependency with its `required` validator;
        // Python's `dependencies` / `dependentRequired` word the failure differently.
        ValidationErrorKind::Required { property }
            if matches!(keyword, "dependencies" | "dependentRequired") =>
        {
            let dependent = schema
                .get(keyword)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .find(|(name, needed)| {
                    instance.get(name).is_some()
                        && needed
                            .as_array()
                            .is_some_and(|items| items.contains(property))
                })
                .map(|(name, _)| name);
            match dependent {
                Some(name) => format!("{} is a dependency of {}", repr(property), repr_str(name)),
                None => format!("{} is a required property", repr(property)),
            }
        }
        ValidationErrorKind::Type { kind } => {
            let reprs = match (schema.get("type"), kind) {
                (Some(Value::Array(types)), _) => {
                    types.iter().map(repr).collect::<Vec<_>>().join(", ")
                }
                (Some(other), _) => repr(other),
                (None, TypeKind::Single(t)) => repr_str(&t.to_string()),
                (None, TypeKind::Multiple(types)) => types
                    .iter()
                    .map(|t| repr_str(&t.to_string()))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            format!("{inst} is not of type {reprs}")
        }
        ValidationErrorKind::Required { property } => {
            format!("{} is a required property", repr(property))
        }
        ValidationErrorKind::AdditionalProperties { unexpected } => {
            let mut extras = unexpected.clone();
            extras.sort();
            match schema.get("patternProperties").and_then(Value::as_object) {
                Some(patterns) => {
                    let verb = if extras.len() == 1 { "does" } else { "do" };
                    let mut patterns: Vec<&String> = patterns.keys().collect();
                    patterns.sort();
                    format!(
                        "{} {verb} not match any of the regexes: {}",
                        join_reprs(extras.iter().map(String::as_str)),
                        join_reprs(patterns.iter().map(|p| p.as_str()))
                    )
                }
                None => {
                    let verb = if extras.len() == 1 { "was" } else { "were" };
                    format!(
                        "Additional properties are not allowed ({} {verb} unexpected)",
                        join_reprs(extras.iter().map(String::as_str))
                    )
                }
            }
        }
        ValidationErrorKind::Enum { .. } => format!("{inst} is not one of {}", value("enum")),
        ValidationErrorKind::Constant { expected_value } => {
            format!("{} was expected", repr(expected_value))
        }
        ValidationErrorKind::Pattern { pattern } => {
            format!("{inst} does not match {}", repr_str(pattern))
        }
        ValidationErrorKind::Format { format } => {
            format!("{inst} is not a {}", repr_str(format))
        }
        ValidationErrorKind::MinLength { limit } | ValidationErrorKind::MinItems { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "is too short"
            };
            format!("{inst} {message}")
        }
        ValidationErrorKind::MaxLength { limit } | ValidationErrorKind::MaxItems { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "is too long"
            };
            format!("{inst} {message}")
        }
        ValidationErrorKind::MinProperties { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "does not have enough properties"
            };
            format!("{inst} {message}")
        }
        ValidationErrorKind::MaxProperties { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "has too many properties"
            };
            format!("{inst} {message}")
        }
        ValidationErrorKind::Minimum { limit } => {
            format!("{inst} is less than the minimum of {}", repr(limit))
        }
        ValidationErrorKind::Maximum { limit } => {
            format!("{inst} is greater than the maximum of {}", repr(limit))
        }
        ValidationErrorKind::ExclusiveMinimum { limit } => {
            format!(
                "{inst} is less than or equal to the minimum of {}",
                repr(limit)
            )
        }
        ValidationErrorKind::ExclusiveMaximum { limit } => {
            format!(
                "{inst} is greater than or equal to the maximum of {}",
                repr(limit)
            )
        }
        ValidationErrorKind::UniqueItems => format!("{inst} has non-unique elements"),
        ValidationErrorKind::AnyOf { .. } | ValidationErrorKind::OneOfNotValid { .. } => {
            format!("{inst} is not valid under any of the given schemas")
        }
        ValidationErrorKind::OneOfMultipleValid { context } => {
            // A subschema is valid when its branch of the context has no errors.
            several_valid_message(instance, schema, "oneOf", |index| {
                context.get(index).is_some_and(Vec::is_empty)
            })
        }
        ValidationErrorKind::Not { schema } => {
            format!("{inst} should not be valid under {}", repr(schema))
        }
        ValidationErrorKind::FalseSchema => format!("False schema does not allow {inst}"),
        ValidationErrorKind::UnevaluatedProperties { unexpected } => {
            let mut extras = unexpected.clone();
            extras.sort();
            format!(
                "Unevaluated properties are not allowed ({} {} unexpected)",
                join_reprs(extras.iter().map(String::as_str)),
                if extras.len() == 1 { "was" } else { "were" }
            )
        }
        _ => error.to_string(),
    }
}

/// `oneOf`'s message when several subschemas are valid: Python lists the later valid
/// ones, then the first.
fn several_valid_message(
    instance: &Value,
    schema: &Value,
    keyword: &str,
    is_valid: impl Fn(usize) -> bool,
) -> String {
    let subschemas = schema
        .get(keyword)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut valid = subschemas
        .iter()
        .enumerate()
        .filter(|(index, _)| is_valid(*index))
        .map(|(_, subschema)| repr(subschema));
    let first = valid.next();
    let reprs: Vec<String> = valid.chain(first).collect();
    format!(
        "{} is valid under each of {}",
        repr(instance),
        reprs.join(", ")
    )
}

fn join_reprs<'s>(items: impl Iterator<Item = &'s str>) -> String {
    items.map(pyrepr::repr_str).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use serde_json::json;

    use super::*;
    use crate::context::{Cancelled, run_cancellable};

    #[test]
    fn converting_errors_stops_once_cancelled() {
        let spec = json!({
            "openapi": "3.0.3",
            "info": {"title": "t", "version": "1"},
            "paths": {},
            "components": {"schemas": {"S": {"allOf": [{"type": 1}]}}},
        });
        let validator = document_validator(SpecVersion::V3_0);
        let errors: Vec<_> = validator.iter_errors(&spec).collect();
        assert!(!errors.is_empty());
        let cancelled = Arc::new(AtomicBool::new(true));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_cancellable(cancelled, || {
                convert_errors(
                    errors.into_iter(),
                    &document_schemas(SpecVersion::V3_0),
                    &spec,
                )
            })
        }));
        assert!(unwound.expect_err("cancelled").is::<Cancelled>());
    }
}
