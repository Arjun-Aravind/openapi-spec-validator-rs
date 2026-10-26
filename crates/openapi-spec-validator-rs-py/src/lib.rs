//! Python bindings: `openapi_spec_validator_rs._core`.
//!
//! The public API is the pure-Python package around this module; here we only convert
//! documents, run the validator with the GIL released, and hand errors back.

mod convert;
mod errors;
mod retrieve;
mod stack;

use std::sync::Arc;

use openapi_spec_validator_rs::{
    DetectError, ErrorMode, LoadError, Options, Outcome, PythonStack, Retrieve, STACK_TOO_SMALL,
    SpecVersion, ValidationError, detect_version, with_python_stack,
};
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyType;

use crate::convert::{PythonObjects, to_python, to_value};
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

/// Where the caller's validation sits on the Python stack (see `_stack.py`); upstream's
/// command line's position when not given.
fn python_stack(depth: Option<usize>, limit: Option<usize>, all_errors: bool) -> PythonStack {
    let command_line = PythonStack::command_line(all_errors);
    PythonStack {
        spec_depth: depth.unwrap_or(command_line.spec_depth),
        limit: limit.unwrap_or(command_line.limit),
    }
}

/// Validation errors in upstream order; a non-validation failure (unresolvable `$ref`,
/// recursion, ...) raises `ValidationAbortedError`.
fn into_errors(
    py: Python<'_>,
    outcome: Outcome,
    objects: PythonObjects,
) -> PyResult<Vec<CoreError>> {
    match outcome {
        Outcome::Valid => Ok(Vec::new()),
        Outcome::Invalid(errors) => {
            // Upstream builds each message as it finds the error: an object whose
            // `repr()` raises fails validation there.
            for error in &errors {
                if let Some(raised) = raised_in(py, error, &objects) {
                    return Err(raised);
                }
            }
            let objects = Arc::new(objects);
            Ok(errors
                .into_iter()
                .map(|error| CoreError::new(error, Arc::clone(&objects)))
                .collect())
        }
        // No thread could be started and the document is too deep for this one.
        Outcome::Error(message) if message == STACK_TOO_SMALL => {
            Err(PyRuntimeError::new_err(message))
        }
        Outcome::Error(message) => Err(objects
            .raised_for(py, &message)
            .unwrap_or_else(|| exception(py, "ValidationAbortedError", (message,)))),
    }
}

/// The exception a description raised that `error`'s messages (its own and those of
/// the subschema errors it carries, which Python builds first) include.
fn raised_in(py: Python<'_>, error: &ValidationError, objects: &PythonObjects) -> Option<PyErr> {
    error
        .schema_error
        .iter()
        .flat_map(|details| details.context.iter())
        .find_map(|child| raised_in(py, child, objects))
        .or_else(|| objects.raised_for(py, &error.message))
}

/// Validates a spec given as Python objects, as the given version (`"2.0"`, `"3.0"`,
/// `"3.1"` or `"3.2"`). `retriever` fetches `http(s)://` `$ref` targets.
#[pyfunction]
#[pyo3(signature = (spec, version, base_uri, all_errors, retriever=None, python_depth=None, recursion_limit=None, follow_root_ref=true))]
#[allow(clippy::too_many_arguments)]
fn validate(
    py: Python<'_>,
    spec: &Bound<'_, PyAny>,
    version: &str,
    base_uri: String,
    all_errors: bool,
    retriever: Option<Py<PyAny>>,
    python_depth: Option<usize>,
    recursion_limit: Option<usize>,
    follow_root_ref: bool,
) -> PyResult<Vec<CoreError>> {
    let stack = python_stack(python_depth, recursion_limit, all_errors);
    let retriever = self::retriever(retriever);
    let options = options(
        parse_version(version)?,
        base_uri,
        all_errors,
        retriever.as_ref(),
    );
    let spec = spec.clone().unbind();
    let (outcome, objects) = stack::run(py, retriever.as_deref(), move || {
        let (value, objects) = stack::attach(|py| to_value(spec.bind(py)))?;
        let outcome = with_python_stack(stack, || {
            openapi_spec_validator_rs::validate_root_ref(&value, &options, follow_root_ref)
        });
        // Freeing a large document takes a while too: do it before re-attaching.
        drop(value);
        Ok((outcome, objects))
    })?;
    check_interrupt(retriever.as_ref())?;
    into_errors(py, outcome, objects)
}

enum BytesFailure {
    Load(LoadError),
    Detect(DetectError),
}

