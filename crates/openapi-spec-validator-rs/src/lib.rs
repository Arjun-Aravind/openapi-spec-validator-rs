//! A fast, drop-in Rust companion to [openapi-spec-validator].
//!
//! Validates Swagger 2.0 and OpenAPI 3.0, 3.1 and 3.2 documents with the same
//! rules, and the same verdicts, as the Python implementation:
//!
//! 1. the whole document is validated against the version's official meta-schema;
//! 2. the spec is walked like upstream's `keywords.py`: every reachable schema is
//!    checked against its JSON Schema dialect, `required` lists are compared with
//!    defined properties, `default` values are validated, and operation IDs,
//!    parameters, path parameters and tags are checked.
//!
//! ```
//! use openapi_spec_validator_rs::{validate, Options, Outcome};
//!
//! let spec = serde_json::json!({
//!     "openapi": "3.0.3",
//!     "info": {"title": "Pets", "version": "1.0"},
//!     "paths": {},
//! });
//! assert_eq!(validate(&spec, &Options::default()), Outcome::Valid);
//! ```
//!
//! [openapi-spec-validator]: https://github.com/python-openapi/openapi-spec-validator

mod document;
pub mod error;
mod formats;
mod keywords;
pub mod loader;
mod meta;
mod pyformat;
mod pyobj;
mod pyregex;
pub mod pyrepr;
mod values;
pub mod version;

#[cfg(test)]
mod pycompat_tests;

use std::path::Path;

use serde_json::Value;

use crate::document::Documents;
use crate::keywords::{Stop, Walker};

pub use crate::error::{ErrorKind, Outcome, SchemaErrorDetails, ValidationError};
pub use crate::loader::{LoadError, TextErrors, load, load_text};
pub use crate::version::{DetectError, SpecVersion, detect_version};

/// How many errors to collect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ErrorMode {
    /// Stop at the first error (upstream's `validate()`).
    #[default]
    First,
    /// Collect every error (upstream's `iter_errors()`).
    All,
}

/// Validation settings.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Validate as this version instead of detecting it from the document.
    pub version: Option<SpecVersion>,
    /// Stop at the first error or collect all of them.
    pub errors: ErrorMode,
    /// URI of the document, used to resolve relative `$ref`s to other files
    /// (e.g. `file:///path/to/openapi.yaml`). Empty means no base.
    pub base_uri: String,
}

/// Validates an already-parsed spec document.
pub fn validate(spec: &Value, options: &Options) -> Outcome {
    let version = match options.version {
        Some(version) => version,
        None => match detect_version(spec) {
            Ok(version) => version,
            Err(error) => return Outcome::Error(error.to_string()),
        },
    };
    let first_only = options.errors == ErrorMode::First;

    let mut errors = Vec::new();
    let validator = meta::document_validator(version);
    if !validator.is_valid(spec) {
        let schema = meta::document_schema(version);
        for error in validator.iter_errors(spec) {
            errors.push(meta::convert_error(&error, &[schema]));
            if first_only {
                return Outcome::Invalid(errors);
            }
        }
    }

    let docs = Documents::new(spec, &options.base_uri);
    let mut walker = Walker::new(&docs, version, first_only);
    if let Err(Stop::Crash(message)) = walker.root(docs.root()) {
        return Outcome::Error(message);
    }
    errors.append(&mut walker.errors);
    if errors.is_empty() {
        Outcome::Valid
    } else {
        Outcome::Invalid(errors)
    }
}

/// Reads, parses and validates a spec file, resolving relative `$ref`s against it.
///
/// Read and parse failures are returned as [`LoadError`], whose message matches what
/// upstream prints for them.
pub fn validate_path(path: &Path, options: &Options) -> Result<Outcome, LoadError> {
    let not_found = || LoadError {
        message: format!("No such file: {}", path.display()),
    };
    if !path.is_file() {
        return Err(not_found());
    }
    let absolute = std::path::absolute(path).map_err(|_| not_found())?;
    let bytes = std::fs::read(&absolute).map_err(|error| LoadError {
        message: error.to_string(),
    })?;
    // Upstream reads files through `urlopen`, which is the stream name PyYAML reports.
    let spec = load(&bytes, "<urllib response>")?;
    let mut options = options.clone();
    if options.base_uri.is_empty() {
        options.base_uri = url::Url::from_file_path(&absolute)
            .map(String::from)
            .unwrap_or_default();
    }
    Ok(validate(&spec, &options))
}
