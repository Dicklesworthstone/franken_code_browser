#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling only. Source identity, full-range admission, bounded create-only
//! publication and terminal write effects belong to the shared reader host.

use std::{ffi::{c_char, c_void}, path::Path};
use super::{answer, AccessError};
use crate::{cstr, reply, SearchCancellationCallback};

/// Export exact bytes of the accepted immutable reader to a NEW destination.
/// Whole-file and arbitrary byte ranges, including empty ranges, are supported.
/// Does not decode, open a live source, write the clipboard or overwrite a file.
/// A cancellation/error receipt can describe a partial or complete destination;
/// always deliver it. Null is uncertain delivery and must never trigger replay.
///
/// # Safety
/// destination is null or readable NUL-terminated UTF-8, stable through return.
/// A valid destination is an absolute path of at most 16384 native bytes. The
/// host retains write authority until this synchronous worker call returns.
/// Optional poll/context stay valid and callable on this thread through return.
/// Poll must be fast, nonblocking, never unwind or reenter this export; nonzero
/// cancels. fcb_reader_cancel/close also revoke work cooperatively. A blocking
/// filesystem syscall itself cannot be interrupted. Free every non-null result
/// once with fcb_free_string, even error or canceled receipts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_export_original_cancelable(handle: u64,
    start: u64, end: u64, destination: *const c_char,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let destination = unsafe { cstr(destination) }.ok_or(AccessError::InvalidArgument)?;
        readers.export_original(handle, start, end, Path::new(destination),
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

#[cfg(all(test, unix))]
#[path = "reader_export_ffi_tests.rs"]
mod tests;
