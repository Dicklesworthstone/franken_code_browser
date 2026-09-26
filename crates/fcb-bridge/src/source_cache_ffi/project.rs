//! Cancelable project preparation over the existing production host services.
//! The native host supplies a worker and keeps grants/callback storage alive
//! until return. No native object, callback or pointer escapes these calls.

use std::{ffi::{c_char, c_void}, path::Path};
use crate::{cstr, reply, string_out, SearchCancellationCallback};
use fcb_app::host;

/// Metadata-only legacy atlas geometry, with cooperative discovery cancellation.
/// Null is failure/cancellation, not a successful empty project. No profiles are
/// scanned: the native consumer obtains text through the source-document route.
/// # Safety
/// root is null or a readable UTF-8 C string through return. poll/context obey
/// the same call-scoped, non-unwinding contract as fcb_search_workspace_cancelable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_layout_cancelable(root: *const c_char,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| {
        let canceled = || poll.is_some_and(|p| unsafe { p(context) != 0 });
        if canceled() { return None; }
        let root = unsafe { cstr(root) }?;
        let options = host::atlas::LegacyAtlasOptions { max_profile_source_bytes: 0, ..Default::default() };
        let result = host::atlas::prepare(Path::new(root), options, canceled).ok()?;
        if canceled() { return None; }
        string_out(result.as_str())
    })
}

/// Complete source and upstream highlighting, with existing source/response
/// budgets. Cancellation cannot interrupt an in-progress operating-system call.
/// # Safety
/// path is null or readable UTF-8 C text through return. poll/context remain
/// callable/alive through return, do not unwind or mutate input, and never reenter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_document_cancelable(path: *const c_char,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| {
        let canceled = || poll.is_some_and(|p| unsafe { p(context) != 0 });
        if canceled() { return None; }
        let path = unsafe { cstr(path) }?;
        let result = host::source_document::read(Path::new(path), canceled).ok()?;
        if canceled() { return None; }
        string_out(result.as_str())
    })
}

/// Preserve the exact cached source/key contract, with cooperative cancellation.
/// out_key is unchanged on failure. A canceled call can have populated an
/// immutable cache record; cancellation is not rollback of cache persistence.
/// The returned C string is freed exactly once with fcb_free_string.
/// # Safety
/// path is readable UTF-8 C text, out_key is null or writable for 32 bytes, and
/// poll/context follow the lifetime contract above. Inputs must not alias the
/// output key. The handle is retained internally while the operation is active.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_document_cached_cancelable(handle: u64,
    path: *const c_char, out_key: *mut u8, poll: Option<SearchCancellationCallback>,
    context: *mut c_void) -> *mut c_char {
    reply(|| {
        let canceled = || poll.is_some_and(|p| unsafe { p(context) != 0 });
        if out_key.is_null() || canceled() { return None; }
        let path = unsafe { cstr(path) }?;
        let cache = super::cache(handle)?;
        let mut cache = cache.lock().ok()?;
        if canceled() { return None; }
        let (response, key) = cache.source(Path::new(path), canceled).ok()?;
        if canceled() { return None; }
        let answer = string_out(response.as_str())?;
        // No cancellation/fallible work after the output pair is published.
        unsafe { std::ptr::copy_nonoverlapping(key.as_bytes().as_ptr(), out_key, 32); }
        Some(answer)
    })
}

#[cfg(all(test, unix))]
mod tests;
