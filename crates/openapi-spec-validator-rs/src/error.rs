//! Validation results, mirroring upstream's exception classes.

use std::cmp::Ordering;
use std::fmt;

use serde_json::Value;

use crate::pyrepr::{self, PathItem};

/// The upstream exception class an error corresponds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `OpenAPIValidationError`: meta-schema violations, invalid schemas,
    /// invalid `default` values, duplicate tags.
    OpenApiValidation,
    /// `ExtraParametersError`: `required` lists properties that are not defined.
    ExtraParameters,
    /// `ParameterDuplicateError`: two parameters share `name` and `in`.
    ParameterDuplicate,
    /// `UnresolvableParameterError`: path template and path parameters disagree.
    UnresolvableParameter,
    /// `DuplicateOperationIDError`: an `operationId` is used twice.
    DuplicateOperationId,
}

impl ErrorKind {
    /// The upstream Python class name.
    pub fn python_name(self) -> &'static str {
        match self {
            Self::OpenApiValidation => "OpenAPIValidationError",
            Self::ExtraParameters => "ExtraParametersError",
            Self::ParameterDuplicate => "ParameterDuplicateError",
            Self::UnresolvableParameter => "UnresolvableParameterError",
            Self::DuplicateOperationId => "DuplicateOperationIDError",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.python_name())
    }
}

/// One validation error.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationError {
    /// Which upstream exception class this corresponds to.
    pub kind: ErrorKind,
    /// The error message (`error.message` upstream).
    pub message: String,
    /// Present when the error comes from JSON Schema validation.
    pub schema_error: Option<Box<SchemaErrorDetails>>,
}

/// The JSON Schema context of an error, used for upstream's verbose rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaErrorDetails {
    /// The failing keyword (`error.validator`).
    pub keyword: String,
    /// Path from the validated schema root to the failing keyword (`relative_schema_path`).
    pub schema_path: Vec<PathItem>,
    /// The (sub)schema containing the failing keyword (`error.schema`).
    pub schema: Value,
    /// Path from the validated instance root to the failing value (`relative_path`).
    pub instance_path: Vec<PathItem>,
    /// The failing value (`error.instance`).
    pub instance: Value,
    /// `str(error.cause)`: the exception a format check raised, if any.
    pub cause: Option<String>,
    /// Errors from subschemas of `anyOf` / `oneOf` (`error.context`).
    pub context: Vec<ValidationError>,
}

impl ValidationError {
    /// An error carrying only a message (upstream's semantic checks).
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            schema_error: None,
        }
    }

    /// An `OpenAPIValidationError` produced by JSON Schema validation.
    pub fn from_schema(message: impl Into<String>, details: SchemaErrorDetails) -> Self {
        Self {
            kind: ErrorKind::OpenApiValidation,
            message: message.into(),
            schema_error: Some(Box::new(details)),
        }
    }
}

impl fmt::Display for ValidationError {
    /// Renders like `str(error)` upstream: the message, plus the
    /// "Failed validating ..." block for JSON Schema errors.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.schema_error {
            None => f.write_str(&self.message),
            Some(details) => f.write_str(&pyrepr::format_jsonschema_error(
                pyrepr::ErrorKind::Validation,
                &self.message,
                // Errors raised by a `false` schema have no keyword (`validator=None`).
                (!details.keyword.is_empty()).then_some(details.keyword.as_str()),
                &details.schema_path,
                &details.schema,
                &details.instance_path,
                &details.instance,
            )),
        }
    }
}

impl std::error::Error for ValidationError {}

/// jsonschema's `best_match`: the error most likely to explain a failure, descending
/// into `anyOf`/`oneOf` contexts. Upstream's CLI prints it as the
/// "Probably due to this subschema error" hint.
pub fn best_match(errors: &[ValidationError]) -> Option<&ValidationError> {
    // `max` keeps the first of equally relevant errors.
    let mut best = errors.iter().reduce(|best, error| {
        if relevance_cmp(error, best) == Ordering::Greater {
            error
        } else {
            best
        }
    })?;
    loop {
        let context = best
            .schema_error
            .as_ref()
            .map(|details| details.context.as_slice())
            .unwrap_or_default();
        if context.is_empty() {
            return Some(best);
        }
        // `heapq.nsmallest(2, ...)` is stable: the earliest of equal errors comes first.
        let mut smallest: Vec<&ValidationError> = Vec::with_capacity(2);
        for error in context {
            let position = smallest
                .iter()
                .position(|s| relevance_cmp(error, s) == Ordering::Less)
                .unwrap_or(smallest.len());
            if position < 2 {
                smallest.insert(position, error);
                smallest.truncate(2);
            }
        }
        if smallest.len() == 2 && relevance_cmp(smallest[0], smallest[1]) == Ordering::Equal {
            return Some(best);
        }
        best = smallest[0];
    }
}

/// Compares jsonschema's `relevance` keys:
/// `(-len(path), path, validator not in weak, validator in strong, not matches_type)`.
fn relevance_cmp(a: &ValidationError, b: &ValidationError) -> Ordering {
    fn key(error: &ValidationError) -> (usize, &[PathItem], bool, bool) {
        match &error.schema_error {
            Some(details) => (
                details.instance_path.len(),
                &details.instance_path,
                !matches!(details.keyword.as_str(), "anyOf" | "oneOf"),
                !matches_type(details),
            ),
            None => (0, &[], true, true),
        }
    }
    let (a_len, a_path, a_strong, a_mismatch) = key(a);
    let (b_len, b_path, b_strong, b_mismatch) = key(b);
    b_len
        .cmp(&a_len)
        .then_with(|| compare_paths(a_path, b_path))
        .then_with(|| a_strong.cmp(&b_strong))
        .then_with(|| a_mismatch.cmp(&b_mismatch))
}

/// Python's comparison of two error path deques (keys and indices).
fn compare_paths(a: &[PathItem], b: &[PathItem]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let ordering = match (x, y) {
            (PathItem::Key(x), PathItem::Key(y)) => x.cmp(y),
            (PathItem::Index(x), PathItem::Index(y)) => x.cmp(y),
            // Python cannot order a str against an int; such siblings never coexist in
            // practice, so any consistent order will do.
            (PathItem::Key(_), PathItem::Index(_)) => Ordering::Greater,
            (PathItem::Index(_), PathItem::Key(_)) => Ordering::Less,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a.len().cmp(&b.len())
}

/// `ValidationError._matches_type`: the instance has the type the failing schema expects.
fn matches_type(details: &SchemaErrorDetails) -> bool {
    let is_type =
        |name: &str| crate::meta::is_draft4_type(&details.instance, name).unwrap_or(false);
    match details.schema.get("type") {
        Some(Value::String(name)) => is_type(name),
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).any(is_type),
        _ => false,
    }
}

/// The result of validating one document.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The document is valid.
    Valid,
    /// Validation errors, in upstream order. In "first error" mode there is exactly one.
    Invalid(Vec<ValidationError>),
    /// Upstream raises a non-validation exception (the CLI prints `Error: ...` and
    /// exits with status 2). Carries the exception message.
    Error(String),
}

impl Outcome {
    /// Whether the document is valid.
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }
}
