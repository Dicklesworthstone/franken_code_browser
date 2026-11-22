#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer conversion for explicit FCBT attachment. Validation, immutable
//! references, readiness and exact saved-source reader copies stay in the host.

use std::ffi::c_void;
use fcb_app::host::saved_repository::trail::{SavedTrailArchiveRequest, SavedTrailWriteParent};
use super::{AccessError, Command, Selection, Path, answer, c_char, cstr, number, pin, reply};

/// Read one bounded FCBT file, retaining all its reference/note bytes. The
/// accepted trail never reopens this path or any entry's source pathname.
/// # Safety
/// path is null or readable NUL-terminated UTF-8 stable until return. Every
/// non-null response, including errors, must be freed with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_attach(handle: u64, generation: u64,
    path: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        sessions.execute(handle, Command::AttachTrail { generation, path: Path::new(path) }, || false)
    }))
}

/// Metadata-only page. Item/start ordinals are zero-based and limit is 1..32.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_trail_page(handle: u64, generation: u64,
    start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::TrailPage {
        generation, start: number(start)?, limit: number(limit)? }, || false)))
}

/// Detach reference state. Already installed readers retain their own captures.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_trail_detach(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::DetachTrail { generation }, || false)))
}

/// Open the generation-bound visit in an EMPTY reader destination. Source
/// digest, length and exact byte range are reverified from the retained archive.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_open_trail_reader(handle: u64, reader: u64,
    generation: u64, item: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader, Selection::Trail { generation, item: number(item)? }, || false)
    }))
}

/// Resolve one frozen other-archive visit through an explicit file grant.
/// Source preparation admits an empty reader before any archive I/O. The
/// primary saved session and accepted trail are not replaced or reattached.
/// # Safety
/// trail_digest/archive_path are readable NUL-terminated UTF-8 until return.
/// The selected read grant and optional callback/context remain valid through
/// the entered worker call. The callback must not block, unwind, or reenter
/// either handle. Free non-null JSON exactly once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_open_trail_reader_with_archive_cancelable(handle: u64,
    reader: u64, generation: u64, item: u64, trail_digest: *const c_char,
    archive_path: *const c_char, validation_bytes: u64,
    poll: Option<crate::SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        if generation == 0 || validation_bytes > 256 * 1024 * 1024 {
            return Err(AccessError::InvalidArgument);
        }
        let digest = pin(unsafe { cstr(trail_digest) }.ok_or(AccessError::InvalidArgument)?)?;
        let path = unsafe { cstr(archive_path) }.ok_or(AccessError::InvalidArgument)?;
        if path.is_empty() || path.len() > 16_384 || !Path::new(path).is_absolute() {
            return Err(AccessError::InvalidArgument);
        }
        let request = SavedTrailArchiveRequest { trail: SavedTrailWriteParent { generation, digest },
            item: number(item)?, archive: Path::new(path), validation_bytes };
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_trail_reader_with_archive(handle, readers, reader, request,
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

#[cfg(all(test, unix))]
#[path = "saved_trail_ffi_tests.rs"]
mod tests;
