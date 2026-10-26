//! Running validator work on a thread with a known, large stack.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use openapi_spec_validator_rs::RetrieveError;
use openapi_spec_validator_rs::{
    Cancelled, LARGE_STACK_SIZE, check_cancelled, on_large_stack_thread, run_cancellable,
    with_python_minor, within_this_stack,
};
use pyo3::exceptions::{PyImportError, PyRuntimeError};
use pyo3::prelude::*;

use crate::retrieve::{PyRetriever, answer};

/// Documents may nest almost 10 000 containers deep (the reader's limit), and loading,
/// validating, rendering and even dropping such a value recurses that deep. The calling
/// Python thread's stack can be far smaller than that needs (threads, debug builds), and
/// overflowing it would abort the interpreter. Only the touched part is ever committed.
/// The library's own size, so that validation runs on this thread instead of another.
const STACK_SIZE: usize = LARGE_STACK_SIZE;

/// How often the calling thread looks for pending signals while the work runs.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// What the worker thread of [`start`] tells the calling thread.
pub enum Event {
    /// The work is over: its result is in the shared slot.
    Done,
    /// A download for the retriever, which only the calling thread makes (see [`Task`]).
    Fetch(Fetch),
}

/// A download the worker asks the calling thread for.
pub struct Fetch {
    pub url: String,
    pub reply: mpsc::Sender<Result<Vec<u8>, RetrieveError>>,
}

/// The result, or the panic that ended the work.
type Slot<T> = Arc<Mutex<Option<std::thread::Result<PyResult<T>>>>>;

/// Work running on a thread of its own, seen from the thread that started it.
struct Worker<T> {
    /// Out only while [`Worker::next`] waits on it.
    receiver: Option<mpsc::Receiver<Event>>,
    result: Slot<T>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// What the work of a [`Worker`] did next.
enum Step<T> {
    Done(PyResult<T>),
    Fetch(Fetch),
}

/// Work that [`start`] began.
enum Started<T> {
    Running(Worker<T>),
    /// No thread could be started: the work ran on the calling thread.
    Finished(PyResult<T>),
}

/// Starts `f` with the GIL released, on a fresh thread with a [`STACK_SIZE`] stack. `f` may
/// re-attach (`Python::attach`) to read or build Python objects. The downloads of
/// `retriever` are asked of the starting thread (see [`Task`]).
fn start<T, F>(py: Python<'_>, retriever: Option<&PyRetriever>, f: F) -> PyResult<Started<T>>
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
    let (sender, receiver) = mpsc::channel();
    let result: Slot<T> = Arc::default();
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
        return Ok(Started::Finished(py.detach(move || {
            let result =
                run_cancellable(cancel, || with_python_minor(minor, || within_this_stack(f)));
            stay_out_if_finalizing();
            result
        })));
    };
    if let Some(retriever) = retriever {
        retriever.serve(sender);
    }
    Ok(Started::Running(Worker {
        receiver: Some(receiver),
        result,
        cancel,
        handle: Some(handle),
    }))
}

