//! Production routes, not a native responsiveness or cache durability claim.
use super::*;
use crate::source_cache_ffi as cache_api;
use std::{ffi::{CStr, CString}, fs, path::PathBuf, sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH}};

struct Stop { polls: AtomicUsize, at: usize }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    let state = unsafe { &*context.cast::<Stop>() };
    i32::from(state.polls.fetch_add(1, Ordering::Relaxed) + 1 >= state.at)
}
fn context(state: &Stop) -> *mut c_void { std::ptr::from_ref(state).cast_mut().cast() }
fn fixture() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!("fcb-project-ffi-{}-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.rs"), "\u{feff}fn main() { /* 😀 */ }\r\n").unwrap(); root
}
fn text(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() { return None; }
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; Some(text)
}
fn native(path: &Path) -> CString { CString::new(path.to_str().unwrap()).unwrap() }

#[test]
fn discovery_uses_the_same_geometry_and_never_enables_source_profiles() {
    let root = fixture(); let path = native(&root);
    let stop = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
    let result = text(unsafe { fcb_atlas_layout_cancelable(path.as_ptr(), Some(poll), context(&stop)) }).unwrap();
    assert_eq!(result, text(unsafe { crate::fcb_atlas_layout(path.as_ptr()) }).unwrap());
    assert!(result.contains("\"profile_state\":\"disabled\""));
    assert!(stop.polls.load(Ordering::Relaxed) > 1);
}
#[test]
fn source_document_preserves_exact_source_and_upstream_runs() {
    let root = fixture(); let path = native(&root.join("a.rs"));
    let stop = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
    let result = text(unsafe { fcb_source_document_cancelable(path.as_ptr(), Some(poll), context(&stop)) }).unwrap();
    assert_eq!(result, text(unsafe { crate::fcb_source_document(path.as_ptr()) }).unwrap());
    assert!(result.contains("😀")); assert!(stop.polls.load(Ordering::Relaxed) > 1);
}
#[test]
fn pre_canceled_calls_do_not_open_missing_inputs_or_change_output_keys() {
    let missing = native(&fixture().join("missing"));
    let stop = Stop { polls: AtomicUsize::new(0), at: 1 }; let mut key = [0xa5; 32];
    unsafe {
        assert!(fcb_atlas_layout_cancelable(missing.as_ptr(), Some(poll), context(&stop)).is_null());
        assert!(fcb_source_document_cancelable(missing.as_ptr(), Some(poll), context(&stop)).is_null());
        assert!(fcb_source_document_cached_cancelable(0, missing.as_ptr(), key.as_mut_ptr(), Some(poll), context(&stop)).is_null());
    }
    assert_eq!(key, [0xa5; 32]);
}
#[test]
fn discovery_and_source_cancel_at_final_handoff_as_well_as_engine_work() {
    let root = fixture(); let root_name = native(&root); let file = native(&root.join("a.rs"));
    for catalog in [false, true] {
        let invoke = |stop: &Stop| unsafe {
            if catalog { fcb_atlas_layout_cancelable(root_name.as_ptr(), Some(poll), context(stop)) }
            else { fcb_source_document_cancelable(file.as_ptr(), Some(poll), context(stop)) }
        };
        let probe = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
        assert!(text(invoke(&probe)).is_some()); let total = probe.polls.load(Ordering::Relaxed);
        for at in [1, 2, total] {
            assert!(text(invoke(&Stop { polls: AtomicUsize::new(0), at })).is_none());
        }
    }
}
#[test]
fn cached_callback_preserves_source_key_and_canceled_handoff_leaves_sentinel() {
    let root = fixture(); let directory = native(&root.join("cache")); let file = native(&root.join("a.rs"));
    let handle = unsafe { cache_api::fcb_source_cache_open(directory.as_ptr()) };
    assert_ne!(handle, 0);
    let mut key = [0; 32]; let mut old_key = [0; 32];
    let first = text(unsafe { cache_api::fcb_source_document_cached(handle, file.as_ptr(), old_key.as_mut_ptr()) }).unwrap();
    let probe = Stop { polls: AtomicUsize::new(0), at: usize::MAX };
    let second = text(unsafe { fcb_source_document_cached_cancelable(handle, file.as_ptr(), key.as_mut_ptr(), Some(poll), context(&probe)) }).unwrap();
    assert_eq!(first, second); assert_eq!(key, old_key);
    let stop = Stop { polls: AtomicUsize::new(0), at: probe.polls.load(Ordering::Relaxed) };
    key = [0xa5; 32];
    assert!(unsafe { fcb_source_document_cached_cancelable(handle, file.as_ptr(), key.as_mut_ptr(), Some(poll), context(&stop)) }.is_null());
    assert_eq!(key, [0xa5; 32]);
    assert!(cache_api::fcb_source_cache_close(handle));
}
#[test]
fn invalid_inputs_are_not_successful_empty_documents() {
    let bad = CString::new(vec![0xff]).unwrap(); let mut key = [1; 32];
    unsafe {
        assert!(fcb_atlas_layout_cancelable(std::ptr::null(), None, std::ptr::null_mut()).is_null());
        assert!(fcb_source_document_cancelable(bad.as_ptr(), None, std::ptr::null_mut()).is_null());
        assert!(fcb_source_document_cached_cancelable(u64::MAX, bad.as_ptr(), key.as_mut_ptr(), None, std::ptr::null_mut()).is_null());
    }
    assert_eq!(key, [1; 32]);
}
#[test]
fn two_native_workers_do_not_share_cancellation_state() {
    let root = fixture(); let file = root.join("a.rs");
    std::thread::scope(|scope| {
        let canceled = scope.spawn(|| {
            let path = native(&file); let stop = Stop { polls: AtomicUsize::new(0), at: 1 };
            text(unsafe { fcb_source_document_cancelable(path.as_ptr(), Some(poll), context(&stop)) })
        });
        let normal = scope.spawn(|| {
            let path = native(&file);
            text(unsafe { fcb_source_document_cancelable(path.as_ptr(), None, std::ptr::null_mut()) })
        });
        assert!(canceled.join().unwrap().is_none()); assert!(normal.join().unwrap().is_some());
    });
}
