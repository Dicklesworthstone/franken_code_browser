#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer conversion for saved declaration, heading and identifier searches.
//! Extraction, archive verification and result ownership stay in the safe host.

use fcb_app::host::saved_repository::structure::{StructureMode, SymbolNameMode};
use super::{AccessError, Command, Selection, answer, c_char, cstr, number, reply};

/// Synchronous worker search over verified members of the opened archive.
/// mode=1 symbols, 2 Markdown headings, 4 whole-token occurrences; name_mode=0
/// exact, 1 prefix, 2 contains. Occurrences require exact matching. Structural
/// attempts/results are independent of literal, expression and index state.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. The
/// pointer is never retained. Free every non-null response with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_structure_search(handle: u64, generation: u64,
    mode: u64, name_mode: u64, needle: *const c_char,
    max_hits: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let mode = match mode {
            1 => StructureMode::Symbols,
            2 => StructureMode::Headings,
            4 => StructureMode::Occurrences,
            _ => return Err(AccessError::InvalidArgument),
        };
        let name_mode = match name_mode {
            0 => SymbolNameMode::Exact,
            1 => SymbolNameMode::Prefix,
            2 => SymbolNameMode::Contains,
            _ => return Err(AccessError::InvalidArgument),
        };
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        sessions.execute(handle, Command::StructureSearch { generation, mode, name_mode,
            needle, max_hits: number(max_hits)?, max_source_bytes }, || false)
    }))
}

/// Retain a later bounded structural-result window under a fresh generation.
/// skip_matches counts qualified candidates across verified members, including
/// metadata omissions; next_match_offset is the authoritative later window.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return.
/// Release every non-null response exactly once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_structure_search_page(handle: u64, generation: u64,
    mode: u64, name_mode: u64, needle: *const c_char,
    max_hits: u64, max_source_bytes: u64, skip_matches: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let mode = match mode {
            1 => StructureMode::Symbols,
            2 => StructureMode::Headings,
            4 => StructureMode::Occurrences,
            _ => return Err(AccessError::InvalidArgument),
        };
        let name_mode = match name_mode {
            0 => SymbolNameMode::Exact,
            1 => SymbolNameMode::Prefix,
            2 => SymbolNameMode::Contains,
            _ => return Err(AccessError::InvalidArgument),
        };
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        sessions.execute(handle, Command::StructureSearchPage { generation, mode, name_mode,
            needle, max_hits: number(max_hits)?, max_source_bytes, skip_matches }, || false)
    }))
}

/// Explicit name-case and count policy without changing the legacy ABI.
/// Flags are exactly 0 or 1. Unicode scalar lowercase is supported for symbol
/// and heading names; whole-token occurrences require case_sensitive=1.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return.
/// Release every non-null response exactly once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_structure_search_matching(handle: u64, generation: u64,
    mode: u64, name_mode: u64, needle: *const c_char, case_sensitive: u64,
    count_all_matches: u64, max_hits: u64, max_source_bytes: u64) -> *mut c_char {
    unsafe { fcb_saved_structure_search_matching_page(handle, generation, mode, name_mode,
        needle, case_sensitive, count_all_matches, max_hits, max_source_bytes, 0) }
}

/// Retain a later window under the same explicit matching and count policy.
/// Counted matches after the retained cap do not move next_match_offset past
/// the first unretained candidate. Zero retained rows counts without witnesses
/// when count_all_matches=1; otherwise it probes for existence.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return.
/// Release every non-null response exactly once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_structure_search_matching_page(handle: u64, generation: u64,
    mode: u64, name_mode: u64, needle: *const c_char, case_sensitive: u64,
    count_all_matches: u64, max_hits: u64, max_source_bytes: u64, skip_matches: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let mode = match mode {
            1 => StructureMode::Symbols,
            2 => StructureMode::Headings,
            4 => StructureMode::Occurrences,
            _ => return Err(AccessError::InvalidArgument),
        };
        let name_mode = match name_mode {
            0 => SymbolNameMode::Exact,
            1 => SymbolNameMode::Prefix,
            2 => SymbolNameMode::Contains,
            _ => return Err(AccessError::InvalidArgument),
        };
        let case_sensitive = match case_sensitive {
            0 => false, 1 => true, _ => return Err(AccessError::InvalidArgument),
        };
        let count_all_matches = match count_all_matches {
            0 => false, 1 => true, _ => return Err(AccessError::InvalidArgument),
        };
        if mode == StructureMode::Occurrences && !case_sensitive {
            return Err(AccessError::InvalidArgument);
        }
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        sessions.execute(handle, Command::StructureSearchMatching { generation, mode, name_mode,
            needle, max_hits: number(max_hits)?, max_source_bytes, skip_matches,
            case_sensitive, count_all_matches }, || false)
    }))
}

/// Page retained structural metadata without rereading archived source.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_structure_results(handle: u64, generation: u64,
    start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::StructureResults { generation, start: number(start)?, limit: number(limit)? }, || false)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_clear_structure(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::ClearStructure { generation }, || false)))
}

/// Reverify the whole selected captured member before installing an EMPTY
/// reader. Structural hit IDs belong only to the saved-structure namespace.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_open_structure_hit_reader(handle: u64, reader: u64,
    generation: u64, hit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader,
            Selection::StructureHit { generation, id: hit }, || false)
    }))
}

#[cfg(all(test, unix))]
#[path = "saved_structure_ffi_tests.rs"]
mod tests;
