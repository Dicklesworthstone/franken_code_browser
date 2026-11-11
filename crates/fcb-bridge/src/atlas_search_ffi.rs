#![deny(unsafe_op_in_unsafe_fn)]

//! C surface for captured repository results attached to existing atlas handles.
//! No source policy, matcher, worker pool or alternate reader lives in this bridge.
use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasSearchOptions, AtlasSearchScope, AtlasSearchPathKind,
    MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use super::{answer, cstr, number, reply, AccessError, Command};

/// Search the frozen atlas catalog to its terminal state and return first-page
/// JSON. Use a fresh query generation for every attempt, including failed ones.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. Returned
/// strings must be freed once with this library's fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, needle, std::ptr::null(), std::ptr::null(),
        max_matches, max_files, max_file_bytes, max_source_bytes, false) }
}

/// Admit a resumable query without reading any source. Copies the bounded query
/// text; the input pointer need not survive this call. Call step on a host worker
/// and use search_in_progress, not search_complete, to decide whether to resume.
/// # Safety
/// Same input and returned-string ownership contract as fcb_atlas_search.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, needle, std::ptr::null(), std::ptr::null(),
        max_matches, max_files, max_file_bytes, max_source_bytes, true) }
}

/// Finish a literal query restricted before source, file and occurrence quotas.
/// Null/null lists mean all; a non-null empty list means explicit empty scope.
/// # Safety
/// All non-null strings must be readable NUL-terminated UTF-8 until return.
/// Lists contain at most 1024 bytes / 32 tokens of 1..64 ASCII bytes. Extensions
/// use alphanumerics, _, +, -; basenames also permit dots, except "." and "..".
/// Input ownership and result freeing match fcb_atlas_search. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_scoped(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, max_matches: u64, max_files: u64,
    max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, needle, extensions, filenames,
        max_matches, max_files, max_file_bytes, max_source_bytes, false) }
}

/// Admit a frozen file-type scope. Progress uses the ordinary search_step/page;
/// scope changes consume new generations and cannot reuse narrowed candidates.
/// # Safety
/// Same pointer/list and returned-string contract as fcb_atlas_search_scoped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin_scoped(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, max_matches: u64, max_files: u64,
    max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit(handle, generation, needle, extensions, filenames,
        max_matches, max_files, max_file_bytes, max_source_bytes, true) }
}

/// Search a frozen directory or exact-file intersection with the file types.
/// path_kind is 0=workspace, 1=directory, 2=file, 3=frozen visible files.
/// Visible paths use a canonical sorted raw-byte hex CSV; other paths use exact UTF-8 native
/// bytes, <=4096 bytes, relative components without empty, dot or dotdot parts.
/// Directory paths omit a trailing slash; matching includes its descendants.
/// Workspace requires a null/empty path. This predicate never grants access.
/// # Safety
/// Every non-null string is readable NUL-terminated UTF-8 until return. Existing
/// scoped-list limits, worker restriction and returned-string ownership apply.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_path_scoped(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit_with_path(handle, generation, needle, extensions, filenames, path_kind, path,
        max_matches, max_files, max_file_bytes, max_source_bytes, false) }
}

/// Admit the same immutable path/type intersection without source I/O. The
/// ordinary search_step/page/overlay/activation routes retain its exact scope.
/// # Safety
/// Same pointer, path/list, worker and returned-string contract as path_scoped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin_path_scoped(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { submit_with_path(handle, generation, needle, extensions, filenames, path_kind, path,
        max_matches, max_files, max_file_bytes, max_source_bytes, true) }
}

/// Admit a literal query with frozen case semantics, path and file-type scope.
/// case_sensitive is 1 for exact spelling or 0 for Unicode lowercase matching;
/// other values are invalid. This is not full Unicode case folding or canonical
/// normalization. Resume through the ordinary step/page/activation operations.
/// # Safety
/// Same pointer/list/path and returned-string contract as begin_path_scoped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin_matching(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    case_sensitive: u64, max_matches: u64, max_files: u64, max_file_bytes: u64,
    max_source_bytes: u64) -> *mut c_char {
    unsafe { submit_matching(handle, generation, needle, extensions, filenames, path_kind, path,
        case_sensitive, max_matches, max_files, max_file_bytes, max_source_bytes, true) }
}

