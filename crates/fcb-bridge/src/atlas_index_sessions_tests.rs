#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use super::*;
use crate::reader_sessions::Command as ReaderCommand;

fn root(source: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-index-registry-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap(); fs::write(root.join("source.rs"), source).unwrap(); root
}
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 16384, max_source_bytes: 65536, max_index_grams: 65536 }
}
fn open(table: &AtlasSessions, root: &std::path::Path) -> u64 {
    let handle = table.create().unwrap();
    table.open(handle, root, AtlasSessionOptions::default(), || false).unwrap(); handle
}
fn build(table: &AtlasSessions, handle: u64, generation: u64) {
    table.execute_index(handle, IndexCommand::Prepare { generation, options: options() }, || false).unwrap();
}
fn query(table: &AtlasSessions, handle: u64, generation: u64, index: u64, needle: &str) -> String {
    table.execute_index(handle, IndexCommand::Query { generation, index_generation: index,
        needle, max_matches: 100, max_scan_bytes: 65536 }, || false).unwrap().as_str().to_owned()
}
fn page(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::SearchPage { generation, start: 0, limit: 100 }, || false).unwrap().as_str().to_owned()
}

#[test]
fn indexed_query_to_captured_reader_survives_live_replacement_index_clear_and_atlas_close() {
    let path = root(b"fn original() { /* needle */ }\n");
    let table = AtlasSessions::new(); let readers = ReaderSessions::new(); let a = open(&table, &path);
    build(&table, a, 1);
    fs::write(path.join("source.rs"), b"fn replacement() {}\n").unwrap();
    assert!(query(&table, a, 2, 1, "needle").contains("\"retained_hits\":\"1\""));
    let r = readers.create().unwrap();
    let response = table.open_search_reader(a, &readers, r, 2, 1, || false).unwrap();
    assert!(response.as_str().contains("\"source_reopened\":false"));
    table.execute_index(a, IndexCommand::Clear { generation: 3 }, || false).unwrap();
    assert!(page(&table, a, 2).contains("\"index_generation\":\"1\""));
    table.close(a).unwrap();
    let result = readers.execute(r, ReaderCommand::Find { generation: 1, needle: "original", limit: 10, scan_bytes: 1000 }, || false).unwrap();
    assert!(result.as_str().contains("\"retained_hits\":\"1\""));
    readers.close(r).unwrap();
}

#[test]
fn equal_query_and_index_numbers_in_two_atlases_do_not_alias_source_or_lifetime() {
    let table = AtlasSessions::new();
    let a = open(&table, &root(b"alpha needle")); let b = open(&table, &root(b"beta target"));
    build(&table, a, 1); build(&table, b, 1);
    assert!(query(&table, a, 2, 1, "needle").contains("\"retained_hits\":\"1\""));
    assert!(query(&table, b, 2, 1, "needle").contains("\"retained_hits\":\"0\""));
    table.execute_index(a, IndexCommand::Clear { generation: 3 }, || false).unwrap();
    table.close(a).unwrap();
    assert!(query(&table, b, 3, 1, "target").contains("\"retained_hits\":\"1\""));
    assert!(table.execute_index(b, IndexCommand::Info, || false).unwrap().as_str().contains("\"index_generation\":\"1\""));
    table.close(b).unwrap();
}

#[test]
fn cancellation_during_rebuild_keeps_previous_index_and_query_usable() {
    let table = AtlasSessions::new(); let a = open(&table, &root(b"needle source"));
    build(&table, a, 1); query(&table, a, 2, 1, "needle");
    let mut polls = 0;
    let result = table.execute_index(a, IndexCommand::Prepare { generation: 3, options: options() }, || {
        polls += 1; if polls == 4 { table.cancel(a).unwrap(); } false
    });
    assert!(result.is_err());
    assert!(page(&table, a, 2).contains("\"retained_hits\":\"1\""));
    assert!(query(&table, a, 4, 1, "source").contains("\"retained_hits\":\"1\""));
    table.close(a).unwrap();
}

