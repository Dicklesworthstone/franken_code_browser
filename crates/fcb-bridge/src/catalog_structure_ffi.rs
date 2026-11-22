#![deny(unsafe_op_in_unsafe_fn)]

//! Thin foreign-pointer conversion for progressive qualified-name search.
//! Discovery, exact captured evidence and extraction remain application-owned.

use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, catalog_session::{CatalogStructureSearchMode,
    CatalogStructureSearchOptions, CatalogDiscoveryPolicy, MAX_CATALOG_STRUCTURE_HITS,
    MAX_CATALOG_STRUCTURE_QUERY_BYTES, MAX_CATALOG_STRUCTURE_READ_CALLS,
    MAX_CATALOG_STRUCTURE_SCAN_BYTES}};
use fcb_app::host::atlas_search::{AtlasSearchPathKind, AtlasSearchScope,
    MAX_SEARCH_SCOPE_LIST_BYTES, MAX_SEARCH_SCOPE_PATH_BYTES, MAX_SEARCH_REGION_WIRE_BYTES};
use fcb::search::SymbolNameMode;
use super::{cstr, reply, string_out};
use super::catalog_structure_sessions::{AccessError, CatalogStructureSessions, Command};

static SEARCHES: OnceLock<CatalogStructureSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&CatalogStructureSessions) -> Result<HostResponse, AccessError>)
    -> Option<*mut c_char> {
    let result = match SEARCHES.get() { Some(searches) => work(searches), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> {
    usize::try_from(value).map_err(|_| AccessError::InvalidArgument)
}

/// Reserve an empty handle without filesystem access.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_create() -> u64 {
    std::panic::catch_unwind(|| SEARCHES.get_or_init(CatalogStructureSessions::new).create().unwrap_or(0)).unwrap_or(0)
}

/// Fix a query and admit one metadata batch using repository rules. Source
/// capture starts on advance. Every integer/tag is validated before pointers.
/// # Safety
/// With valid scalars, root is null or NUL-terminated readable UTF-8, and query
/// is readable for exactly query_bytes. Scope strings are null or readable
/// through their NUL within the public bound plus one. Inputs stay stable until
/// return. A returned JSON buffer must be released with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_catalog_structure_open(handle: u64, root: *const c_char,
    generation: u64, query: *const u8, query_bytes: u64, mode: u64, name_mode: u64,
    start_after: u64, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, max_matches: u64, max_scan_bytes: u64,
    max_read_calls: u64) -> *mut c_char {
    unsafe { fcb_catalog_structure_open_with_policy(handle, root, generation, query, query_bytes,
        mode, name_mode, start_after, extensions, filenames, path_kind, path,
        max_matches, max_scan_bytes, max_read_calls, 0) }
}

/// Explicit immutable discovery policy: 0 repository rules, 1 includes excluded
/// regular files without reading rule configuration.
/// # Safety
/// Valid scalars obey fcb_catalog_structure_open's pointer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_catalog_structure_open_with_policy(handle: u64, root: *const c_char,
    generation: u64, query: *const u8, query_bytes: u64, mode: u64, name_mode: u64,
    start_after: u64, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, max_matches: u64, max_scan_bytes: u64,
    max_read_calls: u64, discovery_policy: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| {
        let policy = match discovery_policy {
            0 => CatalogDiscoveryPolicy::RepositoryRules, 1 => CatalogDiscoveryPolicy::IncludeExcluded,
            _ => return Err(AccessError::InvalidArgument),
        };
        let length = number(query_bytes)?;
        if generation == 0 || query.is_null() || length == 0 || length > MAX_CATALOG_STRUCTURE_QUERY_BYTES
            || !(1..=MAX_CATALOG_STRUCTURE_HITS as u64).contains(&max_matches)
            || !(1..=MAX_CATALOG_STRUCTURE_SCAN_BYTES).contains(&max_scan_bytes)
            || !(1..=MAX_CATALOG_STRUCTURE_READ_CALLS).contains(&max_read_calls)
            || start_after.checked_add(max_matches).and_then(|n| n.checked_add(1)).is_none() {
            return Err(AccessError::InvalidArgument);
        }
        let mode = match mode {
            0 => CatalogStructureSearchMode::Symbols, 1 => CatalogStructureSearchMode::Headings,
            _ => return Err(AccessError::InvalidArgument),
        };
        let name_mode = match name_mode {
            0 => SymbolNameMode::Exact, 1 => SymbolNameMode::Prefix, 2 => SymbolNameMode::Contains,
            _ => return Err(AccessError::InvalidArgument),
        };
        let kind = match path_kind {
            0 => AtlasSearchPathKind::Workspace, 1 => AtlasSearchPathKind::Directory,
            2 => AtlasSearchPathKind::File, 3 => AtlasSearchPathKind::Visible,
            _ => return Err(AccessError::InvalidArgument),
        };
        let query = unsafe { std::slice::from_raw_parts(query, length) };
        let query = std::str::from_utf8(query).map_err(|_| AccessError::InvalidArgument)?;
        let scope = unsafe { bounded_scope(extensions, filenames, kind, path) }?;
        let root = unsafe { cstr(root) }.ok_or(AccessError::InvalidArgument)?;
        let options = CatalogStructureSearchOptions { mode, name_mode, start_after, scope,
            max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        match policy {
            CatalogDiscoveryPolicy::RepositoryRules => searches.open(handle, Path::new(root), generation, query, options, || false),
            CatalogDiscoveryPolicy::IncludeExcluded => searches.open_with_policy(handle, Path::new(root), generation, query, options, policy, || false),
        }
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| searches.execute(handle, Command::Info, || false)))
}

