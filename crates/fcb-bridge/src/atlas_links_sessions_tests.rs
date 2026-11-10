#![forbid(unsafe_code)]

//! Registry integration against real captured Markdown, index identities and
//! independent readers. No live-path fallback can satisfy these assertions.
use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb_app::host::atlas_search::AtlasIndexOptions;
use std::{fs, path::PathBuf};

const DOC: &[u8] = b"# Guide\n\nSee [engine](src/lib.rs#entry), [missing](missing.rs), [remote](https://example.invalid/doc).\n";
const SOURCE: &[u8] = b"fn captured_entry() {}\n";
fn open(table: &AtlasSessions, files: &[(&str, &[u8])]) -> (u64, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-atlas-links-registry-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    for (name, bytes) in files {
        let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let handle = table.create().unwrap();
    table.open(handle, &root, AtlasSessionOptions::default(), || false).unwrap();
    table.execute_index(handle, IndexCommand::Prepare { generation: 1, options: AtlasIndexOptions {
        max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536, max_index_grams: 65536,
    } }, || false).unwrap();
    (handle, root)
}
fn begin(table: &AtlasSessions, handle: u64, generation: u64) -> HostResponse {
    table.execute_links(handle, LinksCommand::Begin { generation, index_generation: 1,
        max_links: 100, max_parse_bytes: 65536 }, || false).unwrap()
}
fn finish(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = table.execute_links(handle, LinksCommand::Step { generation }, || false).unwrap();
        if step.as_str().contains("\"relationships_in_progress\":false") { return step.as_str().to_owned(); }
    }
    panic!("bounded link inventory did not finish");
}
fn page(table: &AtlasSessions, handle: u64, generation: u64,
    direction: AtlasRelationshipDirection, path: Option<&[u8]>) -> Result<HostResponse, AccessError> {
    table.execute_links(handle, LinksCommand::Page { generation, direction,
        relative_path: path, start: 0, limit: 100 }, || false)
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn backlinks_open_exact_evidence_and_target_after_live_sources_change() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let (handle, root) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    let admitted = begin(&table, handle, 1);
    assert!(admitted.as_str().contains("\"source_bytes_read\":\"0\""));
    let result = finish(&table, handle, 1);
    assert_eq!(number(&result, "retained_links"), 3);
    assert!(result.contains("whole-captured-markdown-document"));
    let incoming = page(&table, handle, 1, AtlasRelationshipDirection::Incoming, Some(b"src/lib.rs")).unwrap();
    assert_eq!(incoming.as_str().matches("\"link_id\":").count(), 1);
    assert!(incoming.as_str().contains("src/lib.rs#entry"));
    let id = number(incoming.as_str(), "link_id");
    fs::write(root.join("README.md"), b"replacement documentation").unwrap();
    fs::write(root.join("src/lib.rs"), b"replacement target").unwrap();
    let evidence = readers.create().unwrap(); let target = readers.create().unwrap();
    for (reader, side) in [(evidence, AtlasRelationshipSide::Evidence), (target, AtlasRelationshipSide::Target)] {
        let receipt = table.open_link_reader(handle, &readers, reader, 1, id, side, || false).unwrap();
        assert!(receipt.as_str().contains("\"source_reopened\":false"));
        assert!(receipt.as_str().contains(&format!("\"reader_owner\":\"{reader}\"")));
    }
    table.execute_index(handle, IndexCommand::Clear { generation: 2 }, || false).unwrap();
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::All, None).is_err());
    table.close(handle).unwrap();
    for (reader, bytes) in [(evidence, DOC), (target, SOURCE)] {
        let captured = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: bytes.len() as u64 }, || false).unwrap();
        assert!(captured.as_str().contains(&hex(bytes)));
        readers.close(reader).unwrap();
    }
}

#[test]
fn relationship_attempts_are_independent_and_epoch_cancel_preserves_accepted_inventory() {
    let table = AtlasSessions::new();
    let (handle, _) = open(&table, &[("README.md", DOC), ("more.md", b"[engine](src/lib.rs)"), ("src/lib.rs", SOURCE)]);
    begin(&table, handle, 1); finish(&table, handle, 1);
    // Index/search and relationship generations are independent namespaces.
    table.execute(handle, Command::Search { generation: 2, needle: "captured_entry",
        options: AtlasSearchOptions { max_matches: 10, max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536 } },
        || false).unwrap();
    begin(&table, handle, 2);
    table.execute_links(handle, LinksCommand::Step { generation: 2 }, || false).unwrap();
    table.cancel(handle).unwrap();
    // Another command family must synchronize and retire the paused graph too.
    table.execute_index(handle, IndexCommand::Info, || false).unwrap();
    assert!(table.execute_links(handle, LinksCommand::Step { generation: 2 }, || false).is_err());
    let old = page(&table, handle, 1, AtlasRelationshipDirection::Incoming, Some(b"src/lib.rs")).unwrap();
    assert_eq!(old.as_str().matches("\"link_id\":").count(), 2);
    assert!(table.execute_links(handle, LinksCommand::Begin { generation: 2, index_generation: 1,
        max_links: 100, max_parse_bytes: 65536 }, || false).is_err());
    assert!(table.execute_links(handle, LinksCommand::Begin { generation: 3, index_generation: 999,
        max_links: 100, max_parse_bytes: 65536 }, || false).is_err());
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::All, None).is_ok());
    assert!(table.execute_links(handle, LinksCommand::Begin { generation: 3, index_generation: 1,
        max_links: 100, max_parse_bytes: 65536 }, || false).is_err());
    begin(&table, handle, 4); finish(&table, handle, 4);
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::All, None).is_err());
    assert!(table.execute(handle, Command::SearchPage { generation: 2, start: 0, limit: 10 }, || false).is_ok());
    table.close(handle).unwrap();
}

