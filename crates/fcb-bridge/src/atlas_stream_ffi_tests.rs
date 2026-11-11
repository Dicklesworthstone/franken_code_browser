#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
use crate::reader_ffi::FFI_TEST_LOCK;
use std::{ffi::{CStr, CString}, fs, os::unix::ffi::OsStrExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a complete response");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    text
}
fn field(json: &str, name: &str) -> u64 {
    let prefix = format!("\"{name}\":\"");
    json.split_once(prefix.as_str()).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fcb-stream-ffi-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> u64 {
        // The retained grant may canonicalize an otherwise authorized spelling.
        let root = CString::new(self.0.join(".").to_str().unwrap()).unwrap();
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        let opened = take(unsafe { fcb_atlas_open(handle, root.as_ptr(), 20_000) });
        assert!(opened.contains("\"status\":\"ok\""), "{opened}");
        let canonical = fs::canonicalize(&self.0).unwrap();
        let hex: String = canonical.as_os_str().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(opened.contains("\"root_path\":"));
        assert_eq!(field(&opened, "source_manifest"), 1);
        assert!(opened.contains(&format!("\"hex\":\"{hex}\"")), "{opened}");
        handle
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn search(handle: u64, needle: &str, matches: u64, bytes: u64, calls: u64) -> String {
    let needle = CString::new(needle).unwrap();
    take(unsafe { fcb_atlas_stream_search_cancelable(handle, needle.as_ptr(), matches, bytes,
        calls, None, std::ptr::null_mut()) })
}

#[test]
fn c_stream_finds_large_utf8_and_bom_utf16_files_and_keeps_exact_literal_after_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let far = 1024 * 1024 + 17;
    let mut large = vec![b' '; far]; large.extend_from_slice(b"needle\n");
    fs::write(fixture.0.join("a.rs"), large).unwrap();
    let utf16: Vec<u8> = "\u{feff}x needle".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fs::write(fixture.0.join("b.rs"), utf16).unwrap();
    let handle = fixture.open();
    let result = search(handle, "needle", 256, 4 * 1024 * 1024, 4096);
    assert!(result.contains("\"schema\":\"fcb.atlas-stream-search/1\""), "{result}");
    assert!(result.contains("\"search_complete\":true"), "{result}");
    assert!(result.contains("\"capture_kind\":\"literal-witnesses-only\""));
    assert_eq!(field(&result, "retained_hits"), 2);
    let generation = field(&result, "query_generation");
    assert!(result.contains(&format!("\"original_range\":{{\"start\":\"{far}\",\"end\":\"{}\"}}", far + 6)));
    let second = take(fcb_atlas_stream_search_hit(handle, generation, 1));
    assert!(second.contains("\"original_hex\":\"006e006500650064006c0065\""), "{second}");
    fs::rename(fixture.0.join("a.rs"), fixture.0.join("retired.rs")).unwrap();
    fs::write(fixture.0.join("a.rs"), b"replacement object").unwrap();
    let retained = take(fcb_atlas_stream_search_hit(handle, generation, 0));
    assert!(retained.contains("\"original_hex\":\"6e6565646c65\""), "{retained}");
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(retained.contains("\"additional_read_calls\":\"0\""));
    let page = take(fcb_atlas_stream_search_page(handle, generation, 1, 1));
    assert!(page.contains("b.rs"), "{page}");
    assert!(page.contains("\"next_offset\":null"));
    assert_eq!(fcb_atlas_close(handle), 1);
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("ATLAS_HANDLE_UNKNOWN"));
}

unsafe extern "C" fn canceled(_: *mut c_void) -> i32 { 1 }

