#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer marshaling for retained native reader handles. All source/search/
//! selection behavior lives in fcb_app::host::reader; registry ownership is
//! safe Rust. These services do not acquire a runtime or start worker threads.

use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::HostResponse;
use super::{cstr, reply, string_out};
use super::reader_sessions::{AccessError, Command, ReaderSessions};

static READERS: OnceLock<ReaderSessions> = OnceLock::new();
// Atlas activation targets this exact registry, never an independent reader table.
pub(super) fn registry() -> Option<&'static ReaderSessions> { READERS.get() }
fn answer(handle: u64, work: impl FnOnce(&ReaderSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match READERS.get() { Some(readers) => work(readers), None => Err(AccessError::UnknownHandle) };
    match result {
        Ok(response) => string_out(response.as_str()),
        Err(error) => string_out(&error.json(handle)),
    }
}
fn number(value: u64) -> Result<usize, AccessError> { usize::try_from(value).map_err(|_| AccessError::InvalidArgument) }

/// Explicitly reserve an empty handle. Zero means admission/identity failure.
/// No path is opened until fcb_reader_open is called on the host's worker.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_create() -> u64 {
    std::panic::catch_unwind(|| READERS.get_or_init(ReaderSessions::new).create().unwrap_or(0)).unwrap_or(0)
}

/// Capture once, retaining exact bytes. A loaded handle cannot be reopened.
/// # Safety
/// path is null or readable NUL-terminated UTF-8, stable until return. Returned
/// non-null JSON is released exactly once with this library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_open(handle: u64, path: *const c_char, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        readers.open(handle, Path::new(path), number(max_source_bytes)?, || false)
    }))
}

/// Information about the pinned source and accepted query generation. All
/// returned strings follow the same fcb_free_string ownership convention.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::Info, || false)))
}

/// Logical decoded window in the retained capture, never a live file reread.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_window(handle: u64, offset: u64, bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Window { offset, bytes: number(bytes)? }, || false)))
}

/// One-based physical source lines with a separate bounded byte allowance.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_lines(handle: u64, first: u64, count: u64, max_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Lines { first, count, bytes: number(max_bytes)? }, || false)))
}

/// Exact case-sensitive literal search over retained bytes. generation must
/// increase for each attempted query; old generations cannot select new hits.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8, stable until return. No
/// reference to this foreign string survives the synchronous call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_find(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        readers.execute(handle, Command::Find { generation, needle, limit: number(max_matches)?, scan_bytes: max_scan_bytes }, || false)
    }))
}

/// Exact selection plus bounded source context for a retained query occurrence.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_hit(handle: u64, generation: u64, index: u64, context_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Hit { generation, index: number(index)?, context: number(context_bytes)? }, || false)))
}

/// Original bytes as lossless hex, not a decoded-text or native clipboard write.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_copy_range(handle: u64, start: u64, end: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::CopyRange { start, end }, || false)))
}

/// Original match bytes from the accepted query and this exact capture.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_copy_hit(handle: u64, generation: u64, index: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::CopyHit { generation, index: number(index)? }, || false)))
}

/// Cooperative cancellation. Does not wait on source/query state and does not
/// poison the capture. Subsequent requests may run after the active call drains.
/// Returns 1 when the epoch changed; 0 on an invalid/busy/exhausted handle.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(READERS.get().is_some_and(|readers| readers.cancel(handle).is_ok()))).unwrap_or(0)
}

/// Remove the handle and cancel its in-flight work. The slot's source admission
/// survives until active calls release their ownership. Closing the final idle
/// capture can deallocate source storage: call close on a worker, not redraw.
/// Returns 1 if removed, 0 if unknown/busy. Never frees returned JSON strings.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(READERS.get().is_some_and(|readers| readers.close(handle).is_ok()))).unwrap_or(0)
}

#[path = "reader_outline_ffi.rs"]
mod outline;

#[cfg(test)]
#[path = "reader_ffi_tests.rs"]
mod tests;
