#![deny(unsafe_op_in_unsafe_fn)]

//! Supplied capture marshaling only. ReaderSession owns admission, copying,
//! decoding and document preparation; the label confers no filesystem grant.

use std::{ffi::c_char, path::Path};
use fcb_app::host::{HostResponse, MAX_HOST_TEXT_BYTES};
use fcb_app::host::reader::ReaderSession;
use super::{answer, cstr, number, reply, AccessError, ReaderSessions};

/// Open an empty reader with an owned copy of explicitly supplied bytes. This
/// does not inspect, canonicalize or open the label, or perform any source I/O.
/// Existing reader windows, search, outline and Markdown share this capture.
///
/// # Safety
/// label is null or a readable NUL-terminated UTF-8 string. For an admitted
/// nonzero length, bytes addresses that many initialized readable bytes in one
/// allocation, stable until return. Zero length permits null. No foreign borrow
/// survives the call. Oversized lengths are refused before either dereference.
/// Returned non-null JSON is released once with this library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_open_bytes(handle: u64, label: *const c_char,
    bytes: *const u8, length: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        unsafe { open_in(readers, handle, label, bytes, length, || false) }
    }))
}

// Shared by the C entry point and isolated-registry production tests. No second
// registry or test-only source implementation is used by the shipping route.
unsafe fn open_in(readers: &ReaderSessions, handle: u64, label: *const c_char,
    bytes: *const u8, length: u64, canceled: impl FnMut() -> bool)
    -> Result<HostResponse, AccessError> {
    let length = number(length)?;
    if length > MAX_HOST_TEXT_BYTES || (length != 0 && bytes.is_null()) {
        return Err(AccessError::InvalidArgument);
    }
    let label = unsafe { cstr(label) }.ok_or(AccessError::InvalidArgument)?;
    // Even a zero-length slice requires a non-null pointer with from_raw_parts.
    let source = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(bytes, length) }
    };
    readers.initialize_prepared(handle, |owner, stop| {
        let mut candidate = ReaderSession::from_bytes(owner, Path::new(label), source, &mut *stop)?;
        let response = candidate.info(&mut *stop)?;
        Ok((candidate, response))
    }, canceled)
}

#[cfg(test)]
#[path = "reader_supplied_ffi_tests.rs"]
mod tests;
