#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling for workspace symbol/heading queries. The safe app service owns
//! extraction, source retention, work budgets and evidence qualification.

use fcb::search::SymbolNameMode;
use fcb_app::host::atlas_search::{AtlasStructureMode, AtlasStructureOptions};
use super::{answer, cstr, number, parse_path_scope, reply, AccessError, AtlasSearchOptions,
    Command, MAX_ATLAS_SEARCH_NEEDLE_BYTES, c_char};

/// Admit structural-name search over the atlas's frozen catalog. mode=1 symbols,
/// 2 Markdown headings. name_mode=0 exact, 1 prefix, 2 contains (case-sensitive).
/// All other modes are refused. The existing step/page/clear/open_reader calls
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

#[cfg(all(test, unix))]
#[path = "atlas_structure_ffi_tests.rs"]
mod tests;
