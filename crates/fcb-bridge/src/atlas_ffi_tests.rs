#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::reader_ffi::{fcb_reader_create, fcb_reader_copy_range, fcb_reader_close, fcb_reader_info, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("fcb-atlas-ffi-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap(); fs::write(root.join("a.rs"), b"needle\n").unwrap(); Self(root)
    }
    fn open(&self) -> u64 {
        let a = fcb_atlas_create(); assert_ne!(a, 0);
        let root = CString::new(self.0.to_str().unwrap()).unwrap();
        let reply = take(unsafe { fcb_atlas_open(a, root.as_ptr(), 100) });
        assert!(reply.contains("\"status\":\"ok\""), "{reply}"); a
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn c_atlas_reader_journey_uses_retained_map_and_original_captured_bytes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let fixture = Fixture::new(); let a = fixture.open();
    assert!(take(fcb_atlas_view(a, 1)).contains("\"native_presented\":false"));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("ATLAS_SESSION_NOT_PRESENTED"));
    assert!(take(fcb_atlas_present(a, 1, 1, 1)).contains("host-declared-not-observed"));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("\"can_open_source\":true"));
    let r = fcb_reader_create(); assert_ne!(r, 0); assert_ne!(a, r);
    let receipt = take(fcb_atlas_open_reader(a, r, 1, 1, 512.0, 384.0, 1024));
    assert!(receipt.contains(&format!("\"reader_owner\":\"{r}\"")), "{receipt}");
    fs::write(fixture.0.join("a.rs"), b"changed").unwrap();
    let pending = take(fcb_atlas_pan(a, 2, 10000.0, 10000.0));
    assert!(pending.contains("\"parcels\":[]"));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("\"can_open_source\":true"));
    take(fcb_atlas_present(a, 2, 2, 1));
    assert!(take(fcb_atlas_pick(a, 1, 1, 512.0, 384.0)).contains("ATLAS_SESSION_STALE_FRAME"));
    assert_eq!(fcb_atlas_close(a), 1);
    assert!(take(fcb_reader_copy_range(r, 0, 6)).contains("6e6565646c65"));
    assert_eq!(fcb_reader_close(r), 1);
}

#[test]
fn c_invalid_inputs_and_wrong_handle_kinds_are_explicit_errors() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let fixture = Fixture::new();
    let a = fcb_atlas_create(); assert_ne!(a, 0);
    assert!(take(unsafe { fcb_atlas_open(a, std::ptr::null(), 100) }).contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_atlas_info(a)).contains("ATLAS_HANDLE_NOT_OPEN"));
    let root = CString::new(fixture.0.to_str().unwrap()).unwrap();
    take(unsafe { fcb_atlas_open(a, root.as_ptr(), 100) });
    assert!(take(fcb_atlas_pan(a, 1, f64::NAN, 0.0)).contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_atlas_focus(a, 2, u64::MAX)).contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_atlas_children(a, 0, 0, 0)).contains("ATLAS_SESSION_INVALID_LIMITS"));
    assert!(take(fcb_reader_info(a)).contains("READER_HANDLE_UNKNOWN"));
    let r = fcb_reader_create(); assert_ne!(r, 0);
    assert!(take(fcb_atlas_info(r)).contains("ATLAS_HANDLE_UNKNOWN"));
    assert_eq!(fcb_reader_close(r), 1); assert_eq!(fcb_atlas_close(a), 1);
}

#[test]
fn c_cancel_and_close_do_not_free_already_returned_strings() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let fixture = Fixture::new(); let a = fixture.open();
    assert_eq!(fcb_atlas_cancel(a), 1);
    let pointer = fcb_atlas_view(a, 1);
    assert_eq!(fcb_atlas_close(a), 1);
    assert!(take(pointer).contains("\"plan_generation\":\"1\""));
    assert_eq!(fcb_atlas_close(a), 0); assert_eq!(fcb_atlas_cancel(a), 0);
    assert!(take(fcb_atlas_info(a)).contains("ATLAS_HANDLE_UNKNOWN"));
}

