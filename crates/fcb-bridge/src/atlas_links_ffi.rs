#![deny(unsafe_op_in_unsafe_fn)]

//! Marshaling only: parsing/resolution, frozen membership and capture transfer
//! remain in the safe host service. No URL or filesystem target is opened here.
use std::ffi::{c_char, c_void};
use fcb_app::host::atlas_search::{AtlasRelationshipDirection, AtlasRelationshipSide,
    MAX_SEARCH_SCOPE_PATH_BYTES};
use crate::{atlas_sessions::LinksCommand, search_ffi::SearchCancellationCallback};
use super::{answer, number, reply, AccessError};

/// Admit a link inventory over an already accepted retained index. Generations
/// are independent from content queries and index preparation. No source I/O.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_links_begin(handle: u64, generation: u64, index_generation: u64,
    max_links: u64, max_parse_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_links(handle,
        LinksCommand::Begin { generation, index_generation, max_links: number(max_links)?, max_parse_bytes },
        || false)))
}

/// Cooperative polling during metadata admission. The callback is borrowed
/// only until return and never retained by the session.
/// # Safety
/// poll/context remain valid through return; poll must not unwind or reenter
/// this atlas. Free every non-null result exactly once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_links_begin_cancelable(handle: u64, generation: u64,
    index_generation: u64, max_links: u64, max_parse_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        atlases.execute_links(handle, LinksCommand::Begin { generation, index_generation,
            max_links: number(max_links)?, max_parse_bytes }, &mut canceled)
    }))
}

/// Parse at most one admitted captured Markdown file. Terminal retries are
/// read-only; relationships_in_progress and relationships_complete are independent.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_links_step(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute_links(handle, LinksCommand::Step { generation }, || false)))
}

/// Cooperative polling around parsing and throughout result traversal, not
/// only between documents. The synchronous upstream parse is not interrupted.
/// # Safety
/// Same callback, worker and owned-string contract as links_begin_cancelable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_links_step_cancelable(handle: u64, generation: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        atlases.execute_links(handle, LinksCommand::Step { generation }, &mut canceled)
    }))
}

/// Page all links or a single file's outgoing/incoming links. Exact raw path
/// bytes are decoded from bounded lowercase hex; display labels and foreign
/// atlas FileIds cannot select a member of this captured index.
/// # Safety
/// path_hex is null or readable NUL-terminated ASCII, stable through return,
/// with at most 8192 bytes before NUL. All returned strings use fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_links_page(handle: u64, generation: u64, direction: u64,
    path_hex: *const c_char, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let direction = parse_direction(direction)?;
        let path = unsafe { decode_path(path_hex) }?;
        if direction != AtlasRelationshipDirection::All && path.is_none() {
            return Err(AccessError::InvalidArgument);
        }
        atlases.execute_links(handle, LinksCommand::Page { generation, direction,
            relative_path: path.as_deref(), start: number(start)?, limit: number(limit)? }, || false)
    }))
}

/// Open retained evidence or target bytes into an EXISTING EMPTY reader. The
/// safe service validates link/index identity; no live-path fallback exists.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_links_open_reader(handle: u64, reader: u64, generation: u64,
    link: u64, side: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let side = parse_side(side)?;
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        atlases.open_link_reader(handle, readers, reader, generation, link, side, || false)
    }))
}

fn parse_direction(value: u64) -> Result<AtlasRelationshipDirection, AccessError> {
    match value { 0 => Ok(AtlasRelationshipDirection::All), 1 => Ok(AtlasRelationshipDirection::Outgoing),
        2 => Ok(AtlasRelationshipDirection::Incoming), _ => Err(AccessError::InvalidArgument) }
}
fn parse_side(value: u64) -> Result<AtlasRelationshipSide, AccessError> {
    match value { 0 => Ok(AtlasRelationshipSide::Evidence), 1 => Ok(AtlasRelationshipSide::Target),
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
#[path = "atlas_links_ffi_tests.rs"]
mod tests;
