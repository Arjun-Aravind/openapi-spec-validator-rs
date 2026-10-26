//! How much of the current thread's stack is left.
//!
//! Validation runs on threads of its own with a large stack. When none can be started (a
//! process or address-space limit), it runs on the caller's thread instead, as deeply as
//! that thread's stack allows: see [`crate::context::StackTooSmall`].

/// Bytes between the current stack pointer and the end of the thread's stack, when the
/// platform tells.
pub(crate) fn remaining() -> Option<usize> {
    let here = std::ptr::from_ref(&0u8) as usize;
    let lowest = lowest_address()?;
    here.checked_sub(lowest)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn lowest_address() -> Option<usize> {
    // SAFETY: `attr` is initialised by `pthread_getattr_np` before it is read and
    // destroyed afterwards; the calls only read the current thread's attributes.
    unsafe {
        let mut attr = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        if libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) != 0 {
            return None;
        }
        let mut addr = std::ptr::null_mut();
        let mut size = 0;
        let found = libc::pthread_attr_getstack(attr.as_ptr(), &mut addr, &mut size) == 0;
        libc::pthread_attr_destroy(attr.as_mut_ptr());
        found.then_some(addr as usize)
    }
}

#[cfg(target_vendor = "apple")]
fn lowest_address() -> Option<usize> {
    // SAFETY: both calls only read the current thread's stack bounds.
    unsafe {
        let thread = libc::pthread_self();
        let top = libc::pthread_get_stackaddr_np(thread) as usize;
        top.checked_sub(libc::pthread_get_stacksize_np(thread))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn lowest_address() -> Option<usize> {
    None
}
