#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use super::super::reader_ffi::{fcb_reader_create, fcb_reader_copy_range,
    fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString, OsString}, fs,
    os::unix::{ffi::OsStringExt, fs::symlink}, sync::atomic::{AtomicU64, Ordering}};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "expected a bounded JSON reply");
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) };
    assert!(text.len() <= fcb_app::host::catalog_session::MAX_CATALOG_RESPONSE_BYTES);
    text
}
fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-directory-catalog-ffi-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Retain fixtures for diagnosis.
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn scope(text: &str, directory: &[u8]) {
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert!(text.contains("\"discovery_scope\":\"directory\""), "{text}");
    assert!(text.contains("\"path_basis\":\"project-root\""), "{text}");
    let payload = text.split_once("\"discovery_directory\":{").unwrap().1.split('}').next().unwrap();
    assert!(payload.contains(&format!("\"hex\":\"{}\"", hex(directory))), "{text}");
    assert!(text.contains("\"discovery_source_bytes_read\":\"0\""), "{text}");
    assert!(text.contains("\"discovery_source_read_calls\":\"0\""), "{text}");
}

#[test]
fn c_scoped_catalog_copies_raw_inputs_and_transfers_an_independent_utf16_reader() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); let expected_directory = b"C:\\scope\xff";
    let selected = root.join(OsString::from_vec(expected_directory.to_vec()));
    fs::create_dir(&selected).unwrap();
    let path = selected.join(OsString::from_vec(b"raw-\xfe.md".to_vec()));
    let bytes: Vec<u8> = "\u{feff}αβ\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(&path, &bytes).unwrap(); fs::write(root.join("outside.rs"), b"outside").unwrap();
    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let mut directory = expected_directory.to_vec();
    let opened = take(unsafe { fcb_catalog_open_directory_with_policy(handle, croot.as_ptr(),
        directory.as_ptr(), directory.len() as u64, 1) });
    scope(&opened, expected_directory);
    assert!(opened.contains("\"catalogued_files\":\"1\""), "{opened}");
    assert!(opened.contains("\"discovery_complete\":true"));
    assert!(!opened.contains("\"hex\":\"6f7574736964652e7273\""));
    directory.fill(b'x'); let mut retired_root = croot.into_bytes_with_nul(); retired_root.fill(b'x');
    let page_pointer = fcb_catalog_page(handle, 1);
    scope(&take(fcb_catalog_info(handle)), expected_directory);
    scope(&take(fcb_catalog_stop(handle)), expected_directory);
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let receipt = take(fcb_catalog_open_reader(handle, reader, 1, 1, 4096));
    scope(&receipt, expected_directory); assert!(receipt.contains("\"encoding\":\"utf16le\""));
    let mut expected_path = expected_directory.to_vec(); expected_path.extend_from_slice(b"/raw-\xfe.md");
    assert!(receipt.contains(&format!("\"hex\":\"{}\"", hex(&expected_path))));
    fs::write(&path, b"later live bytes").unwrap();
    assert_eq!(fcb_catalog_close(handle), 1);
    scope(&take(page_pointer), expected_directory);
    let copied = take(fcb_reader_copy_range(reader, 0, bytes.len() as u64));
    assert!(copied.contains("\"original_hex\":\"fffeb103b2030d000a00\""), "{copied}");
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_scope_argument_refusals_precede_pointer_reads_and_leave_the_handle_reusable() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let handle = fcb_catalog_create(); assert_ne!(handle, 0);
    let unreadable_root = std::ptr::NonNull::<c_char>::dangling().as_ptr();
    let unreadable_directory = std::ptr::NonNull::<u8>::dangling().as_ptr();
    // These exact invalid cases are documented to return before touching
    // either input pointer. The remaining cases use actual readable storage.
    for (length, policy) in [(1, 2), (1, u64::MAX), (0, 0),
        (MAX_CATALOG_DIRECTORY_BYTES as u64 + 1, 0), (u64::MAX, 1)] {
        let failed = take(unsafe { fcb_catalog_open_directory_with_policy(handle,
            unreadable_root, unreadable_directory, length, policy) });
        assert!(failed.contains("CATALOG_HANDLE_INVALID_ARGUMENT"), "{failed}");
    }
    let failed = take(unsafe { fcb_catalog_open_directory_with_policy(handle,
        unreadable_root, std::ptr::null(), 1, 0) });
    assert!(failed.contains("CATALOG_HANDLE_INVALID_ARGUMENT"));
    let directory = b"selected";
    let failed = take(unsafe { fcb_catalog_open_directory_with_policy(handle,
        std::ptr::null(), directory.as_ptr(), directory.len() as u64, 0) });
    assert!(failed.contains("CATALOG_HANDLE_INVALID_ARGUMENT"));
    assert!(take(fcb_catalog_info(handle)).contains("CATALOG_HANDLE_NOT_OPEN"));

    let root = root(); fs::create_dir(root.join("selected")).unwrap();
    fs::create_dir(root.join("other")).unwrap(); fs::write(root.join("regular.rs"), b"file").unwrap();
    symlink(root.join("selected"), root.join("alias")).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    for target in [b"missing".as_slice(), b"regular.rs", b"alias", b"selected/../other",
        b"/selected", b"selected/", b"selected//child", b"selected\0child"] {
        let failed = take(unsafe { fcb_catalog_open_directory_with_policy(handle,
            croot.as_ptr(), target.as_ptr(), target.len() as u64, 1) });
        assert!(failed.contains("\"status\":\"error\""), "{failed}");
        assert!(take(fcb_catalog_info(handle)).contains("CATALOG_HANDLE_NOT_OPEN"));
    }
    let opened = take(unsafe { fcb_catalog_open_directory_with_policy(handle, croot.as_ptr(),
        directory.as_ptr(), directory.len() as u64, 0) });
    scope(&opened, directory); assert!(opened.contains("\"files\":[]"));
    assert!(opened.contains("\"discovery_complete\":true"));
    let already_open = take(unsafe { fcb_catalog_open_directory_with_policy(handle,
        croot.as_ptr(), b"other".as_ptr(), 5, 1) });
    assert!(already_open.contains("CATALOG_HANDLE_ALREADY_OPEN"));
    scope(&take(fcb_catalog_info(handle)), directory);
    assert_eq!(fcb_catalog_close(handle), 1);
}

