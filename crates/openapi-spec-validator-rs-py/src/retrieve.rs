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
/// While a [`crate::stack::Task`] serves it, the task's caller downloads in Python instead,
/// never the library's threads (see there why); only when no thread could be started is the
/// function called here.
pub struct PyRetriever {
    callable: Py<PyAny>,
    interrupt: Mutex<Option<PyErr>>,
    /// Where downloads go while a task serves them.
    caller: Mutex<Option<mpsc::Sender<Event>>>,
}

impl PyRetriever {
    pub fn new(callable: Py<PyAny>) -> Self {
        Self {
            callable,
            interrupt: Mutex::new(None),
            caller: Mutex::new(None),
        }
    }

    /// Sends the downloads asked for from now on to `caller`.
    pub fn serve(&self, caller: mpsc::Sender<Event>) {
        *self.caller.lock().unwrap_or_else(PoisonError::into_inner) = Some(caller);
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
    fn call(&self, py: Python<'_>, url: &str) -> Result<Vec<u8>, RetrieveError> {
        let downloaded = self.callable.bind(py).call1((url,));
        if let Err(error) = &downloaded {
            if !error.is_instance_of::<PyException>(py) {
                *self
                    .interrupt
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(error.clone_ref(py));
            }
        }
        answer(downloaded)
    }
}

/// What a download (the function's return value, or what it raised) gives the validator:
/// the bytes of the document, or why the reference is unresolvable.
pub fn answer(downloaded: PyResult<Bound<'_, PyAny>>) -> Result<Vec<u8>, RetrieveError> {
    match downloaded.and_then(|body| body.extract::<PyBackedBytes>().map_err(PyErr::from)) {
        Ok(bytes) => Ok(bytes.to_vec()),
        Err(error) => Err(RetrieveError::new(error.to_string())),
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
