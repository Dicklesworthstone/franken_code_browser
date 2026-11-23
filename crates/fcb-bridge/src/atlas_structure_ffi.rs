#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling for workspace declaration, heading and whole-token occurrence queries. The safe app service owns
//! extraction, source retention, work budgets and evidence qualification.

use fcb::search::SymbolNameMode;
use fcb_app::host::atlas_search::{AtlasStructureMode, AtlasStructureOptions, AtlasIndexedStructureOptions,
    AtlasResultWindow};
use super::{answer, cstr, number, parse_path_scope, parse_case_sensitive, reply, AccessError, AtlasSearchOptions,
    Command, MAX_ATLAS_SEARCH_NEEDLE_BYTES, c_char};
use crate::atlas_sessions::IndexCommand;

/// Admit structural-name search over the atlas's frozen catalog. mode=1 symbols,
/// 2 Markdown headings, 4 whole-token occurrences. name_mode=0 exact, 1 prefix,
/// 2 contains (case-sensitive); occurrences require exact mode and a valid
/// shared-engine token name. All other modes are refused. The existing step/page/clear/open_reader calls
/// operate on this query, including cooperative cancellation during step.
/// # Safety
/// Input strings are null or readable NUL-terminated UTF-8, stable until return.
/// needle is required and nonempty. Scope pointers follow atlas_search_scoped's
/// exact contract. Inputs are copied, never retained. Free the returned string
/// once with fcb_free_string. Call on a worker; no parser runs on admission.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_structure_begin(handle: u64, generation: u64,
    mode: u64, name_mode: u64, needle: *const c_char, extensions: *const c_char,
    filenames: *const c_char, path_kind: u64, path: *const c_char,
    max_matches: u64, max_files: u64, max_file_bytes: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mode = match mode { 1 => AtlasStructureMode::Symbols, 2 => AtlasStructureMode::Headings,
            4 => AtlasStructureMode::Occurrences,
            _ => return Err(AccessError::InvalidArgument) };
        let name_mode = match name_mode { 0 => SymbolNameMode::Exact, 1 => SymbolNameMode::Prefix,
            2 => SymbolNameMode::Contains, _ => return Err(AccessError::InvalidArgument) };
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?;
        let limits = AtlasSearchOptions { max_matches: number(max_matches)?, max_files: number(max_files)?,
            max_file_bytes: number(max_file_bytes)?, max_source_bytes: number(max_source_bytes)? };
        atlases.execute(handle, Command::StructureBegin { generation, needle, scope: &scope,
            options: AtlasStructureOptions { mode, name_mode, limits } }, || false)
    }))
}

/// Admit the same structural extraction over an immutable index capture set.
/// mode/name_mode and scope follow fcb_atlas_structure_begin. max_scan_bytes
/// bounds whole extraction inputs (0..32 MiB), not source I/O. max_matches is
/// 1..4096. No live source path is reopened, and missing captures stay pending.
/// # Safety
/// Input strings are null or readable NUL-terminated UTF-8, stable until return.
/// needle is required. Inputs are copied, never retained. The caller must free
/// the returned string once with fcb_free_string and call this on a worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_structure_indexed_begin(handle: u64, generation: u64,
    index_generation: u64, mode: u64, name_mode: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64,
    path: *const c_char, max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mode = match mode { 1 => AtlasStructureMode::Symbols, 2 => AtlasStructureMode::Headings,
            4 => AtlasStructureMode::Occurrences, _ => return Err(AccessError::InvalidArgument) };
        let name_mode = match name_mode { 0 => SymbolNameMode::Exact, 1 => SymbolNameMode::Prefix,
            2 => SymbolNameMode::Contains, _ => return Err(AccessError::InvalidArgument) };
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES { return Err(AccessError::InvalidArgument); }
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?;
        let options = AtlasIndexedStructureOptions { mode, name_mode,
            max_matches: number(max_matches)?, max_scan_bytes };
        atlases.execute_index(handle, IndexCommand::BeginStructure { generation, index_generation,
            needle, scope: &scope, options }, || false)
    }))
}

/// Admit a bounded result window over the same retained structural sources.
/// result_offset skips matching occurrences, not source files or retained rows.
/// count_all_matches is 0/1; counting continues under the same scan allowance
/// after the window fills. All work uses the existing step/page/reader route.
/// # Safety
/// needle is readable through NUL or 1025 bytes, whichever comes first, and
/// must contain nonempty UTF-8 within 1024 bytes. Scope pointers follow
/// fcb_atlas_structure_indexed_begin. No input borrow survives this worker call;
/// free every non-null returned response once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_structure_indexed_begin_window(handle: u64, generation: u64,
    index_generation: u64, mode: u64, name_mode: u64, needle: *const c_char,
    extensions: *const c_char, filenames: *const c_char, path_kind: u64,
    path: *const c_char, count_all_matches: u64, result_offset: u64,
    max_matches: u64, max_scan_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mode = match mode { 1 => AtlasStructureMode::Symbols, 2 => AtlasStructureMode::Headings,
            4 => AtlasStructureMode::Occurrences, _ => return Err(AccessError::InvalidArgument) };
        let name_mode = match name_mode { 0 => SymbolNameMode::Exact, 1 => SymbolNameMode::Prefix,
            2 => SymbolNameMode::Contains, _ => return Err(AccessError::InvalidArgument) };
        let count_all_matches = parse_case_sensitive(count_all_matches)?;
        let needle = unsafe { bounded_needle(needle) }?;
        if needle.is_empty() { return Err(AccessError::InvalidArgument); }
        let scope = unsafe { parse_path_scope(extensions, filenames, path_kind, path) }?;
        let options = AtlasIndexedStructureOptions { mode, name_mode,
            max_matches: number(max_matches)?, max_scan_bytes };
        let window = AtlasResultWindow { result_offset, count_all_matches };
        atlases.execute_index(handle, IndexCommand::BeginStructureWindow { generation, index_generation,
            needle, scope: &scope, options, window }, || false)
    }))
}

unsafe fn bounded_needle<'a>(pointer: *const c_char) -> Result<&'a str, AccessError> {
    if pointer.is_null() { return Err(AccessError::InvalidArgument); }
    for length in 0..=MAX_ATLAS_SEARCH_NEEDLE_BYTES {
        if unsafe { *pointer.add(length) } == 0 {
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            return std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument);
        }
    }
    Err(AccessError::InvalidArgument)
}

#[cfg(all(test, unix))]
#[path = "atlas_structure_ffi_tests.rs"]
mod tests;
