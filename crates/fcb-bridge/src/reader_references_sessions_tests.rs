#![forbid(unsafe_code)]
use super::*;
use crate::atlas_sessions::{AtlasSessions, Command as AtlasCommand};
use fcb_app::host::atlas_search::AtlasSearchOptions;
use std::fs;

fn opened(readers: &ReaderSessions, bytes: &[u8]) -> u64 {
    let handle = readers.create().unwrap();
    readers.supply(handle, bytes).unwrap(); handle
}
fn references(readers: &ReaderSessions, handle: u64, generation: u64, name: &str) -> HostResponse {
    readers.execute(handle, Command::References { generation, name, max_items: 4096 }, || false).unwrap()
}
fn page(readers: &ReaderSessions, handle: u64, generation: u64) -> HostResponse {
    readers.execute(handle, Command::ReferencePage { generation, start: 0, limit: 128 }, || false).unwrap()
}

#[test]
fn atlas_search_to_reader_occurrences_never_substitutes_current_source() {
    let root = std::env::temp_dir().join(format!("fcb-reference-journey-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("source.rs"), b"fn needle() {}\nneedle(); // needle\nneedle_suffix\n").unwrap();
    let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let atlas = atlases.create().unwrap();
    atlases.open(atlas, &root, Default::default(), || false).unwrap();
    atlases.execute(atlas, AtlasCommand::Search { generation: 1, needle: "needle",
        options: AtlasSearchOptions::default() }, || false).unwrap();
    fs::write(root.join("source.rs"), b"fn replacement() {}\n").unwrap();
    let reader = readers.create().unwrap();
    let handoff = atlases.open_search_reader(atlas, &readers, reader, 1, 1, || false).unwrap();
    assert!(handoff.as_str().contains("\"source_reopened\":false"));
    atlases.close(atlas).unwrap();
    let inventory = references(&readers, reader, 1, "needle");
    assert!(inventory.as_str().contains("\"retained_references\":\"3\""));
    assert!(inventory.as_str().contains("\"includes_comments_and_literals\":true"));
    let exact = readers.execute(reader, Command::Reference { generation: 1, id: 2, context: 128 }, || false).unwrap();
    assert!(exact.as_str().contains("needle(); // needle"));
    assert!(exact.as_str().contains("\"original_range\":{\"start\":\"15\",\"end\":\"21\"}"));
    let copy = readers.execute(reader, Command::CopyReference { generation: 1, id: 2 }, || false).unwrap();
    assert!(copy.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    readers.close(reader).unwrap();
    // Returned evidence remains owned after every producing handle has closed.
    assert!(exact.as_str().contains("\"selection_namespace\":\"reference\""));
    assert!(!exact.as_str().contains("replacement"));
}

#[test]
fn reference_generation_is_independent_and_clear_preserves_find_and_outline() {
    let readers = ReaderSessions::new(); let handle = opened(&readers, b"fn needle() { needle(); }\n");
    readers.execute(handle, Command::Find { generation: 1, needle: "needle", limit: 10, scan_bytes: 1024 }, || false).unwrap();
    readers.execute(handle, Command::Outline { generation: 1, options: Default::default() }, || false).unwrap();
    references(&readers, handle, 1, "needle");
    readers.execute(handle, Command::ClearReferences { generation: 2 }, || false).unwrap();
    assert_eq!(readers.execute(handle, Command::CopyReference { generation: 1, id: 1 }, || false).err(),
        Some(AccessError::Reader(ReaderSessionError::MissingReferences)));
    assert!(readers.execute(handle, Command::CopyHit { generation: 1, index: 0 }, || false).unwrap()
        .as_str().contains("6e6565646c65"));
    assert!(readers.execute(handle, Command::Symbol { generation: 1, id: 1, context: 0 }, || false).unwrap()
        .as_str().contains("\"selection_namespace\":\"outline\""));
    let info = readers.execute(handle, Command::Info, || false).unwrap();
    assert!(info.as_str().contains("\"accepted_reference_generation\":null"));
    assert!(info.as_str().contains("\"last_reference_attempt\":\"2\""));
    assert!(readers.execute(handle, Command::References { generation: 2, name: "needle", max_items: 10 }, || false).is_err());
    references(&readers, handle, 3, "needle"); readers.close(handle).unwrap();
}

#[test]
fn cancellation_and_failed_replacement_preserve_accepted_references() {
    let readers = ReaderSessions::new(); let handle = opened(&readers, b"foo foo_ foo\n");
    references(&readers, handle, 1, "foo");
    let mut canceled = false;
    let result = readers.execute(handle, Command::References { generation: 2, name: "foo_", max_items: 10 }, || {
        if !canceled { canceled = true; readers.cancel(handle).unwrap(); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(page(&readers, handle, 1).as_str().contains("\"retained_references\":\"2\""));
    assert!(readers.execute(handle, Command::Window { offset: 0, bytes: 100 }, || false).is_ok());
    assert_eq!(readers.execute(handle, Command::References { generation: 3, name: "foo.bar", max_items: 10 }, || false).err(),
        Some(AccessError::Reader(ReaderSessionError::Reference(ReferenceError::InvalidName))));
    assert!(page(&readers, handle, 1).as_str().contains("\"name\":\"foo\""));
    assert!(readers.execute(handle, Command::References { generation: 3, name: "foo_", max_items: 10 }, || false).is_err());
    references(&readers, handle, 4, "foo_");
    assert_eq!(readers.execute(handle, Command::Reference { generation: 1, id: 1, context: 0 }, || false).err(),
        Some(AccessError::Reader(ReaderSessionError::Reference(ReferenceError::StaleQuery))));
    readers.close(handle).unwrap();
}

#[test]
fn direct_extractor_cancellation_is_not_serialized_as_generic_failure() {
    let readers = ReaderSessions::new(); let handle = opened(&readers, &vec![b'x'; 16 * 1024]);
    let mut polls = 0;
    let error = readers.execute(handle, Command::References { generation: 1, name: "foo", max_items: 10 }, || {
        polls += 1; polls == 3
    }).err().expect("one-shot cancellation must terminate extraction");
    assert!(error.canceled(), "{error:?}");
    assert!(error.json(handle).contains("\"exit_code\":\"130\""));
    let inner = AccessError::Reader(ReaderSessionError::Reference(ReferenceError::Canceled));
    assert!(inner.canceled()); assert!(inner.json(handle).contains("\"exit_code\":\"130\""));
    references(&readers, handle, 2, "foo"); readers.close(handle).unwrap();
}

#[test]
fn busy_reference_reader_does_not_block_another_owner_or_cancel() {
    let readers = ReaderSessions::new();
    let a = opened(&readers, b"first first"); let b = opened(&readers, b"second second");
    references(&readers, a, 1, "first"); references(&readers, b, 1, "second");
    let cell = readers.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(readers.execute(a, Command::Reference { generation: 1, id: 1, context: 0 }, || false).err(),
        Some(AccessError::Busy));
    assert!(page(&readers, b, 1).as_str().contains("\"name\":\"second\""));
    readers.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(page(&readers, a, 1).as_str().contains("\"name\":\"first\""));
    readers.close(a).unwrap(); readers.close(b).unwrap();
}

#[test]
fn closing_during_occurrence_work_preserves_retiring_capacity() {
    let readers = ReaderSessions::new();
    let handles: Vec<_> = (0..MAX_READER_SESSIONS).map(|_| readers.create().unwrap()).collect();
    readers.supply(handles[0], b"foo foo").unwrap();
    references(&readers, handles[0], 1, "foo"); let mut closed = false;
    let result = readers.execute(handles[0], Command::References { generation: 2, name: "foo", max_items: 10 }, || {
        if !closed {
            closed = true; readers.close(handles[0]).unwrap();
            assert_eq!(readers.live.load(Ordering::Acquire), MAX_READER_SESSIONS);
            assert_eq!(readers.create().err(), Some(AccessError::Capacity));
        }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    assert_eq!(readers.live.load(Ordering::Acquire), MAX_READER_SESSIONS - 1);
    let next = readers.create().unwrap(); assert_ne!(next, handles[0]); readers.close(next).unwrap();
    for &handle in &handles[1..] { readers.close(handle).unwrap(); }
    assert_eq!(readers.live.load(Ordering::Acquire), 0);
    assert_eq!(readers.budget.accounting().reserved().get(), 0);
}

#[test]
fn fork_uses_same_bytes_but_earns_its_own_reference_authority() {
    let readers = ReaderSessions::new(); let original = opened(&readers, b"foo foo");
    references(&readers, original, 1, "foo");
    let fork = readers.create().unwrap(); readers.fork(original, fork, || false).unwrap();
    assert_eq!(readers.execute(fork, Command::Reference { generation: 1, id: 1, context: 0 }, || false).err(),
        Some(AccessError::Reader(ReaderSessionError::MissingReferences)));
    references(&readers, fork, 1, "foo"); readers.close(original).unwrap();
    assert!(readers.execute(fork, Command::CopyReference { generation: 1, id: 2 }, || false).unwrap()
        .as_str().contains("\"original_hex\":\"666f6f\""));
    readers.close(fork).unwrap();
}
