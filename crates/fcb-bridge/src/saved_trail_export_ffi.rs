#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer conversion for retained-archive source-pack export. The safe host
//! owns selection verification, limits, create-only effects and receipt recovery.

use std::{ffi::c_void, path::Path};
use fcb_app::host::saved_repository::trail::{SavedTrailExportLimits,
    SavedTrailExportRequest, SavedTrailWriteParent};
use super::{AccessError, Command, answer, c_char, cstr, number, pin, reply};

/// Export an exact accepted trail against the already opened saved archive.
/// Mixed-archive entries remain explicit unavailable visits; no paths reopen.
/// # Safety
/// digest/destination are readable NUL-terminated UTF-8 until return. Keep the
/// explicit destination grant until the worker drains. Free non-null JSON once
/// with fcb_free_string. A lost reply requires receipt recovery, not a retry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_export(handle: u64, operation: u64,
    generation: u64, digest: *const c_char, destination: *const c_char,
    entries: u64, verification_bytes: u64, selected_bytes: u64,
    output_bytes: u64, output_scalars: u64) -> *mut c_char {
    unsafe { fcb_saved_trail_export_cancelable(handle, operation, generation, digest,
        destination, entries, verification_bytes, selected_bytes, output_bytes,
        output_scalars, None, std::ptr::null_mut()) }
}

/// # Safety
/// Same pointer lifetime as fcb_saved_trail_export. Optional cancellation
/// callback/context remains callable until return; never block, unwind or
/// reenter this handle. Cancellation does not reverse a filesystem effect.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_export_cancelable(handle: u64, operation: u64,
    generation: u64, digest: *const c_char, destination: *const c_char,
    entries: u64, verification_bytes: u64, selected_bytes: u64,
    output_bytes: u64, output_scalars: u64,
    poll: Option<crate::SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        if generation == 0 { return Err(AccessError::InvalidArgument); }
        let digest = pin(unsafe { cstr(digest) }.ok_or(AccessError::InvalidArgument)?)?;
        let destination = unsafe { cstr(destination) }.ok_or(AccessError::InvalidArgument)?;
        let request = SavedTrailExportRequest { operation,
            trail: SavedTrailWriteParent { generation, digest }, destination: Path::new(destination),
            limits: SavedTrailExportLimits { entries: number(entries)?, verification_bytes, selected_bytes,
                output_bytes: number(output_bytes)?, output_scalars: number(output_scalars)? } };
        sessions.export_trail(handle, request, || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Export with explicitly selected supplemental archives. All borrowed inputs
/// and grants stay live through the entered call; only digest-verified archive
/// handles may serve selections. No trail label becomes path authority.
/// # Safety
/// digest/destination and each of archive_count pointers are readable,
/// NUL-terminated UTF-8 until return. archive_paths points to at least that many
/// pointers and may be null only for count zero. Count is at most seven.
/// Optional callback/context has the same lifetime/reentrancy contract as
/// fcb_saved_trail_export_cancelable. Free non-null JSON with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_export_with_archives_cancelable(handle: u64, operation: u64,
    generation: u64, digest: *const c_char, destination: *const c_char,
    entries: u64, verification_bytes: u64, selected_bytes: u64,
    output_bytes: u64, output_scalars: u64,
    archive_paths: *const *const c_char, archive_count: u64, validation_bytes: u64,
    poll: Option<crate::SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        if generation == 0 || archive_count > 7 || validation_bytes > 256 * 1024 * 1024
            || (archive_count > 0 && archive_paths.is_null()) { return Err(AccessError::InvalidArgument); }
        let digest = pin(unsafe { cstr(digest) }.ok_or(AccessError::InvalidArgument)?)?;
        let destination = unsafe { cstr(destination) }.ok_or(AccessError::InvalidArgument)?;
        let mut paths = [Path::new(""); 7];
        for (ordinal, path) in paths.iter_mut().enumerate().take(archive_count as usize) {
            let pointer = unsafe { *archive_paths.add(ordinal) };
            let text = unsafe { cstr(pointer) }.ok_or(AccessError::InvalidArgument)?;
            if text.is_empty() || text.len() > 16_384 || !Path::new(text).is_absolute() {
                return Err(AccessError::InvalidArgument);
            }
            *path = Path::new(text);
        }
        let request = SavedTrailExportRequest { operation,
            trail: SavedTrailWriteParent { generation, digest }, destination: Path::new(destination),
            limits: SavedTrailExportLimits { entries: number(entries)?, verification_bytes, selected_bytes,
                output_bytes: number(output_bytes)?, output_scalars: number(output_scalars)? } };
        sessions.export_trail_with_archives(handle, request, &paths[..archive_count as usize], validation_bytes,
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Recover one exact terminal receipt without source/output I/O or replay.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_trail_export_receipt(handle: u64, operation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::TrailExportReceipt { operation }, || false)))
}

#[cfg(all(test, unix))]
#[path = "saved_trail_export_ffi_tests.rs"]
mod tests;
