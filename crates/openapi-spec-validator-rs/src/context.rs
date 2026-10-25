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
    Context {
        cancel: Some(cancel),
    }
    .install(f)
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

/// Stops the calling validation if it was cancelled.
#[inline]
pub(crate) fn check() {
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
