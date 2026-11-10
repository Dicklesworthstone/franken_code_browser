#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}, path::PathBuf};

struct Fixture { root: PathBuf, before: PathBuf, after: PathBuf, before_payload: u64, after_payload: u64 }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-snapshot-comparison-handles-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&path).unwrap(); path
}
fn archive(root: &Path, name: &str, complete: bool, entries: &[SnapshotEntry<'_>]) -> PathBuf {
    let owner = ArenaOwnerId::new(9980).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, complete, "comparison-test", entries, SnapshotLimits::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join(name); fs::write(&path, bytes.bytes()).unwrap(); path
}
fn captured<'a>(path: &'a [u8], bytes: &'a [u8]) -> SnapshotEntry<'a> {
    SnapshotEntry { path, observed_bytes: bytes.len() as u64, data: SnapshotData::Captured(bytes) }
}
fn fixture(before_bytes: &[u8], after_bytes: &[u8]) -> Fixture {
    assert!(!before_bytes.is_empty() && !after_bytes.is_empty());
    let root = root();
    let before = archive(&root, "before.fcbs", true, &[captured(b"source.rs", before_bytes)]);
    let after = archive(&root, "after.fcbs", true, &[captured(b"source.rs", after_bytes)]);
    let before_payload = fs::read(&before).unwrap().windows(before_bytes.len())
        .position(|bytes| bytes == before_bytes).unwrap() as u64;
    let after_payload = fs::read(&after).unwrap().windows(after_bytes.len())
        .position(|bytes| bytes == after_bytes).unwrap() as u64;
    Fixture { root, before, after, before_payload, after_payload }
}
fn open(table: &SnapshotComparisonSessions, f: &Fixture) -> u64 {
    let handle = table.create().unwrap();
    table.open(handle, &f.before, &f.after, SnapshotLimits::default(), || false).unwrap(); handle
}
fn finish(table: &SnapshotComparisonSessions, handle: u64, generation: u64) -> String {
    let mut text = table.execute(handle, Command::Begin { generation }, || false).unwrap().as_str().to_owned();
    for _ in 0..1024 {
        if text.contains("\"comparison_in_progress\":false") { return text; }
        text = table.execute(handle, Command::Step { generation, max_entries: 1 }, || false).unwrap().as_str().to_owned();
    }
    panic!("bounded fixture did not complete");
}
fn page(table: &SnapshotComparisonSessions, handle: u64, generation: u64, filter: SnapshotChangeFilter) -> String {
    table.execute(handle, Command::Page { generation, filter, start: 0, limit: 128 }, || false)
        .unwrap().as_str().to_owned()
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn numbers(text: &str, key: &str) -> Vec<u64> {
    text.split(&format!("\"{key}\":\"")).skip(1)
        .map(|tail| tail.split('"').next().unwrap().parse().unwrap()).collect()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn pending(table: &SnapshotComparisonSessions, handle: u64) -> Option<u64> {
    let cell = table.get(handle).unwrap();
    let state = lock(&cell.state).unwrap();
    state.as_ref().unwrap().inner.pending_generation()
}

#[test]
fn both_exact_sides_survive_archive_path_replacement_and_comparison_close() {
    let old = b"fn old_capture() {}\n"; let new = b"fn new_capture() {}\n";
    let f = fixture(old, new); let table = SnapshotComparisonSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); finish(&table, handle, 1);
    let rows = page(&table, handle, 1, SnapshotChangeFilter::Changed);
    assert_eq!(number(&rows, "matching_entries"), 1); assert!(rows.contains("\"kind\":\"changed\""));
    fs::rename(&f.before, f.root.join("before-moved.fcbs")).unwrap();
    fs::rename(&f.after, f.root.join("after-moved.fcbs")).unwrap();
    fs::write(&f.before, b"unrelated replacement").unwrap();
    fs::write(&f.after, b"unrelated replacement").unwrap();
    fs::write(f.root.join("source.rs"), b"new live source is not archive authority").unwrap();
    let before_reader = readers.create().unwrap(); let after_reader = readers.create().unwrap();
    for (reader, side) in [(before_reader, SnapshotComparisonSide::Before), (after_reader, SnapshotComparisonSide::After)] {
        let reply = table.open_reader(handle, &readers, reader, 1, 1, side, || false).unwrap();
        assert_eq!(number(reply.as_str(), "reader_owner"), reader);
        assert_eq!(number(reply.as_str(), "entry_id"), 1);
    }
    table.close(handle).unwrap();
    for (reader, bytes) in [(before_reader, old.as_slice()), (after_reader, new.as_slice())] {
        let reply = readers.execute(reader, ReaderCommand::Window { offset: 0, bytes: 1024 }, || false).unwrap();
        assert!(reply.as_str().contains(&hex(bytes))); readers.close(reader).unwrap();
    }
}

#[test]
fn filters_keep_global_entry_ids_and_metadata_work_never_reads_member_payloads() {
    let root = root();
    let before = archive(&root, "before.fcbs", true, &[
        captured(b"a.rs", b"old"), captured(b"gone.rs", b"gone"), captured(b"same.rs", b"same"),
        SnapshotEntry { path: b"unknown.rs", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("NOT_CAPTURED") },
    ]);
    let after = archive(&root, "after.fcbs", true, &[
        captured(b"a.rs", b"new"), captured(b"new.rs", b"added"), captured(b"same.rs", b"same"),
        SnapshotEntry { path: b"unknown.rs", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("NOT_CAPTURED") },
    ]);
    let table = SnapshotComparisonSessions::new(); let handle = table.create().unwrap();
    table.open(handle, &before, &after, SnapshotLimits::default(), || false).unwrap();
    let completed = finish(&table, handle, 7);
    assert!(completed.contains("\"scan_complete\":true"));
    assert_eq!(number(&completed, "retained_entries"), 5);
    let all = page(&table, handle, 7, SnapshotChangeFilter::All);
    assert_eq!(numbers(&all, "entry_id"), vec![1, 2, 3, 4, 5]);
    for (filter, expected) in [(SnapshotChangeFilter::Changed, vec![1]), (SnapshotChangeFilter::Removed, vec![2]),
        (SnapshotChangeFilter::Added, vec![3]), (SnapshotChangeFilter::Unchanged, vec![4]),
        (SnapshotChangeFilter::Unavailable, vec![5])] {
        assert_eq!(numbers(&page(&table, handle, 7, filter), "entry_id"), expected);
    }
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(numbers(info.as_str(), "member_bytes_read"), vec![0, 0]);
    assert_eq!(numbers(info.as_str(), "member_read_calls"), vec![0, 0]);
    table.close(handle).unwrap();
}

#[test]
fn incomplete_opposite_catalog_does_not_claim_added_or_removed() {
    let root = root();
    let before = archive(&root, "before.fcbs", false, &[captured(b"old.rs", b"old")]);
    let after = archive(&root, "after.fcbs", false, &[captured(b"new.rs", b"new")]);
    let table = SnapshotComparisonSessions::new(); let handle = table.create().unwrap();
    table.open(handle, &before, &after, SnapshotLimits::default(), || false).unwrap(); finish(&table, handle, 1);
    assert_eq!(number(&page(&table, handle, 1, SnapshotChangeFilter::Added), "matching_entries"), 0);
    assert_eq!(number(&page(&table, handle, 1, SnapshotChangeFilter::Removed), "matching_entries"), 0);
    assert_eq!(numbers(&page(&table, handle, 1, SnapshotChangeFilter::OnlyAfter), "entry_id"), vec![1]);
    assert_eq!(numbers(&page(&table, handle, 1, SnapshotChangeFilter::OnlyBefore), "entry_id"), vec![2]);
    table.close(handle).unwrap();
}

#[test]
fn failed_second_archive_and_canceled_open_leave_the_reserved_owner_retryable() {
    let f = fixture(b"old", b"new"); let table = SnapshotComparisonSessions::new(); let handle = table.create().unwrap();
    assert!(table.open(handle, &f.before, &f.root.join("missing.fcbs"), SnapshotLimits::default(), || false).is_err());
    assert!(matches!(table.execute(handle, Command::Info, || false), Err(AccessError::NotOpen)));
    let mut signaled = false;
    let canceled = table.open(handle, &f.before, &f.after, SnapshotLimits::default(), || {
        if !signaled { table.cancel(handle).unwrap(); signaled = true; } false
    });
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    assert!(matches!(table.execute(handle, Command::Info, || false), Err(AccessError::NotOpen)));
    table.open(handle, &f.before, &f.after, SnapshotLimits::default(), || false).unwrap();
    assert!(matches!(table.open(handle, &f.before, &f.after, SnapshotLimits::default(), || false),
        Err(AccessError::AlreadyOpen)));
    finish(&table, handle, 1); table.close(handle).unwrap();
}

#[test]
fn cancel_between_steps_retires_only_pending_generation_and_keeps_accepted_rows() {
    let f = fixture(b"old", b"new"); let table = SnapshotComparisonSessions::new(); let handle = open(&table, &f);
    finish(&table, handle, 10);
    table.execute(handle, Command::Begin { generation: 11 }, || false).unwrap();
    assert_eq!(pending(&table, handle), Some(11));
    table.cancel(handle).unwrap();
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert!(info.as_str().contains("\"pending_comparison_generation\":null"));
    assert_eq!(number(info.as_str(), "accepted_comparison_generation"), 10);
    assert_eq!(pending(&table, handle), None);
    assert_eq!(number(&page(&table, handle, 10, SnapshotChangeFilter::All), "matching_entries"), 1);
    assert!(table.execute(handle, Command::Step { generation: 11, max_entries: 1 }, || false).is_err());
    assert!(table.execute(handle, Command::Begin { generation: 11 }, || false).is_err());
    finish(&table, handle, 12);
    assert!(table.execute(handle, Command::Page { generation: 10, filter: SnapshotChangeFilter::All, start: 0, limit: 1 },
        || false).is_err());
    table.close(handle).unwrap();
}

#[test]
fn cancellation_during_step_suppresses_delivery_without_destroying_old_results() {
    let f = fixture(b"old", b"new"); let table = SnapshotComparisonSessions::new(); let handle = open(&table, &f);
    finish(&table, handle, 1);
    table.execute(handle, Command::Begin { generation: 2 }, || false).unwrap();
    let mut signaled = false;
    let canceled = table.execute(handle, Command::Step { generation: 2, max_entries: 1 }, || {
        if !signaled { table.cancel(handle).unwrap(); signaled = true; } false
    });
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    assert_eq!(pending(&table, handle), None);
    assert_eq!(number(&page(&table, handle, 1, SnapshotChangeFilter::All), "matching_entries"), 1);
    table.close(handle).unwrap();
}

#[test]
fn destination_admission_and_reader_cancellation_leave_comparison_authority_intact() {
    let f = fixture(b"old", b"new"); let table = SnapshotComparisonSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); finish(&table, handle, 1);
    let reader = readers.create().unwrap();
    let mut signaled = false;
    let refused = table.open_reader(handle, &readers, reader, 1, 1, SnapshotComparisonSide::Before, || {
        if !signaled { readers.cancel(reader).unwrap(); signaled = true; } false
    });
    assert!(refused.is_err());
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(reader_sessions::AccessError::NotOpen)));
    table.open_reader(handle, &readers, reader, 1, 1, SnapshotComparisonSide::Before, || false).unwrap();
    let before = table.execute(handle, Command::Info, || false).unwrap();
    assert!(matches!(table.open_reader(handle, &readers, reader, 1, 1, SnapshotComparisonSide::After, || false),
        Err(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen))));
    assert!(matches!(table.open_reader(handle, &readers, u64::MAX, 1, 1, SnapshotComparisonSide::After, || false),
        Err(AccessError::Reader(reader_sessions::AccessError::UnknownHandle))));
    let after = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(numbers(before.as_str(), "member_bytes_read"), numbers(after.as_str(), "member_bytes_read"));
    assert_eq!(number(&page(&table, handle, 1, SnapshotChangeFilter::All), "matching_entries"), 1);
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}

