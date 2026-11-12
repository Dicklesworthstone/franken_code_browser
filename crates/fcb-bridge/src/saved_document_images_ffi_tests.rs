#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget, snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};
use crate::reader_ffi::{self, document};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
#[test]
fn actual_saved_image_exports_keep_reader_schema_and_chunks_after_saved_handle_close() {
    let _guard = reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_saved_document_images_authorize;
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_saved_document_image_prepare;
    let header = include_str!("../include/fcb_saved_repository.h");
    for name in ["document_images_authorize", "document_image_prepare"] {
        assert_eq!(header.matches(&format!("fcb_saved_{name}(")).count(), 1);
    }
    let root = std::env::temp_dir().join(format!("fcb-images-c-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap(); let path = root.join("saved.fcbs");
    let owner = ArenaOwnerId::new(58821).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let doc = b"![Captured](logo.png)\n"; let asset = b"\x89PNG\r\n\x1a\nencoded-archive-image";
    let entries = [
        SnapshotEntry { path: b"README.md", observed_bytes: doc.len() as u64, data: SnapshotData::Captured(doc) },
        SnapshotEntry { path: b"logo.png", observed_bytes: asset.len() as u64, data: SnapshotData::Captured(asset) },
    ];
    let bytes = SnapshotBytes::encode(owner, true, "saved-images-c", &entries, Default::default(), &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    std::fs::write(&path, bytes.bytes()).unwrap();
    let input = CString::new(path.to_str().unwrap()).unwrap();
    let saved = fcb_saved_create(); let reader = reader_ffi::fcb_reader_create();
    assert_ne!(saved, 0); assert_ne!(reader, 0);
    assert!(take(unsafe { fcb_saved_open(saved, input.as_ptr()) }).contains("\"status\":\"ok\""));
    assert!(take(fcb_saved_open_member_reader(saved, reader, 0)).contains("\"status\":\"ok\""));
    assert!(take(document::fcb_reader_document(reader, 1, 80, 65536, 8192, 8192, 4096)).contains("\"status\":\"ok\""));
    let display = take(document::fcb_reader_document_display(reader, 1, 640));
    let image = display.split("\"item_index\":\"").skip(1).find(|part| part.contains("\"kind\":\"image\"")).unwrap();
    let item: u64 = image.split('"').next().unwrap().parse().unwrap();
    let request = number(image, "request_id");
    let wrong = take(fcb_saved_document_images_authorize(saved, reader, u64::MAX, 1));
    assert!(wrong.contains("\"schema\":\"fcb.saved-repository/1\""));
    assert!(wrong.contains("\"status\":\"error\""));
    let auth = take(fcb_saved_document_images_authorize(saved, reader, 0, 1));
    assert!(auth.contains("\"schema\":\"fcb.reader-session/1\""));
    assert_eq!(number(&auth, "saved_archive_owner"), saved);
    let retained = fcb_saved_document_image_prepare(saved, reader, 0, 1, item, request);
    assert_eq!(fcb_saved_close(saved), 1); std::fs::remove_file(&path).unwrap();
    let encoded = take(retained);
    assert!(encoded.contains("\"image_observation\":\"verified-saved-member\""));
    assert!(encoded.contains("\"image_member\":\"1\""));
    assert!(encoded.contains("\"decoded\":false"));
    let chunk = take(document::fcb_reader_document_image_chunk(reader, 1, item, request, 0, 64));
    let expected: String = asset.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(chunk.contains(&expected)); assert_eq!(number(&chunk, "image_read_calls"), 0);
    let revoked = take(fcb_saved_document_image_prepare(saved, reader, 0, 1, item, request));
    assert!(revoked.contains("SAVED_HANDLE_UNKNOWN"));
    assert!(take(reader_ffi::fcb_reader_window(reader, 0, 64)).contains("Captured"));
    assert_eq!(reader_ffi::fcb_reader_close(reader), 1);
    std::fs::remove_dir_all(root).unwrap();
}