impl<T> Worker<T> {
    /// Waits, with the GIL released, for the work to finish or to ask for a download.
    ///
    /// Meanwhile the calling thread handles signals as Python code would: the exception a
    /// signal handler raises (`KeyboardInterrupt` for Ctrl-C, or whatever a `SIGALRM`
    /// handler raises) is returned as soon as it arrives, as upstream's pure-Python
    /// validation is abandoned where it stands. The work is cancelled then: it stops at its
    /// next check and its result is dropped.
    fn next(&mut self, py: Python<'_>) -> PyResult<Step<T>> {
        loop {
            // The receiver travels into the detached closure and back: it is `Send`, not
            // `Sync`.
            let receiver = self.receiver.take().expect("the work is waited for once");
            let (receiver, received) = py.detach(move || {
                let received = receiver.recv_timeout(SIGNAL_POLL);
                stay_out_if_finalizing();
                (receiver, received)
            });
            self.receiver = Some(receiver);
            match received {
                Ok(Event::Done) => break,
                Ok(Event::Fetch(fetch)) => return Ok(Step::Fetch(fetch)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Err(interrupted) = py.check_signals() {
                        self.cancel.store(true, Ordering::Relaxed);
                        return Err(interrupted);
                    }
                    let finished = self.handle.as_ref().is_none_or(JoinHandle::is_finished)
                        && self
                            .result
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .is_none();
                    if finished {
                        // Ended without a result or a `Done` still to come.
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let outcome = self
            .result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match outcome {
            Some(Ok(result)) => Ok(Step::Done(result)),
            // The work panicked: the panic propagates.
            Some(Err(panic)) => std::panic::resume_unwind(panic),
            // The thread ended without a result: it panicked, and the panic propagates.
            None => match self.handle.take().map(JoinHandle::join) {
                Some(Err(panic)) => std::panic::resume_unwind(panic),
                _ => Err(PyRuntimeError::new_err(
                    "the validation thread ended without a result",
                )),
            },
        }
    }
}

impl<T> Drop for Worker<T> {
    /// Work nobody waits for any more stops at its next check.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Runs `f` as [`start`] does and returns its result. The work makes no downloads.
pub fn run<T, F>(py: Python<'_>, f: F) -> PyResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> PyResult<T> + Send + 'static,
{
    let mut worker = match start(py, None, f)? {
        Started::Finished(result) => return result,
        Started::Running(worker) => worker,
    };
    loop {
        match worker.next(py)? {
            Step::Done(result) => return result,
            Step::Fetch(fetch) => {
                let _ = fetch.reply.send(Err(RetrieveError::new("no retriever")));
            }
        }
    }
}

/// What a [`Task`]'s work hands back: builds its Python result on the calling thread.
pub type Finish = Box<dyn FnOnce(Python<'_>) -> PyResult<Py<PyAny>> + Send>;

/// Validator work that may need `$ref` targets downloaded, driven from Python
/// (`_stack.run`): `step()` waits for the work and returns the URL of the next download,
/// or `None` once the work is done and `result()` has its result. The Python code then
/// downloads with the package's retriever and passes the outcome to the next `step()`.
///
/// Downloads thus run in Python code called from Python code, on the calling thread, as
/// upstream's do: an interrupt or the shutdown of the interpreter behaves as there. A
/// thread that Python 3.10 to 3.12 end at shutdown while it downloads is unwound by
/// `pthread_exit` through what is on its stack, which must not be the extension's code.
#[pyclass(module = "openapi_spec_validator_rs._core", frozen)]
pub struct Task {
    state: Mutex<TaskState>,
}

enum TaskState {
    Running {
        worker: Worker<Finish>,
        /// Where the outcome of the download asked for last goes.
        reply: Option<mpsc::Sender<Result<Vec<u8>, RetrieveError>>>,
    },
    /// Taken by `step()` while it waits.
    Busy,
    Finished(PyResult<Finish>),
    Closed,
}

impl Task {
    /// Starts `f` as [`start`] does, serving the downloads of `retriever` to the caller.
    pub fn start<F>(py: Python<'_>, retriever: Option<&PyRetriever>, f: F) -> PyResult<Self>
    where
        F: FnOnce() -> PyResult<Finish> + Send + 'static,
    {
        let state = match start(py, retriever, f)? {
            Started::Running(worker) => TaskState::Running {
                worker,
                reply: None,
            },
            Started::Finished(result) => TaskState::Finished(result),
        };
        Ok(Self {
            state: Mutex::new(state),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, TaskState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[pymethods]
impl Task {
    /// Passes on the outcome of the last download (`body`, or the `Exception` raised as
    /// `error`), then waits for the work: returns the URL to download next, or `None` once
    /// the work is done.
    #[pyo3(signature = (body=None, error=None))]
    fn step(
        &self,
        py: Python<'_>,
        body: Option<Bound<'_, PyAny>>,
        error: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Option<String>> {
        let state = std::mem::replace(&mut *self.state(), TaskState::Busy);
        let (mut worker, reply) = match state {
            TaskState::Running { worker, reply } => (worker, reply),
            TaskState::Busy => {
                return Err(PyRuntimeError::new_err("the task is already running"));
            }
            other => {
                *self.state() = other;
                return Ok(None);
            }
        };
        if let Some(reply) = reply {
            let downloaded = match (body, error) {
                (_, Some(error)) => Err(PyErr::from_value(error)),
                (Some(body), None) => Ok(body),
                (None, None) => Ok(py.None().into_bound(py)),
            };
            // The work is gone when it was cancelled meanwhile.
            let _ = reply.send(answer(downloaded));
        }
        let (state, url) = match worker.next(py) {
            Ok(Step::Fetch(fetch)) => (
                TaskState::Running {
                    worker,
                    reply: Some(fetch.reply),
                },
                Ok(Some(fetch.url)),
            ),
            Ok(Step::Done(result)) => (TaskState::Finished(result), Ok(None)),
            Err(error) => (TaskState::Closed, Err(error)),
        };
        *self.state() = state;
        url
    }

    /// The result of the finished work (once only).
    fn result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match std::mem::replace(&mut *self.state(), TaskState::Closed) {
            TaskState::Finished(result) => result.and_then(|finish| finish(py)),
            _ => Err(PyRuntimeError::new_err("the task has no result")),
        }
    }

    /// Stops the work if it still runs.
    fn close(&self) {
        let state = std::mem::replace(&mut *self.state(), TaskState::Closed);
        if let TaskState::Busy = state {
            // `step()` is waiting on another thread and keeps the task.
            *self.state() = TaskState::Busy;
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

/// `Python::attach` for the worker thread of [`start`]: when the call was cancelled, or
/// the interpreter has started shutting down meanwhile (a daemon thread's validation), the
/// work stops instead. Attaching to a finalizing interpreter blocks forever or ends the
/// thread under Rust's feet, depending on the Python version.
pub fn attach<R>(f: impl for<'py> FnOnce(Python<'py>) -> R) -> R {
    check_cancelled();
    if finalizing() {
        std::panic::resume_unwind(Box::new(Cancelled));
    }
    Python::attach(f)
}
