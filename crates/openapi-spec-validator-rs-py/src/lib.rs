//! Python bindings: `openapi_spec_validator_rs._core`.
//!
//! The public API is the pure-Python package around this module; here we only convert
//! documents, run the validator with the GIL released, and hand errors back.

mod convert;
mod errors;
mod retrieve;
mod stack;

use std::sync::Arc;

use clap::Parser;
use openapi_spec_validator_rs::{
    DetectError, ErrorMode, LoadError, Options, Outcome, Retrieve, SpecVersion, detect_version,
};
use openapi_spec_validator_rs_cli::Args;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyType;

use crate::convert::{to_python, to_value};
use crate::retrieve::PyRetriever;

// Converting and parsing large documents makes millions of small allocations.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use crate::errors::CoreError;

/// Where the Python exception classes live (they are plain Python classes so that they
/// can carry jsonschema-style attributes).
const EXCEPTIONS_MODULE: &str = "openapi_spec_validator_rs.exceptions";

fn exception<A>(py: Python<'_>, class: &str, args: A) -> PyErr
where
    A: for<'py> IntoPyObject<'py> + Send + Sync + 'static,
{
    let class = py
        .import(EXCEPTIONS_MODULE)
        .and_then(|module| module.getattr(class))
        .and_then(|class| class.cast_into::<PyType>().map_err(PyErr::from));
    match class {
        Ok(class) => PyErr::from_type(class, args),
        Err(err) => err,
    }
}

fn parse_version(version: &str) -> PyResult<SpecVersion> {
    SpecVersion::from_cli(version)
        .ok_or_else(|| PyValueError::new_err(format!("unknown spec version: {version:?}")))
}

fn options(
    version: SpecVersion,
    base_uri: String,
    all_errors: bool,
    retriever: Option<&Arc<PyRetriever>>,
) -> Options {
    Options {
        version: Some(version),
        errors: if all_errors {
            ErrorMode::All
        } else {
            ErrorMode::First
        },
        base_uri,
        retriever: retriever.map(|retriever| Arc::clone(retriever) as Arc<dyn Retrieve>),
    }
}

/// The retriever around `callable`, a Python function `(url: str) -> bytes` fetching
/// `http(s)://` `$ref` targets; `None` leaves such references unresolvable.
fn retriever(callable: Option<Py<PyAny>>) -> Option<Arc<PyRetriever>> {
    callable.map(|callable| Arc::new(PyRetriever::new(callable)))
}

/// Re-raises a `KeyboardInterrupt` or `SystemExit` the retriever met.
fn check_interrupt(retriever: Option<&Arc<PyRetriever>>) -> PyResult<()> {
    match retriever.and_then(|retriever| retriever.interrupt()) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Validation errors in upstream order; a non-validation failure (unresolvable `$ref`,
/// recursion, ...) raises `ValidationAbortedError`.
fn into_errors(py: Python<'_>, outcome: Outcome) -> PyResult<Vec<CoreError>> {
    match outcome {
        Outcome::Valid => Ok(Vec::new()),
        Outcome::Invalid(errors) => Ok(errors.into_iter().map(CoreError::new).collect()),
        Outcome::Error(message) => Err(exception(py, "ValidationAbortedError", (message,))),
    }
}

/// Validates a spec given as Python objects, as the given version (`"2.0"`, `"3.0"`,
/// `"3.1"` or `"3.2"`). `retriever` fetches `http(s)://` `$ref` targets.
#[pyfunction]
#[pyo3(signature = (spec, version, base_uri, all_errors, retriever=None))]
fn validate(
    py: Python<'_>,
    spec: &Bound<'_, PyAny>,
    version: &str,
    base_uri: String,
    all_errors: bool,
    retriever: Option<Py<PyAny>>,
) -> PyResult<Vec<CoreError>> {
    let retriever = self::retriever(retriever);
    let options = options(
        parse_version(version)?,
        base_uri,
        all_errors,
        retriever.as_ref(),
    );
    let spec = spec.as_unbound();
    let outcome = stack::run(py, || {
        let value = Python::attach(|py| to_value(spec.bind(py)))?;
        let outcome = openapi_spec_validator_rs::validate(&value, &options);
        // Freeing a large document takes a while too: do it before re-attaching.
        drop(value);
        Ok(outcome)
    })?;
    check_interrupt(retriever.as_ref())?;
    into_errors(py, outcome)
}

enum BytesFailure {
    Load(LoadError),
    Detect(DetectError),
}

/// Parses a document read from a binary stream named `stream_name` (as upstream's
/// reader does) and validates it, detecting the version unless one is given.
#[pyfunction]
#[pyo3(signature = (data, stream_name, base_uri, version, all_errors, retriever=None))]
fn validate_bytes(
    py: Python<'_>,
    data: PyBackedBytes,
    stream_name: String,
    base_uri: String,
    version: Option<&str>,
    all_errors: bool,
    retriever: Option<Py<PyAny>>,
) -> PyResult<Vec<CoreError>> {
    let version = version.map(parse_version).transpose()?;
    let retriever = self::retriever(retriever);
    let result = stack::run(py, || {
        Ok(load_and_validate(
            &data,
            &stream_name,
            version,
            base_uri,
            all_errors,
            retriever.as_ref(),
        ))
    })?;
    check_interrupt(retriever.as_ref())?;
    match result {
        Ok(outcome) => into_errors(py, outcome),
        Err(BytesFailure::Load(error)) => Err(exception(py, "SpecReadError", (error.message,))),
        // Upstream raises `ValidatorDetectError()` without a message.
        Err(BytesFailure::Detect(DetectError::NotFound)) => {
            Err(exception(py, "ValidatorDetectError", ()))
        }
        Err(BytesFailure::Detect(DetectError::TypeError(message))) => {
            Err(PyTypeError::new_err(message))
        }
    }
}

fn load_and_validate(
    data: &[u8],
    stream_name: &str,
    version: Option<SpecVersion>,
    base_uri: String,
    all_errors: bool,
    retriever: Option<&Arc<PyRetriever>>,
) -> Result<Outcome, BytesFailure> {
    let spec = openapi_spec_validator_rs::load(data, stream_name).map_err(BytesFailure::Load)?;
    let version = match version {
        Some(version) => version,
        None => detect_version(&spec).map_err(BytesFailure::Detect)?,
    };
    let options = options(version, base_uri, all_errors, retriever);
    Ok(openapi_spec_validator_rs::validate(&spec, &options))
}

/// Parses a document read from a binary stream, like upstream's reader.
#[pyfunction]
fn load<'py>(
    py: Python<'py>,
    data: PyBackedBytes,
    stream_name: String,
) -> PyResult<Bound<'py, PyAny>> {
    let loaded = stack::run(py, || {
        match openapi_spec_validator_rs::load(&data, &stream_name) {
            Ok(spec) => Python::attach(|py| Ok(Ok(to_python(py, &spec)?.unbind()))),
            Err(error) => Ok(Err(error)),
        }
    })?;
    match loaded {
        Ok(spec) => Ok(spec.into_bound(py)),
        Err(error) => Err(exception(py, "SpecReadError", (error.message,))),
    }
}

/// Runs the command-line interface and returns `(exit_code, stdout, stderr, ran)`.
///
/// Output is returned rather than written to the process streams so that Python
/// code (and pytest's `capsys`) sees it through `sys.stdout` / `sys.stderr`. `ran` is
/// false when argument parsing ended the run (usage errors, `--help`, `--version`):
/// upstream's argparse exits in those cases even with status 0, while a successful
/// validation run returns normally.
///
/// `stdin`, when given, is read for the `-` file instead of the process's standard input.
/// `retriever` fetches `http(s)://` `$ref` targets.
#[pyfunction]
#[pyo3(signature = (args, stdin=None, retriever=None))]
fn cli_main(
    py: Python<'_>,
    args: Vec<String>,
    stdin: Option<PyBackedBytes>,
    retriever: Option<Py<PyAny>>,
) -> PyResult<(i32, String, String, bool)> {
    let retriever = self::retriever(retriever);
    let result = py.detach(|| {
        let argv =
            std::iter::once("openapi-spec-validator-rs").chain(args.iter().map(String::as_str));
        let ran = Args::try_parse_from(argv).is_ok();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let fetch = retriever
            .as_ref()
            .map(|retriever| Arc::clone(retriever) as Arc<dyn Retrieve>);
        let code = match stdin {
            Some(data) => openapi_spec_validator_rs_cli::run_with_stdin(
                &args,
                &mut std::io::Cursor::new(&*data),
                fetch,
                &mut out,
                &mut err,
            ),
            None => openapi_spec_validator_rs_cli::run_with_stdin(
                &args,
                &mut std::io::stdin(),
                fetch,
                &mut out,
                &mut err,
            ),
        };
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
            ran,
        )
    });
    check_interrupt(retriever.as_ref())?;
    Ok(result)
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<CoreError>()?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add_function(wrap_pyfunction!(validate_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(load, m)?)?;
    m.add_function(wrap_pyfunction!(cli_main, m)?)?;
    Ok(())
}
