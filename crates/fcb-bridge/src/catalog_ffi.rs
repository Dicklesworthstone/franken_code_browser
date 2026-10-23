#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer conversion for the shared progressive project catalog service.
//! No walker, path resolver, source capture or metadata cache lives here.
use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::HostResponse;
use super::{cstr, reply, string_out};
use super::catalog_sessions::{AccessError, CatalogSessions, Command};

static CATALOGS: OnceLock<CatalogSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&CatalogSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match CATALOGS.get() { Some(catalogs) => work(catalogs), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> { usize::try_from(value).map_err(|_| AccessError::InvalidArgument) }

/// Empty reservation; no filesystem I/O. Zero means admission failure.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_create() -> u64 {
    std::panic::catch_unwind(|| CATALOGS.get_or_init(CatalogSessions::new).create().unwrap_or(0)).unwrap_or(0)
}
/// Admit the root and exactly one bounded metadata discovery batch.
/// # Safety
/// root is null or readable NUL-terminated UTF-8 stable until return. Returned
/// JSON is independently owned and must be freed once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_catalog_open(handle: u64, root: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |catalogs| {
        let root = unsafe { cstr(root) }.ok_or(AccessError::InvalidArgument)?;
        catalogs.open(handle, Path::new(root), || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |catalogs| catalogs.execute(handle, Command::Info, || false)))
}
/// Replay an exact retained metadata page without advancing the directory walk.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_page(handle: u64, page: u64) -> *mut c_char {
    reply(|| answer(handle, |catalogs| catalogs.execute(handle, Command::Page { page }, || false)))
}
/// Advance after the current page, or replay its already consumed successor.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_advance(handle: u64, after_page: u64) -> *mut c_char {
    reply(|| answer(handle, |catalogs| catalogs.execute(handle, Command::Advance { after_page }, || false)))
}
/// Pause traversal while retaining bounded cursor state and page authority.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_stop(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |catalogs| catalogs.execute(handle, Command::Stop, || false)))
}
/// Open a retained row as a new capture in an existing EMPTY reader handle.
/// No caller pathname can substitute for the selected session/page/file proof.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_open_reader(handle: u64, reader: u64, page: u64, file: u64,
    max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |catalogs| {
        let readers = super::reader_ffi::registry().ok_or(super::reader_sessions::AccessError::UnknownHandle)?;
        catalogs.open_reader(handle, readers, reader, page, file, number(max_source_bytes)?, || false)
    }))
}
/// Nonblocking epoch cancellation; consumed pages remain recoverable via info.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(CATALOGS.get().is_some_and(|catalogs| catalogs.cancel(handle).is_ok()))).unwrap_or(0)
}
/// Worker-only retirement. Does not free delivered JSON or retained readers.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_catalog_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(CATALOGS.get().is_some_and(|catalogs| catalogs.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(all(test, unix))]
#[path = "catalog_ffi_tests.rs"]
mod tests;