#[test]
fn unresolved_targets_and_occupied_readers_never_install_another_source() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let (handle, _) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    begin(&table, handle, 1); finish(&table, handle, 1);
    let empty = readers.create().unwrap();
    for id in [0, 2, 3, 999] {
        assert!(table.open_link_reader(handle, &readers, empty, 1, id, AtlasRelationshipSide::Target, || false).is_err());
        assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    }
    table.open_link_reader(handle, &readers, empty, 1, 1, AtlasRelationshipSide::Evidence, || false).unwrap();
    assert_eq!(table.open_link_reader(handle, &readers, empty, 1, 1, AtlasRelationshipSide::Target, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::Incoming, None).is_err());
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::Outgoing, Some(b"absent.md")).is_err());
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::All, Some(b"../README.md")).is_err());
    let evidence = readers.execute(empty, ReaderCommand::CopyRange { start: 0, end: DOC.len() as u64 }, || false).unwrap();
    assert!(evidence.as_str().contains(&hex(DOC)));
    readers.close(empty).unwrap(); table.close(handle).unwrap();
}

#[test]
fn parse_budget_partial_state_cannot_become_complete_empty_backlinks() {
    let table = AtlasSessions::new(); let (handle, _) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    table.execute_links(handle, LinksCommand::Begin { generation: 1, index_generation: 1,
        max_links: 100, max_parse_bytes: 0 }, || false).unwrap();
    let result = finish(&table, handle, 1);
    assert!(result.contains("\"relationships_complete\":false"));
    assert_eq!(number(&result, "retained_links"), 0);
    let incoming = page(&table, handle, 1, AtlasRelationshipDirection::Incoming, Some(b"src/lib.rs")).unwrap();
    assert!(incoming.as_str().contains("\"relationships_complete\":false"));
    begin(&table, handle, 2); finish(&table, handle, 2);
    let incoming = page(&table, handle, 2, AtlasRelationshipDirection::Incoming, Some(b"src/lib.rs")).unwrap();
    assert_eq!(incoming.as_str().matches("\"link_id\":").count(), 1);
    table.close(handle).unwrap();
}

#[test]
fn busy_link_worker_does_not_block_another_atlas_or_cancellation() {
    let table = AtlasSessions::new();
    let (a, _) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    let (b, _) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    begin(&table, a, 1); begin(&table, b, 1);
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute_links(a, LinksCommand::Step { generation: 1 }, || false).err(), Some(AccessError::Busy));
    assert_eq!(number(&finish(&table, b, 1), "retained_links"), 3);
    table.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(table.execute_links(a, LinksCommand::Step { generation: 1 }, || false).is_err());
    table.close(a).unwrap(); table.close(b).unwrap();
}

#[test]
fn stream_progress_cancellation_retires_a_paused_graph_before_another_command() {
    let table = AtlasSessions::new();
    let (handle, _) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    begin(&table, handle, 1); finish(&table, handle, 1);
    begin(&table, handle, 2);
    let mut callbacks = 0;
    let canceled = table.execute_stream_with_progress(handle, "captured_entry",
        fcb_app::host::atlas_stream_search::AtlasStreamSearchOptions::default(), || false, || false, |_| {
            callbacks += 1; table.cancel(handle).unwrap();
        });
    assert_eq!(callbacks, 1);
    assert_eq!(canceled.err(), Some(AccessError::Canceled));
    // Check immediate retirement, before another command can synchronize the
    // epoch and conceal a missed cleanup path in progress dispatch.
    let cell = table.get(handle).unwrap(); let state = lock(&cell.state).unwrap();
    let session = state.as_ref().unwrap();
    assert_eq!(session.search.pending_relationship_generation(), None);
    assert_eq!(session.search.relationship_generation(), Some(1));
    drop(state); drop(cell);
    assert!(page(&table, handle, 1, AtlasRelationshipDirection::Incoming, Some(b"src/lib.rs")).is_ok());
    begin(&table, handle, 3); finish(&table, handle, 3);
    table.close(handle).unwrap();
}

#[test]
fn close_during_link_admission_reserves_capacity_until_worker_drain() {
    let table = AtlasSessions::new(); let (handle, _) = open(&table, &[("README.md", DOC), ("src/lib.rs", SOURCE)]);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let result = table.execute_links(handle, LinksCommand::Begin { generation: 1, index_generation: 1,
        max_links: 100, max_parse_bytes: 65536 }, || {
        if !closed {
            closed = true; table.close(handle).unwrap();
            assert_eq!(table.create().err(), Some(AccessError::Capacity));
        }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    let replacement = table.create().unwrap(); assert_ne!(replacement, handle);
    table.close(replacement).unwrap(); for handle in others { table.close(handle).unwrap(); }
}
