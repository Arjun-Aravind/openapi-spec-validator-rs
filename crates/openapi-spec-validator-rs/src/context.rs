//! Settings of one validation that travel with it across the validator's threads.
//!
//! The library spawns threads of its own (large stacks, the two concurrent passes), so
//! anything that applies to "this validation" lives in a thread-local [`Context`] that
//! [`spawn`](crate::on_large_stack) copies into every thread it starts.
//!
//! Today that is a cancellation flag: a caller that gives up on a validation (the
//! Python bindings on Ctrl-C) sets it, and the work in progress stops at its next
//! check instead of running to completion unobserved.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The unwind payload of a cancelled validation (see [`run_cancellable`]).
#[derive(Debug)]
pub struct Cancelled;

#[derive(Clone, Default)]
pub(crate) struct Context {
    cancel: Option<Arc<AtomicBool>>,
    stack: Option<PythonStack>,
    python_minor: Option<u32>,
}

/// The Python 3 minor version whose behaviour is followed when nothing else is asked for.
pub const DEFAULT_PYTHON_MINOR: u32 = 13;

/// Where upstream's validation would sit on the Python stack: the depth of
/// `SpecValidator.iter_errors` (the outermost frame counts 1) and the recursion limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PythonStack {
    pub spec_depth: usize,
    pub limit: usize,
}

impl PythonStack {
    /// Upstream's console script (`openapi-spec-validator`) with Python's default limit:
    /// it calls `validate()` for the first error, but `iter_errors()` itself, two frames
    /// shallower, with `--validation-errors all`.
    pub fn command_line(all_errors: bool) -> Self {
        Self {
            spec_depth: crate::frames::CLI_SPEC_DEPTH - 2 * usize::from(all_errors),
            limit: crate::frames::DEFAULT_LIMIT,
        }
    }
}

/// Runs `f` as if upstream's validation sat at `stack` on the Python stack, which decides
/// where it would exceed the recursion limit (by default: upstream's command line).
///
/// Not a supported API: used by the Python bindings.
#[doc(hidden)]
pub fn with_python_stack<T>(stack: PythonStack, f: impl FnOnce() -> T) -> T {
    let mut context = Context::current();
    context.stack = Some(stack);
    context.install(f)
}

/// Runs `f` following the standard library of Python 3.`minor` where it changed between
/// versions (by default 3.13): with 3.14, `file:` URLs lose their query, `\z` is a valid
/// regex escape, and several messages are worded differently.
///
/// Not a supported API: used by the command line and the Python bindings.
#[doc(hidden)]
pub fn with_python_minor<T>(minor: u32, f: impl FnOnce() -> T) -> T {
    let mut context = Context::current();
    context.python_minor = Some(minor);
    context.install(f)
}

/// The Python 3 minor version the current validation follows.
pub(crate) fn python_minor() -> u32 {
    CURRENT
        .with(|current| current.borrow().python_minor)
        .unwrap_or(DEFAULT_PYTHON_MINOR)
}

/// The Python stack position of the current validation, if the caller gave one.
pub(crate) fn python_stack() -> Option<PythonStack> {
    CURRENT.with(|current| current.borrow().stack)
}

thread_local! {
    static CURRENT: RefCell<Context> = RefCell::new(Context::default());
}

impl Context {
    /// The context of the calling thread.
    pub(crate) fn current() -> Self {
        CURRENT.with(|current| current.borrow().clone())
    }

    /// Runs `f` with this context installed on the calling thread.
    pub(crate) fn install<T>(self, f: impl FnOnce() -> T) -> T {
        struct Restore(Option<Context>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(previous) = self.0.take() {
                    CURRENT.with(|current| *current.borrow_mut() = previous);
                }
            }
        }
        let previous = CURRENT.with(|current| std::mem::replace(&mut *current.borrow_mut(), self));
        let _restore = Restore(Some(previous));
        f()
    }
}

/// Runs `f`, which stops early once `cancel` is set: the validation work it starts
/// unwinds with a [`Cancelled`] payload at its next check. The unwind does not run the
/// panic hook; catch it with [`std::panic::catch_unwind`] and drop it.
///
/// Not a supported API: used by the Python bindings.
#[doc(hidden)]
pub fn run_cancellable<T>(cancel: Arc<AtomicBool>, f: impl FnOnce() -> T) -> T {
    let mut context = Context::current();
    context.cancel = Some(cancel);
    context.install(f)
}

/// The calling thread's validation context, to carry into threads started outside the
/// library (a thread pool): [`ContextHandle::install`] it there.
///
/// Not a supported API: used by the command line.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct ContextHandle(Context);

impl ContextHandle {
    pub fn capture() -> Self {
        Self(Context::current())
    }

    pub fn install<T>(&self, f: impl FnOnce() -> T) -> T {
        self.0.clone().install(f)
    }
}

/// Stops the calling validation if it was cancelled: unwinds with [`Cancelled`] inside
/// [`run_cancellable`], and does nothing elsewhere.
///
/// Not a supported API: used by the Python bindings before calling back into Python.
#[doc(hidden)]
#[inline]
pub fn check() {
    let cancelled = CURRENT.with(|current| {
        current
            .borrow()
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    });
    if cancelled {
        std::panic::resume_unwind(Box::new(Cancelled));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_unwinds_with_its_payload() {
        let flag = Arc::new(AtomicBool::new(false));
        let reached = run_cancellable(Arc::clone(&flag), || {
            check();
            true
        });
        assert!(reached);
        flag.store(true, Ordering::Relaxed);
        let outcome = std::panic::catch_unwind(|| run_cancellable(flag, check));
        let payload = outcome.expect_err("cancelled");
        assert!(payload.downcast_ref::<Cancelled>().is_some());
        // The previous context is back: nothing is cancelled any more.
        check();
    }
}
