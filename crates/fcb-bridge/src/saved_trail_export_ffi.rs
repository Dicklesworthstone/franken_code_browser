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

/// Recover one exact terminal receipt without source/output I/O or replay.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_trail_export_receipt(handle: u64, operation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::TrailExportReceipt { operation }, || false)))
}
