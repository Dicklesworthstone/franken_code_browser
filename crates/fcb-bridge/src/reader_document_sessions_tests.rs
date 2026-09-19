#![forbid(unsafe_code)]

use super::*;
use crate::atlas_sessions::{AtlasSessions, Command as AtlasCommand};
use fcb_app::host::atlas_search::AtlasSearchOptions;
use std::{fs, time::{SystemTime, UNIX_EPOCH}};

fn prepare(readers: &ReaderSessions, handle: u64, generation: u64) -> HostResponse {
    readers.execute(handle, Command::Document { generation, options: Default::default() }, || false).unwrap()
}
fn window(readers: &ReaderSessions, handle: u64, generation: u64) -> HostResponse {
    readers.execute(handle, Command::DocumentWindow { generation, first: 0, count: 64 }, || false).unwrap()
}
fn span(readers: &ReaderSessions, handle: u64, needle: &str) -> (usize, usize) {
    let cell = readers.get(handle).unwrap();
    let state = cell.state.lock().unwrap();
    let capture = state.as_ref().unwrap().capture();
    let owner = capture.request().file().owner();
    let budget = ResourceBudget::new(owner, ByteLength::new(128 * 1024 * 1024)).unwrap();
    let document = fcb::document::reader::DocumentReader::prepare(capture,
        fcb_core::DocumentId::new(owner, 1).unwrap(), fcb_core::DocumentGeneration::new(owner, 1).unwrap(),
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let start = document.rendered_text().find(needle).unwrap();
    (start, start + needle.len())
}

#[test]
fn repository_search_to_captured_reader_to_document_survives_live_replacement_and_atlas_close() {
    let root = std::env::temp_dir().join(format!("fcb-search-doc-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("README.md"), b"# Original\n\nA **needle** paragraph.\n").unwrap();
    let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let atlas = atlases.create().unwrap(); let reader = readers.create().unwrap();
    atlases.open(atlas, &root, Default::default(), || false).unwrap();
    atlases.execute(atlas, AtlasCommand::Search { generation: 1, needle: "needle", options: AtlasSearchOptions::default() }, || false).unwrap();
    fs::write(root.join("README.md"), b"# Replaced\n\nDifferent source.\n").unwrap();
    let activated = atlases.open_search_reader(atlas, &readers, reader, 1, 1, || false).unwrap();
    assert!(activated.as_str().contains("\"source_reopened\":false"));
    atlases.close(atlas).unwrap();
    let prepared = prepare(&readers, reader, 1);
    assert!(prepared.as_str().contains("Original"));
    assert!(!prepared.as_str().contains("Replaced"));
    assert!(readers.execute(reader, Command::DocumentHeading { generation: 1, slug: "original", count: 2 }, || false).is_ok());
    let (start, end) = span(&readers, reader, "needle");
    let copied = readers.execute(reader, Command::DocumentCopy { generation: 1, start, end,
        mode: DocumentCopyMode::RenderedText }, || false).unwrap();
    assert!(copied.as_str().contains("\"text\":\"needle\""));
    let source = readers.execute(reader, Command::DocumentSource { generation: 1, start, end, context: 8 }, || false).unwrap();
    assert!(source.as_str().contains("\"selection_namespace\":\"document\""));
    assert!(source.as_str().contains("**needle**"));
    assert!(readers.execute(reader, Command::DocumentCopy { generation: 1, start, end,
        mode: DocumentCopyMode::EnclosingMarkdown }, || false).unwrap().as_str().contains("2a2a6e6565646c652a2a"));
    readers.close(reader).unwrap();
    assert!(copied.as_str().contains("needle"));
}

#[test]
fn independent_readers_and_returned_buffers_keep_separate_document_ownership() {
    let readers = ReaderSessions::new(); let a = readers.create().unwrap(); let b = readers.create().unwrap();
    readers.supply(a, b"# Alpha\n").unwrap(); readers.supply(b, b"# Beta\n").unwrap();
    let old_response = prepare(&readers, a, 1); prepare(&readers, b, 1);
    readers.execute(a, Command::ClearDocument { generation: 2 }, || false).unwrap();
    readers.close(a).unwrap();
    assert!(old_response.as_str().contains("Alpha"));
    assert!(window(&readers, b, 1).as_str().contains("Beta"));
    assert_eq!(readers.execute(a, Command::DocumentHeadings { generation: 1, first: 0, count: 1 }, || false).err(), Some(AccessError::UnknownHandle));
}

#[test]
fn reflow_and_clear_do_not_relabel_search_or_accept_obsolete_preview_offsets() {
    let readers = ReaderSessions::new(); let handle = readers.create().unwrap();
    readers.supply(handle, b"# Heading\n\nneedle and more text.\n").unwrap();
    readers.execute(handle, Command::Find { generation: 1, needle: "needle", limit: 10, scan_bytes: 4096 }, || false).unwrap();
    prepare(&readers, handle, 1);
    readers.execute(handle, Command::Document { generation: 2,
        options: ReaderDocumentOptions { width_columns: 12, ..Default::default() } }, || false).unwrap();
    assert!(readers.execute(handle, Command::DocumentWindow { generation: 1, first: 0, count: 1 }, || false).is_err());
    assert!(readers.execute(handle, Command::CopyHit { generation: 1, index: 0 }, || false).is_ok());
    readers.execute(handle, Command::ClearDocument { generation: 3 }, || false).unwrap();
    assert!(readers.execute(handle, Command::DocumentFromSource { generation: 2, offset: 0, count: 1 }, || false).is_err());
    assert!(readers.execute(handle, Command::Info, || false).unwrap().as_str().contains("\"accepted_document_generation\":null"));
    assert!(readers.execute(handle, Command::CopyHit { generation: 1, index: 0 }, || false).is_ok());
}

#[test]
fn cancellation_during_replacement_preserves_old_document_and_is_classified_as_canceled() {
    let readers = ReaderSessions::new(); let handle = readers.create().unwrap();
    readers.supply(handle, b"# Kept\n\nBody.\n").unwrap(); prepare(&readers, handle, 1);
    let mut polls = 0;
    let error = readers.execute(handle, Command::Document { generation: 2, options: Default::default() }, || {
        polls += 1; if polls == 3 { readers.cancel(handle).unwrap(); } false
    }).err().unwrap();
    assert_eq!(error, AccessError::Canceled);
    assert!(window(&readers, handle, 1).as_str().contains("Kept"));
    let mut polls = 0;
    let pulse = readers.execute(handle, Command::Document { generation: 3, options: Default::default() }, || {
        polls += 1; polls == 3
    }).err().unwrap();
    assert!(pulse.canceled());
    assert!(pulse.json(handle).contains("\"exit_code\":\"130\""));
    assert!(AccessError::Reader(ReaderSessionError::Document(DocumentReadError::Canceled)).canceled());
    assert!(window(&readers, handle, 1).as_str().contains("Kept"));
    assert!(readers.execute(handle, Command::ClearDocument { generation: 4 }, || true).is_err());
    assert!(window(&readers, handle, 1).as_str().contains("Kept"));
}

#[test]
fn busy_document_reader_does_not_block_another_reader_or_hold_the_table_lock() {
    let readers = ReaderSessions::new(); let a = readers.create().unwrap(); let b = readers.create().unwrap();
    readers.supply(a, b"# A\n").unwrap(); readers.supply(b, b"# B\n").unwrap();
    prepare(&readers, a, 1); prepare(&readers, b, 1);
    let cell = readers.get(a).unwrap(); let _guard = cell.state.lock().unwrap();
    assert_eq!(readers.execute(a, Command::DocumentWindow { generation: 1, first: 0, count: 1 }, || false).err(), Some(AccessError::Busy));
    assert!(window(&readers, b, 1).as_str().contains('B'));
    assert!(readers.cancel(a).is_ok());
    assert!(readers.create().is_ok());
}

#[test]
fn close_during_document_work_keeps_retiring_capacity_until_the_last_pin_is_released() {
    let readers = ReaderSessions::new(); let handle = readers.create().unwrap();
    readers.supply(handle, b"# Retained\n").unwrap(); prepare(&readers, handle, 1);
    for _ in 1..MAX_READER_SESSIONS { readers.create().unwrap(); }
    let pin = readers.get(handle).unwrap();
    let mut polls = 0;
    let result = readers.execute(handle, Command::Document { generation: 2, options: Default::default() }, || {
        polls += 1;
        if polls == 3 {
            readers.close(handle).unwrap();
            assert_eq!(readers.create(), Err(AccessError::Capacity));
        }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    assert_eq!(readers.create(), Err(AccessError::Capacity));
    drop(pin);
    let next = readers.create().unwrap(); assert_ne!(next, handle);
}

#[test]
fn invalid_document_requests_preserve_capture_and_never_become_empty_success() {
    let readers = ReaderSessions::new(); let empty = readers.create().unwrap();
    assert_eq!(readers.execute(empty, Command::Document { generation: 1, options: Default::default() }, || false).err(), Some(AccessError::NotOpen));
    readers.supply(empty, &[0xff, 0xfe, b'#', 0]).unwrap();
    assert!(readers.execute(empty, Command::Document { generation: 1, options: Default::default() }, || false).is_err());
    assert!(readers.execute(empty, Command::CopyRange { start: 0, end: 4 }, || false).is_ok());
    let handle = readers.create().unwrap(); readers.supply(handle, b"# Good\n").unwrap(); prepare(&readers, handle, 1);
    assert!(readers.execute(handle, Command::DocumentHeadings { generation: 1, first: usize::MAX, count: 1 }, || false).is_err());
    assert!(readers.execute(handle, Command::DocumentWindow { generation: 1, first: 0, count: 0 }, || false).is_err());
    assert!(readers.execute(handle, Command::DocumentFromSource { generation: 1, offset: u64::MAX, count: 1 }, || false).is_err());
    assert!(readers.execute(handle, Command::DocumentSource { generation: 1, start: 0, end: usize::MAX, context: 0 }, || false).is_err());
    assert!(window(&readers, handle, 1).as_str().contains("Good"));
}

#[test]
fn retained_document_registry_is_send_sync_and_can_run_on_host_owned_workers() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<ReaderSessions>();
    let readers = Arc::new(ReaderSessions::new()); let handle = readers.create().unwrap();
    readers.supply(handle, b"# Worker\n").unwrap();
    let worker = Arc::clone(&readers);
    let response = std::thread::spawn(move || prepare(&worker, handle, 1)).join().unwrap();
    assert!(response.as_str().contains("Worker"));
    assert!(window(&readers, handle, 1).as_str().contains("Worker"));
    readers.close(handle).unwrap();
    assert!(response.as_str().contains("Worker"));
}
