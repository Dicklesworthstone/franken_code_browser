#![deny(unsafe_op_in_unsafe_fn)]

//! C surface for captured repository results attached to existing atlas handles.
//! No source policy, matcher, worker pool or alternate reader lives in this bridge.
use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasSearchOptions, MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use super::{answer, cstr, number, reply, AccessError, Command};

/// Search the frozen atlas catalog to its terminal state and return first-page
/// JSON. Use a fresh query generation for every attempt, including failed ones.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. Returned
/// strings must be freed once with this library's fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, needle, max_matches, max_files, max_file_bytes, max_source_bytes, false) }
}

/// Admit a resumable query without reading any source. Copies the bounded query
/// text; the input pointer need not survive this call. Call step on a host worker
/// and use search_in_progress, not search_complete, to decide whether to resume.
/// # Safety
/// Same input and returned-string ownership contract as fcb_atlas_search.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, needle, max_matches, max_files, max_file_bytes, max_source_bytes, true) }
}

// Marshaling only: both public entrypoints use the same safe application engine.
unsafe fn submit(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64,
    progressive: bool) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let options = AtlasSearchOptions { max_matches: number(max_matches)?, max_files: number(max_files)?,
            max_file_bytes: number(max_file_bytes)?, max_source_bytes: number(max_source_bytes)? };
        let command = if progressive { Command::SearchBegin { generation, needle, options } }
            else { Command::Search { generation, needle, options } };
        atlases.execute(handle, command, || false)
    }))
}

/// Examine at most one file (at most the admitted 1 MiB cap), then return control.
/// This remains synchronous worker work, not a UI callback or a latency bound.
/// Camera/paging/activation can run between calls. Cancellation of the atlas
/// invalidates paused work too; an old generation cannot resume in a new epoch.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_step(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle, Command::SearchStep { generation }, || false)))
}

/// Page finished or running results without source I/O. next_offset=null is the
/// end of the CURRENT rows, not the end of a running stream. IDs stay stable as
/// progress appends; use the query generation and hit ID together for activation.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_page(handle: u64, generation: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::SearchPage { generation, start: number(start)?, limit: number(limit)? }, || false)))
}
/// Compact per-file counts for finished/running queries. Successful JSON does
/// not imply complete search coverage; counts describe retained occurrences.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_overlay(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle, Command::SearchOverlay { generation }, || false)))
}
/// Release finished and pending query captures on a worker. Independently opened
/// readers retain their bytes. This consumes a fresh query generation.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle, Command::SearchClear { generation }, || false)))
}
/// Prepare an unpresented camera plan for an exact retained hit. Camera-plan and
/// query generations remain independent, including during partial publication.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_focus(handle: u64, generation: u64, hit: u64, plan_generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::SearchFocus { generation, hit, plan_generation }, || false)))
}
/// Populate an EXISTING EMPTY reader from the whole retained matching capture,
/// never current disk contents. The reply includes exact original selection,
/// decoded context and the explicit atlas-source/reader identity link. Works for
/// running queries; later query cancellation cannot retract a delivered reader.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_open_reader(handle: u64, reader: u64, generation: u64, hit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        atlases.open_search_reader(handle, readers, reader, generation, hit, || false)
    }))
}

#[cfg(all(test, unix))]
#[path = "atlas_search_ffi_tests.rs"]
mod tests;
