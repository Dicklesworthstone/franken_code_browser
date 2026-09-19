#![deny(unsafe_op_in_unsafe_fn)]
//! Explicit bounded cache handles. Calls are synchronous worker operations;
//! no executor is created. Returned buffer ownership transfers to the host.
use std::{ffi::c_char, path::Path, sync::{Arc, Mutex, OnceLock, Weak}};
use fcb_app::host::source_cache::{SourceCache, CacheLimits, MAX_NATIVE_BYTES};
use fcb::store::Sha256Digest;
use super::{cstr, reply, string_out};
type Cache = Mutex<SourceCache>;
struct Slot { id: u64, active: Option<Arc<Cache>>, retired: Weak<Cache> }
struct Registry { next: u64, slots: Vec<Slot> }
static CACHES: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> { CACHES.get_or_init(|| Mutex::new(Registry { next: 1, slots: Vec::new() })) }
fn cache(handle: u64) -> Option<Arc<Cache>> {
    registry().lock().ok()?.slots.iter().find(|s| s.id == handle)?.active.clone()
}
/// # Safety
/// root is null or readable NUL-terminated UTF-8 for the call. Zero is failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_cache_open(root: *const c_char) -> u64 {
    std::panic::catch_unwind(|| {
        let root = unsafe { cstr(root) }?;
        let mut registry = registry().lock().ok()?;
        registry.slots.retain(|s| s.active.is_some() || s.retired.upgrade().is_some());
        if registry.slots.len() >= 8 { return None; }
        let id = registry.next;
        registry.next = id.checked_add(1)?;
        let opened = Arc::new(Mutex::new(SourceCache::open(Path::new(root), CacheLimits::default()).ok()?));
        registry.slots.push(Slot { id, retired: Arc::downgrade(&opened), active: Some(opened) });
        Some(id)
    }).ok().flatten().unwrap_or(0)
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_source_cache_close(handle: u64) -> bool {
    std::panic::catch_unwind(|| {
        let Ok(mut registry) = registry().lock() else { return false; };
        let Some(slot) = registry.slots.iter_mut().find(|s| s.id == handle) else { return false; };
        slot.active.take().is_some()
    }).unwrap_or(false)
}
/// # Safety
/// path readable UTF-8 C string; out_key null or writable 32 bytes. On success
/// key is copied and result is legacy source-document JSON (fcb_free_string).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_document_cached(handle: u64, path: *const c_char, out_key: *mut u8) -> *mut c_char {
    reply(|| {
        let path = unsafe { cstr(path) }?;
        if out_key.is_null() { return None; }
        let cache = cache(handle)?;
        let (response, key) = cache.lock().ok()?.source(Path::new(path), || false).ok()?;
        let answer = string_out(response.as_str())?;
        unsafe { std::ptr::copy_nonoverlapping(key.as_bytes().as_ptr(), out_key, 32); }
        Some(answer)
    })
}
/// # Safety
/// key_hex readable UTF-8 C string, out_len writable u64. Returned pointer is
/// owned, exactly out_len bytes, released with fcb_source_cache_free. Null means
/// miss/error; length is reset to zero. Empty artifacts are valid non-null hits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_cache_get(handle: u64, key_hex: *const c_char, out_len: *mut u64) -> *mut u8 {
    std::panic::catch_unwind(|| {
        if out_len.is_null() { return None; }
        unsafe { *out_len = 0; }
        let key = Sha256Digest::from_hex(unsafe { cstr(key_hex) }?).ok()?;
        let cache = cache(handle)?;
        let bytes = cache.lock().ok()?.get_native(key).ok()??;
        let owned = bytes.as_ref().to_vec().into_boxed_slice();
        unsafe { *out_len = owned.len() as u64; }
        Some(Box::into_raw(owned) as *mut u8)
    }).ok().flatten().unwrap_or(std::ptr::null_mut())
}
/// # Safety
/// key_hex readable C string; bytes readable len bytes for this call (null only
/// permitted for zero length). Buffer is borrowed and copied before return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_cache_put(handle: u64, key_hex: *const c_char, bytes: *const u8, len: u64) -> bool {
    std::panic::catch_unwind(|| {
        let length = usize::try_from(len).ok()?;
        if length > MAX_NATIVE_BYTES || (length != 0 && bytes.is_null()) { return None; }
        let key = Sha256Digest::from_hex(unsafe { cstr(key_hex) }?).ok()?;
        // ubs:ignore -- FFI contract grants readable bytes; null and 64MiB bound checked above.
        let bytes = if length == 0 { &[] } else { unsafe { std::slice::from_raw_parts(bytes, length) } };
        let cache = cache(handle)?;
        cache.lock().ok()?.put_native(key, bytes).ok()?;
        Some(())
    }).ok().flatten().is_some()
}
/// # Safety
/// bytes/len must be the exact still-owned pair returned by get; call once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_source_cache_free(bytes: *mut u8, len: u64) {
    if bytes.is_null() { return; }
    if let Ok(length) = usize::try_from(len) {
        unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes, length))); }
    }
}

/// Diagnostic counters, separate from byte-identical source-document responses.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_source_cache_stats(handle: u64) -> *mut c_char {
    reply(|| {
        let cache = cache(handle)?;
        let cache = cache.lock().ok()?;
        let s = cache.stats();
        string_out(&format!("{{\"source_bytes_read\":\"{}\",\"lexer_calls\":\"{}\",\"ram_hits\":\"{}\",\"disk_hits\":\"{}\",\"misses\":\"{}\",\"corrupt\":\"{}\",\"writes\":\"{}\",\"write_refusals\":\"{}\",\"ram_bytes\":\"{}\",\"disk_bytes\":\"{}\"}}",
            s.source_bytes_read, s.lexer_calls, s.ram_hits, s.disk_hits, s.misses,
            s.corrupt, s.writes, s.write_refusals, cache.ram_bytes(), cache.disk_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_handles_and_invalid_buffers_fail_without_dereference() {
        assert_eq!(unsafe { fcb_source_cache_open(std::ptr::null()) }, 0);
        assert!(!fcb_source_cache_close(u64::MAX));
        assert!(unsafe { fcb_source_document_cached(u64::MAX, std::ptr::null(), std::ptr::null_mut()) }.is_null());
        let mut length = 99;
        assert!(unsafe { fcb_source_cache_get(u64::MAX, std::ptr::null(), &mut length) }.is_null());
        assert_eq!(length, 0);
        assert!(!unsafe { fcb_source_cache_put(u64::MAX, std::ptr::null(), std::ptr::null(), 1) });
        unsafe { fcb_source_cache_free(std::ptr::null_mut(), 0); }
    }
}
