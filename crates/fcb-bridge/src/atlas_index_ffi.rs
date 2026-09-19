#![deny(unsafe_op_in_unsafe_fn)]

//! Thin marshaling for an explicitly prepared reusable capture index. Existing
//! search paging, overlay, focus and reader entrypoints consume its query output.
use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasIndexOptions, MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use crate::atlas_sessions::IndexCommand;
use super::{answer, cstr, number, reply, AccessError};

/// Capture the existing atlas catalog and prepare the shared production index.
/// Synchronous worker work, not a redraw callback. Generation shares the search
/// attempt sequence. Inspect coverage and quotas even when status is ok.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_prepare(handle: u64, generation: u64, max_files: u64,
    max_file_bytes: u64, max_source_bytes: u64, max_index_grams: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let options = AtlasIndexOptions { max_files: number(max_files)?, max_file_bytes: number(max_file_bytes)?,
            max_source_bytes: number(max_source_bytes)?, max_index_grams: number(max_index_grams)? };
        atlases.execute_index(handle, IndexCommand::Prepare { generation, options }, || false)
    }))
}
/// Reconcile the accepted index generation and retained coverage, with no I/O.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_index(handle, IndexCommand::Info, || false)))
}
/// Query only the named captured index. Does not reread live source or rebuild
/// segments. Max scan bytes limits original captured bytes actually verified.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. Every
/// non-null response must be freed once with this library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        atlases.execute_index(handle, IndexCommand::Query { generation, index_generation, needle,
            max_matches: number(max_matches)?, max_scan_bytes }, || false)
    }))
}
/// Release reusable index/source pins. Accepted hit captures and independently
/// opened readers survive. Uses a fresh search-attempt generation. Worker only.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_index(handle, IndexCommand::Clear { generation }, || false)))
}

#[cfg(test)]
#[path = "atlas_index_ffi_tests.rs"]
mod tests;
