//! Running validator work on a thread with a known, large stack.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use openapi_spec_validator_rs::RetrieveError;
use openapi_spec_validator_rs::{
    Cancelled, LARGE_STACK_SIZE, check_cancelled, on_large_stack_thread, run_cancellable,
    with_python_minor, within_this_stack,
};
use pyo3::exceptions::{PyImportError, PyRuntimeError};
use pyo3::prelude::*;

use crate::retrieve::PyRetriever;

/// Documents may nest almost 10 000 containers deep (the reader's limit), and loading,
/// validating, rendering and even dropping such a value recurses that deep. The calling
/// Python thread's stack can be far smaller than that needs (threads, debug builds), and
/// overflowing it would abort the interpreter. Only the touched part is ever committed.
/// The library's own size, so that validation runs on this thread instead of another.
const STACK_SIZE: usize = LARGE_STACK_SIZE;

/// How often the calling thread looks for pending signals while the work runs.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// What the worker thread of [`run`] tells the calling thread.
pub enum Event {
    /// The work is over: its result is in the shared slot.
    Done,
    /// A download for the retriever, which only the calling thread makes (see [`run`]).
    Fetch(Fetch),
}

/// A download the worker asks the calling thread for.
pub struct Fetch {
    pub url: String,
    pub reply: mpsc::Sender<Result<Vec<u8>, RetrieveError>>,
}

/// Runs `f` with the GIL released, on a fresh thread with a [`STACK_SIZE`] stack, and
/// returns its result. `f` may re-attach (`Python::attach`) to read or build Python objects.
///
/// Meanwhile the calling thread handles signals as Python code would: the exception a
/// signal handler raises (`KeyboardInterrupt` for Ctrl-C, or whatever a `SIGALRM`
/// handler raises) is returned as soon as it arrives, as upstream's pure-Python
/// validation is abandoned where it stands. The work is cancelled then: it stops at its
/// next check and its result is dropped.
///
/// The calling thread also makes the downloads of `retriever` that the work asks for:
/// Python code then runs on a thread Python started, where an interrupt or the shutdown of
/// the interpreter behaves as upstream's does. A thread of the library's own that waits
/// to enter a finalizing interpreter is ended by Python 3.10 to 3.12 in a way Rust cannot
/// unwind through, which aborts the process.
pub fn run<T, F>(py: Python<'_>, retriever: Option<&PyRetriever>, f: F) -> PyResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> PyResult<T> + Send + 'static,
{
    // Upstream builds its settings (a lazy pydantic import) for every read and
    // validation, which fails like this once the interpreter is shutting down. Starting
    // the work instead would have the worker call back into a dying interpreter.
    refuse_while_finalizing()?;
    // Where the standard library changed between versions, follow the running one.
    let minor = u32::from(py.version_info().minor);
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel();
    // The result, or the panic that ended the work.
    let result: Arc<Mutex<Option<std::thread::Result<PyResult<T>>>>> = Arc::default();
    // Shared with the thread so that it is still here if the thread cannot start.
    let work = Arc::new(Mutex::new(Some(f)));
    let take = |work: &Mutex<Option<F>>| {
        work.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("the work runs once")
    };
    let spawn = |stack_size: usize| {
        let cancel = Arc::clone(&cancel);
        let work = Arc::clone(&work);
        let result = Arc::clone(&result);
        let sender = sender.clone();
        std::thread::Builder::new()
            .name("openapi-spec-validator-rs".to_owned())
            .stack_size(stack_size)
            .spawn(move || {
                let f = take(&work);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_cancellable(cancel, || {
                        with_python_minor(minor, || {
                            if stack_size == STACK_SIZE {
                                on_large_stack_thread(f)
                            } else {
                                within_this_stack(f)
                            }
                        })
                    })
                }));
                // Stopped on request: the caller has moved on.
                if outcome
                    .as_ref()
                    .is_err_and(|payload| payload.is::<Cancelled>())
                {
                    return;
                }
                *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
                // The receiver is gone when the caller was interrupted: nobody to tell.
                let _ = sender.send(Event::Done);
            })
    };
    // Under an address-space limit a smaller stack may still fit; documents then nest
    // only as deeply as it allows.
    let spawned = [STACK_SIZE, 16 << 20, 4 << 20]
        .into_iter()
        .find_map(|stack_size| spawn(stack_size).ok());
    let Some(handle) = spawned else {
        // No thread can be started at all (a process limit): do the work on this thread,
        // as upstream does all of it, as deeply as its stack allows. Signals wait until
        // the work is done, and the retriever is called right here.
        let f = take(&work);
        return py.detach(move || {
            let result =
                run_cancellable(cancel, || with_python_minor(minor, || within_this_stack(f)));
            stay_out_if_finalizing();
            result
        });
    };
    let _serving = retriever.map(|retriever| retriever.serve(sender));
    loop {
        // The receiver travels into the detached closure and back: it is `Send`, not `Sync`.
        let (returned, received) = py.detach(move || {
            let received = receiver.recv_timeout(SIGNAL_POLL);
            stay_out_if_finalizing();
            (receiver, received)
        });
        receiver = returned;
        match received {
            Ok(Event::Done) => {
                let outcome = result.lock().unwrap_or_else(PoisonError::into_inner).take();
                match outcome {
                    Some(Ok(result)) => return result,
                    // The work panicked: the panic propagates.
                    Some(Err(panic)) => std::panic::resume_unwind(panic),
                    None => break,
                }
            }
            Ok(Event::Fetch(fetch)) => {
                let Some(retriever) = retriever else {
                    let _ = fetch.reply.send(Err(RetrieveError::new("no retriever")));
                    continue;
                };
                let downloaded = retriever.call(py, &fetch.url);
                // `KeyboardInterrupt` or `SystemExit` from the download ends the
                // validation, as it propagates upstream.
                if let Some(interrupted) = retriever.interrupt() {
                    cancel.store(true, Ordering::Relaxed);
                    return Err(interrupted);
                }
                let _ = fetch.reply.send(downloaded);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Err(interrupted) = py.check_signals() {
                    cancel.store(true, Ordering::Relaxed);
                    return Err(interrupted);
                }
                if handle.is_finished()
                    && result
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_none()
                {
                    // Ended without a result or a `Done` still to come.
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // The thread ended without a result: it panicked, and the panic propagates.
    match handle.join() {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(()) => Err(PyRuntimeError::new_err(
            "the validation thread ended without a result",
        )),
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

/// Never returns once the interpreter is shutting down, so that a daemon thread waiting
/// here does not re-enter it: Python 3.10 to 3.12 end a thread that does with
/// `pthread_exit`, whose unwinding through this extension's frames aborts the process.
/// Such a thread ends with the process, as Python 3.14 makes it wait too.
fn stay_out_if_finalizing() {
    if finalizing() {
        loop {
            std::thread::park();
        }
    }
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
