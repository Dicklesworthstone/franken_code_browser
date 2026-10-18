#![deny(unsafe_op_in_unsafe_fn)]

//! Thin marshaling for an explicitly prepared reusable capture index. Existing
//! search step/paging/overlay/focus/reader entrypoints consume progressive output.
use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasIndexOptions, MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use crate::atlas_sessions::IndexCommand;
use super::{answer, cstr, number, parse_path_scope, reply, AccessError};

/// Capture and index the frozen catalog to completion on a host worker.
/// Uses the same one-file pipeline as index_begin/index_step, without yielding.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_prepare(handle: u64, generation: u64, max_files: u64,
    max_file_bytes: u64, max_source_bytes: u64, max_index_grams: u64) -> *mut c_char {
    prepare(handle, generation, max_files, max_file_bytes, max_source_bytes, max_index_grams, false)
}
/// Admit a replacement index without source I/O. Use index_step, not search_step,
/// to capture/build one member. The previous accepted index remains queryable.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_begin(handle: u64, generation: u64, max_files: u64,
    max_file_bytes: u64, max_source_bytes: u64, max_index_grams: u64) -> *mut c_char {
    prepare(handle, generation, max_files, max_file_bytes, max_source_bytes, max_index_grams, true)
}
fn prepare(handle: u64, generation: u64, max_files: u64, max_file_bytes: u64,
    max_source_bytes: u64, max_index_grams: u64, progressive: bool) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let options = AtlasIndexOptions { max_files: number(max_files)?, max_file_bytes: number(max_file_bytes)?,
            max_source_bytes: number(max_source_bytes)?, max_index_grams: number(max_index_grams)? };
        let command = if progressive { IndexCommand::PrepareBegin { generation, options } }
            else { IndexCommand::Prepare { generation, options } };
        atlases.execute_index(handle, command, || false)
    }))
}
/// One bounded member per worker call. Terminal retries are read-only. Check
/// index_build_in_progress separately from capture_complete and gram coverage.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_step(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_index(handle, IndexCommand::PrepareStep { generation }, || false)))
}
/// Reconcile a known build without advancing it or rereading any source. Epoch
/// cancellation first retires obsolete provisional work on this worker.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_progress(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_index(handle, IndexCommand::PrepareProgress { generation }, || false)))
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
    unsafe { submit(handle, generation, index_generation, needle, std::ptr::null(), std::ptr::null(),
        max_matches, max_scan_bytes, false) }
}
/// Admit without verifying source. Continue with fcb_atlas_search_step on a
/// worker; each call visits at most one captured file. All normal search result
/// operations work between steps. The foreign query string is copied at begin.
/// # Safety
/// Same pointer and returned-string ownership contract as fcb_atlas_search_indexed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_begin(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, index_generation, needle, std::ptr::null(), std::ptr::null(),
        max_matches, max_scan_bytes, true) }
}

/// Finish an indexed literal query using a frozen native-path file-type scope.
/// Filtering precedes gram candidates, exact verification and occurrence limits.
/// # Safety
/// Pointer/list limits and ownership match fcb_atlas_search_scoped. Null/null
/// lists mean all; present empty lists select no files. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_scoped(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, extensions: *const c_char, filenames: *const c_char,
    max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, index_generation, needle, extensions, filenames,
        max_matches, max_scan_bytes, false) }
}

/// Admit a scoped indexed query without verifying source. Resume through the
/// ordinary search step. Captures/index identity stay pinned across all steps.
/// # Safety
/// Same pointer/list and returned-string contract as fcb_atlas_search_indexed_scoped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_begin_scoped(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, extensions: *const c_char, filenames: *const c_char,
    max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, index_generation, needle, extensions, filenames,
        max_matches, max_scan_bytes, true) }
}

/// Query retained captures within the exact path and file-type intersection.
/// The existing index is reused; excluded files spend no verification or match
/// quota. Selected catalog members absent from the index still remain partial.
/// # Safety
/// Same pointer/list/path limits and ownership as fcb_atlas_search_path_scoped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_path_scoped(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit_with_path(handle, generation, index_generation, needle, extensions, filenames,
        path_kind, path, max_matches, max_scan_bytes, false) }
}

/// Admit the same retained path/type query; resume with the existing search_step.
/// Input strings are copied at admission and need not remain alive afterward.
/// # Safety
/// Same pointer/list/path and returned-string contract as indexed_path_scoped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_begin_path_scoped(handle: u64, generation: u64, index_generation: u64,
    needle: *const c_char, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    unsafe { submit_with_path(handle, generation, index_generation, needle, extensions, filenames,
        path_kind, path, max_matches, max_scan_bytes, true) }
}
unsafe fn submit(handle: u64, generation: u64, index_generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char,
    max_matches: u64, max_scan_bytes: u64, progressive: bool) -> *mut c_char {
    unsafe { submit_with_path(handle, generation, index_generation, needle, extensions, filenames,
        0, std::ptr::null(), max_matches, max_scan_bytes, progressive) }
}
unsafe fn submit_with_path(handle: u64, generation: u64, index_generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    max_matches: u64, max_scan_bytes: u64, progressive: bool) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?;
        let max_matches = number(max_matches)?;
        let command = match (scope.is_all(), progressive) {
            (true, true) => IndexCommand::Begin { generation, index_generation, needle, max_matches, max_scan_bytes },
            (true, false) => IndexCommand::Query { generation, index_generation, needle, max_matches, max_scan_bytes },
            (false, true) => IndexCommand::BeginScoped { generation, index_generation, needle, scope: &scope, max_matches, max_scan_bytes },
            (false, false) => IndexCommand::QueryScoped { generation, index_generation, needle, scope: &scope, max_matches, max_scan_bytes },
        };
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
#[cfg(test)]
#[path = "atlas_index_build_ffi_tests.rs"]
mod build_tests;

#[path = "atlas_index_source_ffi.rs"]
mod source;
