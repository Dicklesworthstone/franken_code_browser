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


#[test]
fn c_stream_excerpt_import_preserves_both_query_families_and_rejects_cross_family_handles() {
    use crate::{atlas_ffi, atlas_sessions, catalog_stream_ffi};
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    for family in [0, 1] {
        let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
        let stream = if family == 0 { atlas_ffi::fcb_atlas_create() }
            else { catalog_stream_ffi::fcb_catalog_stream_create() };
        assert_ne!(stream, 0); assert_ne!(stream, desk);
        let root = std::env::temp_dir().join(format!("fcb-desk-stream-abi-{}-{stream}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("evidence.rs");
        let bytes: Vec<_> = "\u{feff}prefix needle suffix 😀\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
        fs::write(&path, &bytes).unwrap();
        let croot = CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
        let (generation, revision) = if family == 0 {
            ok(unsafe { atlas_ffi::fcb_atlas_open(stream, croot.as_ptr(), 64) });
            let report = atlas_ffi::registry().unwrap().execute_stream(stream,
                atlas_sessions::StreamCommand::Search { needle: "needle",
                    options: fcb_app::host::atlas_stream_search::AtlasStreamSearchOptions::default() },
                || false).unwrap();
            (field(report.as_str(), "query_generation"), 0)
        } else {
            let query = b"needle";
            ok(unsafe { catalog_stream_ffi::fcb_catalog_stream_open(stream, croot.as_ptr(), 7,
                query.as_ptr(), query.len() as u64, 0, 1, 1, 0, std::ptr::null(), std::ptr::null(),
                0, std::ptr::null(), 32, 1024 * 1024, 1024) });
            let mut report = ok(catalog_stream_ffi::fcb_catalog_stream_info(stream));
            for _ in 0..1000 {
                if report.contains("\"more\":false") { break; }
                report = ok(catalog_stream_ffi::fcb_catalog_stream_advance(stream, 7, field(&report, "revision"), 256));
            }
            assert!(report.contains("\"more\":false"));
            (7, field(&report, "revision"))
        };
        for (bad_family, bad_generation, bad_revision, bad_index) in [
            (2, generation, revision, 0), (family, 0, revision, 0),
            (family, generation, if family == 0 { 1 } else { 0 }, 0),
            (family, generation, revision, u64::MAX),
            (1 - family, generation, if family == 0 { 1 } else { 0 }, 0),
        ] {
            let rejected = consume(fcb_desk_import_stream_excerpt(desk, 0, 1, stream,
                bad_family, bad_generation, bad_revision, bad_index));
            assert!(rejected.contains("\"status\":\"error\""), "{rejected}");
            assert!(rejected.contains("\"schema\":\"fcb.reading-desk/1\""));
        }
        assert!(ok(fcb_desk_state(desk)).contains("\"observed_passages\":[]"));
        fs::write(&path, b"replacement source").unwrap();
        let imported = ok(fcb_desk_import_stream_excerpt(desk, 0, 1, stream, family, generation, revision, 0));
        assert!(imported.contains("\"encoding\":\"utf16be\""));
        assert!(imported.contains("\"path_hex\":\"65766964656e63652e7273\""));
        let passage = field(&imported, "passage");
        let closed = if family == 0 { atlas_ffi::fcb_atlas_close(stream) }
            else { catalog_stream_ffi::fcb_catalog_stream_close(stream) };
        assert_eq!(closed, 1);
        assert!(consume(fcb_desk_import_stream_excerpt(desk, 1, 2, stream, family, generation, revision, 0))
            .contains("\"status\":\"error\""));
        let pending = fcb_desk_observed_passage(desk, 1, passage);
        assert_eq!(fcb_desk_close(desk), 1);
        let read = ok(pending);
        assert!(read.contains("\"text\":\"prefix needle suffix 😀\\r\\n\""));
        assert!(read.contains("\"additional_source_bytes_read\":\"0\""));
        assert!(read.contains("\"whole_file_immutable\":false"));
        assert!(!read.contains("replacement source"));
    }
}

#[test]
fn c_retained_passage_keeps_large_capture_offsets_after_reader_close_and_live_replacement() {
    use crate::reader_ffi;
    let _guard = reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let desk = fcb_desk_create(); assert_ne!(desk, 0); ok(fcb_desk_start(desk));
    let reader = reader_ffi::fcb_reader_create(); assert_ne!(reader, 0);
    let root = std::env::temp_dir().join(format!("fcb-desk-retained-passage-abi-{}-{reader}", std::process::id()));
    fs::create_dir(&root).unwrap(); let path = root.join("source.rs");
    let start = 5 * 1024 * 1024;
    let mut bytes = vec![b'x'; start]; bytes.extend_from_slice(b"exact retained evidence");
    fs::write(&path, &bytes).unwrap();
    let cpath = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    ok(unsafe { reader_ffi::fcb_reader_open(reader, cpath.as_ptr(), 32 * 1024 * 1024) });
    fs::write(&path, b"changed live source").unwrap();
    for (source, from, to) in [(desk, start as u64, start as u64 + 5),
        (reader, 0, 256 * 1024 + 1), (reader, 2, 2), (reader, 0, u64::MAX)] {
        assert!(consume(fcb_desk_import_reader_passage(desk, 0, 1, source, from, to))
            .contains("\"status\":\"error\""));
    }
    assert!(ok(fcb_desk_state(desk)).contains("\"observed_passages\":[]"));
    let kept = ok(fcb_desk_import_reader_passage(desk, 0, 1, reader, start as u64, start as u64 + 5));
    assert_eq!(field(&kept, "retained_source_bytes"), 21);
    assert_eq!(field(&kept, "origin_owner"), reader);
    let passage = field(&kept, "passage");
    assert_eq!(reader_ffi::fcb_reader_close(reader), 1);
    let pending = fcb_desk_observed_passage(desk, 1, passage);
    assert_eq!(fcb_desk_close(desk), 1);
    let read = ok(pending);
    assert!(read.contains("\"text\":\"exact\""));
    assert!(read.contains("\"original_hex\":\"6578616374\""));
    assert!(read.contains("\"consistency\":\"host-supplied-sequence\""));
    assert_eq!(field(&read, "observed_length"), bytes.len() as u64);
    assert!(read.contains(&format!("\"selection\":{{\"start\":\"{start}\",\"end\":\"{}\"}}", start + 5)));
    assert!(read.contains("\"additional_source_bytes_read\":\"0\""));
    assert!(read.contains("\"source_reopened\":false"));
    assert!(read.contains("\"whole_file_immutable\":false"));
}