#[test]
fn c_directory_policy_selects_inherited_rules_or_metadata_only_inclusion() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root(); fs::create_dir(root.join("ignored")).unwrap();
    fs::write(root.join(".gitignore"), b"ignored/\n").unwrap();
    fs::write(root.join("ignored/.gitignore"), b"!kept.rs\n").unwrap();
    fs::write(root.join("ignored/kept.rs"), b"source bytes are not discovery input").unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap(); let directory = b"ignored";
    for policy in [0, 1] {
        let handle = fcb_catalog_create(); assert_ne!(handle, 0);
        let opened = take(unsafe { fcb_catalog_open_directory_with_policy(handle,
            croot.as_ptr(), directory.as_ptr(), directory.len() as u64, policy) });
        scope(&opened, directory);
        if policy == 0 {
            assert!(opened.contains("\"state\":\"partial\""), "{opened}");
            assert!(opened.contains("\"files\":[]"));
            assert!(opened.contains("\"files_loaded\":\"1\""));
        } else {
            assert!(opened.contains("\"discovery_complete\":true"), "{opened}");
            assert!(opened.contains("\"hex\":\"69676e6f7265642f6b6570742e7273\""));
            assert!(opened.contains("\"rule_files_enabled\":false"));
            assert!(opened.contains("\"rule_policy\":null"));
        }
        assert_eq!(fcb_catalog_cancel(handle), 1);
        scope(&take(fcb_catalog_page(handle, 1)), directory);
        assert_eq!(fcb_catalog_close(handle), 1);
    }
}

#[test]
fn c_directory_rows_drill_into_raw_child_folders_through_existing_scoped_open() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let root = root();
    let directory = b"selected";
    let child = b"selected/raw-\xff";
    let child_native = root.join(OsString::from_vec(child.to_vec()));
    fs::create_dir_all(&child_native).unwrap();
    fs::create_dir(root.join("selected/empty")).unwrap();
    let source = b"fn discovered_child() {}\n";
    fs::write(child_native.join("source.rs"), source).unwrap();
    let croot = CString::new(root.to_str().unwrap()).unwrap();
    let parent = fcb_catalog_create(); assert_ne!(parent, 0);
    let opened = take(unsafe { fcb_catalog_open_directory_with_policy(parent, croot.as_ptr(),
        directory.as_ptr(), directory.len() as u64, 1) });
    scope(&opened, directory);
    assert!(opened.contains("\"directory_rows_policy\":\"direct-child-directories-v1\""), "{opened}");
    assert!(opened.contains("\"catalogued_directories\":\"2\""), "{opened}");
    assert!(opened.contains("\"retained_directories\":\"2\""), "{opened}");
    assert!(opened.contains("\"catalogued_files\":\"1\""), "folders do not consume file IDs: {opened}");
    let directories = opened.split_once("\"directories\":[").unwrap().1;
    assert!(directories.contains("\"directory_id\":\"1\""));
    assert!(directories.contains("\"directory_id\":\"2\""));
    assert!(directories.contains(&format!("\"hex\":\"{}\"", hex(child))), "{opened}");
    assert!(directories.contains("\"within_browse_limits\":true"), "{opened}");
    let page = take(fcb_catalog_page(parent, 1));
    assert_eq!(page, take(fcb_catalog_page(parent, 1)), "folder page replay is immutable");

    let nested = fcb_catalog_create(); assert_ne!(nested, 0);
    let next = take(unsafe { fcb_catalog_open_directory_with_policy(nested, croot.as_ptr(),
        child.as_ptr(), child.len() as u64, 1) });
    scope(&next, child);
    assert!(next.contains("\"directories\":[]"), "{next}");
    assert!(next.contains("\"catalogued_directories\":\"0\""), "{next}");
    assert!(next.contains("\"file_id\":\"1\""), "{next}");
    assert_eq!(fcb_catalog_close(parent), 1);
    let reader = fcb_reader_create(); assert_ne!(reader, 0);
    let receipt = take(fcb_catalog_open_reader(nested, reader, 1, 1, 4096));
    scope(&receipt, child);
    assert!(receipt.contains("\"directory_rows_policy\":\"direct-child-directories-v1\""));
    assert_eq!(fcb_catalog_close(nested), 1);
    let copied = take(fcb_reader_copy_range(reader, 0, source.len() as u64));
    assert!(copied.contains(&format!("\"original_hex\":\"{}\"", hex(source))), "{copied}");
    assert_eq!(fcb_reader_close(reader), 1);
}

#[test]
fn c_directory_open_signature_matches_its_additive_public_declaration() {
    let _: unsafe extern "C" fn(u64, *const c_char, *const u8, u64, u64) -> *mut c_char =
        fcb_catalog_open_directory_with_policy;
    let header = include_str!("../include/fcb_project_catalog.h");
    assert_eq!(header.matches("fcb_catalog_open_directory_with_policy(").count(), 1);
    assert!(header.contains("const uint8_t *directory, uint64_t directory_len, uint64_t discovery_policy"));
}
