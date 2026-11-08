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
fn cancellation_after_creation_and_complete_write_still_returns_owned_effect_receipts() {
    for (poll, expected, has_bytes) in [
        (cancel_after_creation as SearchCancellationCallback, "destination-created-incomplete", false),
        (cancel_after_write as SearchCancellationCallback, "complete-file-sync-unconfirmed", true)] {
        let f = Fixture::new();
        let context = std::ptr::from_ref(&f.output).cast_mut().cast();
        let result = f.save(Some(poll), context);
        assert!(result.contains("SNAPSHOT_CANCELED"), "{result}");
        assert!(result.contains(&format!("\"effect\":\"{expected}\"")), "{result}");
        assert_eq!(fs::metadata(&f.output).unwrap().len() > 0, has_bytes);
    }
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
    let _: unsafe extern "C" fn(*const c_char, *const c_char, u64, u64, u64, u64,
        Option<SearchCancellationCallback>, *mut c_void) -> *mut c_char = fcb_snapshot_save_cancelable;
    assert_eq!(include_str!("../include/fcb_snapshot_export.h").matches("fcb_snapshot_save_cancelable(").count(), 1);
}
