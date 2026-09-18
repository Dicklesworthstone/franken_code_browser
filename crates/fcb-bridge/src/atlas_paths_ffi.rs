#![deny(unsafe_op_in_unsafe_fn)]

//! Metadata-only file finding and explicit path activation; no second ranker.
use std::ffi::c_char;
use fcb_app::host::atlas_paths::{AtlasPathOptions, PathCase, PathMatchMode,
    MAX_ATLAS_PATH_QUERY, MAX_ATLAS_PATH_RESULTS};
use crate::atlas_sessions::PathCommand;
use super::{answer, cstr, number, reply, AccessError};

fn options(limit: u64, mode: u8, case_mode: u8) -> Result<AtlasPathOptions, AccessError> {
    let max_results = number(limit)?;
    if !(1..=MAX_ATLAS_PATH_RESULTS).contains(&max_results) { return Err(AccessError::InvalidArgument); }
    Ok(AtlasPathOptions { max_results,
        mode: match mode { 0 => PathMatchMode::Fuzzy, 1 => PathMatchMode::Exact, 2 => PathMatchMode::Prefix,
            _ => return Err(AccessError::InvalidArgument) },
        case: match case_mode { 0 => PathCase::UnicodeLowercase, 1 => PathCase::Sensitive,
            _ => return Err(AccessError::InvalidArgument) } })
}
/// Find paths from the already-open atlas. Keys are prepared once, without
/// opening source files; results have an independent monotone query generation.
/// # Safety
/// query is null or readable NUL-terminated UTF-8 stable until return. Free each
/// returned non-null string once with this library's fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_find_files(handle: u64, generation: u64, query: *const c_char,
    max_results: u64, mode: u8, case_mode: u8) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let needle = unsafe { cstr(query) }.ok_or(AccessError::InvalidArgument)?;
        if needle.is_empty() || needle.len() > MAX_ATLAS_PATH_QUERY { return Err(AccessError::InvalidArgument); }
        atlases.execute_paths(handle, PathCommand::Find { generation, needle: needle.as_bytes(),
            options: options(max_results, mode, case_mode)? }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_file_results(handle: u64, generation: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_paths(handle,
        PathCommand::Page { generation, start: number(start)?, limit: number(limit)? }, || false)))
}
/// Pin a file identity, not a ranked row. Does not move camera or read source.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_file_select(handle: u64, generation: u64, file: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_paths(handle, PathCommand::Select { generation, file }, || false)))
}
/// Produces an unpresented plan; the ordinary atlas presentation protocol applies.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_file_focus(handle: u64, generation: u64, file: u64, plan_generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_paths(handle,
        PathCommand::Focus { generation, file, plan_generation }, || false)))
}
/// Open an explicitly chosen path as a NEW source observation into an EMPTY
/// existing reader handle. Does not accept native paths or display labels.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_file_open_reader(handle: u64, reader: u64, generation: u64,
    file: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        atlases.open_path_reader(handle, readers, reader, generation, file, number(max_source_bytes)?, || false)
    }))
}
/// Clear path results, not content-search captures or previously opened readers.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_file_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_paths(handle, PathCommand::Clear { generation }, || false)))
}

#[cfg(test)]
#[path = "atlas_paths_ffi_tests.rs"]
mod tests;
