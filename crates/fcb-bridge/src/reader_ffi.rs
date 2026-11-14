#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer marshaling for retained native reader handles. All source/search/
//! selection behavior lives in fcb_app::host::reader; registry ownership is
//! safe Rust. These services do not acquire a runtime or start worker threads.

use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::HostResponse;
use super::{cstr, reply, string_out};
use super::reader_sessions::{AccessError, Command, ReaderSessions};

static READERS: OnceLock<ReaderSessions> = OnceLock::new();
/// The exported reader, atlas, saved-source and desk APIs share connected
/// process-global registries. Their C tests acquire this once before ANY
/// exported registry call, including unknown-handle probes (which still take
/// the production try_lock). One outer lock avoids cross-suite capacity races
/// and lock-order cycles; helpers and callbacks never acquire it again.
/// Independent-registry concurrency tests do not use this test-only guard.
#[cfg(test)]
pub(super) static FFI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
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

/// Capture once, retaining up to 32 MiB of exact bytes. A loaded handle cannot
/// be reopened. Window, transfer, analysis and copy limits remain independent.
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

/// Exact one-based physical line and explicitly typed logical column. A column
/// counts Unicode scalars (1), UTF-16 code units (2), or decoded UTF-8 bytes (3).
/// Invalid unit boundaries fail instead of rounding to another source position.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_position(handle: u64, line: u64, column: u64,
    column_kind: u64, max_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        use fcb_app::host::reader::ReaderColumnKind;
        let kind = match column_kind {
            1 => ReaderColumnKind::UnicodeScalar,
            2 => ReaderColumnKind::Utf16CodeUnit,
            3 => ReaderColumnKind::DecodedUtf8Byte,
            _ => return Err(AccessError::InvalidArgument),
        };
        readers.execute(handle, Command::Position { line, column, kind,
            bytes: number(max_bytes)? }, || false)
    }))
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

/// Additive Find controls: flags bit 0 enables Unicode lowercase comparison;
/// bit 1 continues counting after retained rows fill. Other bits are invalid.
/// # Safety
/// Same foreign-string lifetime contract as fcb_reader_find.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_find_with_options(handle: u64, generation: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, flags: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        if flags & !3 != 0 { return Err(AccessError::InvalidArgument); }
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        let options = fcb_app::host::reader::ReaderFindOptions {
            case_sensitive: flags & 1 == 0, count_all_matches: flags & 2 != 0,
        };
        readers.execute(handle, Command::FindWithOptions { generation, needle,
            limit: number(max_matches)?, scan_bytes: max_scan_bytes, options }, || false)
    }))
}

/// Bounded retained Find page after a count of occurrences, never a byte offset.
/// Replays the immutable capture from byte zero within max_scan_bytes. Each page
/// requires a new generation; flags have the same meaning as Find with options.
/// # Safety
/// Same foreign-string lifetime contract as fcb_reader_find.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_find_page(handle: u64, generation: u64,
    needle: *const c_char, start_after: u64, max_matches: u64, max_scan_bytes: u64,
    flags: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        if flags & !3 != 0 { return Err(AccessError::InvalidArgument); }
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        let options = fcb_app::host::reader::ReaderFindOptions {
            case_sensitive: flags & 1 == 0, count_all_matches: flags & 2 != 0,
        };
        readers.execute(handle, Command::FindPage { generation, needle, start_after,
            limit: number(max_matches)?, scan_bytes: max_scan_bytes, options }, || false)
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

/// Resolve decoded page-local UTF-8 boundaries to exact retained source bytes.
/// The visible page and selected source have separate fixed size limits. This
/// does not infer native UTF-16/glyph offsets or mutate any accepted query.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_resolve_selection(handle: u64, window_start: u64,
    window_end: u64, utf8_start: u64, utf8_end: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::ResolveSelection { window_start, window_end, utf8_start, utf8_end }, || false)))
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

#[path = "reader_supplied_ffi.rs"]
mod supplied;

#[path = "reader_outline_ffi.rs"]
mod outline;
#[path = "reader_references_ffi.rs"]
mod references;
#[path = "reader_document_ffi.rs"]
pub(super) mod document;
#[path = "reader_comparison_ffi.rs"]
mod comparison;

#[cfg(test)]
#[path = "reader_ffi_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reader_highlight_ffi_tests.rs"]
mod highlight_tests;

#[cfg(test)]
#[path = "reader_selection_ffi_tests.rs"]
mod selection_tests;