#[test]
fn c_atlas_scope_filters_display_and_labels_responses() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-atlas-scope-ffi-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"rust\n").unwrap();
    fs::write(root.join("b.md"), b"markdown\n").unwrap();
    let fixture = Fixture(root);
    let a = {
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        let croot = CString::new(fixture.0.to_str().unwrap()).unwrap();
        let reply = take(unsafe { fcb_atlas_open(handle, croot.as_ptr(), 100) });
        assert!(reply.contains("\"catalogued_files\":\"2\""), "{reply}"); handle
    };
    // Invalid scope names and malformed custom lists are explicit refusals.
    let bogus = CString::new("bogus").unwrap();
    assert!(take(unsafe { fcb_atlas_scope(a, 1, bogus.as_ptr(), std::ptr::null()) })
        .contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    let empty = CString::new("extensions").unwrap();
    let blank = CString::new("").unwrap();
    assert!(take(unsafe { fcb_atlas_scope(a, 1, empty.as_ptr(), blank.as_ptr()) })
        .contains("ATLAS_HANDLE_INVALID_ARGUMENT"));

    let original = take(fcb_atlas_info(a));
    let rust = CString::new("rust").unwrap();
    let scoped = take(unsafe { fcb_atlas_scope(a, 2, rust.as_ptr(), std::ptr::null()) });
    assert!(scoped.contains("\"catalogued_files\":\"1\""), "{scoped}");
    assert!(scoped.contains("\"scope\":{\"kind\":\"extensions\",\"extensions\":[\"rs\"]}"));
    assert!(scoped.contains("\"workspace_files\":\"2\""));
    // Restoring All reuses the retained original layout revision, while the
    // scoped plan reported a fresh, distinct revision.
    let all = CString::new("all").unwrap();
    let restored = take(unsafe { fcb_atlas_scope(a, 3, all.as_ptr(), std::ptr::null()) });
    assert!(restored.contains("\"catalogued_files\":\"2\""));
    let after = take(fcb_atlas_info(a));
    let revision = |text: &str| text[text.find("\"layout_revision\":").unwrap()
        + "\"layout_revision\":".len()..].split(',').next().unwrap().to_string();
    assert_eq!(revision(&after), revision(&original));
    assert_ne!(revision(&scoped), revision(&original));

    // Custom extension lists parse exactly; case folds are applied.
    let custom = CString::new("extensions").unwrap();
    let list = CString::new("MD,TXT").unwrap();
    let mixed = take(unsafe { fcb_atlas_scope(a, 4, custom.as_ptr(), list.as_ptr()) });
    assert!(mixed.contains("\"extensions\":[\"md\",\"txt\"]"), "{mixed}");
    assert_eq!(fcb_atlas_close(a), 1);
}

#[test]
fn c_atlas_function_types_match_header() {
    let _: extern "C" fn() -> u64 = fcb_atlas_create;
    let _: unsafe extern "C" fn(u64, *const c_char, u64) -> *mut c_char = fcb_atlas_open;
    let _: unsafe extern "C" fn(u64, *const c_char, u64,
        Option<crate::search_ffi::SearchCancellationCallback>, *mut std::ffi::c_void) -> *mut c_char
        = fcb_atlas_open_cancelable;
    let _: unsafe extern "C" fn(u64, *const c_char, u64, u64,
        Option<crate::search_ffi::SearchCancellationCallback>, *mut std::ffi::c_void) -> *mut c_char
        = fcb_atlas_open_with_policy_cancelable;
    let header = include_str!("../include/fcb_atlas_sessions.h");
    for symbol in ["fcb_atlas_open(", "fcb_atlas_open_cancelable(", "fcb_atlas_open_with_policy_cancelable("] {
        assert_eq!(header.matches(symbol).count(), 1, "{symbol}");
    }
    let _: extern "C" fn(u64) -> *mut c_char = fcb_atlas_info;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_view;
    let _: extern "C" fn(u64, u64, f64, f64) -> *mut c_char = fcb_atlas_pan;
    let _: extern "C" fn(u64, u64, f64, f64, f64) -> *mut c_char = fcb_atlas_zoom;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_atlas_focus;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_back;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_forward;
    let _: extern "C" fn(u64, u64, f64, f64, f64) -> *mut c_char = fcb_atlas_resize;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_present;
    let _: extern "C" fn(u64, u64, u64, f64, f64) -> *mut c_char = fcb_atlas_pick;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char) -> *mut c_char = fcb_atlas_scope;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_atlas_children;
    let _: extern "C" fn(u64, u64, u64, u64, f64, f64, u64) -> *mut c_char = fcb_atlas_open_reader;
    let _: extern "C" fn(u64) -> u8 = fcb_atlas_cancel;
    let _: extern "C" fn(u64) -> u8 = fcb_atlas_close;
}


