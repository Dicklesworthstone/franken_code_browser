#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer marshaling only; shared desk APIs own source and personal state.
use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::HostResponse;
use super::{cstr, reply, string_out, desk_sessions::{AccessError, Command, DeskSessions}};

static DESKS: OnceLock<DeskSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&DeskSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match DESKS.get() { Some(desks) => work(desks), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(e) => string_out(&e.json(handle)) }
}
fn number(n: u64) -> Result<usize, AccessError> { usize::try_from(n).map_err(|_| AccessError::InvalidArgument) }

#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_create() -> u64 {
    std::panic::catch_unwind(|| DESKS.get_or_init(DeskSessions::new).create().unwrap_or(0)).unwrap_or(0)
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_start(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| desks.start(handle, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_state(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| desks.execute(handle, Command::State, || false)))
}
/// # Safety
/// path is null or a readable NUL-terminated UTF-8 string stable until return.
/// Free non-null responses exactly once with fcb_free_string, including errors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_open(handle: u64, expected: u64, attempt: u64, path: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        desks.execute(handle, Command::Open { expected, attempt, path: Path::new(path) }, || false)
    }))
}
/// # Safety
/// label is null (empty) or readable NUL-terminated UTF-8 stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_apply(handle: u64, expected: u64, attempt: u64, kind: u64,
    subject: u64, start: u64, end: u64, label: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let label = if label.is_null() { "" } else { unsafe { cstr(label) }.ok_or(AccessError::InvalidArgument)? };
        desks.execute(handle, Command::Apply { expected, attempt, kind, subject, start, end, label }, || false)
    }))
}
/// # Safety
/// label is null or readable NUL-terminated UTF-8 stable until return. For an
/// admitted nonzero body_bytes, body addresses that many initialized readable
/// bytes in one allocation, stable until return. A zero-length body may be null.
/// Body text is exact UTF-8: NUL/newline are data, never terminators. No foreign
/// borrow survives the call. Oversized lengths are rejected before dereference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_annotate(handle: u64, expected: u64, attempt: u64,
    pane: u64, label: *const c_char, body: *const u8, body_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let body = unsafe { note_body(body, body_bytes) }?;
        let label = unsafe { cstr(label) }.ok_or(AccessError::InvalidArgument)?;
        desks.execute(handle, Command::Annotate { expected, attempt, pane, label, body }, || false)
    }))
}
/// Edit only personal text; the bookmark's capture and range remain unchanged.
/// # Safety
/// The label/body pointer contracts are identical to fcb_desk_annotate. Free
/// every non-null response with fcb_free_string, including rejection responses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_edit_bookmark(handle: u64, expected: u64, attempt: u64,
    bookmark: u64, label: *const c_char, body: *const u8, body_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let body = unsafe { note_body(body, body_bytes) }?;
        let label = unsafe { cstr(label) }.ok_or(AccessError::InvalidArgument)?;
        desks.execute(handle, Command::EditBookmark { expected, attempt, bookmark, label, body }, || false)
    }))
}
unsafe fn note_body<'a>(body: *const u8, body_bytes: u64) -> Result<&'a str, AccessError> {
    let length = number(body_bytes)?;
    if length > fcb::ui::reading_panes::desk::MAX_ANNOTATION_BODY_BYTES
        || (length != 0 && body.is_null()) { return Err(AccessError::InvalidArgument); }
    let bytes = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(body, length) }
    };
    std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument)
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_window(handle: u64, expected: u64, pane: u64, offset: u64, bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| desks.execute(handle,
        Command::Window { expected, pane, offset, bytes: number(bytes)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_import_reader(handle: u64, expected: u64, attempt: u64,
    reader: u64, start: u64, end: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let readers = super::reader_ffi::registry().ok_or(AccessError::InvalidArgument)?;
        desks.import_reader(handle, readers, reader, expected, attempt, start, end, || false)
    }))
}
/// Open an exact retained pane into an admitted empty reader. No path argument
/// exists: the desk's label cannot authorize a live source read. A non-null
/// response is always desk JSON; successful receipts contain ordinary reader
/// info under `reader`. Retire the destination if its receipt is not accepted.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_open_reader(handle: u64, expected: u64, pane: u64,
    destination: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let readers = super::reader_ffi::registry().ok_or(AccessError::InvalidArgument)?;
        desks.open_reader(handle, readers, expected, pane, destination, || false)
    }))
}
/// # Safety
/// path is null or readable NUL-terminated UTF-8 stable until return. The host
/// grants the selected checkpoint, not live source paths stored inside it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_restore(handle: u64, expected: u64, attempt: u64, path: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        desks.execute(handle, Command::Restore { expected, attempt, path: Path::new(path) }, || false)
    }))
}
/// # Safety
/// path is a selected NEW destination, null or readable NUL-terminated UTF-8
/// stable until return. Saving discloses retained source and personal labels.
/// Always inspect the write receipt, including errors and canceled writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_save(handle: u64, expected: u64, path: *const c_char, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |desks| {
        let path = unsafe { cstr(path) }.ok_or(AccessError::InvalidArgument)?;
        desks.save(handle, expected, Path::new(path), max_source_bytes, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(DESKS.get().is_some_and(|desks| desks.cancel(handle).is_ok()))).unwrap_or(0)
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_desk_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(DESKS.get().is_some_and(|desks| desks.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(all(test, unix))]
#[path = "desk_ffi_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "desk_reader_ffi_tests.rs"]
mod reader_tests;

#[cfg(all(test, unix))]
#[path = "desk_annotations_ffi_tests.rs"]
mod annotation_tests;
