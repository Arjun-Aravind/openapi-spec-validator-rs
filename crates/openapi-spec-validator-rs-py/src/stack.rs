//! Running validator work on a thread with a known, large stack.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Documents may nest almost 10 000 containers deep (the reader's limit), and loading,
/// validating, rendering and even dropping such a value recurses that deep. The calling
/// Python thread's stack can be far smaller than that needs (threads, debug builds), and
/// overflowing it would abort the interpreter. Only the touched part is ever committed.
const STACK_SIZE: usize = 64 << 20;

/// Runs `f` with the GIL released, on a fresh thread with a [`STACK_SIZE`] stack.
/// `f` may re-attach (`Python::attach`) to read or build Python objects.
pub fn run<T, F>(py: Python<'_>, f: F) -> PyResult<T>
where
    T: Send,
    F: FnOnce() -> PyResult<T> + Send,
{
    py.detach(|| {
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name("openapi-spec-validator-rs".to_owned())
                .stack_size(STACK_SIZE)
                .spawn_scoped(scope, f)
                .map_err(|err| {
                    PyRuntimeError::new_err(format!("cannot start the validation thread: {err}"))
                })?;
            handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    })
}
