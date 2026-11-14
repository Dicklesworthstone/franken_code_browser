#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling for bounded whole-file search. It deliberately has no captured
//! reader activation: a retained literal is not an immutable whole source.

use std::ffi::{c_char, c_void};
use fcb_app::host::atlas_stream_search::{AtlasStreamSearchOptions,
    MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES};
use fcb_app::host::atlas_search::{AtlasSearchPathKind, AtlasSearchScope,
    MAX_SEARCH_SCOPE_LIST_BYTES, MAX_SEARCH_SCOPE_PATH_BYTES, MAX_SEARCH_REGION_WIRE_BYTES};
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

/// Search a frozen file-type and path intersection of the base catalog. Scope
/// exclusion occurs before any source open or scan quota; original IDs survive.
/// # Safety
/// The legacy needle/callback requirements apply. Scope lists are null or
/// readable through NUL or 1025 bytes; path is null or readable through NUL or
/// its declared allowance plus one (4096 bytes for ordinary paths, the public
/// MAX_SEARCH_REGION_WIRE_BYTES for visible-region CSV). Strings remain stable
/// until return. Both null lists mean all types; a present empty list means no
/// types. path_kind is 0 workspace, 1 directory, 2 file, 3 visible. Only workspace
/// accepts null path; visible uses canonical sorted lowercase raw-path hex CSV.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_cancelable(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        let needle = unsafe { bounded_needle(needle) }?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?;
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream(handle, StreamCommand::SearchScoped { needle, options, scope: &scope }, &mut canceled)
    }))
}

/// Borrowed UTF-8 bytes valid only during this synchronous callback. They are
/// length-delimited, not NUL terminated, and must never be freed by the caller.
pub type AtlasStreamProgressCallback = unsafe extern "C" fn(*const u8, u64, *mut c_void);

/// Search with provisional first-page progress and a distinct Stop-and-keep
/// poll. Nonzero stop accepts finalized-file results as explicitly partial;
/// nonzero poll cancels delivery and preserves the previous accepted inventory.
/// # Safety
/// The needle and cancellation requirements match the cancelable entry point.
/// All callbacks/context must remain valid until return and must not unwind,
/// reenter this session, or retain borrowed pointers. Copy progress bytes within
/// the callback. Only the final non-null return is freed with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        // A foreign poll may signal once. Every observation revokes the whole
        // entered operation, including the extra check before callback delivery.
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        let needle = unsafe { bounded_needle(needle) }?;
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?,
            max_scan_bytes, max_read_calls };
        atlases.execute_stream_with_progress(handle, needle, options, &canceled,
            || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                // A callback may revoke delivery. The host polls again before
                // another source quantum or terminal inventory publication.
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Scoped progress uses the same frozen membership for previews, terminal
/// acceptance, pages and witnesses. Scope changes require a fresh search.
/// # Safety
/// String requirements match fcb_atlas_stream_search_scoped_cancelable;
/// callback requirements match fcb_atlas_stream_search_progress. Progress bytes
/// remain borrowed only during the callback and cannot authorize activation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        let needle = unsafe { bounded_needle(needle) }?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?;
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_with_progress(handle, needle, options, &scope, &canceled,
            || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Count all occurrences within the scan budgets while retaining at most
/// max_matches rows. Callbacks are optional, including progress and Stop.
/// # Safety
/// Needle/callback/return ownership requirements match
/// fcb_atlas_stream_search_progress. No foreign pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_counted_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    unsafe { fcb_atlas_stream_search_scoped_counted_progress(handle, needle, max_matches, max_scan_bytes,
        max_read_calls, std::ptr::null(), std::ptr::null(), 0, std::ptr::null(), poll, stop, progress, context) }
}

