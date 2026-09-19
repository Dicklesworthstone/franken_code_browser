#![forbid(unsafe_code)]

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use super::*;
use crate::reader_sessions::Command as ReaderCommand;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-index-construction-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rs"), b"old needle").unwrap();
        fs::write(root.join("b.rs"), b"second needle").unwrap(); Self(root)
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 1024, max_source_bytes: 65536, max_index_grams: 65536 }
}
fn open(table: &AtlasSessions, root: &Path) -> u64 {
    let handle = table.create().unwrap();
    table.open(handle, root, AtlasSessionOptions::default(), || false).unwrap(); handle
}
fn begin(table: &AtlasSessions, handle: u64, generation: u64) {
    table.execute_index(handle, IndexCommand::PrepareBegin { generation, options: options() }, || false).unwrap();
}
fn step(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    table.execute_index(handle, IndexCommand::PrepareStep { generation }, || false).unwrap().as_str().to_owned()
}
fn query(table: &AtlasSessions, handle: u64, generation: u64, index: u64) -> String {
    table.execute_index(handle, IndexCommand::Query { generation, index_generation: index,
        needle: "old", max_matches: 100, max_scan_bytes: 65536 }, || false).unwrap().as_str().to_owned()
}
fn prepared(table: &AtlasSessions, handle: u64) {
    table.execute_index(handle, IndexCommand::Prepare { generation: 1, options: options() }, || false).unwrap();
    query(table, handle, 2, 1);
}

#[test]
fn registry_begin_is_metadata_only_and_steps_build_real_source_members() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let a = open(&table, &f.0); begin(&table, a, 1);
    let info = table.execute_index(a, IndexCommand::PrepareProgress { generation: 1 }, || false).unwrap();
    assert!(info.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    assert!(info.as_str().contains("\"captured_files\":\"0\""));
    fs::remove_file(f.0.join("a.rs")).unwrap();
    assert!(step(&table, a, 1).contains("\"index_build_in_progress\":true"));
    let done = step(&table, a, 1);
    assert!(done.contains("\"index_build_in_progress\":false"));
    assert!(done.contains("\"unavailable_files\":\"1\""));
    assert!(done.contains("\"captured_files\":\"1\""));
    assert!(done.contains("\"capture_complete\":false"));
    table.close(a).unwrap();
    assert!(info.as_str().contains("\"index_generation\":\"1\"")); // Owned reply survives close.
}

#[test]
fn canceled_paused_builder_is_retired_even_by_an_ordinary_atlas_operation() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let a = open(&table, &f.0); prepared(&table, a);
    begin(&table, a, 3); step(&table, a, 3); table.cancel(a).unwrap();
    table.execute(a, Command::Info, || false).unwrap();
    assert!(table.execute_index(a, IndexCommand::PrepareStep { generation: 3 }, || false).is_err());
    assert!(table.execute_index(a, IndexCommand::PrepareProgress { generation: 3 }, || false).is_err());
    assert!(query(&table, a, 4, 1).contains("\"retained_hits\":\"1\""));
    table.close(a).unwrap();
}

#[test]
fn cancel_during_construction_step_keeps_the_preceding_index_and_rows() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let a = open(&table, &f.0); prepared(&table, a);
    begin(&table, a, 3); let mut polls = 0;
    let result = table.execute_index(a, IndexCommand::PrepareStep { generation: 3 }, || {
        polls += 1; if polls == 10 { table.cancel(a).unwrap(); } false
    });
    assert!(polls >= 10); assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(table.execute(a, Command::SearchPage { generation: 2, start: 0, limit: 10 }, || false).is_ok());
    assert!(query(&table, a, 4, 1).contains("\"retained_hits\":\"1\""));
    begin(&table, a, 5); step(&table, a, 5); step(&table, a, 5);
    table.close(a).unwrap();
}

