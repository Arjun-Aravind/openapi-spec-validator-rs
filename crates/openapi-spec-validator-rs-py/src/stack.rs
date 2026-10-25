//! Running validator work on a thread with a known, large stack.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use openapi_spec_validator_rs::{Cancelled, run_cancellable};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Documents may nest almost 10 000 containers deep (the reader's limit), and loading,
/// validating, rendering and even dropping such a value recurses that deep. The calling
/// Python thread's stack can be far smaller than that needs (threads, debug builds), and
/// overflowing it would abort the interpreter. Only the touched part is ever committed.
const STACK_SIZE: usize = 64 << 20;

/// How often the calling thread looks for pending signals while the work runs.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// Runs `f` with the GIL released, on a fresh thread with a [`STACK_SIZE`] stack, and
/// returns its result. `f` may re-attach (`Python::attach`) to read or build Python objects.
///
/// Meanwhile the calling thread handles signals as Python code would: the exception a
/// signal handler raises (`KeyboardInterrupt` for Ctrl-C, or whatever a `SIGALRM`
/// handler raises) is returned as soon as it arrives, as upstream's pure-Python
/// validation is abandoned where it stands. The work is cancelled then: it stops at its
/// next check and its result is dropped.
pub fn run<T, F>(py: Python<'_>, f: F) -> PyResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> PyResult<T> + Send + 'static,
{
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel();
    let handle = {
        let cancel = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("openapi-spec-validator-rs".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_cancellable(cancel, f)
                }));
                match outcome {
                    // The receiver is gone when the caller was interrupted: nobody to
                    // report to.
                    Ok(result) => {
                        let _ = sender.send(result);
                    }
                    // Stopped on request: the caller has moved on.
                    Err(payload) if payload.is::<Cancelled>() => {}
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            })
            .map_err(|err| {
                PyRuntimeError::new_err(format!("cannot start the validation thread: {err}"))
            })?
    };
    loop {
        // The receiver travels into the detached closure and back: it is `Send`, not `Sync`.
        let (returned, received) = py.detach(move || {
            let received = receiver.recv_timeout(SIGNAL_POLL);
            (receiver, received)
        });
        receiver = returned;
        match received {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Err(interrupted) = py.check_signals() {
                    cancel.store(true, Ordering::Relaxed);
                    return Err(interrupted);
                }
            }
            // The thread ended without sending: it panicked, and the panic propagates.
            Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => {
                    return Err(PyRuntimeError::new_err(
                        "the validation thread ended without a result",
                    ));
                }
            },
        }
    }
}