#[test]
fn c_atlas_back_and_forward_restore_camera_endpoints_and_refocus_branches_history() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); let a = fixture.open();
    let original = take(fcb_atlas_view(a, 1));
    let focus = original.find("\"focus\":{\"ordinal\":").unwrap() + "\"focus\":{\"ordinal\":".len();
    let root = original[focus..].split('"').nth(1).unwrap().parse::<u64>().unwrap();
    let geometry = |text: &str| {
        ["origin_x", "origin_y", "points_per_unit"].map(|key| {
            let prefix = format!("\"{key}\":");
            let start = text.find(&prefix).unwrap() + prefix.len();
            text[start..].split(',').next().unwrap().to_string()
        })
    };
    let panned = take(fcb_atlas_pan(a, 2, 25.0, 50.0));
    assert_ne!(geometry(&original), geometry(&panned));
    let focused = take(fcb_atlas_focus(a, 3, root));
    assert_eq!(geometry(&focused), geometry(&original));
    let back = take(fcb_atlas_back(a, 4));
    assert_eq!(geometry(&back), geometry(&panned));
    assert!(take(fcb_atlas_info(a)).contains("\"forward_history_depth\":\"1\""));
    let forward = take(fcb_atlas_forward(a, 5));
    assert_eq!(geometry(&forward), geometry(&original));
    assert!(forward.contains("\"plan_generation\":\"5\""));

    take(fcb_atlas_back(a, 6));
    take(fcb_atlas_focus(a, 7, root));
    assert!(take(fcb_atlas_forward(a, 8)).contains("ATLAS_SESSION_EMPTY_HISTORY"));
    assert_eq!(fcb_atlas_close(a), 1);
}

struct OpenPoll {
    polls: AtomicU64,
    stop_at: u64,
}
unsafe extern "C" fn poll_open(context: *mut std::ffi::c_void) -> i32 {
    let state = unsafe { &*context.cast::<OpenPoll>() };
    i32::from(state.polls.fetch_add(1, Ordering::Relaxed) + 1 >= state.stop_at)
}
fn open_poll_context(state: &OpenPoll) -> *mut std::ffi::c_void {
    std::ptr::from_ref(state).cast_mut().cast()
}