#[test]
fn c_stream_rejects_foreign_input_and_caps_without_losing_previous_witness() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.rs"), "needle ".repeat(40)).unwrap();
    let handle = fixture.open();
    let accepted = search(handle, "needle", 256, 4096, 4096);
    let generation = field(&accepted, "query_generation");
    assert_eq!(field(&accepted, "retained_hits"), 40);
    assert!(accepted.contains("\"next_offset\":\"32\""));
    let needle = CString::new("needle").unwrap();
    let overlong = [b'x'; MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES + 1]; // No NUL, still bounded.
    let invalid_utf8 = [255u8, 0];
    for pointer in [std::ptr::null(), overlong.as_ptr().cast(), invalid_utf8.as_ptr().cast()] {
        let result = take(unsafe { fcb_atlas_stream_search_cancelable(handle, pointer,
            256, 4096, 4096, None, std::ptr::null_mut()) });
        assert!(result.contains("ATLAS_HANDLE_INVALID_ARGUMENT"), "{result}");
    }
    for (matches, bytes, calls) in [(0, 4096, 4096), (257, 4096, 4096),
        (1, 0, 1), (1, (4u64 << 30) + 1, 1), (1, 4096, 0), (1, 4096, 1_048_577)] {
        let rejected = search(handle, "needle", matches, bytes, calls);
        assert!(rejected.contains("\"status\":\"error\""), "{rejected}");
    }
    let stopped = take(unsafe { fcb_atlas_stream_search_cancelable(handle, needle.as_ptr(),
        256, 4096, 4096, Some(canceled), std::ptr::null_mut()) });
    assert!(stopped.contains("ATLAS_HANDLE_CANCELED"), "{stopped}");
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("6e6565646c65"));
    for (start, count) in [(0, 0), (0, 33), (u64::MAX, 1)] {
        assert!(take(fcb_atlas_stream_search_page(handle, generation, start, count))
            .contains("\"status\":\"error\""));
    }
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 40)).contains("\"status\":\"error\""));
    let limited = search(handle, "needle", 1, 4096, 4096);
    assert!(limited.contains("\"search_complete\":false"));
    assert_eq!(field(&limited, "retained_hits"), 1);
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("\"status\":\"error\""));
    let current = field(&limited, "query_generation");
    assert!(take(fcb_atlas_stream_search_clear(handle)).contains("\"status\":\"ok\""));
    assert!(take(fcb_atlas_stream_search_hit(handle, current, 0)).contains("\"status\":\"error\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_stream_public_signatures_and_header_are_consistent() {
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_cancelable;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        Option<SearchCancellationCallback>, Option<SearchCancellationCallback>,
        Option<AtlasStreamProgressCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_progress;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        *const c_char, *const c_char, u64, *const c_char,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_scoped_cancelable;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        *const c_char, *const c_char, u64, *const c_char,
        Option<SearchCancellationCallback>, Option<SearchCancellationCallback>,
        Option<AtlasStreamProgressCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_scoped_progress;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        Option<SearchCancellationCallback>, Option<SearchCancellationCallback>,
        Option<AtlasStreamProgressCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_counted_progress;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64, u64,
        *const c_char, *const c_char, u64, *const c_char,
        Option<SearchCancellationCallback>, Option<SearchCancellationCallback>,
        Option<AtlasStreamProgressCallback>, *mut c_void) -> *mut c_char = fcb_atlas_stream_search_scoped_counted_progress;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_stream_search_page;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_atlas_stream_search_hit;
    let _: extern "C" fn(u64) -> *mut c_char = fcb_atlas_stream_search_clear;
    let header = include_str!("../include/fcb_atlas_stream_search.h");
    for name in ["fcb_atlas_stream_search_cancelable(", "fcb_atlas_stream_search_progress(",
        "fcb_atlas_stream_search_scoped_cancelable(", "fcb_atlas_stream_search_scoped_progress(",
        "fcb_atlas_stream_search_counted_progress(", "fcb_atlas_stream_search_scoped_counted_progress(",
        "fcb_atlas_stream_search_page(",
        "fcb_atlas_stream_search_hit(", "fcb_atlas_stream_search_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn c_scoped_stream_intersects_types_and_utf8_directory_before_global_limits() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a-excluded.rs"), vec![b'x'; 1024 * 1024]).unwrap();
    fs::create_dir_all(fixture.0.join("src/é")).unwrap();
    fs::write(fixture.0.join("src/é/a.rs"), b"needle").unwrap();
    fs::write(fixture.0.join("src/é/b.md"), b"needle").unwrap();
    fs::write(fixture.0.join("src/é/c.RS"), b"needle").unwrap();
    fs::write(fixture.0.join("src/é-other.rs"), b"needle").unwrap();
    let handle = fixture.open();
    let needle = CString::new("needle").unwrap(); let extensions = CString::new("RS").unwrap();
    let path = CString::new("src/é").unwrap();
    let result = take(unsafe { fcb_atlas_stream_search_scoped_cancelable(handle, needle.as_ptr(), 256, 12, 2,
        extensions.as_ptr(), std::ptr::null(), 1, path.as_ptr(), None, std::ptr::null_mut()) });
    assert!(result.contains("\"status\":\"ok\""), "{result}");
    assert!(result.contains("\"search_complete\":true"));
    assert_eq!(field(&result, "catalogued_files"), 5);
    assert_eq!(field(&result, "eligible_files"), 2); assert_eq!(field(&result, "excluded_files"), 3);
    assert_eq!(field(&result, "files_examined"), 2); assert_eq!(field(&result, "files_unexamined"), 0);
    assert_eq!(field(&result, "payload_bytes_read"), 12); assert_eq!(field(&result, "read_calls"), 2);
    assert_eq!(field(&result, "retained_hits"), 2);
    let generation = field(&result, "query_generation");
    for response in [result, take(fcb_atlas_stream_search_page(handle, generation, 0, 32)),
        take(fcb_atlas_stream_search_hit(handle, generation, 0))] {
        assert!(response.contains("\"search_scope\":\"file-types\""));
        assert!(response.contains("\"scope_extensions\":\"rs\""));
        assert!(response.contains("\"scope_path_kind\":\"directory\""));
        assert!(response.contains("\"scope_path_hex\":\"7372632fc3a9\""));
        assert!(response.contains("\"case_sensitive\":true"));
        assert_eq!(field(&response, "eligible_files"), 2);
        assert_eq!(field(&response, "excluded_files"), 3);
    }
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_scoped_stream_bounds_foreign_scope_strings_and_preserves_explicit_empty_membership() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); fs::write(fixture.0.join("a.rs"), b"needle").unwrap();
    let handle = fixture.open();
    let previous = search(handle, "needle", 256, 4096, 4096); let generation = field(&previous, "query_generation");
    let needle = CString::new("needle").unwrap(); let empty = CString::new("").unwrap();
    let invalid_path = CString::new("../a.rs").unwrap(); let invalid_region = CString::new("612e7273,612e7273").unwrap();
    let list_overflow = [b'x'; MAX_SEARCH_SCOPE_LIST_BYTES + 1];
    let path_overflow = [b'x'; MAX_SEARCH_SCOPE_PATH_BYTES + 1];
    let region_overflow = vec![b'1'; MAX_SEARCH_REGION_WIRE_BYTES + 1];
    let invalid_utf8 = [255u8, 0];
    for (extensions, filenames, kind, path) in [
        (list_overflow.as_ptr().cast(), std::ptr::null(), 0, std::ptr::null()),
        (std::ptr::null(), list_overflow.as_ptr().cast(), 0, std::ptr::null()),
        (invalid_utf8.as_ptr().cast(), std::ptr::null(), 0, std::ptr::null()),
        (std::ptr::null(), std::ptr::null(), 1, path_overflow.as_ptr().cast()),
        (std::ptr::null(), std::ptr::null(), 1, invalid_utf8.as_ptr().cast()),
        (std::ptr::null(), std::ptr::null(), 1, invalid_path.as_ptr()),
        (std::ptr::null(), std::ptr::null(), 3, invalid_region.as_ptr()),
        (std::ptr::null(), std::ptr::null(), 3, region_overflow.as_ptr().cast()),
        (std::ptr::null(), std::ptr::null(), 1, std::ptr::null()),
        (std::ptr::null(), std::ptr::null(), 2, std::ptr::null()),
        (std::ptr::null(), std::ptr::null(), 3, std::ptr::null()),
        (std::ptr::null(), std::ptr::null(), u64::MAX, std::ptr::null()),
    ] {
        let rejected = take(unsafe { fcb_atlas_stream_search_scoped_cancelable(handle, needle.as_ptr(), 256, 4096, 4096,
            extensions, filenames, kind, path, None, std::ptr::null_mut()) });
        assert!(rejected.contains("\"status\":\"error\""), "{rejected}");
        assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("6e6565646c65"));
    }
    for (extensions, kind, path) in [(empty.as_ptr(), 0, std::ptr::null()),
        (std::ptr::null(), 3, empty.as_ptr())] {
        let response = take(unsafe { fcb_atlas_stream_search_scoped_cancelable(handle, needle.as_ptr(), 256, 4096, 4096,
            extensions, std::ptr::null(), kind, path, None, std::ptr::null_mut()) });
        assert!(response.contains("\"search_complete\":true"), "{response}");
        assert_eq!(field(&response, "eligible_files"), 0); assert_eq!(field(&response, "excluded_files"), 1);
        assert_eq!(field(&response, "payload_bytes_read"), 0); assert_eq!(field(&response, "read_calls"), 0);
    }
    let missing = CString::new("6d697373696e672e7273").unwrap();
    let response = take(unsafe { fcb_atlas_stream_search_scoped_cancelable(handle, needle.as_ptr(), 256, 4096, 4096,
        std::ptr::null(), std::ptr::null(), 3, missing.as_ptr(), None, std::ptr::null_mut()) });
    assert!(response.contains("\"search_complete\":false"));
    assert_eq!(field(&response, "scope_region_missing_files"), 1);
    assert_eq!(field(&response, "eligible_files"), 0); assert_eq!(field(&response, "payload_bytes_read"), 0);
    assert_eq!(fcb_atlas_close(handle), 1);
}