/// Count all scoped occurrences without expanding the retained row/witness
/// buffers. Scope, cancellation and Stop have the existing progress contract.
/// # Safety
/// String/callback/return ownership requirements match
/// fcb_atlas_stream_search_scoped_progress. All callbacks are optional.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_counted_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        let needle = unsafe { bounded_needle(needle) }?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?;
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_counted_with_progress(handle, needle, options, &scope, &canceled,
            || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Frozen scope and matching/count policies over a continuous file observation.
/// case_sensitive and count_all_matches must be 0 or 1. Unicode lowercase
/// witnesses preserve each original spelling, including partial expansions.
/// All callbacks may be null for a synchronous terminal response.
/// # Safety
/// String, callback and return ownership requirements match
/// fcb_atlas_stream_search_scoped_progress. No foreign pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_matching_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    case_sensitive: u64, count_all_matches: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        if case_sensitive > 1 || count_all_matches > 1 { return Err(AccessError::InvalidArgument); }
        let needle = unsafe { bounded_needle(needle) }?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive == 1);
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_matching_with_progress(handle, needle, options, &scope, count_all_matches == 1,
            &canceled, || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Evaluate a bounded decoded-text expression over each complete observed
/// file stream. The host owns parsing, metadata filters and same-file content
/// predicates. All qualified primary matches are counted within the I/O budget;
/// max_matches limits retained rows only. case_sensitive must be 0 or 1.
/// # Safety
/// expression is null or readable through NUL or 1025 bytes, whichever comes
/// first, and remains stable until return. Scope strings, callbacks and return
/// ownership follow fcb_atlas_stream_search_scoped_progress. No foreign pointer
/// is retained. Progress cannot authorize activation before terminal acceptance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_expression_progress(handle: u64,
    expression: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    case_sensitive: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        if case_sensitive > 1 { return Err(AccessError::InvalidArgument); }
        // Empty and malformed expressions belong to the shared query parser;
        // foreign null, invalid UTF-8 and overlong strings are ABI errors.
        let expression = unsafe { bounded_optional(expression, MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES) }?
            .ok_or(AccessError::InvalidArgument)?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive == 1);
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_expression_with_progress(handle, expression, options, &scope,
            &canceled, || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Rescan a bounded expression and retain primary matches after start_after
/// qualified occurrences. The skipped prefix consumes the same I/O budget;
/// required/excluded content is evaluated through each observed file's end.
/// Every attempt has fresh identities and counts all qualified matches. The
/// offset does not authorize combining separate live observations.
/// # Safety
/// String, callback and return ownership requirements match
/// fcb_atlas_stream_search_scoped_expression_progress. No pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_expression_page_progress(handle: u64,
    expression: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    case_sensitive: u64, start_after: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        if case_sensitive > 1 { return Err(AccessError::InvalidArgument); }
        let expression = unsafe { bounded_optional(expression, MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES) }?
            .ok_or(AccessError::InvalidArgument)?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive == 1);
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_expression_page_with_progress(handle, expression, options, &scope, start_after,
            &canceled, || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Exact whole-identifier occurrences with bounded progress and independent
/// count/row limits. Comments and literals are included; no compiler resolution
/// is implied. Identifier syntax and token boundaries belong to the shared
/// engine. All callbacks are optional for a terminal-only response.
/// # Safety
/// String, callback and return ownership requirements match
/// fcb_atlas_stream_search_scoped_progress. No foreign pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_occurrences_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    count_all_matches: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        if count_all_matches > 1 { return Err(AccessError::InvalidArgument); }
        let needle = unsafe { bounded_needle(needle) }?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?;
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_occurrences_with_progress(handle, needle, options, &scope, count_all_matches == 1,
            &canceled, || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
    }))
}

/// Scan a new live observation and retain matches after start_after occurrences
/// across eligible files in catalog order. This is not immutable continuation:
/// edits can change ordinal membership. Query/source revisions are fresh; all
/// skipped source bytes and calls consume the normal global scan limits.
/// case_sensitive, count_all_matches and occurrences must be 0 or 1. Whole
/// identifier occurrences require case_sensitive = 1. Progress/terminal replies
/// echo page_start_after, skipped_matches and page_has_more_matches.
/// # Safety
/// String, callback and return ownership requirements match
/// fcb_atlas_stream_search_scoped_progress. No foreign pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_stream_search_scoped_page_progress(handle: u64,
    needle: *const c_char, max_matches: u64, max_scan_bytes: u64, max_read_calls: u64,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64, path: *const c_char,
    case_sensitive: u64, count_all_matches: u64, occurrences: u64, start_after: u64,
    poll: Option<SearchCancellationCallback>, stop: Option<SearchCancellationCallback>,
    progress: Option<AtlasStreamProgressCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let cancellation_seen = std::cell::Cell::new(false);
        let canceled = || {
            let requested = cancellation_seen.get()
                || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
            cancellation_seen.set(requested);
            requested
        };
        if canceled() { return Err(AccessError::Canceled); }
        if case_sensitive > 1 || count_all_matches > 1 || occurrences > 1
            || (occurrences == 1 && case_sensitive == 0) {
            return Err(AccessError::InvalidArgument);
        }
        let needle = unsafe { bounded_needle(needle) }?;
        let scope = unsafe { bounded_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive == 1);
        let options = AtlasStreamSearchOptions { max_matches: number(max_matches)?, max_scan_bytes, max_read_calls };
        atlases.execute_stream_scoped_page_with_progress(handle, needle, options, &scope, start_after,
            count_all_matches == 1, occurrences == 1,
            &canceled, || stop.is_some_and(|stop| unsafe { stop(context) != 0 }),
            |response| {
                if canceled() { return; }
                if let Some(progress) = progress {
                    let bytes = response.as_str().as_bytes();
                    unsafe { progress(bytes.as_ptr(), bytes.len() as u64, context) };
                }
            })
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

// Marshaling only: the existing shared scope parser owns validation, canonical
// tokens, raw region membership and path semantics. Never widen invalid input.
unsafe fn bounded_scope(extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char) -> Result<AtlasSearchScope, AccessError> {
    let kind = match path_kind {
        0 => AtlasSearchPathKind::Workspace, 1 => AtlasSearchPathKind::Directory,
        2 => AtlasSearchPathKind::File, 3 => AtlasSearchPathKind::Visible,
        _ => return Err(AccessError::InvalidArgument),
    };
    let extensions = unsafe { bounded_optional(extensions, MAX_SEARCH_SCOPE_LIST_BYTES) }?;
    let filenames = unsafe { bounded_optional(filenames, MAX_SEARCH_SCOPE_LIST_BYTES) }?;
    let allowance = if kind == AtlasSearchPathKind::Visible { MAX_SEARCH_REGION_WIRE_BYTES } else { MAX_SEARCH_SCOPE_PATH_BYTES };
    let path = unsafe { bounded_optional(path, allowance) }?;
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
#[path = "atlas_stream_ffi_tests.rs"]
mod tests;
