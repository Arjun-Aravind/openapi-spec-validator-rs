//! Running validator work on a thread with a known, large stack.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use openapi_spec_validator_rs::{
    Cancelled, LARGE_STACK_SIZE, check_cancelled, on_large_stack_thread, run_cancellable,
};
use pyo3::exceptions::{PyImportError, PyRuntimeError};
use pyo3::prelude::*;

/// Documents may nest almost 10 000 containers deep (the reader's limit), and loading,
/// validating, rendering and even dropping such a value recurses that deep. The calling
/// Python thread's stack can be far smaller than that needs (threads, debug builds), and
/// overflowing it would abort the interpreter. Only the touched part is ever committed.
/// The library's own size, so that validation runs on this thread instead of another.
const STACK_SIZE: usize = LARGE_STACK_SIZE;

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
    // Upstream builds its settings (a lazy pydantic import) for every read and
    // validation, which fails like this once the interpreter is shutting down. Starting
    // the work instead would have the worker call back into a dying interpreter.
    refuse_while_finalizing()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel();
    // Shared with the thread so that it is still here if the thread cannot start.
    let work = Arc::new(Mutex::new(Some(f)));
    let take = |work: &Mutex<Option<F>>| {
        work.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("the work runs once")
    };
    let spawned = {
        let cancel = Arc::clone(&cancel);
        let work = Arc::clone(&work);
        std::thread::Builder::new()
            .name("openapi-spec-validator-rs".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                let f = take(&work);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_cancellable(cancel, || on_large_stack_thread(f))
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
    };
    let handle = match spawned {
        Ok(handle) => handle,
        // No thread can be started (an address-space or process limit): do the work on
        // this thread, as upstream does all of it. The library still tries to move deep
        // recursion to threads of its own, and signals wait until the work is done.
        Err(_) => {
            let f = take(&work);
            return py.detach(move || run_cancellable(cancel, f));
        }
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

/// The `ImportError` upstream raises for any read or validation once the interpreter is
/// shutting down.
pub fn refuse_while_finalizing() -> PyResult<()> {
    if finalizing() {
        return Err(PyImportError::new_err(
            "sys.meta_path is None, Python is likely shutting down",
        ));
    }
    Ok(())
}

/// Whether the interpreter is shutting down (or gone).
fn finalizing() -> bool {
    // SAFETY: `Py_IsInitialized` only reads the runtime's state and may be called from any
    // thread, attached or not.
    unsafe { pyo3::ffi::Py_IsInitialized() == 0 }
}

/// `Python::attach` for the worker thread of [`run`]: when the call was cancelled, or the
/// interpreter has started shutting down meanwhile (a daemon thread's validation), the
/// work stops instead. Attaching to a finalizing interpreter blocks forever or ends the
/// thread under Rust's feet, depending on the Python version.
pub fn attach<R>(f: impl for<'py> FnOnce(Python<'py>) -> R) -> R {
    check_cancelled();
    if finalizing() {
        std::panic::resume_unwind(Box::new(Cancelled));
    }
    Python::attach(f)
}
