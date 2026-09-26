//! Cooperative cancellation for the native workspace-search consumer.
//!
//! This remains a synchronous call to the existing safe host service. The
//! native host chooses its worker and owns the callback context until return;
//! no callback, context pointer, source grant or thread escapes this call.

use std::ffi::{c_char, c_void};
use std::path::Path;
use super::{cstr, host, reply, string_out};

/// Return zero to continue, nonzero to cancel. Called only on the calling
/// thread, potentially frequently; it must be fast, nonblocking and not unwind.
/// A nullable function pointer is represented as a null C function pointer.
pub type SearchCancellationCallback = unsafe extern "C" fn(*mut c_void) -> i32;

/// Run the SAME exact workspace search as `fcb_search_workspace`, on a worker
/// supplied by the native host. No replacement scanner or global token table.
///
/// Cancellation is cooperative at existing engine checkpoints; it cannot abort
/// an in-progress filesystem call. Null means canceled or failed admission/FFI
/// handoff, not an empty result set. A non-null JSON response has the ordinary
/// host success/error/partial schema and must be freed with `fcb_free_string`.
/// Cancellation is checked again before allocating the returned C string.
///
/// # Safety
/// Non-null `root` and `query` must be valid NUL-terminated UTF-8 strings stable
/// until return. When supplied, `poll` must remain callable for this call and
/// accept `context` (which may be null if the callback supports it). The caller
/// keeps all context storage and native root-access leases alive until RETURN,
/// even after requesting cancellation. The callback must not unwind, mutate
/// the input strings, or reenter this operation. No callback occurs after return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_search_workspace_cancelable(
    root: *const c_char,
    query: *const c_char,
    poll: Option<SearchCancellationCallback>,
    context: *mut c_void,
) -> *mut c_char {
    reply(|| {
        let canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return None; }
        let root = unsafe { cstr(root) }?;
        let query = unsafe { cstr(query) }?;
        let result = host::search_workspace(Path::new(root), query, canceled).ok()?;
        if canceled() { return None; }
        string_out(result.as_str())
    })
}

#[cfg(all(test, unix))]
mod tests;
