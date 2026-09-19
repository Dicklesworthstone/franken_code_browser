#![forbid(unsafe_code)]

//! Real registry/host workflows. No process-global FFI capacity is consumed.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use super::*;
use crate::reader_sessions::Command as ReaderCommand;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-index-paused-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    for name in ["a.rs", "b.rs", "c.rs"] { fs::write(root.join(name), b"fn source() { /* needle old */ }\n").unwrap(); }
    root
}
fn open(table: &AtlasSessions) -> (u64, PathBuf) {
    let root = root(); let handle = table.create().unwrap();
    table.open(handle, &root, AtlasSessionOptions::default(), || false).unwrap();
    table.execute_index(handle, IndexCommand::Prepare { generation: 1, options: AtlasIndexOptions {
        max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536, max_index_grams: 65536,
    } }, || false).unwrap();
    (handle, root)
}
fn begin(table: &AtlasSessions, handle: u64, generation: u64) {
    let response = table.execute_index(handle, IndexCommand::Begin { generation, index_generation: 1,
        needle: "needle", max_matches: 100, max_scan_bytes: 65536 }, || false).unwrap();
    assert!(response.as_str().contains("\"search_in_progress\":true"));
}
fn step(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::SearchStep { generation }, || false).unwrap().as_str().to_owned()
}
fn page(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::SearchPage { generation, start: 0, limit: 100 }, || false).unwrap().as_str().to_owned()
}
fn accepted(table: &AtlasSessions, handle: u64) {
    table.execute_index(handle, IndexCommand::Query { generation: 2, index_generation: 1,
        needle: "old", max_matches: 100, max_scan_bytes: 65536 }, || false).unwrap();
}

#[test]
fn cancel_between_steps_retires_provisional_rows_before_page_or_resume() {
    let table = AtlasSessions::new(); let (a, _) = open(&table); accepted(&table, a);
    begin(&table, a, 3); step(&table, a, 3);
    table.cancel(a).unwrap();
    assert!(table.execute(a, Command::SearchPage { generation: 3, start: 0, limit: 10 }, || false).is_err());
    assert!(table.execute(a, Command::SearchStep { generation: 3 }, || false).is_err());
    assert!(page(&table, a, 2).contains("\"needle\":\"old\""));
    assert!(table.execute_index(a, IndexCommand::Info, || false).unwrap().as_str().contains("\"index_generation\":\"1\""));
    begin(&table, a, 4); for _ in 0..3 { step(&table, a, 4); }
    assert!(page(&table, a, 4).contains("\"search_complete\":true"));
    table.close(a).unwrap();
}

