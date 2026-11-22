//! The public observed-passage ABI retains exact bytes and owns every response.
use super::*;
use std::{ffi::{CStr, CString}, fs};
use crate::observed_reader_ffi::*;

fn consume(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}
fn ok(pointer: *mut c_char) -> String {
    let text = consume(pointer); assert!(text.contains("\"status\":\"ok\""), "{text}"); text
}
fn field(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn c_observed_passage_outlives_source_and_round_trips_length_prefixed_notes() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let reader = fcb_observed_reader_create(); assert_ne!(reader, 0);
    let root = std::env::temp_dir().join(format!("fcb-desk-observed-abi-{}-{desk}", std::process::id()));
    fs::create_dir(&root).unwrap(); let path = root.join("source"); fs::write(&path, "A🦀B").unwrap();
    let path_hex: String = path.as_os_str().as_encoded_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    let path_hex = CString::new(path_hex).unwrap();
    ok(unsafe { fcb_observed_reader_open_file(reader, path_hex.as_ptr(), None, std::ptr::null_mut()) });
    ok(unsafe { fcb_observed_reader_window(reader, 0, 64, 1, None, std::ptr::null_mut()) });
    let imported = ok(fcb_desk_import_observation(desk, 0, 1, reader, 1, 1, 5));
    let passage = field(&imported, "passage");
    assert_eq!(fcb_observed_reader_close(reader), 1);
    fs::write(&path, "different live bytes").unwrap();
    let label = CString::new("Rationale").unwrap(); let body = "Keep\nexact\0evidence";
    let edited = ok(unsafe { fcb_desk_edit_observed_passage(desk, 1, 2, passage, 1,
        label.as_ptr(), body.as_ptr(), body.len() as u64) });
    assert!(edited.contains("Keep\\nexact\\u0000evidence"));
    assert!(consume(unsafe { fcb_desk_edit_observed_passage(desk, 2, 3, passage, 2,
        label.as_ptr(), std::ptr::null(), 1) }).contains("DESK_HANDLE_INVALID_ARGUMENT"));
    assert!(consume(fcb_desk_forget_observed_passage(desk, 2, 4, passage, 1))
        .contains("\"status\":\"error\""));
    let destination = root.join("observed-pack.json");
    let destination_c = CString::new(destination.as_os_str().as_encoded_bytes()).unwrap();
    let export = ok(unsafe { fcb_desk_export(desk, 2, destination_c.as_ptr(), 64 * 1024, 1024 * 1024, 1024 * 1024) });
    assert!(export.contains("\"pack_schema\":\"fcb.desk-source-pack/2\""));
    assert!(export.contains("\"observed_passages_included\":true"));
    assert_eq!(field(&export, "total_observed_passages"), 1);
    assert_eq!(field(&export, "selected_original_bytes"), 4);
    let pack = fs::read_to_string(&destination).unwrap();
    assert!(pack.contains("\"selected_original_hex\":\"f09fa680\""));
    assert!(pack.contains("Keep\\nexact\\u0000evidence"));
    assert!(pack.contains("\"capture_kind\":\"observed-extent\""));
    assert!(pack.contains("\"whole_file_immutable\":false"));
    assert!(pack.contains("\"source_reopened\":false"));
    assert!(!pack.contains("different live bytes"));
    let pending = fcb_desk_observed_passage(desk, 2, passage);
    assert_eq!(fcb_desk_close(desk), 1);
    let retained = ok(pending);
    assert!(retained.contains("\"text\":\"🦀\""));
    assert!(retained.contains("\"original_hex\":\"f09fa680\""));
    assert!(retained.contains("\"whole_file_immutable\":false"));
    assert!(retained.contains("\"source_reopened\":false"));
    assert!(retained.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(consume(fcb_desk_observed_passage(desk, 2, passage)).contains("DESK_HANDLE_UNKNOWN"));
}
