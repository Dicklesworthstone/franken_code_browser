#![forbid(unsafe_code)]

use std::{fs::{self, File, OpenOptions}, io::{Seek, SeekFrom, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use super::*;
use fcb::search::{IndexLimits, snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry},
    paged_snapshot::PagedSnapshot, snapshot_index::SnapshotIndex};
use crate::reader_sessions::Command as ReaderCommand;

struct Fixture { path: PathBuf, index: PathBuf, pin: Sha256Digest, source_at: u64, index_tail: u64 }
fn fixture(source: &[u8]) -> Fixture {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-saved-handles-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(8820).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let id = |n| ResourceAllocationId::new(n).unwrap();
    let entries = [SnapshotEntry { path: b"a.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source) },
        SnapshotEntry { path: b"b.rs", observed_bytes: 9, data: SnapshotData::Captured(b"unrelated") }];
    let bytes = SnapshotBytes::encode(owner, true, "test", &entries, Default::default(), &budget, id(1), || false).unwrap();
    let source_at = bytes.bytes().windows(source.len()).position(|b| b == source).unwrap() as u64;
    let path = root.join("saved.fcbs"); fs::write(&path, bytes.bytes()).unwrap();
    let mut archive = PagedSnapshot::open(File::open(&path).unwrap(), owner, Default::default(), &budget, id(2), || false).unwrap();
    let forward = SnapshotIndex::build(&mut archive, IndexLimits::default(), &budget, [id(3), id(4), id(5), id(6)], || false).unwrap();
    let inverted = forward.invert(&budget, id(7), || false).unwrap();
    let artifact = inverted.encode_paged(&budget, id(8), || false).unwrap();
    let index = root.join("saved.fcbd"); fs::write(&index, artifact.bytes()).unwrap();
    Fixture { path, index, pin: artifact.digest(), source_at, index_tail: artifact.bytes().len() as u64 - 1 }
}
fn open(table: &SavedSessions, f: &Fixture) -> u64 {
    let handle = table.create().unwrap(); table.open(handle, &f.path, Default::default(), || false).unwrap(); handle
}
fn attach(table: &SavedSessions, handle: u64, f: &Fixture) {
    table.execute(handle, Command::AttachIndex { generation: 1, path: &f.index, pin: f.pin }, || false).unwrap();
}
fn query(table: &SavedSessions, handle: u64, generation: u64, needle: &str) -> String {
    table.execute(handle, Command::Search { generation, needle, limit: 10 }, || false).unwrap().as_str().to_owned()
}
fn page(table: &SavedSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::Results { generation, start: 0, limit: 10 }, || false).unwrap().as_str().to_owned()
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}

#[test]
fn saved_index_search_reader_and_outline_routes_share_real_capture_ownership() {
    let f = fixture(b"fn original() { /* needle */ }\n");
    let saved = SavedSessions::new(); let readers = ReaderSessions::new(); let handle = open(&saved, &f);
    attach(&saved, handle, &f); assert_eq!(number(&query(&saved, handle, 1, "needle"), "retained_hits"), 1);
    let reader = readers.create().unwrap();
    let reply = saved.open_reader(handle, &readers, reader, Selection::Hit { generation: 1, id: 1 }, || false).unwrap();
    assert_eq!(number(reply.as_str(), "reader_owner"), reader);
    assert!(reply.as_str().contains("\"source_observation\":\"verified-saved-member\""));
    saved.execute(handle, Command::Clear { generation: 2 }, || false).unwrap(); saved.close(handle).unwrap();
    let found = readers.execute(reader, ReaderCommand::Find { generation: 1, needle: "original", limit: 10, scan_bytes: 1024 }, || false).unwrap();
    assert_eq!(number(found.as_str(), "retained_hits"), 1);
    let outline = readers.execute(reader, ReaderCommand::Outline { generation: 1, options: Default::default() }, || false).unwrap();
    assert!(outline.as_str().contains("original")); readers.close(reader).unwrap();
}

#[test]
fn destination_admission_happens_before_source_reads_and_loaded_readers_cannot_be_replaced() {
    let f = fixture(b"needle source"); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); query(&table, handle, 1, "needle");
    let reader = readers.create().unwrap();
    table.open_reader(handle, &readers, reader, Selection::Member(0), || false).unwrap();
    let before = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(table.open_reader(handle, &readers, reader, Selection::Hit { generation: 1, id: 1 }, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    assert_eq!(table.open_reader(handle, &readers, u64::MAX, Selection::Member(0), || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::UnknownHandle)));
    let after = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}

#[test]
fn canceling_query_keeps_old_results_archive_and_attached_index_usable() {
    let f = fixture(b"old needle needle"); let table = SavedSessions::new(); let handle = open(&table, &f);
    attach(&table, handle, &f); query(&table, handle, 1, "old");
    let mut polls = 0;
    let failed = table.execute(handle, Command::Search { generation: 2, needle: "needle", limit: 10 }, || {
        polls += 1; if polls == 6 { table.cancel(handle).unwrap(); } false
    });
    assert_eq!(failed.err(), Some(AccessError::Canceled));
    assert!(page(&table, handle, 1).contains("\"needle\":\"old\""));
    assert_eq!(number(&query(&table, handle, 3, "needle"), "retained_hits"), 2);
    table.close(handle).unwrap();
}

#[test]
fn cancel_during_open_leaves_the_reserved_handle_empty_and_retryable() {
    let f = fixture(b"needle"); let table = SavedSessions::new(); let handle = table.create().unwrap();
    let mut polls = 0;
    let result = table.open(handle, &f.path, Default::default(), || {
        polls += 1; if polls == 3 { table.cancel(handle).unwrap(); } false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert_eq!(table.execute(handle, Command::Info, || false).err(), Some(AccessError::NotOpen));
    table.open(handle, &f.path, Default::default(), || false).unwrap();
    assert_eq!(number(&query(&table, handle, 1, "needle"), "retained_hits"), 1); table.close(handle).unwrap();
}

#[test]
fn reader_only_cancellation_cannot_cancel_saved_source_or_query() {
    let f = fixture(b"needle"); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); query(&table, handle, 1, "needle"); let reader = readers.create().unwrap();
    let mut signaled = false;
    assert!(table.open_reader(handle, &readers, reader, Selection::Member(0), || {
        if !signaled { readers.cancel(reader).unwrap(); signaled = true; } false
    }).is_err());
    assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    table.open_reader(handle, &readers, reader, Selection::Hit { generation: 1, id: 1 }, || false).unwrap();
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}

#[test]
fn busy_sessions_and_handle_kinds_do_not_block_or_alias_other_instances() {
    let f = fixture(b"needle"); let table = SavedSessions::new();
    let a = open(&table, &f); let b = open(&table, &f);
    let readers = ReaderSessions::new(); let r = readers.create().unwrap();
    let atlases = crate::atlas_sessions::AtlasSessions::new(); let atlas = atlases.create().unwrap();
    assert_ne!(a, b); assert_ne!(a, r); assert_ne!(a, atlas);
    for wrong in [r, atlas] { assert_eq!(table.execute(wrong, Command::Info, || false).err(), Some(AccessError::Unknown)); }
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute(a, Command::Info, || false).err(), Some(AccessError::Busy));
    assert_eq!(number(&query(&table, b, 1, "needle"), "retained_hits"), 1);
    drop(guard); drop(cell); table.close(a).unwrap();
    assert_eq!(number(&page(&table, b, 1), "owner"), b);
    table.close(b).unwrap(); readers.close(r).unwrap(); atlases.close(atlas).unwrap();
}

#[test]
fn close_during_work_retains_capacity_until_its_archive_owner_drains() {
    let f = fixture(b"needle"); let table = SavedSessions::new(); let handle = open(&table, &f);
    let rest: Vec<_> = (1..MAX_SAVED_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let result = table.execute(handle, Command::Search { generation: 1, needle: "needle", limit: 10 }, || {
        if !closed { table.close(handle).unwrap(); closed = true; assert_eq!(table.create().err(), Some(AccessError::Capacity)); } false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    let fresh = table.create().unwrap(); assert_ne!(fresh, handle);
    assert_eq!(table.execute(handle, Command::Info, || false).err(), Some(AccessError::Unknown));
    table.close(fresh).unwrap(); for id in rest { table.close(id).unwrap(); }
}

#[test]
fn archive_and_index_descriptors_can_move_to_an_explicit_host_worker() {
    let f = fixture(b"needle"); let table = SavedSessions::new(); let handle = open(&table, &f); attach(&table, handle, &f);
    fs::rename(&f.path, f.path.with_extension("moved")).unwrap();
    fs::rename(&f.index, f.index.with_extension("moved")).unwrap();
    let (table, reply) = std::thread::spawn(move || {
        let reply = query(&table, handle, 1, "needle"); (table, reply)
    }).join().unwrap();
    assert_eq!(number(&reply, "retained_hits"), 1); assert_eq!(number(&reply, "member_bytes_read"), 6);
    table.close(handle).unwrap();
}

#[test]
fn damaged_member_cannot_install_a_reader_and_damaged_postings_do_not_erase_old_rows() {
    let f = fixture(b"unique needle body"); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); query(&table, handle, 1, "needle"); attach(&table, handle, &f);
    let mut writer = OpenOptions::new().write(true).open(&f.index).unwrap();
    writer.seek(SeekFrom::Start(f.index_tail)).unwrap(); writer.write_all(&[0xff]).unwrap(); writer.flush().unwrap();
    assert!(table.execute(handle, Command::Search { generation: 2, needle: "needle", limit: 10 }, || false).is_err());
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    table.execute(handle, Command::DetachIndex { generation: 2 }, || false).unwrap();
    assert_eq!(number(&query(&table, handle, 3, "needle"), "retained_hits"), 1);
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(f.source_at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    let reader = readers.create().unwrap();
    assert!(table.open_reader(handle, &readers, reader, Selection::Hit { generation: 3, id: 1 }, || false).is_err());
    assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}

#[test]
fn stale_search_activation_and_failed_index_replacement_preserve_completed_state() {
    let f = fixture(b"old needle"); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); attach(&table, handle, &f); query(&table, handle, 1, "old"); query(&table, handle, 2, "needle");
    let reader = readers.create().unwrap();
    assert_eq!(table.open_reader(handle, &readers, reader, Selection::Hit { generation: 1, id: 1 }, || false).err(),
        Some(AccessError::Saved(SavedRepositoryError::StaleQuery)));
    assert!(table.execute(handle, Command::AttachIndex { generation: 2, path: &f.index, pin: Sha256Digest::new([0; 32]) }, || false).is_err());
    assert_eq!(number(table.execute(handle, Command::Info, || false).unwrap().as_str(), "accepted_index_generation"), 1);
    table.open_reader(handle, &readers, reader, Selection::Hit { generation: 2, id: 1 }, || false).unwrap();
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}
