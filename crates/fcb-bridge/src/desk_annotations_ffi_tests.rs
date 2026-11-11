//! Exact length-prefixed UTF-8 notes, including NUL, through the public C ABI.
use super::*;
use std::{ffi::{CStr, CString}, fs};

fn consume(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn ok(pointer: *mut c_char) -> String {
    let text = consume(pointer); assert!(text.contains("\"status\":\"ok\""), "{text}"); text
}
fn number(text: &str, key: &str) -> u64 {
    let key = format!("\"{key}\":\"");
    text.split_once(&key).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn note_abi_keeps_embedded_nuls_and_refuses_bad_lengths_or_utf8_before_mutation() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let root = std::env::temp_dir().join(format!("fcb-desk-note-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("saved.rs"); fs::write(&source, b"exact").unwrap();
    let path = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = ok(unsafe { fcb_desk_open(desk, 0, 1, path.as_ptr()) });
    let pane = number(&opened, "pane");
    let label = CString::new("Reason").unwrap(); let body = b"line one\nline two\0still present";
    let added = ok(unsafe { fcb_desk_annotate(desk, 1, 2, pane, label.as_ptr(), body.as_ptr(), body.len() as u64) });
    assert!(added.contains("\"body\":\"line one\\nline two\\u0000still present\""));
    let bookmark = number(&added, "bookmark");
    for (pointer, length) in [(std::ptr::null(), 1), (std::ptr::null(), u64::MAX), (b"\xff".as_ptr(), 1)] {
        let refused = consume(unsafe { fcb_desk_edit_bookmark(desk, 2, 3, bookmark, label.as_ptr(), pointer, length) });
        assert!(refused.contains("DESK_HANDLE_INVALID_ARGUMENT"));
        let state = ok(fcb_desk_state(desk));
        assert_eq!(number(&state, "model_revision"), 2);
        assert!(state.contains("still present"));
    }
    let renamed = CString::new("Plain landmark").unwrap();
    let emptied = ok(unsafe { fcb_desk_edit_bookmark(desk, 2, 4, bookmark, renamed.as_ptr(), std::ptr::null(), 0) });
    assert!(emptied.contains("\"body\":\"\""));
    assert!(emptied.contains("\"label\":\"Plain landmark\""));
    assert!(emptied.contains("\"update_sequence\":\"2\""));
    let stale = consume(unsafe { fcb_desk_annotate(desk, 4, 4, pane, label.as_ptr(), body.as_ptr(), body.len() as u64) });
    assert!(stale.contains("DESK_STALE_ATTEMPT"));
    assert_eq!(ok(fcb_desk_state(desk)).matches("\"bookmark\":").count(), 1);
    assert_eq!(fcb_desk_close(desk), 1);
}


#[test]
fn apply_abi_reattaches_to_verified_retained_bytes_without_reopening_the_path() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let root = std::env::temp_dir().join(format!("fcb-desk-reattach-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("source.rs"); fs::write(&source, b"old exact").unwrap();
    let path = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = ok(unsafe { fcb_desk_open(desk, 0, 1, path.as_ptr()) });
    let pane = number(&opened, "pane");
    ok(unsafe { fcb_desk_apply(desk, 1, 2, 2, pane, 4, 9, std::ptr::null()) });
    let label = CString::new("My reason").unwrap(); let body = b"Keep this note\0exactly.";
    let annotated = ok(unsafe { fcb_desk_annotate(desk, 2, 3, pane,
        label.as_ptr(), body.as_ptr(), body.len() as u64) });
    let bookmark = number(&annotated, "bookmark");
    fs::write(&source, b"new prefix exact").unwrap();
    let target = ok(unsafe { fcb_desk_open(desk, 3, 4, path.as_ptr()) });
    let pane = number(&target, "pane");
    ok(unsafe { fcb_desk_apply(desk, 4, 5, 2, pane, 11, 16, std::ptr::null()) });
    fs::write(&source, b"unrelated live replacement").unwrap();
    let mapped = ok(unsafe { fcb_desk_apply(desk, 5, 6, 12, bookmark, pane, 1, std::ptr::null()) });
    assert_eq!(number(&mapped, "model_revision"), 6);
    assert!(mapped.contains("\"body\":\"Keep this note\\u0000exactly.\""));
    assert!(mapped.contains("\"update_sequence\":\"2\""));
    let page = ok(fcb_desk_window(desk, 6, pane, 0, 64));
    assert!(page.contains("\"selection\":{\"start\":\"11\",\"end\":\"16\"}"));
    assert!(page.contains("\"text\":\"new prefix exact\""));
    let stale = consume(unsafe { fcb_desk_apply(desk, 6, 7, 12, bookmark, pane, 1, std::ptr::null()) });
    assert!(stale.contains("DESK_STALE_BOOKMARK"));
    assert_eq!(number(&ok(fcb_desk_state(desk)), "model_revision"), 6);
    assert_eq!(fcb_desk_close(desk), 1);
}

#[test]
fn export_abi_keeps_exact_bookmarked_bytes_and_full_nul_note_after_source_is_gone() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let root = std::env::temp_dir().join(format!("fcb-desk-export-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("source.rs"); fs::write(&source, b"A\xff\0Zprivate").unwrap();
    let source_path = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = ok(unsafe { fcb_desk_open(desk, 0, 1, source_path.as_ptr()) }); let pane = number(&opened, "pane");
    ok(unsafe { fcb_desk_apply(desk, 1, 2, 2, pane, 1, 4, std::ptr::null()) });
    let label = CString::new("Reason").unwrap(); let body = "é\0keep full tail".as_bytes();
    ok(unsafe { fcb_desk_annotate(desk, 2, 3, pane, label.as_ptr(), body.as_ptr(), body.len() as u64) });
    ok(unsafe { fcb_desk_apply(desk, 3, 4, 8, pane, 0, 0, std::ptr::null()) });
    fs::rename(&source, root.join("moved.rs")).unwrap();
    let destination = root.join("source-pack.json"); let path = CString::new(destination.as_os_str().as_encoded_bytes()).unwrap();
    let receipt = ok(unsafe { fcb_desk_export(desk, 4, path.as_ptr(), 3, 65536, 65536) });
    assert!(receipt.contains("\"command\":\"export\""));
    assert_eq!(number(&receipt, "bytes_written"), fs::metadata(&destination).unwrap().len());
    let pack = fs::read_to_string(&destination).unwrap();
    assert!(pack.contains("\"selected_original_hex\":\"ff005a\""));
    assert!(pack.contains("\"body\":\"é\\u0000keep full tail\""));
    assert!(!pack.contains("private"));
    let retry = consume(unsafe { fcb_desk_export(desk, 4, path.as_ptr(), 3, 65536, 65536) });
    assert!(retry.contains("DESK_SAVE_DESTINATION_EXISTS")); assert_eq!(fs::read_to_string(&destination).unwrap(), pack);
    let invalid = root.join("invalid-budget.json"); let path = CString::new(invalid.as_os_str().as_encoded_bytes()).unwrap();
    let rejected = consume(unsafe { fcb_desk_export(desk, 4, path.as_ptr(), 3, u64::MAX, 65536) });
    assert!(rejected.contains("\"status\":\"error\"")); assert!(!invalid.exists());
    let rejected = consume(unsafe { fcb_desk_export(desk, 4, std::ptr::null(), 3, 65536, 65536) });
    assert!(rejected.contains("DESK_HANDLE_INVALID_ARGUMENT"));
    assert_eq!(number(&ok(fcb_desk_state(desk)), "model_revision"), 4);
    assert_eq!(fcb_desk_close(desk), 1);
}


#[test]
fn guided_reattachment_abi_reviews_retained_sources_and_confirms_without_selecting_a_pane() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let root = std::env::temp_dir().join(format!("fcb-desk-guided-reattach-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("source.rs"); fs::write(&source, b"old exact").unwrap();
    let path = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    let opened = ok(unsafe { fcb_desk_open(desk, 0, 1, path.as_ptr()) }); let pane = number(&opened, "pane");
    ok(unsafe { fcb_desk_apply(desk, 1, 2, 2, pane, 4, 9, std::ptr::null()) });
    let label = CString::new("My reason").unwrap(); let body = b"Unchanged\0personal text";
    let note = ok(unsafe { fcb_desk_annotate(desk, 2, 3, pane, label.as_ptr(), body.as_ptr(), body.len() as u64) });
    let bookmark = number(&note, "bookmark");
    fs::write(&source, b"new prefix exact").unwrap();
    let target = ok(unsafe { fcb_desk_open(desk, 3, 4, path.as_ptr()) });
    let pane = number(&target, "pane"); let file = number(&target, "file_id"); let revision = number(&target, "source_revision");
    fs::rename(&source, root.join("moved-source.rs")).unwrap();
    let proposal = ok(fcb_desk_suggest_reattachment(desk, 4, bookmark, 1, pane, 8 * 1024 * 1024, 256));
    assert!(proposal.contains("\"outcome\":\"mapped\""));
    assert!(proposal.contains("\"original_range\":{\"start\":\"4\",\"end\":\"9\"}"));
    assert!(proposal.contains("\"target_range\":{\"start\":\"11\",\"end\":\"16\"}"));
    assert!(proposal.contains("\"source_reopened\":false"));
    assert!(proposal.contains("\"additional_source_bytes_read\":\"0\""));
    assert_eq!(ok(fcb_desk_state(desk)), target);
    let stale = consume(fcb_desk_suggest_reattachment(desk, 4, bookmark, 2, pane, 1024, 256));
    assert!(stale.contains("DESK_STALE_BOOKMARK"));
    let limit = consume(fcb_desk_suggest_reattachment(desk, 4, bookmark, 1, pane, 1024, 513));
    assert!(limit.contains("COMPARISON_LIMIT"));
    assert_eq!(ok(fcb_desk_state(desk)), target);

    let wrong_capture = consume(fcb_desk_reattach_range(desk, 4, 5, bookmark, 1, pane, file + 1, revision, 11, 16));
    assert!(wrong_capture.contains("DESK_STALE_REVISION"));
    let changed = consume(fcb_desk_reattach_range(desk, 4, 6, bookmark, 1, pane, file, revision, 0, 5));
    assert!(changed.contains("DESK_SELECTION_MISMATCH"));
    let stale = consume(fcb_desk_reattach_range(desk, 4, 7, bookmark, 2, pane, file, revision, 11, 16));
    assert!(stale.contains("DESK_STALE_BOOKMARK"));
    let before = ok(fcb_desk_state(desk));
    assert_eq!(number(&before, "model_revision"), 4);
    assert!(before.contains("\"update_sequence\":\"1\""));
    let accepted = ok(fcb_desk_reattach_range(desk, 4, 8, bookmark, 1, pane, file, revision, 11, 16));
    assert_eq!(number(&accepted, "model_revision"), 8);
    assert!(accepted.contains("\"update_sequence\":\"2\""));
    assert!(accepted.contains("\"body\":\"Unchanged\\u0000personal text\""));
    let page = ok(fcb_desk_window(desk, 8, pane, 0, 64));
    assert!(page.contains("\"selection\":null"));
    assert!(page.contains("\"text\":\"new prefix exact\""));
    ok(unsafe { fcb_desk_apply(desk, 8, 9, 4, bookmark, 0, 0, std::ptr::null()) });
    let recalled = ok(fcb_desk_window(desk, 9, pane, 11, 64));
    assert!(recalled.contains("\"selection\":{\"start\":\"11\",\"end\":\"16\"}"));
    assert!(recalled.contains("\"text\":\"exact\""));
    assert_eq!(fcb_desk_close(desk), 1);
}
