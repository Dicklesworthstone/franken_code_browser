#![deny(unsafe_op_in_unsafe_fn)]

//! C ABI marshaling over safe `fcb_app::host` worker services. Source opening,
//! discovery, layout, decoding, search and document policy belong to the shared
//! application/library, not this bridge. No separate walk or heuristic lexer.
//!
//! Every returned non-null string is owned by the caller and must be released
//! exactly once with `fcb_free_string`. Functions are synchronous worker work.
//! Legacy one-shot calls observe sources independently. The reader_ffi exports
//! instead share one immutable capture through explicit retained handles.
//! Unwinding Rust panics become null; aborting failures cannot be recovered here.

mod reader_sessions;
mod reader_ffi;
mod atlas_sessions;
mod atlas_ffi;
mod text_layout_ffi;
mod source_cache_ffi;

use std::{ffi::{c_char, CStr, CString}, panic::{catch_unwind, UnwindSafe}, path::Path};
use fcb_app::host;

/// # Safety
/// Non-null input must be readable and NUL-terminated for the borrow's lifetime.
unsafe fn cstr<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() { return None; }
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}
fn string_out(text: &str) -> Option<*mut c_char> {
    // Never delete NULs or replace invalid bytes to force an FFI handoff.
    CString::new(text).ok().map(CString::into_raw)
}
fn reply(work: impl FnOnce() -> Option<*mut c_char> + UnwindSafe) -> *mut c_char {
    catch_unwind(work).ok().flatten().unwrap_or(std::ptr::null_mut())
}

/// Shared captured-workspace literal search JSON, including error/partial states.
/// # Safety
/// Inputs are valid NUL-terminated UTF-8 strings or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_search_workspace(root: *const c_char, query: *const c_char) -> *mut c_char {
    reply(|| {
        let root = unsafe { cstr(root) }?;
        let query = unsafe { cstr(query) }?;
        let result = host::search_workspace(Path::new(root), query, || false).ok()?;
        string_out(result.as_str())
    })
}

/// Exact whole-file UTF-8 up to 4 MiB, or null. No truncation, replacement or NUL
/// deletion. Empty source returns an allocated empty string. Use window JSON for
/// larger files, UTF-16, embedded NUL, exact byte ranges and diagnostic outcomes.
/// # Safety
/// `path` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_read_file(path: *const c_char) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        let result = host::read_text(Path::new(path), host::MAX_HOST_TEXT_BYTES, || false).ok()?;
        string_out(result.as_str())
    })
}

/// Shared source-window JSON with exact original-byte/decoded-text domains.
/// # Safety
/// `path` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_read_file_window(path: *const c_char, offset: u64, bytes: u64) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        let result = host::read_window(Path::new(path), offset, bytes, || false).ok()?;
        string_out(result.as_str())
    })
}

/// One-based exact source-line navigation using the shared bounded scanner.
/// # Safety
/// `path` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_read_file_lines(path: *const c_char, line: u64, lines: u64) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        let result = host::read_lines(Path::new(path), line, lines, || false).ok()?;
        string_out(result.as_str())
    })
}

/// Versioned metadata-only atlas plan. Retain Rust map objects for camera input;
/// this one-shot service is not intended to rebuild on each gesture or redraw.
/// # Safety
/// `root` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_plan(root: *const c_char) -> *mut c_char {
    reply(|| {
        let root = unsafe { cstr(root) }?;
        let result = host::atlas_plan(Path::new(root), || false).ok()?;
        string_out(result.as_str())
    })
}

/// Legacy world/files tile fields over the SAME catalog/layout engine, with
/// bounded neutral line-density profiles and explicit coverage metadata. Null
/// refuses incomplete discovery or unrepresentable legacy UTF-8 paths.
/// # Safety
/// `root` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_layout(root: *const c_char) -> *mut c_char {
    reply(|| {
        let root = unsafe { cstr(root) }?;
        let result = host::atlas::prepare(Path::new(root), host::atlas::LegacyAtlasOptions::default(), || false).ok()?;
        string_out(result.as_str())
    })
}

/// Logical Markdown flow window. `line` is one-based RENDERED flow, not source.
/// # Safety
/// `path` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_markdown_window(path: *const c_char, line: u64, lines: u64, width: u64) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        let result = host::markdown_window(Path::new(path), line, lines, width, || false).ok()?;
        string_out(result.as_str())
    })
}

/// Markdown heading navigation by canonical slug, using the upstream engine.
/// # Safety
/// Inputs are valid NUL-terminated UTF-8 strings or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_markdown_heading(path: *const c_char, heading: *const c_char, lines: u64, width: u64) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        let heading = unsafe { cstr(heading) }?;
        let result = host::markdown_heading(Path::new(path), heading, lines, width, || false).ok()?;
        string_out(result.as_str())
    })
}

/// Release a returned string, or accept null as a no-op.
/// # Safety
/// A non-null pointer must be an unmodified, still-owned result from this exact
/// library allocation domain, and must not have been freed or aliased for use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_free_string(pointer: *mut c_char) {
    if !pointer.is_null() { drop(unsafe { CString::from_raw(pointer) }); }
}

#[cfg(test)]
mod tests;

/// Complete UTF-8 source and upstream syntax runs in UTF-16 coordinates.
/// Returns null on read, encoding, admission or response-limit failure.
/// # Safety
/// `path` is a valid NUL-terminated UTF-8 string or null, stable until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_document(path: *const c_char) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        let result = host::source_document::read(Path::new(path), || false).ok()?;
        string_out(result.as_str())
    })
}
