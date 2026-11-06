//! Returns heap freed by long-lived processes to the kernel.
//!
//! jemalloc purges freed pages only while a process keeps allocating, so an idle TUI would
//! keep the peak of the last conversation it replayed. Background threads purge on a timer
//! instead. They start here, after `arg0` dispatch, so helper processes such as the Linux
//! sandbox stay single-threaded.

/// Starts jemalloc's background purging threads.
#[cfg(all(
    target_os = "linux",
    any(target_env = "gnu", target_env = "musl"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(crate) fn purge_freed_heap_in_background() {
    let mut enabled = true;
    // SAFETY: `background_thread` is jemalloc's documented bool control; `newp` points to a
    // live bool of `newlen` bytes, and no old value is requested.
    let status = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"background_thread".as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            (&raw mut enabled).cast(),
            size_of::<bool>(),
        )
    };
    if status != 0 {
        tracing::warn!(status, "jemalloc background purging is unavailable");
    }
}

/// Other allocators need no help.
#[cfg(not(all(
    target_os = "linux",
    any(target_env = "gnu", target_env = "musl"),
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub(crate) fn purge_freed_heap_in_background() {}