#[derive(Default)]
struct ProgressContext {
    frames: Vec<String>,
    stop: bool,
    canceled: bool,
    cancel_on_match: bool,
    invalid: bool,
}
unsafe extern "C" fn progress_poll(context: *mut c_void) -> i32 {
    let state = unsafe { &*(context.cast::<ProgressContext>()) };
    i32::from(state.canceled || state.invalid)
}
unsafe extern "C" fn progress_stop(context: *mut c_void) -> i32 {
    i32::from(unsafe { &*(context.cast::<ProgressContext>()) }.stop)
}
unsafe extern "C" fn progress_receive(bytes: *const u8, length: u64, context: *mut c_void) {
    let state = unsafe { &mut *(context.cast::<ProgressContext>()) };
    // Never panic across the C boundary, including on a malformed receipt.
    if bytes.is_null() || length == 0 || length > 8 * 1024 * 1024 {
        state.invalid = true; return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
    let Ok(frame) = std::str::from_utf8(bytes) else { state.invalid = true; return; };
    if !frame.contains("\"command\":\"progress\"") ||
        !frame.contains("\"terminal\":false,\"selection_available\":false") ||
        state.frames.len() >= 4 {
        state.invalid = true; return;
    }
    state.frames.push(frame.to_owned());
    if frame.contains("\"retained_hits\":\"1\"") {
        if state.cancel_on_match { state.canceled = true; } else { state.stop = true; }
    }
}
fn progressive(handle: u64, state: &mut ProgressContext) -> String {
    let needle = CString::new("needle").unwrap();
    take(unsafe { fcb_atlas_stream_search_progress(handle, needle.as_ptr(), 256, 4096, 4096,
        Some(progress_poll), Some(progress_stop), Some(progress_receive),
        std::ptr::from_mut(state).cast()) })
}

#[test]
fn c_progress_is_borrowed_finalized_evidence_and_stop_accepts_actionable_partial_results() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.rs"), b"needle\n").unwrap();
    fs::write(fixture.0.join("z.rs"), b"needle\n").unwrap();
    let handle = fixture.open();
    let mut state = ProgressContext::default();
    let accepted = progressive(handle, &mut state);
    assert!(!state.invalid && state.stop && !state.canceled);
    assert_eq!(state.frames.len(), 2);
    assert_eq!(field(&state.frames[0], "retained_hits"), 0);
    assert_eq!(field(&state.frames[1], "retained_hits"), 1);
    assert_eq!(field(&state.frames[0], "progress_sequence"), 1);
    assert_eq!(field(&state.frames[1], "progress_sequence"), 2);
    assert!(accepted.contains("\"status\":\"ok\""), "{accepted}");
    assert!(accepted.contains("\"stop_reason\":\"user-stopped\""), "{accepted}");
    assert!(accepted.contains("\"terminal\":true,\"selection_available\":true"));
    assert!(accepted.contains("\"search_complete\":false"));
    assert_eq!(field(&accepted, "files_examined"), 1);
    assert_eq!(field(&accepted, "files_unexamined"), 1);
    let generation = field(&accepted, "query_generation");
    // The copied callback frame outlives its borrowed buffer and terminal reply.
    assert_eq!(field(&state.frames[1], "query_generation"), generation);
    fs::rename(fixture.0.join("a.rs"), fixture.0.join("retired.rs")).unwrap();
    let retained = take(fcb_atlas_stream_search_hit(handle, generation, 0));
    assert!(retained.contains("\"original_hex\":\"6e6565646c65\""));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    let page = take(fcb_atlas_stream_search_page(handle, generation, 0, 1));
    assert!(page.contains("\"terminal\":true,\"selection_available\":true"));
    assert_eq!(field(&page, "retained_hits"), 1);
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_progress_hard_cancellation_and_invalid_input_preserve_previous_inventory() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.rs"), b"old needle\n").unwrap();
    fs::write(fixture.0.join("z.rs"), b"needle\n").unwrap();
    let handle = fixture.open();
    let previous = search(handle, "old", 256, 4096, 4096);
    let generation = field(&previous, "query_generation");
    let mut state = ProgressContext { cancel_on_match: true, ..Default::default() };
    let result = progressive(handle, &mut state);
    assert!(!state.invalid && state.canceled);
    assert!(result.contains("ATLAS_HANDLE_CANCELED"), "{result}");
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("6f6c64"));
    let mut both = ProgressContext { canceled: true, stop: true, ..Default::default() };
    assert!(progressive(handle, &mut both).contains("ATLAS_HANDLE_CANCELED"));
    assert!(both.frames.is_empty());
    let mut invalid = ProgressContext::default();
    let invalid_result = take(unsafe { fcb_atlas_stream_search_progress(handle, std::ptr::null(),
        256, 4096, 4096, Some(progress_poll), Some(progress_stop), Some(progress_receive),
        std::ptr::from_mut(&mut invalid).cast()) });
    assert!(invalid_result.contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(invalid.frames.is_empty());
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("6f6c64"));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_scoped_progress_stop_and_cancellation_echo_only_the_frozen_eligible_universe() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); fs::write(fixture.0.join("a-excluded.rs"), vec![b'x'; 1024 * 1024]).unwrap();
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/a.rs"), b"needle").unwrap();
    fs::write(fixture.0.join("src/z.rs"), b"needle").unwrap();
    let handle = fixture.open(); let needle = CString::new("needle").unwrap(); let path = CString::new("src").unwrap();
    let run = |state: &mut ProgressContext| take(unsafe {
        fcb_atlas_stream_search_scoped_progress(handle, needle.as_ptr(), 256, 4096, 4096,
            std::ptr::null(), std::ptr::null(), 1, path.as_ptr(),
            Some(progress_poll), Some(progress_stop), Some(progress_receive), std::ptr::from_mut(state).cast())
    });
    let mut state = ProgressContext::default(); let accepted = run(&mut state);
    assert!(!state.invalid && state.stop && !state.canceled); assert_eq!(state.frames.len(), 2);
    assert!(accepted.contains("\"stop_reason\":\"user-stopped\""));
    assert_eq!(field(&accepted, "payload_bytes_read"), 6); assert_eq!(field(&accepted, "files_unexamined"), 1);
    for response in state.frames.iter().chain(std::iter::once(&accepted)) {
        assert_eq!(field(response, "eligible_files"), 2); assert_eq!(field(response, "excluded_files"), 1);
        assert_eq!(field(response, "files_examined") + field(response, "files_unexamined"), 2);
        assert!(response.contains("\"scope_path_kind\":\"directory\""));
        assert!(response.contains("\"scope_path_hex\":\"737263\""));
    }
    let generation = field(&accepted, "query_generation");
    let mut canceled = ProgressContext { cancel_on_match: true, ..Default::default() };
    let rejected = run(&mut canceled);
    assert!(!canceled.invalid && canceled.canceled); assert!(rejected.contains("ATLAS_HANDLE_CANCELED"));
    let retained = take(fcb_atlas_stream_search_hit(handle, generation, 0));
    assert!(retained.contains("\"original_hex\":\"6e6565646c65\""));
    assert!(retained.contains("\"scope_path_hex\":\"737263\""));
    assert_eq!(field(&retained, "eligible_files"), 2);
    assert_eq!(fcb_atlas_close(handle), 1);
}