#[test]
fn cancellation_during_step_discards_only_replacement_and_preserves_index() {
    let table = AtlasSessions::new(); let (a, _) = open(&table); accepted(&table, a); begin(&table, a, 3);
    let mut polls = 0;
    let result = table.execute(a, Command::SearchStep { generation: 3 }, || {
        polls += 1; if polls == 3 { table.cancel(a).unwrap(); } false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(page(&table, a, 2).contains("\"needle\":\"old\""));
    begin(&table, a, 4); for _ in 0..3 { step(&table, a, 4); }
    assert!(page(&table, a, 4).contains("\"retained_hits\":\"3\""));
    table.close(a).unwrap();
}

#[test]
fn early_reader_remains_exact_after_cancel_index_clear_and_atlas_close() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new(); let (a, root) = open(&table);
    begin(&table, a, 2); step(&table, a, 2);
    fs::write(root.join("a.rs"), b"replacement").unwrap();
    let reader = readers.create().unwrap();
    let response = table.open_search_reader(a, &readers, reader, 2, 1, || false).unwrap();
    assert!(response.as_str().contains("\"search_in_progress\":true"));
    table.cancel(a).unwrap();
    table.execute_index(a, IndexCommand::Clear { generation: 3 }, || false).unwrap();
    table.close(a).unwrap();
    let result = readers.execute(reader, ReaderCommand::Find { generation: 1, needle: "needle", limit: 10, scan_bytes: 1000 }, || false).unwrap();
    assert!(result.as_str().contains("\"retained_hits\":\"1\""));
    readers.close(reader).unwrap();
}

#[test]
fn reader_only_cancel_does_not_destroy_a_query_shared_by_other_readers() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new(); let (a, _) = open(&table);
    begin(&table, a, 2); step(&table, a, 2);
    let reader = readers.create().unwrap(); let mut canceled = false;
    let result = table.open_search_reader(a, &readers, reader, 2, 1, || {
        if !canceled { readers.cancel(reader).unwrap(); canceled = true; } false
    });
    assert!(result.is_err());
    assert!(page(&table, a, 2).contains("\"search_in_progress\":true"));
    step(&table, a, 2); step(&table, a, 2);
    table.open_search_reader(a, &readers, reader, 2, 1, || false).unwrap();
    assert_eq!(table.open_search_reader(a, &readers, reader, 2, 1, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    readers.close(reader).unwrap(); table.close(a).unwrap();
}

#[test]
fn busy_paused_atlas_does_not_block_other_instances_and_cancel_is_owner_local() {
    let table = AtlasSessions::new(); let (a, _) = open(&table); let (b, _) = open(&table);
    begin(&table, a, 2); begin(&table, b, 2);
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute(a, Command::SearchStep { generation: 2 }, || false).err(), Some(AccessError::Busy));
    assert!(step(&table, b, 2).contains("\"retained_hits\":\"1\""));
    table.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(table.execute(a, Command::SearchStep { generation: 2 }, || false).is_err());
    step(&table, b, 2); step(&table, b, 2);
    assert!(page(&table, b, 2).contains("\"search_complete\":true"));
    table.close(a).unwrap(); table.close(b).unwrap();
}

#[test]
fn close_during_step_keeps_retiring_admission_until_the_call_drains() {
    let table = AtlasSessions::new(); let (a, _) = open(&table); begin(&table, a, 2);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let response = table.execute(a, Command::SearchStep { generation: 2 }, || {
        if !closed {
            table.close(a).unwrap(); closed = true;
            assert_eq!(table.create().err(), Some(AccessError::Capacity));
        }
        false
    });
    assert_eq!(response.err(), Some(AccessError::Closed));
    let replacement = table.create().unwrap(); assert_ne!(replacement, a);
    table.close(replacement).unwrap(); for handle in others { table.close(handle).unwrap(); }
}

#[test]
fn stale_step_and_failed_begin_cannot_replace_the_wrong_query() {
    let table = AtlasSessions::new(); let (a, _) = open(&table); accepted(&table, a);
    begin(&table, a, 3); begin(&table, a, 4);
    assert!(table.execute(a, Command::SearchStep { generation: 3 }, || false).is_err());
    assert!(step(&table, a, 4).contains("\"retained_hits\":\"1\""));
    let invalid = table.execute_index(a, IndexCommand::Begin { generation: 5, index_generation: 999,
        needle: "needle", max_matches: 100, max_scan_bytes: 1000 }, || false);
    assert!(invalid.is_err());
    assert!(table.execute(a, Command::SearchStep { generation: 4 }, || false).is_err());
    assert!(page(&table, a, 2).contains("\"needle\":\"old\""));
    begin(&table, a, 6); for _ in 0..3 { step(&table, a, 6); }
    let before = page(&table, a, 6);
    step(&table, a, 6);
    assert_eq!(page(&table, a, 6), before);
    table.close(a).unwrap();
}

#[test]
fn paused_cursor_survives_explicit_worker_handoff_without_recapturing_source() {
    let table = AtlasSessions::new(); let (a, root) = open(&table); begin(&table, a, 2);
    step(&table, a, 2);
    fs::rename(root.join("b.rs"), root.join("moved.rs")).unwrap();
    let table = std::thread::spawn(move || { step(&table, a, 2); table }).join().unwrap();
    step(&table, a, 2);
    let result = page(&table, a, 2);
    assert!(result.contains("\"retained_hits\":\"3\""));
    assert!(result.contains("\"source_bytes_read\":\"0\""));
    assert!(result.contains("\"step_count\":\"3\""));
    table.close(a).unwrap();
}
