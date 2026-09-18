#![deny(unsafe_op_in_unsafe_fn)]

//! C marshaling for retained atlas sessions. No native pixels are inferred from
//! returning geometry. Hosts acknowledge the plan actually displayed before pick.

use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, atlas_session::{AtlasAction, AtlasSessionOptions}};
use fcb_core::Point2D;
use super::{cstr, reply, string_out};
use super::atlas_sessions::{AccessError, AtlasSessions, Command};

static ATLASES: OnceLock<AtlasSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&AtlasSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match ATLASES.get() { Some(atlases) => work(atlases), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> { usize::try_from(value).map_err(|_| AccessError::InvalidArgument) }
fn ordinal(value: u64) -> Result<u32, AccessError> { u32::try_from(value).map_err(|_| AccessError::InvalidArgument) }
fn point(x: f64, y: f64) -> Result<Point2D, AccessError> { Point2D::new(x, y).map_err(|_| AccessError::InvalidArgument) }

/// Empty handle reservation, no directory or source I/O. Zero means refusal.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_create() -> u64 {
    std::panic::catch_unwind(|| ATLASES.get_or_init(AtlasSessions::new).create().unwrap_or(0)).unwrap_or(0)
}
/// Discover metadata and retain the shared geometry/index once. Not a refresh.
/// # Safety
/// root is null or readable NUL-terminated UTF-8 stable until return. Returned
/// JSON must be freed exactly once by the same library's fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_open(handle: u64, root: *const c_char, max_files: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let root = unsafe { cstr(root) }.ok_or(AccessError::InvalidArgument)?;
        atlases.open(handle, Path::new(root), AtlasSessionOptions { max_files: number(max_files)?, ..Default::default() }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle, Command::Info, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_view(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::View }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_pan(handle: u64, generation: u64, x: f64, y: f64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::Pan(point(x, y)?) }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_zoom(handle: u64, generation: u64, x: f64, y: f64, factor: f64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::Zoom { anchor: point(x, y)?, factor } }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_focus(handle: u64, generation: u64, node: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::Focus(ordinal(node)?) }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_back(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::Back }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_resize(handle: u64, generation: u64, width: f64, height: f64, scale: f64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::Resize { width, height, scale } }, || false)))
}
/// Explicit host declaration, not a GPU/OS presentation observation.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_present(handle: u64, generation: u64, frame: u64, display: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Present { generation, frame, display }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_pick(handle: u64, frame: u64, display: u64, x: f64, y: f64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Pick { frame, display, point: point(x, y)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_children(handle: u64, parent: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Children { parent: ordinal(parent)?, start: number(start)?, limit: number(limit)? }, || false)))
}
/// Populate an EXISTING EMPTY reader handle from a picked file's new capture.
/// Reuse all fcb_reader_* calls afterward. No foreign path is used for selection.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_open_reader(handle: u64, reader: u64, frame: u64, display: u64,
    x: f64, y: f64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let readers = super::reader_ffi::registry().ok_or(super::reader_sessions::AccessError::UnknownHandle)?;
        atlases.open_reader(handle, readers, reader, frame, display, point(x, y)?, number(max_source_bytes)?, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(ATLASES.get().is_some_and(|atlases| atlases.cancel(handle).is_ok()))).unwrap_or(0)
}
/// Final geometry destruction may be substantial. Call close on a host worker.
/// Already-delivered reader captures and JSON strings remain independently owned.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(ATLASES.get().is_some_and(|atlases| atlases.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(all(test, unix))]
#[path = "atlas_ffi_tests.rs"]
mod tests;
