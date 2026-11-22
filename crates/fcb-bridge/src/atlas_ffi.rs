#![deny(unsafe_op_in_unsafe_fn)]

//! C marshaling for retained atlas sessions. No native pixels are inferred from
//! returning geometry. Hosts acknowledge the plan actually displayed before pick.

use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, atlas_session::{AtlasAction, AtlasExtensionScope, AtlasScope,
    AtlasSession, AtlasSessionOptions, MAX_ATLAS_DIRECTORY_BYTES, MAX_ATLAS_FILE_BYTES,
    MAX_ATLAS_SESSION_FILES, directory_path, file_path}, catalog_session::CatalogDiscoveryPolicy};
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
pub extern "C" fn fcb_atlas_forward(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| atlases.execute(handle,
        Command::Prepare { generation, action: AtlasAction::Forward }, || false)))
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

/// Apply an explicit file-type display scope to the session's atlas:
/// `scope` is "all", "markdown", "python", "rust", or "extensions". For
/// "extensions", `extensions` is a comma-separated custom list (for example
/// "md,toml"; ASCII case-insensitive, 1..16 tokens of 1..16 bytes). The next
/// plan may repack geometry from the frozen catalog on this worker; camera
/// ticks never repack and no source is read or parsed for any scope change.
/// Restoring "all" reuses the retained original layout, so earlier All-frame
/// geometry stays valid. Returning JSON carries the active scope identity.
/// # Safety
/// scope/extensions are null or readable NUL-terminated UTF-8 stable until
/// return. Returned strings must be freed once by this library's
/// fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_scope(handle: u64, generation: u64,
    scope: *const c_char, extensions: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let name = unsafe { cstr(scope) }.ok_or(AccessError::InvalidArgument)?;
        let scope = match name {
            "extensions" => {
                let list = unsafe { cstr(extensions) }.ok_or(AccessError::InvalidArgument)?;
                let parsed = AtlasExtensionScope::from_extensions(
                    list.split(',').map(|token| token.as_bytes()))
                    .map_err(|_| AccessError::InvalidArgument)?;
                AtlasScope::Extensions(parsed)
            }
            other => AtlasSession::preset_scope(other).ok_or(AccessError::InvalidArgument)?,
        };
        atlases.execute(handle, Command::Scope { generation, scope }, || false)
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

#[path = "atlas_search_ffi.rs"]
mod search;
#[path = "atlas_paths_ffi.rs"]
mod paths;
#[path = "atlas_stream_ffi.rs"]
mod stream;

#[cfg(all(test, unix))]
#[path = "atlas_ffi_tests.rs"]
mod tests;

/// Discover through the same retained atlas service with cooperative polling.
/// Null means cancellation, failed admission or another error, never an empty
/// workspace. Close the handle on a worker even when this call returns null.
/// # Safety
/// root is null or readable NUL-terminated UTF-8 stable until return. poll and
/// its context must remain valid until return; poll must not unwind or reenter
/// this session. No callback is retained or invoked after return. A non-null
/// response must be freed once by fcb_free_string. This cannot abort a syscall.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_open_cancelable(handle: u64, root: *const c_char,
    max_files: u64, poll: Option<super::search_ffi::SearchCancellationCallback>,
    context: *mut std::ffi::c_void) -> *mut c_char {
    unsafe { fcb_atlas_open_with_policy_cancelable(handle, root, max_files, 0, poll, context) }
}

/// Open one retained atlas under the explicit shared discovery policy:
/// 0 repository rules (the compatibility default), 1 include excluded files.
/// Unknown policy values return null before pointer, callback or registry access.
/// A policy change requires a new handle; an open session is never relabeled.
/// # Safety
/// For a supported policy, root/poll/context follow fcb_atlas_open_cancelable's
/// contract. A late cancellation can suppress the reply after installation;
/// inspect or close the known handle rather than assuming initialization rolled
/// back. No callbacks are retained. Free non-null JSON once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_open_with_policy_cancelable(handle: u64, root: *const c_char,
    max_files: u64, discovery_policy: u64,
    poll: Option<super::search_ffi::SearchCancellationCallback>,
    context: *mut std::ffi::c_void) -> *mut c_char {
    reply(|| {
        let policy = match discovery_policy {
            0 => CatalogDiscoveryPolicy::RepositoryRules,
            1 => CatalogDiscoveryPolicy::IncludeExcluded,
            _ => return None,
        };
        let canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return None; }
        let root = unsafe { cstr(root) }?;
        let response = ATLASES.get()?.open_with_policy(handle, Path::new(root),
            AtlasSessionOptions { max_files: number(max_files).ok()?, ..Default::default() }, policy, canceled).ok()?;
        if canceled() { return None; }
        string_out(response.as_str())
    })
}

