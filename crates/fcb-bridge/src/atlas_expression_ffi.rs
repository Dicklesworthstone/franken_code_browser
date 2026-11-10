#![deny(unsafe_op_in_unsafe_fn)]

//! Expression admission only. The shared Rust query parser and existing atlas
//! search own grammar, matching, progressive work and retained hit authority.
use std::ffi::c_char;
use fcb::search::MAX_QUERY_LEN;
use crate::atlas_sessions::IndexCommand;
use super::{answer, number, parse_case_sensitive, parse_path_scope, reply,
    AccessError, AtlasSearchOptions, Command};

/// Admit a bounded expression over newly captured atlas sources. No source I/O
/// occurs until ordinary fcb_atlas_search_step calls. Use the existing page,
/// overlay, focus and exact captured-reader activation entrypoints afterward.
/// # Safety
/// expression is null or readable until NUL or 1025 bytes, whichever is first.
/// Other non-null strings obey the existing matching scope/path contracts and
/// are readable NUL-terminated UTF-8 until return. No foreign borrow survives.
/// Every non-null response is owned and must be freed once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_begin_expression(handle: u64, generation: u64,
    expression: *const c_char, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, case_sensitive: u64, max_matches: u64,
    max_files: u64, max_file_bytes: u64, max_source_bytes: u64, max_scan_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let expression = unsafe { bounded_expression(expression) }?;
        let case_sensitive = parse_case_sensitive(case_sensitive)?;
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive);
        let options = AtlasSearchOptions { max_matches: number(max_matches)?, max_files: number(max_files)?,
            max_file_bytes: number(max_file_bytes)?, max_source_bytes: number(max_source_bytes)? };
        atlases.execute(handle, Command::SearchBeginExpression { generation, expression,
            scope: &scope, options, max_scan_bytes }, || false)
    }))
}

/// Query an accepted prepared index's retained source captures. No live source
/// pathname is reopened. Expression predicates are verified before hits receive
/// ordinary atlas-search activation authority; gram absence is not a substitute
/// for conjunction/exclusion or Unicode matching proof.
/// # Safety
/// Same pointers, workers and owned-response contract as begin_expression.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_indexed_begin_expression(handle: u64, generation: u64,
    index_generation: u64, expression: *const c_char, extensions: *const c_char, filenames: *const c_char,
    path_kind: u64, path: *const c_char, case_sensitive: u64,
    max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let expression = unsafe { bounded_expression(expression) }?;
        let case_sensitive = parse_case_sensitive(case_sensitive)?;
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?
            .with_case_sensitive(case_sensitive);
        atlases.execute_index(handle, IndexCommand::BeginExpression { generation, index_generation,
            expression, scope: &scope, max_matches: number(max_matches)?, max_scan_bytes }, || false)
    }))
}

// Only foreign string admission lives here. Empty/invalid syntax reaches the
// shared parser so callers receive its specific QUERY_* diagnostic.
unsafe fn bounded_expression<'a>(pointer: *const c_char) -> Result<&'a str, AccessError> {
    if pointer.is_null() { return Err(AccessError::InvalidArgument); }
    for length in 0..=MAX_QUERY_LEN {
        if unsafe { *pointer.add(length) } == 0 {
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            return std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument);
        }
    }
    Err(AccessError::InvalidArgument)
}

#[cfg(all(test, unix))]
#[path = "atlas_expression_ffi_tests.rs"]
mod tests;
