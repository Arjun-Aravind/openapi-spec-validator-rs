//! Fetching `http(s)://` `$ref` targets through a Python callable, so that the Python
//! package downloads them exactly as upstream's handlers do in the same interpreter.

use std::sync::{Mutex, PoisonError};

use openapi_spec_validator_rs::{Retrieve, RetrieveError};
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;

/// A [`Retrieve`] implementation calling a Python function `(url: str) -> bytes`.
///
/// An `Exception` raised by the function makes the reference unresolvable, as upstream's
/// `referencing` catches `Exception` around its retrievers. Anything else
/// (`KeyboardInterrupt`, `SystemExit`) would propagate upstream, so it is kept and
/// re-raised once the validation returns (see [`PyRetriever::interrupt`]).
pub struct PyRetriever {
    callable: Py<PyAny>,
    interrupt: Mutex<Option<PyErr>>,
}

impl PyRetriever {
    pub fn new(callable: Py<PyAny>) -> Self {
        Self {
            callable,
            interrupt: Mutex::new(None),
        }
    }

    /// The non-`Exception` error the function raised, if any, to re-raise.
    pub fn interrupt(&self) -> Option<PyErr> {
        self.interrupt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Retrieve for PyRetriever {
    fn retrieve(&self, url: &str) -> Result<Vec<u8>, RetrieveError> {
        Python::attach(|py| {
            let result = self
                .callable
                .call1(py, (url,))
                .and_then(|body| body.extract::<PyBackedBytes>(py).map_err(PyErr::from));
            match result {
                Ok(bytes) => Ok(bytes.to_vec()),
                Err(error) => {
                    let message = error.to_string();
                    if !error.is_instance_of::<PyException>(py) {
                        *self
                            .interrupt
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = Some(error);
                    }
                    Err(RetrieveError::new(message))
                }
            }
        })
    }
}