/// Open a bounded folder catalog under the original project grant. Directory
/// bytes are raw Unix components, never a replacement root or a lossy label.
/// Policy, length and file limits are refused before pointer/callback/handle
/// access. Component admission is shared with the metadata directory route.
/// # Safety
/// For admitted limits, root/poll/context follow fcb_atlas_open_cancelable.
/// directory must point to directory_len readable bytes stable until return.
/// It is not NUL-terminated; malformed raw paths are refused. A late null can
/// follow installation: inspect or close the known handle on a worker. Free
/// every non-null reply once through fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_open_directory_with_policy_cancelable(
    handle: u64, root: *const c_char, directory: *const u8, directory_len: u64,
    max_files: u64, discovery_policy: u64,
    poll: Option<super::search_ffi::SearchCancellationCallback>,
    context: *mut std::ffi::c_void) -> *mut c_char {
    reply(|| {
        let policy = match discovery_policy {
            0 => CatalogDiscoveryPolicy::RepositoryRules,
            1 => CatalogDiscoveryPolicy::IncludeExcluded,
            _ => return None,
        };
        let max_files = number(max_files).ok()?;
        let directory_len = number(directory_len).ok()?;
        if !(1..=MAX_ATLAS_SESSION_FILES).contains(&max_files)
            || !(1..=MAX_ATLAS_DIRECTORY_BYTES).contains(&directory_len)
            || directory.is_null() { return None; }
        let canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return None; }
        let root = unsafe { cstr(root) }?;
        let directory = unsafe { std::slice::from_raw_parts(directory, directory_len) };
        let directory = directory_path(directory).ok()?;
        let response = ATLASES.get()?.open_directory_with_policy(handle, Path::new(root),
            directory, AtlasSessionOptions { max_files, ..Default::default() },
            policy, canceled).ok()?;
        if canceled() { return None; }
        string_out(response.as_str())
    })
}

/// Discover exactly one regular file under the original project grant without
/// enumerating its parent or siblings. Raw path components retain their native
/// identity; inherited repository rules and root confinement remain in force.
/// # Safety
/// root/poll/context follow fcb_atlas_open_cancelable. file is a readable raw
/// byte buffer of file_len bytes stable until return, not a C string. Invalid
/// policy/length/file limits and a null file pointer return null before pointer
/// dereference, callback polling or handle-table access. Other failures leave
/// the ordinary late-null ownership contract: inspect or close the known handle
/// on a worker and free every non-null reply once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_open_file_with_policy_cancelable(
    handle: u64, root: *const c_char, file: *const u8, file_len: u64,
    max_files: u64, discovery_policy: u64,
    poll: Option<super::search_ffi::SearchCancellationCallback>,
    context: *mut std::ffi::c_void) -> *mut c_char {
    reply(|| {
        let policy = match discovery_policy {
            0 => CatalogDiscoveryPolicy::RepositoryRules,
            1 => CatalogDiscoveryPolicy::IncludeExcluded,
            _ => return None,
        };
        let max_files = number(max_files).ok()?;
        let file_len = number(file_len).ok()?;
        if !(1..=MAX_ATLAS_SESSION_FILES).contains(&max_files)
            || !(1..=MAX_ATLAS_FILE_BYTES).contains(&file_len)
            || file.is_null() { return None; }
        let canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return None; }
        let root = unsafe { cstr(root) }?;
        let file = unsafe { std::slice::from_raw_parts(file, file_len) };
        let file = file_path(file).ok()?;
        let response = ATLASES.get()?.open_file_with_policy(handle, Path::new(root),
            file, AtlasSessionOptions { max_files, ..Default::default() },
            policy, canceled).ok()?;
        if canceled() { return None; }
        string_out(response.as_str())
    })
}

/// Advance the existing one-file search pipeline with cooperative polling.
/// Cancellation is checked inside capture, verification, hashing and encoding,
/// not only between native calls. Null is not a successful empty search page.
/// # Safety
/// poll/context follow fcb_atlas_open_cancelable's lifetime and non-reentrancy
/// contract. The returned string must be freed once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_search_step_cancelable(handle: u64, generation: u64,
    poll: Option<super::search_ffi::SearchCancellationCallback>,
    context: *mut std::ffi::c_void) -> *mut c_char {
    reply(|| {
        let canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return None; }
        let response = ATLASES.get()?.execute(handle, Command::SearchStep { generation }, canceled).ok()?;
        if canceled() { return None; }
        string_out(response.as_str())
    })
}

#[cfg(all(test, unix))]
#[path = "atlas_directory_ffi_tests.rs"]
mod directory_tests;

#[cfg(all(test, unix))]
#[path = "atlas_file_ffi_tests.rs"]
mod file_tests;
