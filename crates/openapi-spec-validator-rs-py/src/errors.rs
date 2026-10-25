//! Validation errors handed to Python.

use std::sync::Arc;

use openapi_spec_validator_rs::{SchemaErrorDetails, ValidationError};
use pyo3::prelude::*;
use pyo3::types::PyList;
use serde_json::Value;

use crate::convert::{PythonObjects, path_to_python, to_python};
use crate::stack;

/// One validation error, as produced by the validator.
///
/// The Python exception classes wrap this and read its fields on first use: a
/// top-level error's `schema` and `instance` can be the whole meta-schema and the
/// whole document, which are expensive to convert and rarely looked at.
#[pyclass(frozen, module = "openapi_spec_validator_rs._core")]
pub struct CoreError {
    root: Arc<ValidationError>,
    /// Indices into nested `context` lists, leading from `root` to this error.
    context_path: Vec<usize>,
    /// The Python objects the validated document stood in for, for `instance`.
    objects: Arc<PythonObjects>,
}

impl CoreError {
    pub fn new(error: ValidationError, objects: Arc<PythonObjects>) -> Self {
        Self {
            root: Arc::new(error),
            context_path: Vec::new(),
            objects,
        }
    }

    fn error(&self) -> &ValidationError {
        resolve(&self.root, &self.context_path)
    }

    /// Converts one of the error's values to Python (`None` for errors without details).
    fn convert<'py>(
        &self,
        py: Python<'py>,
        field: fn(&SchemaErrorDetails) -> &Value,
    ) -> PyResult<Bound<'py, PyAny>> {
        if self.error().schema_error.is_none() {
            return Ok(py.None().into_bound(py));
        }
        let root = Arc::clone(&self.root);
        let context_path = self.context_path.clone();
        let objects = Arc::clone(&self.objects);
        let object = stack::run(py, move || {
            let details = resolve(&root, &context_path)
                .schema_error
                .as_deref()
                .expect("checked above");
            Python::attach(|py| to_python(py, field(details), &objects).map(Bound::unbind))
        })?;
        Ok(object.into_bound(py))
    }
}

fn resolve<'a>(root: &'a ValidationError, context_path: &[usize]) -> &'a ValidationError {
    context_path.iter().fold(root, |error, &index| {
        &error
            .schema_error
            .as_ref()
            .expect("only schema errors have a context")
            .context[index]
    })
}

#[pymethods]
impl CoreError {
    /// The upstream exception class name, e.g. `OpenAPIValidationError`.
    #[getter]
    fn kind(&self) -> &'static str {
        self.error().kind.python_name()
    }

    #[getter]
    fn message(&self) -> &str {
        &self.error().message
    }

    /// Whether the error comes from JSON Schema validation and so carries
    /// `validator`, `schema`, `instance` and the paths.
    #[getter]
    fn has_details(&self) -> bool {
        self.error().schema_error.is_some()
    }

    /// The failing keyword; `None` for errors raised by a `false` schema or without details.
    #[getter]
    fn keyword(&self) -> Option<&str> {
        let details = self.error().schema_error.as_ref()?;
        (!details.keyword.is_empty()).then_some(details.keyword.as_str())
    }

    #[getter]
    fn schema_path<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let details = self.error().schema_error.as_deref();
        path_to_python(py, details.map_or(&[], |d| &d.schema_path))
    }

    #[getter]
    fn instance_path<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let details = self.error().schema_error.as_deref();
        path_to_python(py, details.map_or(&[], |d| &d.instance_path))
    }

    /// `str()` of the exception a format check raised, if any.
    #[getter]
    fn cause(&self) -> Option<&str> {
        self.error().schema_error.as_ref()?.cause.as_deref()
    }

    fn schema<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.convert(py, |details| &details.schema)
    }

    fn instance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.convert(py, |details| &details.instance)
    }

    /// Errors from the subschemas of `anyOf` / `oneOf`.
    fn context(&self) -> Vec<CoreError> {
        let count = self
            .error()
            .schema_error
            .as_ref()
            .map_or(0, |details| details.context.len());
        (0..count)
            .map(|index| {
                let mut context_path = self.context_path.clone();
                context_path.push(index);
                CoreError {
                    root: Arc::clone(&self.root),
                    context_path,
                    objects: Arc::clone(&self.objects),
                }
            })
            .collect()
    }

    /// `str(error)` as upstream renders it.
    fn render(&self, py: Python<'_>) -> PyResult<String> {
        let root = Arc::clone(&self.root);
        let context_path = self.context_path.clone();
        // Rendering pretty-prints the schema and instance, which can be large and deep.
        stack::run(py, move || Ok(resolve(&root, &context_path).to_string()))
    }

    fn __repr__(&self) -> String {
        format!("<CoreError {}: {:?}>", self.kind(), self.message())
    }
}
