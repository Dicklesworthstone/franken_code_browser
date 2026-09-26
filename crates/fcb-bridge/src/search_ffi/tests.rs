//! Production FFI and engine tests; not a native UI responsiveness qualification.
use super::*;
use std::{ffi::{CStr, CString}, fs, path::PathBuf,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering}, time::{SystemTime, UNIX_EPOCH}};

struct Cancellation { polls: AtomicUsize, stop_after: usize }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    // All callers below retain this context on their stack through the call.
    let state = unsafe { &*context.cast::<Cancellation>() };
    if state.polls.fetch_add(1, Ordering::Relaxed) + 1 >= state.stop_after { 1 } else { 0 }
}
fn context(state: &Cancellation) -> *mut c_void {
    std::ptr::from_ref(state).cast_mut().cast()
}
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-cancel-search-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.rs"), b"needle first\nneedle second\n").unwrap();
    root
}
fn take(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() { return None; }
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    Some(text)
}
fn search(root: &Path, state: &Cancellation) -> Option<String> {
    let root = CString::new(root.to_str().unwrap()).unwrap();
    let query = CString::new("needle").unwrap();
    take(unsafe { fcb_search_workspace_cancelable(root.as_ptr(), query.as_ptr(), Some(poll), context(state)) })
}

#[test]
fn permitted_callback_route_is_identical_to_legacy_search() {
    let root = root();
    let state = Cancellation { polls: AtomicUsize::new(0), stop_after: usize::MAX };
    let result = search(&root, &state).unwrap();
    assert_eq!(result, host::search_workspace(&root, "needle", || false).unwrap().as_str());
    assert!(state.polls.load(Ordering::Relaxed) > 2, "engine must receive the cancellation callback");
}

#[test]
fn null_callback_preserves_the_existing_search_contract() {
    let root = root(); let root = CString::new(root.to_str().unwrap()).unwrap();
    let query = CString::new("needle").unwrap();
    let legacy = take(unsafe { crate::fcb_search_workspace(root.as_ptr(), query.as_ptr()) });
    let result = take(unsafe { fcb_search_workspace_cancelable(root.as_ptr(), query.as_ptr(), None, std::ptr::null_mut()) });
    assert_eq!(result, legacy); assert!(result.is_some());
}

#[test]
fn cancellation_at_engine_and_final_handoff_boundaries_discards_results() {
    let root = root();
    let probe = Cancellation { polls: AtomicUsize::new(0), stop_after: usize::MAX };
    assert!(search(&root, &probe).is_some());
    let total = probe.polls.load(Ordering::Relaxed);
    // The last checkpoint exercises cancellation AFTER host response creation.
    for stop_after in [1, 2, total / 2, total] {
        let state = Cancellation { polls: AtomicUsize::new(0), stop_after };
        assert!(search(&root, &state).is_none(), "checkpoint {stop_after}/{total}");
        assert!(state.polls.load(Ordering::Relaxed) >= stop_after);
    }
    assert!(search(&root, &Cancellation { polls: AtomicUsize::new(0), stop_after: usize::MAX }).is_some());
}

#[test]
fn cancellation_before_admission_does_not_open_a_missing_root() {
    let root = root().join("not-present");
    let state = Cancellation { polls: AtomicUsize::new(0), stop_after: 1 };
    assert!(search(&root, &state).is_none());
    assert_eq!(state.polls.load(Ordering::Relaxed), 1);
    assert!(!root.exists());
}

#[test]
fn null_and_invalid_utf8_are_not_empty_successes() {
    let invalid = CString::new(vec![0xff]).unwrap(); let query = CString::new("needle").unwrap();
    unsafe {
        assert!(fcb_search_workspace_cancelable(std::ptr::null(), query.as_ptr(), None, std::ptr::null_mut()).is_null());
        assert!(fcb_search_workspace_cancelable(invalid.as_ptr(), query.as_ptr(), None, std::ptr::null_mut()).is_null());
    }
}

#[test]
fn independent_native_calls_do_not_share_cancellation_state() {
    let root = root();
    std::thread::scope(|scope| {
        let canceled = scope.spawn(|| search(&root, &Cancellation { polls: AtomicUsize::new(0), stop_after: 2 }));
        let accepted = scope.spawn(|| search(&root, &Cancellation { polls: AtomicUsize::new(0), stop_after: usize::MAX }));
        assert!(canceled.join().unwrap().is_none());
        assert!(accepted.join().unwrap().unwrap().contains("\"status\":\"ok\""));
    });
}

#[test]
fn utf16_and_partial_coverage_stay_owned_by_the_shared_engine() {
    let root = root(); let mut bytes = vec![0xff, 0xfe];
    for unit in "needle😀\r\n".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(root.join("utf16.txt"), bytes).unwrap();
    fs::write(root.join("malformed.txt"), [0xff, 0, 0xff]).unwrap();
    let result = search(&root, &Cancellation { polls: AtomicUsize::new(0), stop_after: usize::MAX }).unwrap();
    assert_eq!(result, host::search_workspace(&root, "needle", || false).unwrap().as_str());
    assert!(result.contains("utf16.txt"));
}