#[test]
fn corrupted_archived_member_cannot_install_a_reader_or_replace_a_prior_capture() {
    let f = fixture(b"unique old exact payload", b"unique new exact payload");
    let table = SnapshotComparisonSessions::new(); let readers = ReaderSessions::new(); let handle = open(&table, &f);
    finish(&table, handle, 1);
    let prior = readers.create().unwrap();
    table.open_reader(handle, &readers, prior, 1, 1, SnapshotComparisonSide::After, || false).unwrap();
    let mut file = OpenOptions::new().write(true).open(&f.after).unwrap();
    file.seek(SeekFrom::Start(f.after_payload)).unwrap(); file.write_all(b"X").unwrap(); file.flush().unwrap();
    let empty = readers.create().unwrap();
    assert!(table.open_reader(handle, &readers, empty, 1, 1, SnapshotComparisonSide::After, || false).is_err());
    assert!(matches!(readers.execute(empty, ReaderCommand::Info, || false), Err(reader_sessions::AccessError::NotOpen)));
    let old = readers.execute(prior, ReaderCommand::Window { offset: 0, bytes: 1024 }, || false).unwrap();
    assert!(old.as_str().contains(&hex(b"unique new exact payload")));
    table.open_reader(handle, &readers, empty, 1, 1, SnapshotComparisonSide::Before, || false).unwrap();
    assert!(f.before_payload > 0);
    readers.close(prior).unwrap(); readers.close(empty).unwrap(); table.close(handle).unwrap();
}

