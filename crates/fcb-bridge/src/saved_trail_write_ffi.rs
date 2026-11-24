#![deny(unsafe_op_in_unsafe_fn)]

//! UTF-8 pointer conversion for immutable FCBT authoring. Canonical source
//! verification, append semantics, create-only publication and receipts remain
//! in the shared host. Borrowed text never survives this synchronous call.

use std::{ffi::c_void, path::Path};
use fcb::{ByteOffset, ByteRange};
use fcb::store::trail::{MAX_RATIONALE_BYTES, MAX_TRAIL_TITLE_BYTES};
use fcb::search::trail::TrailEdit;
use fcb_app::host::saved_repository::trail::{SavedTrailWriteParent, SavedTrailWriteRequest, SavedTrailEditRequest};
use super::{AccessError, Command, answer, c_char, cstr, number, pin, reply};

unsafe fn bounded_text<'a>(pointer: *const u8, length: u64, limit: usize) -> Result<&'a str, AccessError> {
    let length = number(length)?;
    if length > limit || (length != 0 && pointer.is_null()) { return Err(AccessError::InvalidArgument); }
    let bytes = if length == 0 { &[] } else { unsafe { std::slice::from_raw_parts(pointer, length) } };
    std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument)
}

/// Create or append a verified exact selection to a NEW immutable FCBT file.
/// Zero parent generation and empty digest creates a new trail. Append requires
/// the current accepted attachment's generation/digest and unchanged title.
/// # Safety
/// digest/destination are NUL-terminated UTF-8 readable until return. title and
/// rationale are readable UTF-8 byte slices (max 256/4096); null is valid only
/// with length zero. Embedded NUL text is preserved. Keep destination access
/// until the worker drains; free every non-null response with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_write(handle: u64, operation: u64,
    expected_trail_generation: u64, expected_trail_digest: *const c_char,
    member: u64, start: u64, end: u64, title: *const u8, title_length: u64,
    rationale: *const u8, rationale_length: u64, destination: *const c_char) -> *mut c_char {
    unsafe { fcb_saved_trail_write_cancelable(handle, operation, expected_trail_generation,
        expected_trail_digest, member, start, end, title, title_length, rationale,
        rationale_length, destination, None, std::ptr::null_mut()) }
}

/// Optional synchronous worker cancellation. fcb_saved_cancel also interrupts
/// the shared engine/writer. Effects remain in the returned terminal receipt.
/// # Safety
/// Same pointer contract as fcb_saved_trail_write. Optional poll/context remains
/// callable until return; callback is fast, nonblocking, never unwinds/reenters,
/// and returns nonzero to cancel. Null output is uncertain: recover this exact
/// operation via receipt lookup; never automatically retry publication.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_write_cancelable(handle: u64, operation: u64,
    expected_trail_generation: u64, expected_trail_digest: *const c_char,
    member: u64, start: u64, end: u64, title: *const u8, title_length: u64,
    rationale: *const u8, rationale_length: u64, destination: *const c_char,
    poll: Option<crate::SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let digest = unsafe { cstr(expected_trail_digest) }.ok_or(AccessError::InvalidArgument)?;
        let parent = if expected_trail_generation == 0 {
            if !digest.is_empty() { return Err(AccessError::InvalidArgument); }
            None
        } else { Some(SavedTrailWriteParent { generation: expected_trail_generation, digest: pin(digest)? }) };
        let title = unsafe { bounded_text(title, title_length, MAX_TRAIL_TITLE_BYTES) }?;
        let rationale = unsafe { bounded_text(rationale, rationale_length, MAX_RATIONALE_BYTES) }?;
        let destination = unsafe { cstr(destination) }.ok_or(AccessError::InvalidArgument)?;
        let selection = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end))
            .map_err(|_| AccessError::InvalidArgument)?;
        let request = SavedTrailWriteRequest { operation, parent, member: number(member)?, selection,
            title, rationale, destination: Path::new(destination) };
        sessions.write_trail(handle, request, || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Apply one metadata-only edit to the exact attached parent and create a NEW
/// immutable FCBT. No source payload is opened or reverified. Kinds: 0 rename,
/// 1 set rationale, 2 move, 3 remove. item/position are zero-based; a move's
/// position is the final ordinal. Reserved arguments must be zero.
/// # Safety
/// digest/destination are NUL-terminated UTF-8 readable until return. text is
/// a UTF-8 slice, max 256 bytes for rename or 4096 for rationale; embedded NUL
/// is preserved. Rename requires item=position=0; rationale/remove position=0;
/// move/remove text_length=0. Null text is valid with length zero. Inputs and
/// destination write access live until return. Free every non-null reply once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_edit(handle: u64, operation: u64,
    expected_trail_generation: u64, expected_trail_digest: *const c_char,
    kind: u32, item: u64, position: u64, text: *const u8, text_length: u64,
    destination: *const c_char) -> *mut c_char {
    unsafe { fcb_saved_trail_edit_cancelable(handle, operation, expected_trail_generation,
        expected_trail_digest, kind, item, position, text, text_length, destination,
        None, std::ptr::null_mut()) }
}

/// Cancelable form of fcb_saved_trail_edit, sharing the create/append operation
/// clock and fcb_saved_trail_write_receipt recovery. Never replay a lost reply.
/// # Safety
/// Same pointer contract as fcb_saved_trail_edit. Optional poll/context remains
/// callable until return, never blocks, unwinds or reenters, and returns nonzero
/// to cancel. A cancel/close after creation cannot roll back the reported effect.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_trail_edit_cancelable(handle: u64, operation: u64,
    expected_trail_generation: u64, expected_trail_digest: *const c_char,
    kind: u32, item: u64, position: u64, text: *const u8, text_length: u64,
    destination: *const c_char, poll: Option<crate::SearchCancellationCallback>,
    context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        if expected_trail_generation == 0 { return Err(AccessError::InvalidArgument); }
        let digest = unsafe { cstr(expected_trail_digest) }.ok_or(AccessError::InvalidArgument)?;
        let parent = SavedTrailWriteParent { generation: expected_trail_generation, digest: pin(digest)? };
        let edit = match kind {
            0 if item == 0 && position == 0 => TrailEdit::Rename {
                title: unsafe { bounded_text(text, text_length, MAX_TRAIL_TITLE_BYTES) }?,
            },
            1 if position == 0 => TrailEdit::SetRationale {
                ordinal: number(item)?,
                rationale: unsafe { bounded_text(text, text_length, MAX_RATIONALE_BYTES) }?,
            },
            2 if text_length == 0 => TrailEdit::Move { ordinal: number(item)?, destination: number(position)? },
            3 if position == 0 && text_length == 0 => TrailEdit::Remove { ordinal: number(item)? },
            _ => return Err(AccessError::InvalidArgument),
        };
        let destination = unsafe { cstr(destination) }.ok_or(AccessError::InvalidArgument)?;
        let request = SavedTrailEditRequest { operation, parent, edit, destination: Path::new(destination) };
        sessions.edit_trail(handle, request, || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Return the retained terminal create/append/edit receipt without source/output I/O,
/// automatic attachment, or replaying any operation. Free returned JSON once.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_trail_write_receipt(handle: u64, operation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::TrailWriteReceipt { operation }, || false)))
}

#[cfg(all(test, unix))]
#[path = "saved_trail_write_ffi_tests.rs"]
mod tests;
