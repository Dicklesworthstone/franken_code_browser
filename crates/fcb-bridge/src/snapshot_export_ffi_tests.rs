#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use std::{ffi::{CStr, CString}, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

struct Fixture { root: PathBuf, output: PathBuf }
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir().join(format!("fcb-export-ffi-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        let root = base.join("root"); fs::create_dir_all(&root).unwrap();
        fs::write(root.join("source.rs"), b"needle").unwrap();
        Self { root, output: base.join("repository.fcbs") }
    }
    fn save(&self, poll: Option<SearchCancellationCallback>, context: *mut c_void) -> String {
        let root = CString::new(self.root.to_str().unwrap()).unwrap();
        let destination = CString::new(self.output.to_str().unwrap()).unwrap();
        take(unsafe { fcb_snapshot_save_cancelable(root.as_ptr(), destination.as_ptr(),
            100, 1024, 4096, 0, poll, context) })
    }
}
fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null(), "export must deliver its known write effect");
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
unsafe extern "C" fn cancel_after_creation(context: *mut c_void) -> i32 {
    let destination = unsafe { &*context.cast::<PathBuf>() };
    i32::from(destination.exists())
}
unsafe extern "C" fn cancel_after_write(context: *mut c_void) -> i32 {
    let destination = unsafe { &*context.cast::<PathBuf>() };
    i32::from(fs::metadata(destination).is_ok_and(|metadata| metadata.len() > 0))
}
unsafe extern "C" fn cancel_after_complete_archive(context: *mut c_void) -> i32 {
    let destination = unsafe { &*context.cast::<PathBuf>() };
    i32::from(fs::read(destination).is_ok_and(|bytes| {
        fcb::search::snapshot::SnapshotView::open(&bytes, Default::default(), || false).is_ok()
    }))
}

#[test]
fn ffi_export_produces_the_shared_archive_and_preserves_existing_destination() {
    let f = Fixture::new();
    let result = f.save(None, std::ptr::null_mut());
    assert!(result.contains("\"command\":\"snapshot-save\""), "{result}");
    assert!(result.contains("\"effect\":\"complete-file-sync-requested\""));
    assert!(result.contains("\"captured_files\":\"1\""));
    let original = fs::read(&f.output).unwrap();
    let denied = f.save(None, std::ptr::null_mut());
    assert!(denied.contains("SNAPSHOT_DESTINATION_EXISTS"));
    assert!(denied.contains("\"effect\":\"none\""));
    assert_eq!(fs::read(&f.output).unwrap(), original);
}

#[test]
fn cancellation_after_creation_partial_write_and_complete_write_preserves_effect_receipts() {
    for (poll, expected, has_bytes) in [
        (cancel_after_creation as SearchCancellationCallback, "destination-created-incomplete", false),
        (cancel_after_write as SearchCancellationCallback, "destination-created-incomplete", true),
        (cancel_after_complete_archive as SearchCancellationCallback, "complete-file-sync-unconfirmed", true)] {
        let f = Fixture::new();
        let context = std::ptr::from_ref(&f.output).cast_mut().cast();
        let result = f.save(Some(poll), context);
        assert!(result.contains("SNAPSHOT_CANCELED"), "{result}");
        assert!(result.contains(&format!("\"effect\":\"{expected}\"")), "{result}");
        assert_eq!(fs::metadata(&f.output).unwrap().len() > 0, has_bytes);
    }
}