#[test]
fn raw_paths_utf16_and_full_width_generations_survive_reader_transfer() {
    let root = root(); let source = [0xff, 0xfe, b'a', 0, b'\n', 0];
    let before = archive(&root, "before.fcbs", true, &[captured(b"raw-\xff.rs", &source)]);
    let after = archive(&root, "after.fcbs", true, &[captured(b"raw-\xff.rs", &source)]);
    let table = SnapshotComparisonSessions::new(); let readers = ReaderSessions::new(); let handle = table.create().unwrap();
    table.open(handle, &before, &after, SnapshotLimits::default(), || false).unwrap();
    let generation = u64::MAX - 1; finish(&table, handle, generation);
    let rows = page(&table, handle, generation, SnapshotChangeFilter::Unchanged);
    assert_eq!(number(&rows, "comparison_generation"), generation);
    assert!(rows.contains(&format!("\"hex\":\"{}\"", hex(b"raw-\xff.rs"))));
    let reader = readers.create().unwrap();
    let receipt = table.open_reader(handle, &readers, reader, generation, 1, SnapshotComparisonSide::Before, || false).unwrap();
    assert!(receipt.as_str().contains(&format!("\"hex\":\"{}\"", hex(b"raw-\xff.rs"))));
    let bytes = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: source.len() as u64 }, || false).unwrap();
    assert!(bytes.as_str().contains(&hex(&source)));
    finish(&table, handle, u64::MAX);
    assert!(table.execute(handle, Command::Begin { generation: 1 }, || false).is_err());
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}