#[test]
fn cancellation_inside_indexed_query_restores_the_reusable_engine() {
    let table = AtlasSessions::new(); let a = open(&table, &root(b"old needle needle"));
    build(&table, a, 1); query(&table, a, 2, 1, "old");
    let mut polls = 0;
    let result = table.execute_index(a, IndexCommand::Query { generation: 3, index_generation: 1,
        needle: "needle", max_matches: 100, max_scan_bytes: 65536 }, || {
        polls += 1; if polls == 6 { table.cancel(a).unwrap(); } false
    });
    assert!(result.is_err());
    assert!(page(&table, a, 2).contains("\"needle\":\"old\""));
    assert!(query(&table, a, 4, 1, "needle").contains("\"retained_hits\":\"2\""));
    table.close(a).unwrap();
}

#[test]
fn a_busy_index_owner_does_not_block_another_atlas_or_the_table() {
    let table = AtlasSessions::new();
    let a = open(&table, &root(b"needle one")); let b = open(&table, &root(b"needle two"));
    build(&table, a, 1); build(&table, b, 1);
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute_index(a, IndexCommand::Info, || false).err(), Some(AccessError::Busy));
    assert!(query(&table, b, 2, 1, "needle").contains("\"retained_hits\":\"1\""));
    drop(guard); drop(cell);
    assert!(query(&table, a, 2, 1, "needle").contains("\"retained_hits\":\"1\""));
    table.close(a).unwrap(); table.close(b).unwrap();
}

#[test]
fn close_during_query_keeps_retiring_source_admission_until_the_call_drains() {
    let table = AtlasSessions::new(); let a = open(&table, &root(b"needle source")); build(&table, a, 1);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let result = table.execute_index(a, IndexCommand::Query { generation: 2, index_generation: 1,
        needle: "needle", max_matches: 100, max_scan_bytes: 65536 }, || {
        if !closed {
            table.close(a).unwrap(); closed = true;
            assert_eq!(table.create().err(), Some(AccessError::Capacity));
        }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    let replacement = table.create().unwrap(); assert_ne!(replacement, a);
    assert_eq!(table.execute_index(a, IndexCommand::Info, || false).err(), Some(AccessError::Unknown));
    table.close(replacement).unwrap(); for handle in others { table.close(handle).unwrap(); }
}

#[test]
fn destination_reader_admission_and_reader_cancellation_do_not_destroy_shared_index() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = open(&table, &root(b"needle source")); build(&table, a, 1); query(&table, a, 2, 1, "needle");
    let r = readers.create().unwrap(); table.open_search_reader(a, &readers, r, 2, 1, || false).unwrap();
    assert_eq!(table.open_search_reader(a, &readers, r, 2, 1, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    let empty = readers.create().unwrap();
    assert!(table.open_search_reader(a, &readers, empty, 2, 1, || true).is_err());
    assert!(query(&table, a, 3, 1, "source").contains("\"retained_hits\":\"1\""));
    table.open_search_reader(a, &readers, empty, 3, 1, || false).unwrap();
    readers.close(empty).unwrap(); readers.close(r).unwrap(); table.close(a).unwrap();
}

#[test]
fn prepared_index_can_move_with_its_atlas_owner_to_a_host_worker() {
    let path = root(b"needle source"); let table = AtlasSessions::new(); let a = open(&table, &path); build(&table, a, 1);
    fs::rename(path.join("source.rs"), path.join("moved.rs")).unwrap();
    let (table, result) = std::thread::spawn(move || {
        let result = query(&table, a, 2, 1, "needle"); (table, result)
    }).join().unwrap();
    assert!(result.contains("\"source_bytes_read\":\"0\""));
    assert!(result.contains("\"retained_hits\":\"1\""));
    assert!(table.execute_index(a, IndexCommand::Info, || false).is_ok());
    table.close(a).unwrap();
}

#[test]
fn one_byte_source_opens_without_inventing_decoder_padding_bytes() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = open(&table, &root(b"x")); build(&table, a, 1); query(&table, a, 2, 1, "x");
    let r = readers.create().unwrap();
    let reply = table.open_search_reader(a, &readers, r, 2, 1, || false).unwrap();
    assert!(reply.as_str().contains("\"text\":\"x\""));
    let copied = readers.execute(r, ReaderCommand::CopyRange { start: 0, end: 1 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"78\""));
    readers.close(r).unwrap(); table.close(a).unwrap();
}
