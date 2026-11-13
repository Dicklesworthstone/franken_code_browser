#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use crate::atlas_ffi::{fcb_atlas_create, fcb_atlas_open, fcb_atlas_close};
use crate::atlas_ffi::search::index::{fcb_atlas_index_prepare, fcb_atlas_index_clear};
use crate::reader_ffi::{fcb_reader_create, fcb_reader_info, fcb_reader_copy_range,
    fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, path::PathBuf};

const SOURCE: &[u8] = b"// module evidence\nmod child;\nuse crate::child::Child;\n";
const CHILD: &[u8] = b"pub struct Child;\n";
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let value = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; value
}
fn workspace() -> (PathBuf, u64) {
    let root = std::env::temp_dir().join(format!("fcb-atlas-imports-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("README.md"), b"[source](src/lib.rs)\n").unwrap();
    fs::write(root.join("src/lib.rs"), SOURCE).unwrap();
    fs::write(root.join("src/child.rs"), CHILD).unwrap();
    let handle = fcb_atlas_create(); assert_ne!(handle, 0);
    let text = CString::new(root.to_str().unwrap()).unwrap();
    assert!(take(unsafe { fcb_atlas_open(handle, text.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_atlas_index_prepare(handle, 1, 100, 8192, 65536, 65536)).contains("\"capture_complete\":true"));
    (root, handle)
}
fn begin(handle: u64, generation: u64) -> String {
    take(fcb_atlas_imports_begin(handle, generation, 1, 100, 65536))
}
fn finish(handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = take(fcb_atlas_imports_step(handle, generation));
        assert!(step.contains("\"status\":\"ok\""), "{step}");
        if step.contains("\"relationships_in_progress\":false") { return step; }
    }
    panic!("bounded import inventory did not finish");
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn c_imports_open_exact_statement_and_target_from_frozen_captures() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let (root, handle) = workspace();
    let admitted = begin(handle, 1);
    assert!(admitted.contains("\"schema\":\"fcb.atlas-imports/1\""), "{admitted}");
    assert!(admitted.contains("\"command\":\"imports-begin\""));
    let finished = finish(handle, 1);
    assert_eq!(number(&finished, "retained_links"), 2, "{finished}");
    assert!(finished.contains("\"compiler_resolved\":false"));
    assert!(finished.contains("\"semantic_complete\":false"));
    let path = CString::new(hex(b"src/child.rs")).unwrap();
    let incoming = take(unsafe { fcb_atlas_imports_page(handle, 1, 2, path.as_ptr(), 0, 1) });
    assert_eq!(incoming.matches("\"link_id\":").count(), 1);
    assert!(incoming.contains("\"lexical_import_kind\":\"rust-module\""), "{incoming}");
    assert!(incoming.contains("\"resolution_basis\":\"rust-module-file-candidate\""));
    assert!(incoming.contains("\"source_bytes_read\":\"0\""));
    let id = number(&incoming, "link_id");
    fs::write(root.join("src/lib.rs"), b"mod changed;\n").unwrap();
    fs::write(root.join("src/child.rs"), b"pub struct Changed;\n").unwrap();
    let evidence = fcb_reader_create(); let target = fcb_reader_create();
    assert_ne!(evidence, 0); assert_ne!(target, 0);
    let start = SOURCE.windows(b"mod child;".len()).position(|w| w == b"mod child;").unwrap() as u64;
    for (reader, side, expected) in [(evidence, 0, start), (target, 1, 0)] {
        let receipt = take(fcb_atlas_imports_open_reader(handle, reader, 1, id, side));
        assert!(receipt.contains("\"command\":\"imports-open-reader\""), "{receipt}");
        assert!(receipt.contains("\"relationship\":{") && receipt.contains("\"evidence_precision\":\"exact-import-source-range\""));
        assert!(receipt.contains("\"source_reopened\":false"));
        assert_eq!(number(&receipt, "initial_original_offset"), expected);
        assert!(receipt.contains(&format!("\"reader_owner\":\"{reader}\"")));
    }
    let retained_reply = unsafe { fcb_atlas_imports_page(handle, 1, 0, std::ptr::null(), 0, 128) };
    assert!(take(fcb_atlas_index_clear(handle, 2)).contains("\"status\":\"ok\""));
    assert!(take(unsafe { fcb_atlas_imports_page(handle, 1, 0, std::ptr::null(), 0, 128) }).contains("\"status\":\"error\""));
    assert_eq!(fcb_atlas_close(handle), 1);
    assert!(take(retained_reply).contains("\"destination\":\"child\""));
    for (reader, bytes) in [(evidence, SOURCE), (target, CHILD)] {
        assert!(take(fcb_reader_copy_range(reader, 0, bytes.len() as u64)).contains(&hex(bytes)));
        assert_eq!(fcb_reader_close(reader), 1);
    }
}

#[test]
fn c_import_path_limits_and_unresolved_targets_do_not_install_live_readers() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let (_, handle) = workspace();
    begin(handle, 1); finish(handle, 1);
    for path in ["", "0", "6A", "zz", "00", "2e2e2f7372632f6c69622e7273", "616273656e742e7273"] {
        let path = CString::new(path).unwrap();
        let refused = take(unsafe { fcb_atlas_imports_page(handle, 1, 1, path.as_ptr(), 0, 128) });
        assert!(refused.contains("\"status\":\"error\""), "{refused}");
    }
    let oversized = vec![b'a'; 2 * MAX_SEARCH_SCOPE_PATH_BYTES + 1];
    assert!(take(unsafe { fcb_atlas_imports_page(handle, 1, 1, oversized.as_ptr().cast(), 0, 128) })
        .contains("ATLAS_HANDLE_INVALID_ARGUMENT"));
    for (direction, start, limit) in [(1, 0, 128), (3, 0, 128), (0, 0, 0), (0, 0, 129), (0, u64::MAX, 1)] {
        assert!(take(unsafe { fcb_atlas_imports_page(handle, 1, direction, std::ptr::null(), start, limit) })
            .contains("\"status\":\"error\""));
    }
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    // The second row is a lexical Rust use statement whose binding is not resolved.
    for (id, side) in [(0, 0), (999, 0), (1, 2), (2, 1)] {
        assert!(take(fcb_atlas_imports_open_reader(handle, reader, 1, id, side)).contains("\"status\":\"error\""));
        assert!(take(fcb_reader_info(reader)).contains("READER_HANDLE_NOT_OPEN"));
    }
    assert!(take(fcb_atlas_imports_open_reader(handle, reader, 1, 2, 0)).contains("\"status\":\"ok\""));
    assert!(take(fcb_reader_copy_range(reader, 0, SOURCE.len() as u64)).contains(&hex(SOURCE)));
    assert!(take(fcb_atlas_imports_open_reader(handle, reader, 1, 1, 1)).contains("\"status\":\"error\""));
    assert!(take(fcb_reader_copy_range(reader, 0, SOURCE.len() as u64)).contains(&hex(SOURCE)));
    assert_eq!(fcb_reader_close(reader), 1); assert_eq!(fcb_atlas_close(handle), 1);
}

struct PollCounter { calls: usize, stop_at: usize }
unsafe extern "C" fn poll(context: *mut c_void) -> i32 {
    let counter = unsafe { &mut *context.cast::<PollCounter>() };
    counter.calls += 1; i32::from(counter.calls >= counter.stop_at)
}
#[test]
fn c_cooperative_import_cancellation_preserves_the_accepted_inventory() {
    let _guard = FFI_TEST_LOCK.lock().unwrap(); let (_, handle) = workspace();
    begin(handle, 1); finish(handle, 1);
    let mut counter = PollCounter { calls: 0, stop_at: 1 };
    let canceled = take(unsafe { fcb_atlas_imports_begin_cancelable(handle, 2, 1, 100, 65536,
        Some(poll), (&mut counter as *mut PollCounter).cast()) });
    assert!(canceled.contains("CANCELED"), "{canceled}"); assert_eq!(counter.calls, 1);
    assert!(begin(handle, 2).contains("\"status\":\"ok\""), "Pre-entry refusal must not consume a host generation");
    counter.calls = 0; counter.stop_at = 2;
    let canceled = take(unsafe { fcb_atlas_imports_step_cancelable(handle, 2,
        Some(poll), (&mut counter as *mut PollCounter).cast()) });
    assert!(canceled.contains("CANCELED"), "{canceled}"); assert!(counter.calls >= 2);
    let previous = take(unsafe { fcb_atlas_imports_page(handle, 1, 0, std::ptr::null(), 0, 128) });
    assert_eq!(number(&previous, "retained_links"), 2);
    assert!(begin(handle, 3).contains("\"status\":\"ok\""));
    assert_eq!(number(&finish(handle, 3), "retained_links"), 2);
    assert_eq!(fcb_atlas_close(handle), 1);
}

#[test]
fn c_import_signatures_and_hex_paths_preserve_raw_bytes_without_normalization() {
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_imports_begin;
    let _: unsafe extern "C" fn(u64, u64, u64, u64, u64, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_atlas_imports_begin_cancelable;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_atlas_imports_step;
    let _: unsafe extern "C" fn(u64, u64, Option<SearchCancellationCallback>, *mut c_void)
        -> *mut c_char = fcb_atlas_imports_step_cancelable;
    let _: unsafe extern "C" fn(u64, u64, u64, *const c_char, u64, u64) -> *mut c_char = fcb_atlas_imports_page;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_imports_open_reader;
    let header = include_str!("../include/fcb_atlas_imports.h");
    for name in ["fcb_atlas_imports_begin(", "fcb_atlas_imports_begin_cancelable(", "fcb_atlas_imports_step(",
        "fcb_atlas_imports_step_cancelable(", "fcb_atlas_imports_page(", "fcb_atlas_imports_open_reader("] {
        assert_eq!(header.matches(name).count(), 1);
    }
    let raw = b"src/e\xcc\x81-\xff.rs";
    let encoded = CString::new(hex(raw)).unwrap();
    assert_eq!(unsafe { decode_path(encoded.as_ptr()) }.unwrap().as_deref(), Some(raw.as_slice()));
    assert_eq!(unsafe { decode_path(std::ptr::null()) }.unwrap(), None);
    let maximum = CString::new("61".repeat(MAX_SEARCH_SCOPE_PATH_BYTES)).unwrap();
    assert_eq!(unsafe { decode_path(maximum.as_ptr()) }.unwrap().unwrap().len(), MAX_SEARCH_SCOPE_PATH_BYTES);
    let unterminated = vec![b'0'; 2 * MAX_SEARCH_SCOPE_PATH_BYTES + 1];
    assert_eq!(unsafe { decode_path(unterminated.as_ptr().cast()) }.err(), Some(AccessError::InvalidArgument));
}


#[test]
fn c_python_import_candidates_keep_exact_evidence_and_targets_after_live_edits() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-python-imports-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let source = b"# Python evidence\nfrom .worker import Tool\n";
    let worker = b"class Tool:\n    pass\n";
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(root.join("pkg/__init__.py"), b"").unwrap();
    fs::write(root.join("pkg/main.py"), source).unwrap();
    fs::write(root.join("pkg/worker.py"), worker).unwrap();
    let atlas = fcb_atlas_create(); assert_ne!(atlas, 0);
    let path = CString::new(root.to_str().unwrap()).unwrap();
    assert!(take(unsafe { fcb_atlas_open(atlas, path.as_ptr(), 100) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_atlas_index_prepare(atlas, 1, 100, 8192, 65536, 65536)).contains("\"capture_complete\":true"));
    assert!(begin(atlas, 1).contains("\"status\":\"ok\""));
    let result = finish(atlas, 1);
    assert_eq!(number(&result, "retained_links"), 1, "{result}");
    assert!(result.contains("\"language\":\"python\""));
    assert!(result.contains("\"extraction_policy\":\"rust-use-tree-js-static-and-python-import-v3\""));
    let filter = CString::new(hex(b"pkg/worker.py")).unwrap();
    let incoming = take(unsafe { fcb_atlas_imports_page(atlas, 1, 2, filter.as_ptr(), 0, 64) });
    assert_eq!(number(&incoming, "matching_links"), 1);
    assert!(incoming.contains("\"resolution_basis\":\"python-relative-module-candidate\""));
    let id = number(&incoming, "link_id");
    fs::write(root.join("pkg/main.py"), b"import changed\n").unwrap();
    fs::write(root.join("pkg/worker.py"), b"changed source\n").unwrap();
    let evidence = fcb_reader_create(); let target = fcb_reader_create();
    assert_ne!(evidence, 0); assert_ne!(target, 0);
    for (reader, side, start) in [(evidence, 0, b"# Python evidence\n".len() as u64), (target, 1, 0)] {
        let receipt = take(fcb_atlas_imports_open_reader(atlas, reader, 1, id, side));
        assert!(receipt.contains("\"status\":\"ok\""), "{receipt}");
        assert!(receipt.contains("\"lexical_import_kind\":\"python-from\""));
        assert!(receipt.contains("\"source_reopened\":false"));
        assert_eq!(number(&receipt, "initial_original_offset"), start);
    }
    assert_eq!(fcb_atlas_close(atlas), 1);
    for (reader, bytes) in [(evidence, source.as_slice()), (target, worker.as_slice())] {
        assert!(take(fcb_reader_copy_range(reader, 0, bytes.len() as u64)).contains(&hex(bytes)));
        assert_eq!(fcb_reader_close(reader), 1);
    }
}
