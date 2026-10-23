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

use crate::error::{SchemaErrorDetails, ValidationError};
use crate::formats::{self, FormatOutcome, FormatSet};
use crate::pyregex;
use crate::pyrepr::{self, PathItem, repr, repr_str};
use crate::version::SpecVersion;

fn bundled(json: &str) -> Value {
    serde_json::from_str(json).expect("bundled meta-schemas are valid JSON")
}

static SCHEMA_V20: LazyLock<Value> =
    LazyLock::new(|| bundled(include_str!("../resources/schemas/v2.0/schema.json")));
static SCHEMA_V30: LazyLock<Value> =
    LazyLock::new(|| bundled(include_str!("../resources/schemas/v3.0/schema.json")));
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

static OAS_REGISTRY: LazyLock<Registry<'static>> = LazyLock::new(|| {
    Registry::new()
        .add(OAS31_DIALECT_ID, &*OAS31_DIALECT)
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
        let mut options = jsonschema::options().should_validate_formats(false);
        if matches!(version, SpecVersion::V2_0 | SpecVersion::V3_0) {
            options = options.with_keyword("type", draft4_type);
        }
        options
            .build(document_schema(version))
            .expect("bundled document meta-schemas compile")
    })
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
            Self::Draft4 => "http://json-schema.org/draft-04/schema#",
            Self::Draft6 => "http://json-schema.org/draft-06/schema#",
            Self::Draft7 => "http://json-schema.org/draft-07/schema#",
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
            .with_keyword("format", python_format(formats));
        options = match dialect {
            Dialect::Draft4 => options
                .with_draft(jsonschema::Draft::Draft4)
                .with_keyword("type", draft4_type),
            Dialect::Draft6 => options.with_draft(jsonschema::Draft::Draft6),
            Dialect::Draft7 => options.with_draft(jsonschema::Draft::Draft7),
            Dialect::Draft201909 => options.with_draft(jsonschema::Draft::Draft201909),
            _ => options.with_draft(jsonschema::Draft::Draft202012),
        };
        options
            .build(&reference)
            .expect("bundled dialect meta-schemas compile")
    })
}

/// Python Draft 4 `type`: booleans are not numbers, floats are never integers.
struct Draft4Type {
    types: Vec<String>,
    /// The rendered `type` value, as upstream prints it.
    reprs: String,
}

fn draft4_type<'a>(
    _: &'a Map<String, Value>,
    value: &'a Value,
    _: Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>> {
    let types: Vec<String> = match value {
        Value::String(t) => vec![t.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|t| t.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let reprs = types
        .iter()
        .map(|t| repr_str(t))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Box::new(Draft4Type { types, reprs }))
}

/// `TypeChecker.is_type` for Python's Draft 4 type checker (and openapi-schema-validator's
/// OAS 3.0 checker, which is identical for JSON values).
pub(crate) fn is_draft4_type(instance: &Value, name: &str) -> Option<bool> {
    Some(match name {
        "array" => instance.is_array(),
        "boolean" => instance.is_boolean(),
        "integer" => instance.is_i64() || instance.is_u64(),
        "null" => instance.is_null(),
        "number" => instance.is_number(),
        "object" => instance.is_object(),
        "string" => instance.is_string(),
        _ => return None,
    })
}

impl<'i> Keyword<'i> for Draft4Type {
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
        self.types
            .iter()
            .any(|t| is_draft4_type(instance, t).unwrap_or(false))
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

/// The `format` keyword with openapi-schema-validator's format checker.
struct PythonFormat {
    format: Option<String>,
    set: FormatSet,
}

impl PythonFormat {
    fn check(&self, instance: &Value) -> FormatOutcome {
        match &self.format {
            Some(format) => formats::check_format(self.set, format, instance, &pyregex::is_valid),
            None => FormatOutcome::Unknown,
        }
    }
}

impl<'i> Keyword<'i> for PythonFormat {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        match self.check(instance) {
            FormatOutcome::Unknown | FormatOutcome::Valid => Ok(()),
            FormatOutcome::Invalid(message) | FormatOutcome::Crash(message) => {
                Err(jsonschema::ValidationError::custom(message))
            }
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
pub(crate) fn convert_error(
    error: &jsonschema::ValidationError<'_>,
    roots: &[&Value],
) -> ValidationError {
    let instance = error.instance().as_ref();
    let location = path_items(error.schema_path());
    let keyword = match location.last() {
        Some(PathItem::Key(keyword)) => keyword.clone(),
        _ => String::new(),
    };
    let parent = &location[..location.len().saturating_sub(1)];
    let schema = roots
        .iter()
        .filter_map(|root| subschema(root, parent))
        .find(|schema| schema.get(&keyword).is_some())
        .cloned()
        .unwrap_or(Value::Null);
    // Upstream's relative schema path skips `$ref` hops.
    let schema_path = path_items(error.evaluation_path())
        .into_iter()
        .filter(|item| !matches!(item, PathItem::Key(key) if key == "$ref"))
        .collect();
    let message = python_message(error, instance, &schema);
    ValidationError::from_schema(
        message,
        SchemaErrorDetails {
            keyword,
            schema_path,
            schema,
            instance_path: path_items(error.instance_path()),
            instance: instance.clone(),
            context: Vec::new(),
        },
    )
}

/// Schema resources a meta-check error can point into.
pub(crate) fn meta_check_roots() -> [&'static Value; 13] {
    use referencing::meta;
    [
        &**meta::DRAFT4,
        &**meta::DRAFT6,
        &**meta::DRAFT7,
        &**meta::DRAFT202012,
        &**meta::DRAFT202012_CORE,
        &**meta::DRAFT202012_APPLICATOR,
        &**meta::DRAFT202012_UNEVALUATED,
        &**meta::DRAFT202012_VALIDATION,
        &**meta::DRAFT202012_META_DATA,
        &**meta::DRAFT202012_FORMAT_ANNOTATION,
        &**meta::DRAFT202012_CONTENT,
        &*OAS31_VOCAB,
        &*OAS32_VOCAB,
    ]
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
    instance: &Value,
    schema: &Value,
) -> String {
    let inst = repr(instance);
    let value = |key: &str| schema.get(key).map(repr).unwrap_or_default();
    match error.kind() {
        ValidationErrorKind::Custom { message, .. } => message.clone(),
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
        ValidationErrorKind::OneOfMultipleValid { .. } => {
            format!("{inst} is valid under each of {}", value("oneOf"))
        }
        ValidationErrorKind::Not { schema } => {
            format!("{inst} should not be valid under {}", repr(schema))
        }
        ValidationErrorKind::FalseSchema => format!("False schema does not allow {inst}"),
        ValidationErrorKind::UnevaluatedProperties { unexpected } => format!(
            "Unevaluated properties are not allowed ({} {} unexpected)",
            join_reprs(unexpected.iter().map(String::as_str)),
            if unexpected.len() == 1 { "was" } else { "were" }
        ),
        _ => error.to_string(),
    }
}

fn join_reprs<'s>(items: impl Iterator<Item = &'s str>) -> String {
    items.map(pyrepr::repr_str).collect::<Vec<_>>().join(", ")
}
