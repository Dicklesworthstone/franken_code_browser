#![deny(unsafe_op_in_unsafe_fn)]

//! C surface for captured repository results attached to existing atlas handles.
//! No source policy, matcher, worker pool or alternate reader lives in this bridge.
use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasSearchOptions, MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use super::{answer, cstr, number, reply, AccessError, Command};

/// Search the frozen atlas catalog, capture matching files and return first-page
/// JSON. Use a fresh query generation for every attempt, including failed ones.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. Returned
/// strings must be freed once with this library's fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let options = AtlasSearchOptions { max_matches: number(max_matches)?, max_files: number(max_files)?,
            max_file_bytes: number(max_file_bytes)?, max_source_bytes: number(max_source_bytes)? };
        atlases.execute(handle, Command::Search { generation, needle, options }, || false)
    }))
}
/// Read a page of accepted results without source I/O. Offsets are zero-based;
/// hit IDs are one-based, query-local identities and must not be used as rows.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_page(handle: u64, generation: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::SearchPage { generation, start: number(start)?, limit: number(limit)? }, || false)))
}
/// Compact per-file match counts tied to the same layout/query. Counts describe
/// retained occurrences; successful JSON does not imply complete search coverage.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_overlay(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle, Command::SearchOverlay { generation }, || false)))
}
/// Release the accepted source/results snapshot on a worker. Previously opened
/// readers retain their bytes. This consumes a fresh query generation.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle, Command::SearchClear { generation }, || false)))
}
/// Prepare a camera plan for an accepted hit. plan_generation is independent
/// from query generation. The caller must present/acknowledge the returned plan.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_search_focus(handle: u64, generation: u64, hit: u64, plan_generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::SearchFocus { generation, hit, plan_generation }, || false)))
}
/// Populate an EXISTING EMPTY reader from the whole retained matching capture,
/// never current disk contents. The reply includes the exact original selection,
/// a decoded context window and the explicit atlas-source/reader identity link.
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