/// At most sixteen bounded metadata/file-extraction steps. An accepted
/// predecessor retry replays progress without repeating I/O or counting.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_advance(handle: u64, generation: u64,
    after_revision: u64, max_steps: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| searches.execute(handle, Command::Advance {
        generation, after_revision, max_steps: number(max_steps)?,
    }, || false)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_page(handle: u64, generation: u64,
    revision: u64, start: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| searches.execute(handle, Command::Page {
        generation, revision, start: number(start)?, count: number(count)?,
    }, || false)))
}

/// Retained exact source selection; never reads the live pathname.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_hit(handle: u64, generation: u64,
    revision: u64, position: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| searches.execute(handle, Command::Hit {
        generation, revision, position: number(position)?,
    }, || false)))
}

/// Install a retained hit's complete capture in an empty reader handle. The
/// returned receipt contains the selected range, reader info and a source window.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_open_hit_reader(handle: u64, reader: u64,
    generation: u64, revision: u64, position: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| {
        let readers = super::reader_ffi::registry().ok_or(super::reader_sessions::AccessError::UnknownHandle)?;
        searches.open_hit_reader(handle, readers, reader, generation, revision, number(position)?, || false)
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_stop(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |searches| searches.execute(handle, Command::Stop { generation }, || false)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(SEARCHES.get().is_some_and(|searches| searches.cancel(handle).is_ok()))).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_structure_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(SEARCHES.get().is_some_and(|searches| searches.close(handle).is_ok()))).unwrap_or(0)
}

unsafe fn bounded_scope(extensions: *const c_char, filenames: *const c_char,
    kind: AtlasSearchPathKind, path: *const c_char) -> Result<AtlasSearchScope, AccessError> {
    let extensions = unsafe { bounded_optional(extensions, MAX_SEARCH_SCOPE_LIST_BYTES) }?;
    let filenames = unsafe { bounded_optional(filenames, MAX_SEARCH_SCOPE_LIST_BYTES) }?;
    let limit = if kind == AtlasSearchPathKind::Visible { MAX_SEARCH_REGION_WIRE_BYTES } else { MAX_SEARCH_SCOPE_PATH_BYTES };
    let path = unsafe { bounded_optional(path, limit) }?;
    if path.is_none() && kind != AtlasSearchPathKind::Workspace { return Err(AccessError::InvalidArgument); }
    AtlasSearchScope::from_csv(extensions, filenames)?.with_path(kind, path.unwrap_or("")).map_err(AccessError::from)
}
unsafe fn bounded_optional<'a>(pointer: *const c_char, limit: usize) -> Result<Option<&'a str>, AccessError> {
    if pointer.is_null() { return Ok(None); }
    for length in 0..=limit {
        if unsafe { *pointer.add(length) } == 0 {
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            return std::str::from_utf8(bytes).map(Some).map_err(|_| AccessError::InvalidArgument);
        }
    }
    Err(AccessError::InvalidArgument)
}

#[cfg(all(test, unix))]
#[path = "catalog_structure_ffi_tests.rs"]
mod tests;
