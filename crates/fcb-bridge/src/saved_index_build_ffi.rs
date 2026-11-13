#![deny(unsafe_op_in_unsafe_fn)]

//! Worker-only FCBD creation from the retained archive. The host owns the
//! existing index engine, create-new writer and immutable recovery receipt.
use super::{AccessError, Command, answer, c_char, cstr, number, reply};
use std::{ffi::c_void, path::Path};
use fcb_app::host::saved_repository::index_build::SavedIndexBuildLimits;

/// Build without a foreign cancellation callback. fcb_saved_cancel still
/// interrupts the entered operation at the shared engine/writer checkpoints.
/// # Safety
/// destination is null or readable NUL-terminated UTF-8 stable until return.
/// Retain the destination write grant through return. Free every non-null JSON
/// once with fcb_free_string, including error receipts describing write effects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_build_index(handle: u64, operation: u64,
    destination: *const c_char, max_file_bytes: u64, max_source_bytes: u64,
    max_grams: u64) -> *mut c_char {
    unsafe { fcb_saved_build_index_cancelable(handle, operation, destination,
        max_file_bytes, max_source_bytes, max_grams, None, std::ptr::null_mut()) }
}

/// The callback runs synchronously on the calling worker. Cancellation does
/// not erase a terminal write-effect receipt and does not delete any output.
/// # Safety
/// destination is null or readable NUL-terminated UTF-8 stable until return.
/// If supplied, poll/context must remain callable until return. poll is fast,
/// nonblocking, must not unwind or reenter the bridge, and returns nonzero to
/// cancel. Retain the output grant through drain and free every owned response
/// once with fcb_free_string. A null response is uncertain: recover this exact
/// operation's receipt before considering another write; never auto-retry it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_build_index_cancelable(handle: u64, operation: u64,
    destination: *const c_char, max_file_bytes: u64, max_source_bytes: u64,
    max_grams: u64, poll: Option<crate::SearchCancellationCallback>,
    context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let destination = unsafe { cstr(destination) }.ok_or(AccessError::InvalidArgument)?;
        let limits = SavedIndexBuildLimits { max_file_bytes: number(max_file_bytes)?,
            max_source_bytes, max_grams: number(max_grams)? };
        sessions.build_index(handle, operation, Path::new(destination), limits,
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Replay the last admitted build's immutable receipt without source/output
/// I/O or writing again. Stale/unknown operations return a structured error.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_index_build_receipt(handle: u64, operation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::IndexBuildReceipt { operation }, || false)))
}

#[cfg(all(test, unix))]
#[path = "saved_index_build_ffi_tests.rs"]
mod tests;
