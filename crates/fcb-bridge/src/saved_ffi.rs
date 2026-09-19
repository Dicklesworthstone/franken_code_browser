#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer conversion only. Saved-source, index and search behavior lives in
//! fcb-app; reader activation targets the existing shared reader registry.
use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, saved_repository::{Sha256Digest, SnapshotLimits}};
use super::{cstr, reply, string_out};
use super::saved_sessions::{AccessError, Command, SavedSessions, Selection};

static SAVED: OnceLock<SavedSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&SavedSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match SAVED.get() { Some(sessions) => work(sessions), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> { usize::try_from(value).map_err(|_| AccessError::InvalidArgument) }
fn pin(text: &str) -> Result<Sha256Digest, AccessError> {
    if text.len() != 64 { return Err(AccessError::InvalidArgument); }
    fn digit(byte: u8) -> Result<u8, AccessError> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'), b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10), _ => Err(AccessError::InvalidArgument),
        }
    }
    let mut bytes = [0; 32];
    for (out, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        *out = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Ok(Sha256Digest::new(bytes))
}

/// Reserve a cancelable empty saved-repository handle; zero means failure.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_create() -> u64 {
    std::panic::catch_unwind(|| SAVED.get_or_init(SavedSessions::new).create().unwrap_or(0)).unwrap_or(0)
}
/// Open FCBS once under the existing default source/archive limits. Worker only.
/// # Safety
/// path is null or readable NUL-terminated UTF-8 stable until return. Every
/// non-null response, including errors, must be freed once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_open(handle: u64, path: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        sessions.open(handle, Path::new(path), SnapshotLimits::default(), || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Info, || false)))
}
/// Metadata-only member page; ordinal zero denotes the first archive member.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_members(handle: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::Members { start: number(start)?, limit: number(limit)? }, || false)))
}
/// Attach demand-paged FCBD using a separately retained trusted manifest pin.
/// # Safety
/// path and trusted_digest are null or readable NUL-terminated UTF-8 stable
/// until return. The digest has 64 hexadecimal characters; it is NOT derived
/// from this input file. Returned JSON follows fcb_free_string ownership.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_attach_index(handle: u64, generation: u64,
    path: *const c_char, trusted_digest: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        let digest = unsafe { cstr(trusted_digest) }.ok_or(AccessError::InvalidArgument)?;
        sessions.execute(handle, Command::AttachIndex { generation, path: Path::new(path), pin: pin(digest)? }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_detach_index(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::DetachIndex { generation }, || false)))
}
/// Exact decoded literal, synchronous worker search, retaining only hit metadata.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return. No
/// foreign pointer survives this call; release responses with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_search(handle: u64, generation: u64, needle: *const c_char,
    max_matches: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let needle = unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)?;
        sessions.execute(handle, Command::Search { generation, needle, limit: number(max_matches)? }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_results(handle: u64, generation: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::Results { generation, start: number(start)?, limit: number(limit)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_clear_results(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Clear { generation }, || false)))
}
/// Existing EMPTY reader destination. Source is reverified from the open archive,
/// never its original live path. An independently delivered reader keeps bytes.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_open_hit_reader(handle: u64, reader: u64, generation: u64, hit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader, Selection::Hit { generation, id: hit }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_open_member_reader(handle: u64, reader: u64, member: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader, Selection::Member(number(member)?), || false)
    }))
}
/// Nonblocking cooperative cancellation of the current operation. Accepted
/// archive/index/results and independently owned readers are not rolled back.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(SAVED.get().is_some_and(|sessions| sessions.cancel(handle).is_ok()))).unwrap_or(0)
}
/// Closing may reclaim archive/index memory: worker only. Never frees returned
/// JSON or readers; in-flight calls retain capacity until they actually drain.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(SAVED.get().is_some_and(|sessions| sessions.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(test)]
#[path = "saved_ffi_tests.rs"]
mod tests;
