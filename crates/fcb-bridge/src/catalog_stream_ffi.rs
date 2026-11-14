#![deny(unsafe_op_in_unsafe_fn)]

//! Thin foreign-pointer conversion for progressive workspace content search.
//! The shared application service owns discovery, matching and source identity.

use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, catalog_session::{CatalogContentSearchMode,
    CatalogContentSearchOptions}};
use fcb_app::host::atlas_stream_search::{AtlasStreamSearchOptions,
    MAX_ATLAS_STREAM_SEARCH_HITS, MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES,
    MAX_ATLAS_STREAM_SEARCH_READ_CALLS, MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES};
use fcb_app::host::atlas_search::{AtlasSearchPathKind, AtlasSearchScope,
    MAX_SEARCH_SCOPE_LIST_BYTES, MAX_SEARCH_SCOPE_PATH_BYTES, MAX_SEARCH_REGION_WIRE_BYTES};
use super::{cstr, reply, string_out};
use super::catalog_stream_sessions::{AccessError, CatalogStreamSessions, Command};

static STREAMS: OnceLock<CatalogStreamSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&CatalogStreamSessions) -> Result<HostResponse, AccessError>)
    -> Option<*mut c_char> {
    let result = match STREAMS.get() { Some(streams) => work(streams), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> {
    usize::try_from(value).map_err(|_| AccessError::InvalidArgument)
}
fn boolean(value: u64) -> Result<bool, AccessError> {
    match value { 0 => Ok(false), 1 => Ok(true), _ => Err(AccessError::InvalidArgument) }
}

/// Reserve an empty, cancelable handle without filesystem access.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_create() -> u64 {
    std::panic::catch_unwind(|| STREAMS.get_or_init(CatalogStreamSessions::new).create().unwrap_or(0)).unwrap_or(0)
}

/// Fix one immutable query and admit its first metadata batch. No source content
/// is read until advance; info recovers an accepted query after a lost reply.
/// # Safety
/// When scalar inputs are valid, root is null or readable NUL-terminated UTF-8,
/// and query is readable for exactly query_bytes. Optional scope strings are null or
/// readable through their NUL, at most the public bound plus one byte. Inputs
/// remain stable until return. Returned JSON is owned; free with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_catalog_stream_open(handle: u64, root: *const c_char,
    generation: u64, query: *const u8, query_bytes: u64, mode: u64, case_sensitive: u64,
    count_all: u64, start_after: u64, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, max_matches: u64, max_scan_bytes: u64,
    max_read_calls: u64) -> *mut c_char {
    reply(|| answer(handle, |streams| {
        let length = number(query_bytes)?;
        if generation == 0 || query.is_null() || length == 0
            || length > MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES
            || !(1..=MAX_ATLAS_STREAM_SEARCH_HITS as u64).contains(&max_matches)
            || !(1..=MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES).contains(&max_scan_bytes)
            || !(1..=MAX_ATLAS_STREAM_SEARCH_READ_CALLS).contains(&max_read_calls) {
            return Err(AccessError::InvalidArgument);
        }
        let mode = match mode {
            0 => CatalogContentSearchMode::Text, 1 => CatalogContentSearchMode::Occurrences,
            2 => CatalogContentSearchMode::Expression, _ => return Err(AccessError::InvalidArgument),
        };
        let case_sensitive = boolean(case_sensitive)?;
        let count_all_matches = boolean(count_all)?;
        // Validate every scalar and path kind before reading any foreign string.
        let kind = match path_kind {
            0 => AtlasSearchPathKind::Workspace, 1 => AtlasSearchPathKind::Directory,
            2 => AtlasSearchPathKind::File, 3 => AtlasSearchPathKind::Visible,
            _ => return Err(AccessError::InvalidArgument),
        };
        let query = unsafe { std::slice::from_raw_parts(query, length) };
        let query = std::str::from_utf8(query).map_err(|_| AccessError::InvalidArgument)?;
        let scope = unsafe { bounded_scope(extensions, filenames, kind, path) }?
            .with_case_sensitive(case_sensitive);
        let root = unsafe { cstr(root) }.ok_or(AccessError::InvalidArgument)?;
        let options = CatalogContentSearchOptions { mode, count_all_matches, start_after, scope,
            limits: AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls } };
        streams.open(handle, Path::new(root), generation, query, options, || false)
    }))
}

/// Return retained query progress without discovery or source reads.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |streams| streams.execute(handle, Command::Info, || false)))
}

/// Process a bounded number of streaming/discovery steps. Retrying the accepted
/// predecessor revision replays retained progress instead of consuming work.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_advance(handle: u64, generation: u64,
    after_revision: u64, max_steps: u64) -> *mut c_char {
    reply(|| answer(handle, |streams| streams.execute(handle, Command::Advance {
        generation, after_revision, max_steps: number(max_steps)?,
    }, || false)))
}

/// Page only the current accepted query/revision, without source I/O.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_page(handle: u64, generation: u64,
    revision: u64, start: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |streams| streams.execute(handle, Command::Page {
        generation, revision, start: number(start)?, count: number(count)?,
    }, || false)))
}

/// Deliver one retained exact witness. Discovery-page eviction and later source
/// mutation cannot replace these independently owned original bytes.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_hit(handle: u64, generation: u64,
    revision: u64, position: u64) -> *mut c_char {
    reply(|| answer(handle, |streams| streams.execute(handle, Command::Hit {
        generation, revision, position: number(position)?,
    }, || false)))
}

/// Finish early after a bounded advance returns, keeping finalized witnesses
/// and discarding any unfinished file. The result explicitly reports user stop.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_stop(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |streams| streams.execute(handle, Command::Stop { generation }, || false)))
}

/// Nonblocking epoch cancellation. Interrupting an active shared matcher revokes
/// the query; use a fresh handle. Cancellation before work may remain recoverable.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(STREAMS.get().is_some_and(|streams| streams.cancel(handle).is_ok()))).unwrap_or(0)
}

/// Retire a handle. Entered work keeps its capacity permit until it drains.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stream_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(STREAMS.get().is_some_and(|streams| streams.close(handle).is_ok()))).unwrap_or(0)
}

// Reuse the shared immutable predicate. Invalid scopes never broaden into a
// whole-workspace query, and no scope parser reads repository source payloads.
unsafe fn bounded_scope(extensions: *const c_char, filenames: *const c_char,
    kind: AtlasSearchPathKind, path: *const c_char) -> Result<AtlasSearchScope, AccessError> {
    let extensions = unsafe { bounded_optional(extensions, MAX_SEARCH_SCOPE_LIST_BYTES) }?;
    let filenames = unsafe { bounded_optional(filenames, MAX_SEARCH_SCOPE_LIST_BYTES) }?;
    let limit = if kind == AtlasSearchPathKind::Visible { MAX_SEARCH_REGION_WIRE_BYTES }
        else { MAX_SEARCH_SCOPE_PATH_BYTES };
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
#[path = "catalog_stream_ffi_tests.rs"]
mod tests;
