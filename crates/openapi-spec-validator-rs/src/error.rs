//! Validation results, mirroring upstream's exception classes.

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
            Some(details) => {
                let keyword_path = details
                    .schema_path
                    .split_last()
                    .map_or(&[][..], |(_, rest)| rest);
                f.write_str(&pyrepr::format_validation_error(
                    &self.message,
                    &details.keyword,
                    keyword_path,
                    &details.schema,
                    &details.instance_path,
                    &details.instance,
                ))
            }
        }
    }
}

impl std::error::Error for ValidationError {}

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
