//! Fetching `http(s)://` `$ref` targets through a Python callable, so that the Python
//! package downloads them exactly as upstream's handlers do in the same interpreter.

use std::sync::{Mutex, PoisonError, mpsc};

use openapi_spec_validator_rs::{Cancelled, Retrieve, RetrieveError, check_cancelled};
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;

use crate::stack::{Event, Fetch};

/// A [`Retrieve`] implementation calling a Python function `(url: str) -> bytes`.
///
/// An `Exception` raised by the function makes the reference unresolvable, as upstream's
/// `referencing` catches `Exception` around its retrievers. Anything else
/// (`KeyboardInterrupt`, `SystemExit`) would propagate upstream, so it is kept and
/// re-raised once the validation returns (see [`PyRetriever::interrupt`]).
///
/// While a [`crate::stack::run`] serves it, the function is called on that run's calling
/// thread, never on the library's threads (see there why).
pub struct PyRetriever {
    callable: Py<PyAny>,
    interrupt: Mutex<Option<PyErr>>,
    /// Where downloads go while a run serves them.
    caller: Mutex<Option<mpsc::Sender<Event>>>,
}

/// Ends the serving of a [`PyRetriever`] when dropped.
pub struct Serving<'r>(&'r PyRetriever);

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        *self.0.caller.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl PyRetriever {
    pub fn new(callable: Py<PyAny>) -> Self {
        Self {
            callable,
            interrupt: Mutex::new(None),
            caller: Mutex::new(None),
        }
    }

    /// Sends the downloads asked for from now on to `caller`, until the guard is dropped.
    pub fn serve(&self, caller: mpsc::Sender<Event>) -> Serving<'_> {
        *self.caller.lock().unwrap_or_else(PoisonError::into_inner) = Some(caller);
        Serving(self)
    }

    /// The non-`Exception` error the function raised, if any, to re-raise.
    pub fn interrupt(&self) -> Option<PyErr> {
        self.interrupt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl PyRetriever {
    /// Calls the function, on a thread attached to the interpreter.
    pub fn call(&self, py: Python<'_>, url: &str) -> Result<Vec<u8>, RetrieveError> {
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
    }
}

impl Retrieve for PyRetriever {
    fn retrieve(&self, url: &str) -> Result<Vec<u8>, RetrieveError> {
        let caller = self
            .caller
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(caller) = caller else {
            // Not served: the work runs on the calling thread itself.
            return crate::stack::attach(|py| self.call(py, url));
        };
        check_cancelled();
        let (reply, answer) = mpsc::channel();
        let fetch = Fetch {
            url: url.to_owned(),
            reply,
        };
        // The caller is gone (interrupted) or did not answer: the work stops.
        if caller.send(Event::Fetch(fetch)).is_err() {
            std::panic::resume_unwind(Box::new(Cancelled));
        }
        answer
            .recv()
            .unwrap_or_else(|_| std::panic::resume_unwind(Box::new(Cancelled)))
    }
}