#[test]
fn busy_handles_and_other_registry_kinds_cannot_alias_or_block_independent_owners() {
    let f = fixture(b"old", b"new"); let table = SnapshotComparisonSessions::new();
    let a = open(&table, &f); let b = open(&table, &f);
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    let saved = crate::saved_sessions::SavedSessions::new(); let archive = saved.create().unwrap();
    for wrong in [reader, archive] {
        assert_ne!(a, wrong);
        assert!(matches!(table.execute(wrong, Command::Info, || false), Err(AccessError::Unknown)));
    }
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert!(matches!(table.execute(a, Command::Info, || false), Err(AccessError::Busy)));
    assert!(table.execute(b, Command::Info, || false).is_ok());
    drop(guard); drop(cell); table.close(a).unwrap();
    assert_eq!(number(table.execute(b, Command::Info, || false).unwrap().as_str(), "owner"), b);
    table.close(b).unwrap(); readers.close(reader).unwrap(); saved.close(archive).unwrap();
}

#[test]
fn close_during_work_keeps_capacity_reserved_until_the_entered_owner_drains() {
    let f = fixture(b"old", b"new"); let table = SnapshotComparisonSessions::new(); let handle = open(&table, &f);
    let second = table.create().unwrap();
    let mut closed = false;
    let result = table.execute(handle, Command::Begin { generation: 1 }, || {
        if !closed {
            table.close(handle).unwrap(); closed = true;
            assert!(matches!(table.create(), Err(AccessError::Capacity)));
        }
        false
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    let fresh = table.create().unwrap(); assert_ne!(fresh, handle);
    assert!(matches!(table.execute(handle, Command::Info, || false), Err(AccessError::Unknown)));
    table.close(fresh).unwrap(); table.close(second).unwrap();
}
