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

mod combinators;
mod context;
mod document;
pub mod error;
mod formats;
mod frames;
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
mod pyurl;
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

pub use crate::context::{
    Cancelled, ContextHandle, DEFAULT_PYTHON_MINOR, PythonStack, check as check_cancelled,
    python_minor, run_cancellable, with_python_minor, with_python_stack,
};
use crate::context::{Context, python_stack};
use crate::document::Documents;
use crate::keywords::{Stop, Walker};

pub use crate::error::{ErrorKind, Outcome, SchemaErrorDetails, ValidationError, best_match};
pub use crate::loader::{
    LoadError, TextErrors, load, load_text, read_text_until_unreadable, read_until_unreadable,
};
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
    // Upstream's `validate()` follows a `$ref` at the root; its `iter_errors()`, which
    // collects every error (and which its command line uses for that), does not.
    validate_document(spec, options, options.errors == ErrorMode::First)
}

/// [`validate`], checking `spec` itself against the meta-schema even when its root is a
/// `$ref`, as upstream's validator classes do when given a plain mapping.
///
/// Not a supported API: used by the Python bindings.
#[doc(hidden)]
pub fn validate_as_given(spec: &Value, options: &Options) -> Outcome {
    validate_document(spec, options, false)
}

fn validate_document(spec: &Value, options: &Options, follow_root_ref: bool) -> Outcome {
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

    // Upstream's `validate()` reads the document through a jsonschema-path `SchemaPath`,
    // which follows a `$ref` at its root, ignoring the keys beside it: the meta-schema
    // check sees what the reference points to (the walk resolves it as it goes).
    let root_documents;
    let checked = if follow_root_ref && document::as_ref(spec).is_some() {
        root_documents = Documents::new(spec, base_uri, retriever);
        match root_documents.resolve(root_documents.root()) {
            Ok(node) => node.value,
            Err(crash) => return Outcome::Error(crash.0),
        }
    } else {
        spec
    };

    // Upstream's check against the meta-schema recurses as deep as the document nests,
    // and gives up past Python's recursion limit.
    let stack = python_stack()
        .unwrap_or_else(|| PythonStack::command_line(options.errors == ErrorMode::All));
    let budget = frames::Budget {
        root: stack.spec_depth + 1,
        limit: stack.limit,
    };
    let (recursion, nesting) = std::thread::scope(|scope| {
        on_large_stack(scope, || {
            let nesting = frames::nesting(spec);
            let recursion = if std::ptr::eq(checked, spec) {
                document_recursion(spec, version, first_only, budget, nesting)
            } else {
                document_recursion(
                    checked,
                    version,
                    first_only,
                    budget,
                    frames::nesting(checked),
                )
            };
            (recursion, nesting)
        })
        .join()
    });
    if let Some(outcome) = recursion {
        return outcome;
    }

    // Upstream checks the whole document against the meta-schema first, then walks it.
    // The two are independent, so on large documents they run concurrently; the results
    // are combined in upstream's order.
    let (document_errors, walk) = std::thread::scope(|scope| {
        if spec_is_large(spec) {
            // A thread of its own even on a large-stack thread: the point is concurrency.
            let document = spawn_large(scope, || document_errors(checked, version, first_only));
            let walk = on_large_stack(scope, || {
                walk(
                    spec, version, first_only, base_uri, retriever, stack, nesting,
                )
            });
            (document.join(), walk.join())
        } else {
            on_large_stack(scope, || {
                let errors = document_errors(checked, version, first_only);
                if first_only && !errors.is_empty() {
                    (errors, Ok(Vec::new()))
                } else {
                    (
                        errors,
                        walk(
                            spec, version, first_only, base_uri, retriever, stack, nesting,
                        ),
                    )
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

/// Reads a document file as PyYAML reads the stream `urlopen` gives upstream: only as far
/// as the parser could get (see [`read_until_unreadable`]).
pub(crate) fn read_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    if let Ok(metadata) = file.metadata() {
        // Only a hint: a sparse or special file may claim far more than it holds.
        let _ = bytes.try_reserve_exact(usize::try_from(metadata.len()).unwrap_or(0));
    }
    read_until_unreadable(&mut file, &mut bytes)?;
    Ok(bytes)
}

/// The deepest Python stack upstream's check of `spec` against a meta-schema reaches,
/// in frames above that check's `iter_errors`, as the frame model predicts it: the
/// document check, or with `at` (a JSON pointer) the dialect check of the schema there.
///
/// Not a supported API: used to fit the model to measurements of upstream.
#[doc(hidden)]
pub fn predicted_frames(spec: &Value, version: SpecVersion, at: Option<&str>) -> Option<usize> {
    let (schemas, instance) = match at {
        None => (meta::document_schemas(version), spec),
        Some(pointer) => {
            // The walk follows references on its way and checks the schema a reference
            // leads to.
            fn follow<'v>(spec: &'v Value, mut value: &'v Value) -> Option<&'v Value> {
                for _ in 0..64 {
                    match value.get("$ref").and_then(Value::as_str) {
                        Some(reference) if reference.starts_with('#') => {
                            let decoded = percent_encoding::percent_decode_str(&reference[1..])
                                .decode_utf8_lossy()
                                .into_owned();
                            value = spec.pointer(&decoded)?;
                        }
                        _ => return Some(value),
                    }
                }
                Some(value)
            }
            let mut target = spec;
            for segment in pointer.split('/').skip(1) {
                let segment = segment.replace("~1", "/").replace("~0", "~");
                target = follow(spec, target)?;
                target = match target {
                    Value::Object(map) => map.get(&segment)?,
                    Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
                    _ => return None,
                };
            }
            let target = follow(spec, target)?;
            let dialect = match version {
                SpecVersion::V2_0 | SpecVersion::V3_0 => meta::Dialect::Draft4,
                SpecVersion::V3_1 => meta::Dialect::Oas31,
                SpecVersion::V3_2 => meta::Dialect::Oas32,
            };
            (meta::meta_check_schemas(dialect), target)
        }
    };
    let budget = frames::Budget {
        root: 0,
        limit: usize::MAX,
    };
    let mut probe = frames::Probe::new(&schemas, budget, at.is_some());
    probe.run(instance);
    if std::env::var_os("FRAMES_DEBUG").is_some() {
        let (schema_path, instance_path) = &probe.max_at;
        let tail = &schema_path[schema_path.len().saturating_sub(12)..];
        eprintln!(
            "max {} at ...{:?} (instance depth {})",
            probe.max,
            tail,
            instance_path.len()
        );
    }
    Some(probe.max)
}

/// Stack size for validation threads: enough for documents nested as deeply as the
/// loader allows (pages are only committed as they are used). The deepest documents
/// tried need 8 MiB in release builds and 32 MiB in debug builds; a reservation much
/// larger than that only costs address space, which a `ulimit -v` limits.
const STACK_SIZE: usize = 64 << 20;

/// The stack size [`on_large_stack_thread`] expects of its caller's thread.
///
/// Not a supported API: used by the command line and the Python bindings.
#[doc(hidden)]
pub const LARGE_STACK_SIZE: usize = STACK_SIZE;

thread_local! {
    /// Whether the current thread's stack is at least [`STACK_SIZE`].
    static LARGE_STACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with the calling thread treated as one of the library's large-stack threads,
/// so that validation runs on it instead of starting another: the caller promises a stack
/// of at least [`LARGE_STACK_SIZE`].
///
/// Not a supported API: used by the command line and the Python bindings, whose worker
/// threads are started that large.
#[doc(hidden)]
pub fn on_large_stack_thread<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            LARGE_STACK.set(self.0);
        }
    }
    let _restore = Restore(LARGE_STACK.replace(true));
    f()
}

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

/// Runs `work` where its recursion fits: on the current thread if it is a large-stack
/// thread already, else on a new one.
fn on_large_stack<'scope, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    work: impl Fn() -> T + Send + Sync + 'scope,
) -> Running<'scope, T> {
    if LARGE_STACK.get() {
        return Running::Done(work());
    }
    spawn_large(scope, work)
}

/// Starts `work` on a new large-stack thread, or runs it on the current thread when no
/// thread can be started.
fn spawn_large<'scope, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    work: impl Fn() -> T + Send + Sync + 'scope,
) -> Running<'scope, T> {
    let work = std::sync::Arc::new(work);
    let spawned = {
        let work = std::sync::Arc::clone(&work);
        // The validation's settings (cancellation) follow it onto the new thread.
        let context = Context::current();
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, move || {
                context.install(|| on_large_stack_thread(|| work()))
            })
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

/// The outcome when upstream's check of `spec` against its meta-schema exceeds the
/// recursion limit: `RecursionError`, unless that check yields an error first and only
/// the first is wanted.
fn document_recursion(
    spec: &Value,
    version: SpecVersion,
    first_only: bool,
    budget: frames::Budget,
    nesting: usize,
) -> Option<Outcome> {
    let schemas = meta::document_schemas(version);
    let probe = frames::find(spec, &schemas, budget, false, Some(nesting))?;
    let crossing = probe.crossing.as_ref()?;
    let recursion = || Outcome::Error(document::Crash::recursion().0);
    if !first_only {
        return Some(recursion());
    }
    // The rest of the document, without what lies beyond the limit, may hold an error
    // that upstream reports before it gets that deep.
    let pruned = frames::pruned(spec, &probe.prune);
    let first = first_document_error(&pruned, version, |error| {
        error.schema_error.as_deref().is_some_and(|details| {
            probe.prune.iter().any(|location| {
                details.instance_path.len() > location.len()
                    && details.instance_path.starts_with(location)
            })
        })
    });
    Some(match first {
        Some(error) if frames::precedes(&schemas, spec, &error, crossing) => {
            Outcome::Invalid(vec![error])
        }
        _ => recursion(),
    })
}

/// Errors from validating the whole document against the version's meta-schema: all of
/// them, or only the first Python's iteration yields.
fn document_errors(spec: &Value, version: SpecVersion, first_only: bool) -> Vec<ValidationError> {
    if first_only {
        return first_document_error(spec, version, |_| false)
            .into_iter()
            .collect();
    }
    if meta::document_validator(version).is_valid(spec) {
        return Vec::new();
    }
    meta::convert_errors(
        errors_validator(version).iter_errors(spec),
        &meta::document_schemas(version),
        spec,
    )
}

/// The first error of the document check, among those `skip` does not reject.
fn first_document_error(
    spec: &Value,
    version: SpecVersion,
    skip: impl Fn(&ValidationError) -> bool,
) -> Option<ValidationError> {
    if meta::document_validator(version).is_valid(spec) {
        return None;
    }
    meta::first_error(
        errors_validator(version).iter_errors(spec),
        &meta::document_schemas(version),
        spec,
        skip,
    )
}

/// The validator that reports the document check's errors (validity is decided by the
/// faster [`meta::document_validator`]).
fn errors_validator(version: SpecVersion) -> &'static jsonschema::Validator {
    meta::lazy_document_validator(version).unwrap_or_else(|| meta::document_validator(version))
}

/// Upstream's keyword walk: its errors, or the message of the exception it would raise.
fn walk(
    spec: &Value,
    version: SpecVersion,
    first_only: bool,
    base_uri: &str,
    retriever: Option<&dyn Retrieve>,
    stack: PythonStack,
    nesting: usize,
) -> Result<Vec<ValidationError>, String> {
    let docs = Documents::new(spec, base_uri, retriever);
    let mut walker = Walker::new(&docs, version, first_only, stack, nesting);
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
    let bytes = read_file(&absolute).map_err(|error| LoadError {
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
