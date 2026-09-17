//! C ABI bridge for host application shells (SwiftUI) over the real fcb
//! engine.
//!
//! Thin marshaling only. Search and file reads route through
//! [`fcb_app::run`] — the same command routing the `fcb` CLI uses — so a
//! native shell and the terminal share one implementation, one policy,
//! and one JSON contract. No parsing, policy, or caching lives here.
//!
//! FFI safety: the `unsafe` surface is exactly four sites, each limited
//! to C-string marshaling at the ABI boundary. Callers own returned
//! strings and release them with [`fcb_free_string`].

use std::ffi::{c_char, CStr, CString, OsString};

/// The longest file body the bridge will hand a shell. The engine's own
/// bounded-read policy stays authoritative; this only caps host memory.
const MAX_READ_BYTES: usize = 4 << 20;

/// Reads a C string at the ABI boundary; null or non-UTF-8 yields None.
unsafe fn cstr<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}

/// Marshals an owned result into a caller-owned C string.
fn string_out(value: Option<String>) -> *mut c_char {
    match value.and_then(|text| CString::new(text.replace('\0', "")).ok()) {
        Some(c_string) => c_string.into_raw(),
        None => std::ptr::null_mut(),
    }
}

fn run_capture(arguments: &[&str]) -> Option<String> {
    let args: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut empty: &[u8] = &[];
    let code = fcb_app::run(&args, &mut empty, &mut stdout, &mut stderr, || false);
    if !stdout.is_empty() {
        return String::from_utf8(stdout).ok();
    }
    if code == 0 || code == 1 {
        // 0 = complete, 1 = complete-without-match: both are valid empty
        // answers for a shell. Anything else carries diagnostics.
        return Some(String::new());
    }
    String::from_utf8(stderr).ok()
}

/// Searches a workspace for exact text matches. Returns the engine's own
/// JSON response (`fcb search ROOT --workspace --text Q --json`).
///
/// # Safety
/// `root` and `query` must be valid NUL-terminated UTF-8 C strings, or
/// null (null yields an empty reply). The returned string is released by
/// the caller through [`fcb_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn fcb_search_workspace(root: *const c_char, query: *const c_char) -> *mut c_char {
    let reply = (|| {
        let root = unsafe { cstr(root) }?;
        let query = unsafe { cstr(query) }?;
        run_capture(&["search", root, "--workspace", "--text", query, "--json"])
    })();
    string_out(reply)
}

/// Reads one named file as lossy UTF-8 text, capped at 4 MiB.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 C string, or null. The
/// returned string is released by the caller through [`fcb_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn fcb_read_file(path: *const c_char) -> *mut c_char {
    let reply = (|| {
        let path = unsafe { cstr(path) }?;
        let bytes = std::fs::read(path).ok()?;
        let capped = &bytes[..bytes.len().min(MAX_READ_BYTES)];
        Some(String::from_utf8_lossy(capped).into_owned())
    })();
    string_out(reply)
}

/// Releases a string previously returned by this bridge.
///
/// # Safety
/// `pointer` must be null or a string returned by this bridge that has
/// not already been released.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_free_string(pointer: *mut c_char) {
    if !pointer.is_null() {
        drop(unsafe { CString::from_raw(pointer) });
    }
}
