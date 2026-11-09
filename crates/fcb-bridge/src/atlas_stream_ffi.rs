#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling for bounded whole-file search. It deliberately has no captured
//! reader activation: a retained literal is not an immutable whole source.

use std::ffi::{c_char, c_void};
use fcb_app::host::atlas_stream_search::{AtlasStreamSearchOptions,
    MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES};
use crate::SearchCancellationCallback;
use crate::atlas_sessions::StreamCommand;
use super::{answer, number, reply, AccessError};

/// Search all catalogued files through bounded streaming reads, then retain
/// compact literal witnesses. This is synchronous worker work. The backend
/// allocates a fresh query generation; page/hit must use the returned generation.
/// # Safety
/// needle is null or readable until its NUL terminator or 1025 bytes, whichever
/// comes first, and remains stable until return. poll/context remain valid until
/// return; poll must not unwind or reenter this session. Neither is retained.
/// Free each non-null returned string once with this library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_cancelable(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        let needle = unsafe { bounded_needle(needle) }?;
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?,
            max_scan_bytes, max_read_calls };
        atlases.execute_stream(handle, StreamCommand::Search { needle, options }, &mut canceled)
    }))
}

/// Page at most 32 retained rows, without source I/O. A missing or replaced
/// generation is an error, never an empty successful result set.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_stream_search_page(handle: u64, generation: u64,
    start: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_stream(handle,
        StreamCommand::Page { generation, start: number(start)?, count: number(count)? }, || false)))
}

/// Return an exact retained matching literal and its original encoded bytes.
/// This performs no source I/O and never supplies surrounding current context.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_stream_search_hit(handle: u64, generation: u64,
    position: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_stream(handle,
        StreamCommand::Hit { generation, position: number(position)? }, || false)))
}

/// Revoke the accepted report and advance the stream query identity. Worker only.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_stream_search_clear(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_stream(handle, StreamCommand::Clear, || false)))
}

// Scan only the declared query allowance. CStr::from_ptr would first traverse
// an unbounded foreign string before we could reject an oversized request.
unsafe fn bounded_needle<'a>(pointer: *const c_char) -> Result<&'a str, AccessError> {
    if pointer.is_null() { return Err(AccessError::InvalidArgument); }
    for length in 0..=MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES {
        if unsafe { *pointer.add(length) } == 0 {
            if length == 0 { return Err(AccessError::InvalidArgument); }
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            return std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument);
        }
    }
    Err(AccessError::InvalidArgument)
}

#[cfg(all(test, unix))]
#[path = "atlas_stream_ffi_tests.rs"]
mod tests;