/// Parses a document read from a binary stream named `stream_name` (as upstream's
/// reader does) and validates it, detecting the version unless one is given.
#[pyfunction]
#[pyo3(signature = (data, stream_name, base_uri, version, all_errors, retriever=None, python_depth=None, recursion_limit=None))]
#[allow(clippy::too_many_arguments)]
fn validate_bytes(
    py: Python<'_>,
    data: PyBackedBytes,
    stream_name: String,
    base_uri: String,
    version: Option<&str>,
    all_errors: bool,
    retriever: Option<Py<PyAny>>,
    python_depth: Option<usize>,
    recursion_limit: Option<usize>,
) -> PyResult<Vec<CoreError>> {
    let stack = python_stack(python_depth, recursion_limit, all_errors);
    let version = version.map(parse_version).transpose()?;
    let retriever = self::retriever(retriever);
    let worker_retriever = retriever.clone();
    let result = stack::run(py, retriever.as_deref(), move || {
        Ok(with_python_stack(stack, || {
            load_and_validate(
                &data,
                &stream_name,
                version,
                base_uri,
                all_errors,
                worker_retriever.as_ref(),
            )
        }))
    })?;
    check_interrupt(retriever.as_ref())?;
    match result {
        Ok(outcome) => into_errors(py, outcome, PythonObjects::default()),
        Err(BytesFailure::Load(error)) if error.message == STACK_TOO_SMALL => {
            Err(PyRuntimeError::new_err(error.message))
        }
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
    let loaded = stack::run(py, None, move || {
        match openapi_spec_validator_rs::load(&data, &stream_name) {
            Ok(spec) => stack::attach(|py| {
                Ok(Ok(to_python(py, &spec, &PythonObjects::default())?.unbind()))
            }),
            Err(error) => Ok(Err(error)),
        }
    })?;
    match loaded {
        Ok(spec) => Ok(spec.into_bound(py)),
        Err(error) if error.message == STACK_TOO_SMALL => {
            Err(PyRuntimeError::new_err(error.message))
        }
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
    let worker_retriever = retriever.clone();
    let result = stack::run(py, retriever.as_deref(), move || {
        let retriever = worker_retriever;
        let argv =
            std::iter::once("openapi-spec-validator-rs").chain(args.iter().map(String::as_str));
        let ran = openapi_spec_validator_rs_cli::python_command_parses(argv);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let fetch = retriever
            .as_ref()
            .map(|retriever| Arc::clone(retriever) as Arc<dyn Retrieve>);
        let code = match stdin {
            Some(data) => openapi_spec_validator_rs_cli::run_python_command(
                &args,
                &mut std::io::Cursor::new(&*data),
                fetch,
                &mut out,
                &mut err,
            ),
            None => openapi_spec_validator_rs_cli::run_python_command(
                &args,
                &mut std::io::stdin(),
                fetch,
                &mut out,
                &mut err,
            ),
        };
        Ok((
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
            ran,
        ))
    })?;
    check_interrupt(retriever.as_ref())?;
    Ok(result)
}

/// The JSON text of the meta-schema of `version` (`"2.0"` to `"3.2"`), as upstream ships it.
#[pyfunction]
fn _meta_schema(version: &str) -> PyResult<&'static str> {
    Ok(openapi_spec_validator_rs::meta_schema_text(parse_version(
        version,
    )?))
}

/// Raises upstream's `ImportError` once the interpreter is shutting down, for Python code
/// that would otherwise fail differently before reaching the extension.
#[pyfunction]
fn _check_finalizing() -> PyResult<()> {
    stack::refuse_while_finalizing()
}

/// The interpreter the extension was initialised in.
static HOME_INTERPRETER: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

fn current_interpreter() -> i64 {
    // SAFETY: called with an attached thread state, so there is a current interpreter.
    unsafe { pyo3::ffi::PyInterpreterState_GetID(pyo3::ffi::PyInterpreterState_Get()) }
}

/// Refuses to run in an interpreter other than the one that loaded the extension.
///
/// CPython initialises a single-phase extension once per process and hands later
/// (sub)interpreters a copy of its module's namespace, so those would use classes and
/// objects that belong to the first interpreter, which breaks in confusing ways and can
/// crash. The package calls this first thing, so that such an import fails cleanly.
#[pyfunction]
fn _check_interpreter() -> PyResult<()> {
    let home = *HOME_INTERPRETER.get_or_init(current_interpreter);
    if current_interpreter() == home {
        Ok(())
    } else {
        Err(pyo3::exceptions::PyImportError::new_err(
            "openapi_spec_validator_rs can only be used in one interpreter per process: it \
             was first imported in another (sub)interpreter",
        ))
    }
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let _ = HOME_INTERPRETER.get_or_init(current_interpreter);
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(_check_interpreter, m)?)?;
    m.add_function(wrap_pyfunction!(_check_finalizing, m)?)?;
    m.add_function(wrap_pyfunction!(_meta_schema, m)?)?;
    m.add_class::<CoreError>()?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add_function(wrap_pyfunction!(validate_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(load, m)?)?;
    m.add_function(wrap_pyfunction!(cli_main, m)?)?;
    Ok(())
}
