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
//! `$ref`s to other documents are followed like upstream's: relative references are
//! resolved against [`Options::base_uri`], `file://` documents are read from disk, and
//! `http://` and `https://` documents are downloaded when a reference to them is first
//! reached (see [`Retrieve`]; the built-in client needs the `http` feature, on by
//! default).
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
mod loader;
mod meta;
mod pyformat;
mod pyjsonschema;
mod pyobj;
mod pyregex;
/// Python `repr`/`pprint` emulation, shared with the Python bindings. Not a supported API.
#[doc(hidden)]
pub mod pyrepr;
mod retrieve;
mod schema_path;
mod values;
pub mod version;

#[cfg(test)]
mod pycompat_tests;
#[cfg(test)]
mod pyregex_tests;

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::document::Documents;
use crate::keywords::{Stop, Walker};

pub use crate::error::{ErrorKind, Outcome, SchemaErrorDetails, ValidationError, best_match};
pub use crate::loader::{LoadError, TextErrors, load, load_text};
pub use crate::pyrepr::PathItem;
#[cfg(feature = "http")]
pub use crate::retrieve::HttpRetriever;
pub use crate::retrieve::{Retrieve, RetrieveError};
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
#[derive(Clone)]
pub struct Options {
    /// Validate as this version instead of detecting it from the document.
    pub version: Option<SpecVersion>,
    /// Stop at the first error or collect all of them.
    pub errors: ErrorMode,
    /// URI of the document, used to resolve relative `$ref`s to other documents
    /// (e.g. `file:///path/to/openapi.yaml`). Empty means no base.
    pub base_uri: String,
    /// How documents that `http://` and `https://` `$ref`s point to are downloaded.
    /// `None` makes every such reference unresolvable. The default, with the `http`
    /// feature, is [`HttpRetriever`], which behaves like upstream's handler.
    pub retriever: Option<Arc<dyn Retrieve>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            version: None,
            errors: ErrorMode::default(),
            base_uri: String::new(),
            retriever: default_retriever(),
        }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("version", &self.version)
            .field("errors", &self.errors)
            .field("base_uri", &self.base_uri)
            .field(
                "retriever",
                &self.retriever.as_ref().map(|_| "dyn Retrieve"),
            )
            .finish()
    }
}

#[cfg(feature = "http")]
fn default_retriever() -> Option<Arc<dyn Retrieve>> {
    Some(Arc::new(HttpRetriever::new()))
}

#[cfg(not(feature = "http"))]
fn default_retriever() -> Option<Arc<dyn Retrieve>> {
    None
}

/// Validates an already-parsed spec document.
///
/// Validation recurses with the document's nesting and along `$ref` chains, so it runs
/// on threads of its own with a large stack: any document [`load`] accepts can be
/// validated from any thread.
pub fn validate(spec: &Value, options: &Options) -> Outcome {
    let version = match options.version {
        Some(version) => version,
        None => match detect_version(spec) {
            Ok(version) => version,
            Err(error) => return Outcome::Error(error.to_string()),
        },
    };
    let first_only = options.errors == ErrorMode::First;
    let base_uri = options.base_uri.as_str();
    let retriever = options.retriever.as_deref();

    // Upstream checks the whole document against the meta-schema first, then walks it.
    // The two are independent, so on large documents they run concurrently; the results
    // are combined in upstream's order.
    let (document_errors, walk) = std::thread::scope(|scope| {
        if spec_is_large(spec) {
            let document = on_large_stack(scope, || document_errors(spec, version));
            let walk = on_large_stack(scope, || {
                walk(spec, version, first_only, base_uri, retriever)
            });
            (document.join(), walk.join())
        } else {
            on_large_stack(scope, || {
                let errors = document_errors(spec, version);
                if first_only && !errors.is_empty() {
                    (errors, Ok(Vec::new()))
                } else {
                    (errors, walk(spec, version, first_only, base_uri, retriever))
                }
            })
            .join()
        }
    });

    let mut errors = document_errors;
    if first_only && !errors.is_empty() {
        errors.truncate(1);
        return Outcome::Invalid(errors);
    }
    match walk {
        Err(message) => Outcome::Error(message),
        Ok(mut walk_errors) => {
            errors.append(&mut walk_errors);
            if errors.is_empty() {
                Outcome::Valid
            } else {
                Outcome::Invalid(errors)
            }
        }
    }
}

/// Stack size for validation threads: enough for documents nested as deeply as the
/// loader allows (pages are only committed as they are used).
const STACK_SIZE: usize = 256 << 20;

/// Work running on a large-stack thread, or already done on the current thread when no
/// thread could be spawned.
enum Running<'scope, T> {
    Spawned(std::thread::ScopedJoinHandle<'scope, T>),
    Done(T),
}

impl<T> Running<'_, T> {
    fn join(self) -> T {
        match self {
            Self::Spawned(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Self::Done(value) => value,
        }
    }
}

fn on_large_stack<'scope, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    work: impl Fn() -> T + Send + Sync + 'scope,
) -> Running<'scope, T> {
    let work = std::sync::Arc::new(work);
    let spawned = {
        let work = std::sync::Arc::clone(&work);
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, move || work())
    };
    match spawned {
        Ok(handle) => Running::Spawned(handle),
        Err(_) => Running::Done(work()),
    }
}

/// Whether a document is big enough for running its two passes concurrently to pay off.
fn spec_is_large(spec: &Value) -> bool {
    const THRESHOLD: usize = 512;
    fn count(value: &Value, budget: &mut usize) {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        match value {
            Value::Object(map) => map.values().for_each(|child| count(child, budget)),
            Value::Array(items) => items.iter().for_each(|child| count(child, budget)),
            _ => {}
        }
    }
    let mut budget = THRESHOLD;
    count(spec, &mut budget);
    budget == 0
}

/// Errors from validating the whole document against the version's meta-schema.
fn document_errors(spec: &Value, version: SpecVersion) -> Vec<ValidationError> {
    let validator = meta::document_validator(version);
    if validator.is_valid(spec) {
        return Vec::new();
    }
    meta::convert_errors(
        validator.iter_errors(spec),
        &meta::document_schemas(version),
        spec,
    )
}

/// Upstream's keyword walk: its errors, or the message of the exception it would raise.
fn walk(
    spec: &Value,
    version: SpecVersion,
    first_only: bool,
    base_uri: &str,
    retriever: Option<&dyn Retrieve>,
) -> Result<Vec<ValidationError>, String> {
    let docs = Documents::new(spec, base_uri, retriever);
    let mut walker = Walker::new(&docs, version, first_only);
    match walker.root(docs.root()) {
        Err(Stop::Crash(message)) => Err(message),
        Ok(()) | Err(Stop::Done) => Ok(walker.errors),
    }
}

/// Reads, parses and validates a spec file, resolving relative `$ref`s against it.
///
/// Read and parse failures are returned as [`LoadError`], whose message matches what
/// upstream prints for them. Like [`validate`], this runs on a thread with a large stack.
pub fn validate_path(path: &Path, options: &Options) -> Result<Outcome, LoadError> {
    std::thread::scope(|scope| on_large_stack(scope, || read_and_validate(path, options)).join())
}

fn read_and_validate(path: &Path, options: &Options) -> Result<Outcome, LoadError> {
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