/// Admit an exact-case literal query that keeps scanning/counting after its
/// retained hit table fills. Other matching modes keep their existing contracts.
/// # Safety
/// Pointer/list/path ownership matches fcb_atlas_search_begin_matching.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin_counted_matching(handle: u64, generation: u64,
    needle: *const c_char, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, case_sensitive: u64,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        if !parse_case_sensitive(case_sensitive)? { return Err(AccessError::InvalidArgument); }
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES {
            return Err(AccessError::InvalidArgument);
        }
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(true);
        let options = AtlasSearchOptions { max_matches: number(max_matches)?, max_files: number(max_files)?,
            max_file_bytes: number(max_file_bytes)?, max_source_bytes: number(max_source_bytes)? };
        atlases.execute(handle, Command::SearchBeginCountedScoped { generation, needle, scope: &scope, options }, || false)
    }))
}

fn parse_case_sensitive(value: u64) -> Result<bool, AccessError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(AccessError::InvalidArgument),
    }
}

unsafe fn parse_scope(extensions: *const c_char, filenames: *const c_char) -> Result<AtlasSearchScope, AccessError> {
    let extensions = if extensions.is_null() { None }
        else { Some(unsafe { cstr(extensions) }.ok_or(AccessError::InvalidArgument)?) };
    let filenames = if filenames.is_null() { None }
        else { Some(unsafe { cstr(filenames) }.ok_or(AccessError::InvalidArgument)?) };
    AtlasSearchScope::from_csv(extensions, filenames).map_err(AccessError::from)
}

unsafe fn parse_path_scope(extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char) -> Result<AtlasSearchScope, AccessError> {
    let kind = match path_kind { 0 => AtlasSearchPathKind::Workspace,
        1 => AtlasSearchPathKind::Directory, 2 => AtlasSearchPathKind::File,
        3 => AtlasSearchPathKind::Visible,
        _ => return Err(AccessError::InvalidArgument) };
    let path = if path.is_null() { "" }
        else { unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)? };
    unsafe { parse_scope(extensions, filenames) }?.with_path(kind, path).map_err(AccessError::from)
}

// Marshaling only: both public entrypoints use the same safe application engine.
unsafe fn submit(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64,
    progressive: bool) -> *mut c_char {
    unsafe { submit_with_path(handle, generation, needle, extensions, filenames, 0, std::ptr::null(),
        max_matches, max_files, max_file_bytes, max_source_bytes, progressive) }
}
unsafe fn submit_with_path(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64,
    progressive: bool) -> *mut c_char {
    unsafe { submit_matching(handle, generation, needle, extensions, filenames, path_kind, path,
        1, max_matches, max_files, max_file_bytes, max_source_bytes, progressive) }
}
unsafe fn submit_matching(handle: u64, generation: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    case_sensitive: u64, max_matches: u64, max_files: u64, max_file_bytes: u64,
    max_source_bytes: u64, progressive: bool) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let case_sensitive = parse_case_sensitive(case_sensitive)?;
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive);
        let options = AtlasSearchOptions { max_matches: number(max_matches)?, max_files: number(max_files)?,
            max_file_bytes: number(max_file_bytes)?, max_source_bytes: number(max_source_bytes)? };
        // The compatibility variants construct their own exact-case scope.
        // Broad membership must not discard an insensitive query's semantics.
        let command = match (scope.is_all() && case_sensitive, progressive) {
            (true, true) => Command::SearchBegin { generation, needle, options },
            (true, false) => Command::Search { generation, needle, options },
            (false, true) => Command::SearchBeginScoped { generation, needle, scope: &scope, options },
            (false, false) => Command::SearchScoped { generation, needle, scope: &scope, options },
        };
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
/// readers and an explicitly prepared reusable index retain their own bytes.
/// This consumes a fresh query generation.
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

#[path = "atlas_index_ffi.rs"]
mod index;
#[path = "atlas_structure_ffi.rs"]
mod structure;
#[path = "atlas_expression_ffi.rs"]
mod expression;
#[path = "atlas_links_ffi.rs"]
mod links;

#[cfg(all(test, unix))]
#[path = "atlas_search_ffi_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "atlas_case_ffi_tests.rs"]
mod case_tests;