#[test]
fn old_index_queries_and_readers_work_during_rebuild_and_survive_replacement() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = open(&table, &f.0); prepared(&table, a);
    fs::write(f.0.join("a.rs"), b"new source").unwrap();
    begin(&table, a, 3); step(&table, a, 3);
    assert!(query(&table, a, 4, 1).contains("\"retained_hits\":\"1\""));
    let r = readers.create().unwrap(); table.open_search_reader(a, &readers, r, 4, 1, || false).unwrap();
    step(&table, a, 3);
    assert!(table.execute_index(a, IndexCommand::Info, || false).unwrap().as_str().contains("\"index_generation\":\"3\""));
    let other = readers.create().unwrap(); table.open_search_reader(a, &readers, other, 4, 1, || false).unwrap();
    table.close(a).unwrap();
    for reader in [r, other] {
        let copy = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 3 }, || false).unwrap();
        assert!(copy.as_str().contains("\"original_hex\":\"6f6c64\"")); readers.close(reader).unwrap();
    }
}

#[test]
fn reader_only_cancellation_cannot_cancel_the_atlas_builder() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = open(&table, &f.0); prepared(&table, a);
    let r = readers.create().unwrap(); table.open_search_reader(a, &readers, r, 2, 1, || false).unwrap();
    begin(&table, a, 3); readers.cancel(r).unwrap();
    step(&table, a, 3); step(&table, a, 3);
    assert!(table.execute_index(a, IndexCommand::Info, || false).is_ok());
    assert!(readers.execute(r, ReaderCommand::Window { offset: 0, bytes: 100 }, || false).is_ok());
    readers.close(r).unwrap(); table.close(a).unwrap();
}

#[test]
fn close_during_step_holds_retiring_capacity_until_captured_work_drains() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let a = open(&table, &f.0); begin(&table, a, 1);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let result = table.execute_index(a, IndexCommand::PrepareStep { generation: 1 }, || {
        if !closed { closed = true; table.close(a).unwrap(); assert_eq!(table.create().err(), Some(AccessError::Capacity)); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    let replacement = table.create().unwrap(); assert_ne!(replacement, a);
    assert_eq!(table.execute_index(a, IndexCommand::PrepareProgress { generation: 1 }, || false).err(), Some(AccessError::Unknown));
    table.close(replacement).unwrap(); for handle in others { table.close(handle).unwrap(); }
}

#[test]
fn contention_and_worker_handoff_do_not_alias_independent_construction_sessions() {
    let f = Fixture::new(); let g = Fixture::new(); let table = AtlasSessions::new();
    let a = open(&table, &f.0); let b = open(&table, &g.0); begin(&table, a, 1); begin(&table, b, 1);
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute_index(a, IndexCommand::PrepareStep { generation: 1 }, || false).err(), Some(AccessError::Busy));
    step(&table, b, 1); step(&table, b, 1); drop(guard); drop(cell);
    let table = std::thread::spawn(move || { step(&table, a, 1); step(&table, a, 1); table }).join().unwrap();
    table.close(b).unwrap(); assert!(query(&table, a, 2, 1).contains("\"retained_hits\":\"1\""));
    table.close(a).unwrap();
}

#[test]
fn supersession_and_clear_cannot_resume_an_older_build_or_discard_accepted_hit_pins() {
    let f = Fixture::new(); let table = AtlasSessions::new(); let a = open(&table, &f.0); prepared(&table, a);
    begin(&table, a, 3); step(&table, a, 3); begin(&table, a, 4);
    assert!(table.execute_index(a, IndexCommand::PrepareStep { generation: 3 }, || false).is_err());
    let progress = table.execute_index(a, IndexCommand::PrepareProgress { generation: 4 }, || false).unwrap();
    assert!(progress.as_str().contains("\"examined_files\":\"0\""));
    table.execute_index(a, IndexCommand::Clear { generation: 5 }, || false).unwrap();
    assert!(table.execute_index(a, IndexCommand::PrepareStep { generation: 4 }, || false).is_err());
    assert!(table.execute(a, Command::SearchPage { generation: 2, start: 0, limit: 10 }, || false).is_ok());
    table.close(a).unwrap();
}