#[derive(Default)]
struct OneShotCancellation {
    polls: usize,
    cancel_at: Option<usize>,
    first_delivery_poll: Option<usize>,
    callbacks: usize,
    stopped: bool,
}
unsafe extern "C" fn one_shot_poll(context: *mut c_void) -> i32 {
    let state = unsafe { &mut *(context.cast::<OneShotCancellation>()) };
    state.polls += 1;
    i32::from(state.cancel_at == Some(state.polls))
}
unsafe extern "C" fn one_shot_stop(context: *mut c_void) -> i32 {
    i32::from(unsafe { &*(context.cast::<OneShotCancellation>()) }.stopped)
}
unsafe extern "C" fn one_shot_receive(_: *const u8, _: u64, context: *mut c_void) {
    let state = unsafe { &mut *(context.cast::<OneShotCancellation>()) };
    state.callbacks += 1;
    state.first_delivery_poll.get_or_insert(state.polls);
    state.stopped = true;
}

#[test]
fn c_progress_latches_one_shot_cancellation_before_callback_delivery() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.rs"), b"needle\n").unwrap();
    let handle = fixture.open();
    let needle = CString::new("needle").unwrap();
    let run = |state: &mut OneShotCancellation| take(unsafe {
        fcb_atlas_stream_search_progress(handle, needle.as_ptr(), 256, 4096, 4096,
            Some(one_shot_poll), Some(one_shot_stop), Some(one_shot_receive),
            std::ptr::from_mut(state).cast())
    });
    // Find the poll immediately before the initial borrowed callback without
    // coupling this regression to the encoder's internal number of gates.
    let mut probe = OneShotCancellation::default();
    let accepted = run(&mut probe);
    assert_eq!(probe.callbacks, 1);
    assert!(accepted.contains("\"stop_reason\":\"user-stopped\""));
    let generation = field(&accepted, "query_generation");
    let mut once = OneShotCancellation {
        cancel_at: probe.first_delivery_poll, ..Default::default()
    };
    let rejected = run(&mut once);
    assert!(rejected.contains("ATLAS_HANDLE_CANCELED"), "{rejected}");
    assert_eq!(once.callbacks, 0);
    let retained = take(fcb_atlas_stream_search_page(handle, generation, 0, 1));
    assert!(retained.contains("\"status\":\"ok\""), "{retained}");
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_scoped_progress_latches_one_shot_cancellation_before_callback_delivery() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); fs::write(fixture.0.join("a.rs"), b"needle").unwrap();
    let handle = fixture.open(); let needle = CString::new("needle").unwrap(); let path = CString::new("a.rs").unwrap();
    let run = |state: &mut OneShotCancellation| take(unsafe {
        fcb_atlas_stream_search_scoped_progress(handle, needle.as_ptr(), 256, 4096, 4096,
            std::ptr::null(), std::ptr::null(), 2, path.as_ptr(),
            Some(one_shot_poll), Some(one_shot_stop), Some(one_shot_receive), std::ptr::from_mut(state).cast())
    });
    let mut probe = OneShotCancellation::default(); let accepted = run(&mut probe);
    assert_eq!(probe.callbacks, 1); assert!(accepted.contains("\"stop_reason\":\"user-stopped\""));
    let generation = field(&accepted, "query_generation");
    let mut once = OneShotCancellation { cancel_at: probe.first_delivery_poll, ..Default::default() };
    assert!(run(&mut once).contains("ATLAS_HANDLE_CANCELED")); assert_eq!(once.callbacks, 0);
    let previous = take(fcb_atlas_stream_search_page(handle, generation, 0, 1));
    assert!(previous.contains("\"status\":\"ok\""));
    assert!(previous.contains("\"scope_path_kind\":\"file\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_counted_routes_finish_all_matches_beyond_retained_rows_and_preserve_scope() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/a.rs"), b"aaaa").unwrap();
    let utf16: Vec<u8> = "\u{feff}aaa".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(fixture.0.join("src/b.rs"), utf16).unwrap();
    fs::write(fixture.0.join("z-excluded.rs"), b"aaaa").unwrap();
    let handle = fixture.open();
    let legacy = search(handle, "aa", 1, 4096, 4096);
    assert!(legacy.contains("\"count_all_matches\":false"));
    assert_eq!(field(&legacy, "matches_seen"), 2);
    assert!(legacy.contains("\"stop_reason\":\"match-limit\""));
    let needle = CString::new("aa").unwrap();
    let result = take(unsafe {
        fcb_atlas_stream_search_counted_progress(handle, needle.as_ptr(), 1, 4096, 4096,
            None, None, None, std::ptr::null_mut())
    });
    assert!(result.contains("\"status\":\"ok\""), "{result}");
    assert!(result.contains("\"search_complete\":true"));
    assert!(result.contains("\"truncated\":true"));
    assert!(result.contains("\"stop_reason\":null"));
    assert_eq!(field(&result, "matches_seen"), 8);
    assert_eq!(field(&result, "matching_files"), 3);
    assert_eq!(field(&result, "retained_hits"), 1);
    assert_eq!(field(&result, "retained_witness_bytes"), 2);
    let generation = field(&result, "query_generation");
    for receipt in [result, take(fcb_atlas_stream_search_page(handle, generation, 0, 1)),
        take(fcb_atlas_stream_search_hit(handle, generation, 0))] {
        assert!(receipt.contains("\"count_all_matches\":true"), "{receipt}");
        assert!(receipt.contains("\"match_count_complete\":true"), "{receipt}");
        assert_eq!(field(&receipt, "retained_matching_files"), 1);
    }
    let path = CString::new("src").unwrap();
    let scoped = take(unsafe {
        fcb_atlas_stream_search_scoped_counted_progress(handle, needle.as_ptr(), 1, 4096, 4096,
            std::ptr::null(), std::ptr::null(), 1, path.as_ptr(),
            None, None, None, std::ptr::null_mut())
    });
    assert!(scoped.contains("\"search_complete\":true"), "{scoped}");
    assert!(scoped.contains("\"match_count_complete\":true"));
    assert!(scoped.contains("\"scope_path_hex\":\"737263\""));
    assert_eq!(field(&scoped, "matches_seen"), 5);
    assert_eq!(field(&scoped, "matching_files"), 2);
    assert_eq!(field(&scoped, "retained_matching_files"), 1);
    assert_eq!(field(&scoped, "eligible_files"), 2); assert_eq!(field(&scoped, "excluded_files"), 1);
    assert_eq!(field(&scoped, "payload_bytes_read"), 12);
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_counted_progress_stop_and_cancel_remain_distinct_after_retention_fills() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/a.rs"), b"aaaa").unwrap();
    fs::write(fixture.0.join("src/b.rs"), b"aaaa").unwrap();
    let handle = fixture.open(); let needle = CString::new("aa").unwrap(); let path = CString::new("src").unwrap();
    let run = |state: &mut ProgressContext| take(unsafe {
        fcb_atlas_stream_search_scoped_counted_progress(handle, needle.as_ptr(), 1, 4096, 4096,
            std::ptr::null(), std::ptr::null(), 1, path.as_ptr(),
            Some(progress_poll), Some(progress_stop), Some(progress_receive), std::ptr::from_mut(state).cast())
    });
    let mut stopped = ProgressContext::default(); let accepted = run(&mut stopped);
    assert!(!stopped.invalid && stopped.stop && !stopped.canceled); assert_eq!(stopped.frames.len(), 2);
    assert!(accepted.contains("\"stop_reason\":\"user-stopped\""), "{accepted}");
    assert!(accepted.contains("\"truncated\":true"));
    assert_eq!(field(&accepted, "matches_seen"), 3);
    assert_eq!(field(&accepted, "retained_hits"), 1);
    assert_eq!(field(&accepted, "files_examined"), 1);
    assert_eq!(field(&accepted, "files_unexamined"), 1);
    for frame in stopped.frames.iter().chain(std::iter::once(&accepted)) {
        assert!(frame.contains("\"count_all_matches\":true"));
        assert!(frame.contains("\"match_count_complete\":false"));
        assert!(frame.contains("\"scope_path_hex\":\"737263\""));
    }
    let generation = field(&accepted, "query_generation");
    let mut aborted = ProgressContext { cancel_on_match: true, ..Default::default() };
    let rejected = run(&mut aborted);
    assert!(!aborted.invalid && aborted.canceled);
    assert!(rejected.contains("ATLAS_HANDLE_CANCELED"), "{rejected}");
    let retained = take(fcb_atlas_stream_search_hit(handle, generation, 0));
    assert!(retained.contains("\"original_hex\":\"6161\""));
    assert!(retained.contains("\"count_all_matches\":true"));
    let invalid = take(unsafe {
        fcb_atlas_stream_search_counted_progress(handle, std::ptr::null(), 1, 4096, 4096,
            None, None, None, std::ptr::null_mut())
    });
    assert!(invalid.contains("ATLAS_HANDLE_INVALID_ARGUMENT"), "{invalid}");
    assert!(take(fcb_atlas_stream_search_hit(handle, generation, 0)).contains("\"original_hex\":\"6161\""));
    assert_eq!(fcb_atlas_close(handle), 1);
}
