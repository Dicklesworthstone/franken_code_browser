#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling for captured-source import navigation. Extraction, closed-catalog
//! resolution, evidence ranges and reader transfer stay in the safe host service.
use std::ffi::{c_char, c_void};
use fcb_app::host::atlas_search::{AtlasImportDirection, AtlasImportSide, MAX_SEARCH_SCOPE_PATH_BYTES};
use crate::{atlas_sessions::ImportsCommand, search_ffi::SearchCancellationCallback};
use super::{answer, number, reply, AccessError};

/// Admit a separate lexical-import inventory over the accepted captured index.
/// No source reads, package tooling, external resolver or network work occurs.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_imports_begin(handle: u64, generation: u64, index_generation: u64,
    max_imports: u64, max_parse_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_imports(handle,
        ImportsCommand::Begin { generation, index_generation, max_imports: number(max_imports)?, max_parse_bytes },
        || false)))
}

/// Admit with cooperative cancellation on the invoking worker.
/// # Safety
/// poll/context remain valid until return. poll must not unwind or reenter this
/// atlas. The callback is not retained. Free non-null replies with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_imports_begin_cancelable(handle: u64, generation: u64,
    index_generation: u64, max_imports: u64, max_parse_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        atlases.execute_imports(handle, ImportsCommand::Begin { generation, index_generation,
            max_imports: number(max_imports)?, max_parse_bytes }, &mut canceled)
    }))
}

/// Advance one admitted captured source. Accepted documentation links and their
/// generation remain independent of this import projection.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_imports_step(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_imports(handle, ImportsCommand::Step { generation }, || false)))
}

/// Cooperative polling before/after bounded lexical work and during traversal.
/// # Safety
/// Same borrowed callback and owned reply contract as imports_begin_cancelable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_imports_step_cancelable(handle: u64, generation: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        atlases.execute_imports(handle, ImportsCommand::Step { generation }, &mut canceled)
    }))
}

/// Page all imports or exact-file incoming/outgoing evidence. Native path bytes
/// are canonical lowercase hex; labels and foreign file IDs are not authority.
/// # Safety
/// path_hex is null or readable NUL-terminated ASCII, stable until return, with
/// at most 8192 bytes before NUL. Free the non-null result once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_imports_page(handle: u64, generation: u64, direction: u64,
    path_hex: *const c_char, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let direction = parse_direction(direction)?;
        let path = unsafe { decode_path(path_hex) }?;
        if direction != AtlasImportDirection::All && path.is_none() {
            return Err(AccessError::InvalidArgument);
        }
        atlases.execute_imports(handle, ImportsCommand::Page { generation, direction,
            relative_path: path.as_deref(), start: number(start)?, limit: number(limit)? }, || false)
    }))
}

/// Transfer exact captured evidence or an available captured target into an
/// existing EMPTY reader. The receipt supplies the original evidence offset;
/// this operation never turns an unresolved import into a live file open.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_imports_open_reader(handle: u64, reader: u64, generation: u64,
    import: u64, side: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let side = parse_side(side)?;
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        atlases.open_import_reader(handle, readers, reader, generation, import, side, || false)
    }))
}

fn parse_direction(value: u64) -> Result<AtlasImportDirection, AccessError> {
    match value { 0 => Ok(AtlasImportDirection::All), 1 => Ok(AtlasImportDirection::Outgoing),
        2 => Ok(AtlasImportDirection::Incoming), _ => Err(AccessError::InvalidArgument) }
}
fn parse_side(value: u64) -> Result<AtlasImportSide, AccessError> {
    match value { 0 => Ok(AtlasImportSide::Evidence), 1 => Ok(AtlasImportSide::Target),
        _ => Err(AccessError::InvalidArgument) }
}
unsafe fn decode_path(pointer: *const c_char) -> Result<Option<Vec<u8>>, AccessError> {
    if pointer.is_null() { return Ok(None); }
    let maximum = 2 * MAX_SEARCH_SCOPE_PATH_BYTES;
    let mut length = 0;
    while length <= maximum {
        if unsafe { *pointer.add(length) } == 0 { break; }
        length += 1;
    }
    if length == 0 || length > maximum || length % 2 != 0 { return Err(AccessError::InvalidArgument); }
    let encoded = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length / 2).map_err(|_| AccessError::Capacity)?;
    for pair in encoded.chunks_exact(2) { bytes.push((nibble(pair[0])? << 4) | nibble(pair[1])?); }
    Ok(Some(bytes))
}
fn nibble(byte: u8) -> Result<u8, AccessError> {
    match byte { b'0'..=b'9' => Ok(byte - b'0'), b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(AccessError::InvalidArgument) }
}

#[cfg(all(test, unix))]
#[path = "atlas_imports_ffi_tests.rs"]
mod tests;
