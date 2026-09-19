#![deny(unsafe_op_in_unsafe_fn)]

//! Thin marshaling for an explicitly prepared reusable capture index. Existing
//! search step/paging/overlay/focus/reader entrypoints consume progressive output.
use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasIndexOptions, MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use crate::atlas_sessions::IndexCommand;
use super::{answer, cstr, number, reply, AccessError};

/// Capture and index the frozen catalog. Preparation itself is a synchronous
/// worker operation, not a redraw callback or a one-file continuation.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_prepare(handle: u64, generation: u64, max_files: u64,
    max_file_bytes: u64, max_source_bytes: u64, max_index_grams: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let options = AtlasIndexOptions { max_files: number(max_files)?, max_file_bytes: number(max_file_bytes)?,
            max_source_bytes: number(max_source_bytes)?, max_index_grams: number(max_index_grams)? };
        atlases.execute_index(handle, IndexCommand::Prepare { generation, options }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_index(handle, IndexCommand::Info, || false)))
}
/// Finish an indexed literal query on this host worker.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. Every
/// non-null response must be freed once with this library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, index_generation, needle, max_matches, max_scan_bytes, false) }
}
/// Admit without verifying source. Continue with fcb_atlas_search_step on a
/// worker; each call visits at most one captured file. All normal search result
/// operations work between steps. The foreign query string is copied at begin.
/// # Safety
/// Same pointer and returned-string ownership contract as fcb_atlas_search_indexed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_begin(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, index_generation, needle, max_matches, max_scan_bytes, true) }
}
unsafe fn submit(handle: u64, generation: u64, index_generation: u64, needle: *const c_char,
    max_matches: u64, max_scan_bytes: u64, progressive: bool) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let max_matches = number(max_matches)?;
        let command = if progressive { IndexCommand::Begin { generation, index_generation, needle, max_matches, max_scan_bytes } }
            else { IndexCommand::Query { generation, index_generation, needle, max_matches, max_scan_bytes } };
        atlases.execute_index(handle, command, || false)
    }))
}
/// Clear the reusable index and obsolete paused work, not accepted hit captures
/// or independently opened readers. Fresh search-attempt generation; worker only.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_index(handle, IndexCommand::Clear { generation }, || false)))
}

#[cfg(test)]
#[path = "atlas_index_ffi_tests.rs"]
mod tests;
