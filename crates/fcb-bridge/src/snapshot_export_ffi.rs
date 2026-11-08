#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling for the shared create-only FCBS writer. Cancellation never erases
//! a terminal receipt for an operation that could already have written bytes.
use std::{ffi::{c_char, c_void}, path::Path};
use fcb_app::host::snapshot_export::{save_repository, SnapshotExportOptions};
use super::{cstr, reply, string_out, SearchCancellationCallback};

/// Save a bounded repository archive at an explicitly chosen new destination.
/// `respect_ignores` is 1 to read bounded repository ignore policies or 0 for the
/// existing default exclusions; other values are rejected before any operation.
/// Returns the shared snapshot-save JSON, including partial/error write effects.
/// Null means invalid input/admission or uncertain response delivery, not proof
/// that no destination was created. Never retry automatically after null.
///
/// # Safety
/// root/destination are readable NUL-terminated UTF-8 through return. The host
/// retains current root read authority and destination write access until this
/// synchronous worker call drains. If supplied, poll/context remain callable
/// and valid through return. Poll is invoked on this calling thread only, must
/// not block, unwind, mutate inputs, or reenter the export. Nonzero cancels.
/// Every non-null result must be freed once with this library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_snapshot_save_cancelable(root: *const c_char,
    destination: *const c_char, max_files: u64, max_file_bytes: u64,
    max_source_bytes: u64, respect_ignores: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| {
        let root = unsafe { cstr(root) }?;
        let destination = unsafe { cstr(destination) }?;
        let respect_ignores = match respect_ignores { 0 => false, 1 => true, _ => return None };
        let options = SnapshotExportOptions {
            max_files: usize::try_from(max_files).ok()?,
            max_file_bytes: usize::try_from(max_file_bytes).ok()?,
            max_source_bytes: usize::try_from(max_source_bytes).ok()?,
            respect_ignores,
        };
        let canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        let receipt = save_repository(Path::new(root), Path::new(destination), options, canceled).ok()?;
        // Do not poll/filter after the writer. A cancellation/error can carry an
        // incomplete or already-complete destination effect that must reach UI.
        string_out(receipt.as_str())
    })
}

#[cfg(all(test, unix))]
#[path = "snapshot_export_ffi_tests.rs"]
mod tests;