#[test]
fn c_policy_open_search_and_reader_preserve_scope_and_exact_captured_source() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join(".gitignore"), b"ignored.rs\n").unwrap();
    fs::write(fixture.0.join(".fcbignore"), b"custom.rs\n").unwrap();
    fs::write(fixture.0.join("ignored.rs"), b"hidden needle\n").unwrap();
    fs::write(fixture.0.join("custom.rs"), b"custom needle\n").unwrap();
    fs::create_dir(fixture.0.join("target")).unwrap();
    fs::write(fixture.0.join("target/generated.rs"), b"generated needle\n").unwrap();
    std::os::unix::fs::symlink("ignored.rs", fixture.0.join("alias.rs")).unwrap();
    let root = CString::new(fixture.0.to_str().unwrap()).unwrap();
    let legacy = fixture.open();
    let compatible = fcb_atlas_create(); assert_ne!(compatible, 0);
    let compatibility = take(unsafe {
        fcb_atlas_open_cancelable(compatible, root.as_ptr(), 100, None, std::ptr::null_mut())
    });
    assert!(compatibility.contains("\"discovery_policy\":\"repository-rules-v1"));
    let included = fcb_atlas_create(); assert_ne!(included, 0);
    let opened = take(unsafe {
        fcb_atlas_open_with_policy_cancelable(included, root.as_ptr(), 100, 1, None, std::ptr::null_mut())
    });
    assert!(opened.contains("\"discovery_policy\":\"all-regular-files/no-rule-files-v1\""));
    assert!(opened.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    let query = CString::new("needle").unwrap();
    for (handle, hits) in [(legacy, 1), (compatible, 1), (included, 4)] {
        let found = take(unsafe {
            super::search::fcb_atlas_search(handle, 1, query.as_ptr(), 16, 100, 1024, 8192)
        });
        assert!(found.contains(&format!("\"retained_hits\":\"{hits}\"")), "{found}");
        if handle == included {
            assert!(found.contains("\"discovery_policy\":\"all-regular-files/no-rule-files-v1\""));
            assert!(found.contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
            assert!(!found.contains("\"display\":\"alias.rs\""), "include-excluded must not follow symlinks");
        } else {
            assert!(found.contains("\"discovery_policy\":\"repository-rules-v1"));
            assert!(found.contains("\"rule_files_enabled\":true"));
        }
    }
    let hidden = CString::new("hidden needle").unwrap();
    let found = take(unsafe {
        super::search::fcb_atlas_search(included, 2, hidden.as_ptr(), 16, 100, 1024, 8192)
    });
    assert!(found.contains("\"retained_hits\":\"1\""), "{found}");
    assert!(found.contains("\"display\":\"ignored.rs\""), "{found}");
    fs::write(fixture.0.join("ignored.rs"), b"changed current source").unwrap();
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let opened_reader = take(super::search::fcb_atlas_search_open_reader(included, reader, 2, 1));
    assert!(opened_reader.contains("\"source_reopened\":false"), "{opened_reader}");
    assert_eq!(fcb_atlas_close(included), 1);
    assert!(take(fcb_reader_copy_range(reader, 0, 14)).contains("68696464656e206e6565646c650a"));
    assert!(take(super::search::fcb_atlas_search_page(legacy, 1, 0, 16)).contains("\"retained_hits\":\"1\""));
    assert_eq!(fcb_reader_close(reader), 1);
    assert_eq!(fcb_atlas_close(compatible), 1); assert_eq!(fcb_atlas_close(legacy), 1);
}

#[test]
fn c_policy_open_refuses_invalid_policy_without_callbacks_or_handle_changes() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); let root = CString::new(fixture.0.to_str().unwrap()).unwrap();
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    let empty = take(fcb_atlas_info(handle));
    let poll = OpenPoll { polls: AtomicU64::new(0), stop_at: u64::MAX };
    for policy in [2, u64::MAX] {
        assert!(unsafe {
            fcb_atlas_open_with_policy_cancelable(handle, std::ptr::null(), 100, policy,
                Some(poll_open), open_poll_context(&poll))
        }.is_null());
    }
    assert_eq!(poll.polls.load(Ordering::Relaxed), 0);
    assert_eq!(take(fcb_atlas_info(handle)), empty);
    for policy in [0, 1] {
        for limit in [0, 20_001, u64::MAX] {
            assert!(unsafe {
                fcb_atlas_open_with_policy_cancelable(handle, root.as_ptr(), limit, policy,
                    None, std::ptr::null_mut())
            }.is_null());
        }
        assert!(unsafe {
            fcb_atlas_open_with_policy_cancelable(handle, std::ptr::null(), 100, policy,
                None, std::ptr::null_mut())
        }.is_null());
        assert_eq!(take(fcb_atlas_info(handle)), empty);
    }
    let opened = take(unsafe {
        fcb_atlas_open_with_policy_cancelable(handle, root.as_ptr(), 100, 0, None, std::ptr::null_mut())
    });
    assert!(opened.contains("\"discovery_policy\":\"repository-rules-v1"));
    let before = take(fcb_atlas_info(handle));
    assert!(unsafe {
        fcb_atlas_open_with_policy_cancelable(handle, root.as_ptr(), 100, 2,
            Some(poll_open), open_poll_context(&poll))
    }.is_null());
    assert_eq!(poll.polls.load(Ordering::Relaxed), 0);
    assert_eq!(take(fcb_atlas_info(handle)), before);
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_policy_open_cancellation_keeps_early_handles_retryable_and_late_handles_owned() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new(); let root = CString::new(fixture.0.to_str().unwrap()).unwrap();
    for policy in [0, 1] {
        let probe = OpenPoll { polls: AtomicU64::new(0), stop_at: u64::MAX };
        let handle = fcb_atlas_create(); assert_ne!(handle, 0);
        take(unsafe {
            fcb_atlas_open_with_policy_cancelable(handle, root.as_ptr(), 100, policy,
                Some(poll_open), open_poll_context(&probe))
        });
        let total = probe.polls.load(Ordering::Relaxed); assert!(total > 2);
        assert_eq!(fcb_atlas_close(handle), 1);
        for stop_at in [1, 2, total] {
            let handle = fcb_atlas_create(); assert_ne!(handle, 0);
            let stop = OpenPoll { polls: AtomicU64::new(0), stop_at };
            assert!(unsafe {
                fcb_atlas_open_with_policy_cancelable(handle, root.as_ptr(), 100, policy,
                    Some(poll_open), open_poll_context(&stop))
            }.is_null());
            let info = take(fcb_atlas_info(handle));
            if stop_at < total {
                assert!(info.contains("ATLAS_HANDLE_NOT_OPEN"), "{info}");
                assert!(take(unsafe {
                    fcb_atlas_open_with_policy_cancelable(handle, root.as_ptr(), 100, policy,
                        None, std::ptr::null_mut())
                }).contains("\"status\":\"ok\""));
            } else {
                assert!(info.contains("\"status\":\"ok\""), "late null is not rollback: {info}");
            }
            assert_eq!(fcb_atlas_close(handle), 1);
        }
    }
}