#[test]
fn explicit_large_export_opens_in_saved_and_comparison_readers_without_live_source() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let f = Fixture::new();
    // Exceed both the former 1 MiB member profile and the 4 MiB textual transfer
    // limit. Reader handoff transfers owned capture metadata, not a huge JSON.
    let source_bytes = 5 * 1024 * 1024;
    let mut source = vec![b'x'; source_bytes];
    source[source_bytes - 6..].copy_from_slice(b"needle");
    fs::write(f.root.join("source.rs"), &source).unwrap();
    drop(source);
    let root = CString::new(f.root.to_str().unwrap()).unwrap();
    let destination = CString::new(f.output.to_str().unwrap()).unwrap();
    let saved = take(unsafe { fcb_snapshot_save_cancelable(root.as_ptr(), destination.as_ptr(),
        65_536, 32 * 1024 * 1024, 1024 * 1024 * 1024, 1, None, std::ptr::null_mut()) });
    assert!(saved.contains("\"captured_files\":\"1\""), "{saved}");
    assert!(saved.contains("\"captured_bytes\":\"5242880\""), "{saved}");
    assert!(saved.contains("\"effect\":\"complete-file-sync-requested\""));
    fs::write(f.root.join("source.rs"), b"replacement live bytes").unwrap();

    let archive = crate::saved_ffi::fcb_saved_create(); assert_ne!(archive, 0);
    let opened = take(unsafe { crate::saved_ffi::fcb_saved_open(archive, destination.as_ptr()) });
    assert!(opened.contains("\"status\":\"ok\""), "{opened}");
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let handoff = take(crate::saved_ffi::fcb_saved_open_member_reader(archive, reader, 0));
    assert!(handoff.contains("\"captured_bytes\":\"5242880\""), "{handoff}");
    assert_eq!(crate::saved_ffi::fcb_saved_close(archive), 1);
    let tail = take(crate::reader_ffi::fcb_reader_copy_range(reader, source_bytes as u64 - 6, source_bytes as u64));
    assert!(tail.contains("6e6565646c65"), "{tail}");
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);

    let comparison = crate::snapshot_comparison_ffi::fcb_snapshot_comparison_create();
    assert_ne!(comparison, 0);
    let opened = take(unsafe { crate::snapshot_comparison_ffi::fcb_snapshot_comparison_open_cancelable(
        comparison, destination.as_ptr(), destination.as_ptr(), None, std::ptr::null_mut()) });
    assert!(opened.contains("\"max_source_file_bytes\":\"33554432\""), "{opened}");
    assert!(opened.contains("\"max_archive_bytes\":\"1090519040\""), "{opened}");
    take(crate::snapshot_comparison_ffi::fcb_snapshot_comparison_begin(comparison, 1));
    let classified = take(crate::snapshot_comparison_ffi::fcb_snapshot_comparison_step(comparison, 1, 8));
    assert!(classified.contains("\"unchanged\":\"1\""), "{classified}");
    let reader = crate::reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let handoff = take(crate::snapshot_comparison_ffi::fcb_snapshot_comparison_open_reader(comparison, reader, 1, 1, 1));
    assert!(handoff.contains("\"captured_bytes\":\"5242880\""), "{handoff}");
    assert_eq!(crate::snapshot_comparison_ffi::fcb_snapshot_comparison_close(comparison), 1);
    let tail = take(crate::reader_ffi::fcb_reader_copy_range(reader, source_bytes as u64 - 6, source_bytes as u64));
    assert!(tail.contains("6e6565646c65"), "{tail}");
    assert_eq!(crate::reader_ffi::fcb_reader_close(reader), 1);
}

#[test]
fn invalid_ffi_inputs_and_quotas_never_create_a_destination() {
    let f = Fixture::new();
    let root = CString::new(f.root.to_str().unwrap()).unwrap();
    let destination = CString::new(f.output.to_str().unwrap()).unwrap();
    let invalid = CString::new(vec![0xff]).unwrap();
    for (root, policy, files) in [(std::ptr::null(), 1, 100), (invalid.as_ptr(), 1, 100),
        (root.as_ptr(), 2, 100), (root.as_ptr(), 1, u64::MAX)] {
        assert!(unsafe { fcb_snapshot_save_cancelable(root, destination.as_ptr(), files,
            1024, 4096, policy, None, std::ptr::null_mut()) }.is_null());
    }
    assert!(!f.output.exists());
    for (file_bytes, source_bytes) in [(32 * 1024 * 1024 + 1, 1024), (1, 1024 * 1024 * 1024 + 1)] {
        assert!(unsafe { fcb_snapshot_save_cancelable(root.as_ptr(), destination.as_ptr(), 1,
            file_bytes, source_bytes, 1, None, std::ptr::null_mut()) }.is_null());
    }
    assert!(!f.output.exists());
    let _: unsafe extern "C" fn(*const c_char, *const c_char, u64, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_snapshot_save_cancelable;
    assert_eq!(include_str!("../include/fcb_snapshot_export.h").matches("fcb_snapshot_save_cancelable(").count(), 1);
}
