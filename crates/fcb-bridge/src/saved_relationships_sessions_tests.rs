#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use std::{fs, io::{Seek, SeekFrom, Write}, path::PathBuf};

const DOC: &[u8] = b"# Guide\n\nSee [engine](src/lib.rs#entry), [missing](missing.rs) and [remote](https://example.invalid/private).\n";
const SOURCE: &[u8] = b"fn retained_entry() {}\n";
struct Fixture { root: PathBuf, archive: PathBuf, source_at: u64 }
fn fixture() -> Fixture {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-link-handles-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(root.join("src")).unwrap();
    // These unrelated live bytes must never replace the archive's source.
    fs::write(root.join("src/lib.rs"), b"unrelated live file").unwrap();
    let owner = ArenaOwnerId::new(8840).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries = [
        SnapshotEntry { path: b"README.md", observed_bytes: DOC.len() as u64, data: SnapshotData::Captured(DOC) },
        SnapshotEntry { path: b"src/lib.rs", observed_bytes: SOURCE.len() as u64, data: SnapshotData::Captured(SOURCE) },
    ];
    let bytes = SnapshotBytes::encode(owner, true, "relationships", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let source_at = bytes.bytes().windows(SOURCE.len()).position(|b| b == SOURCE).unwrap() as u64;
    let archive = root.join("saved.fcbs"); fs::write(&archive, bytes.bytes()).unwrap();
    Fixture { root, archive, source_at }
}
fn open(table: &SavedSessions, fixture: &Fixture) -> u64 {
    let handle = table.create().unwrap();
    table.open(handle, &fixture.archive, Default::default(), || false).unwrap(); handle
}
fn prepare(table: &SavedSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::PrepareRelationships { generation, max_links: 64, max_source_bytes: 4096 },
        || false).unwrap().as_str().to_owned()
}
fn page(table: &SavedSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::Relationships { generation, member: Some(1), direction: RelationshipDirection::Incoming,
        start: 0, limit: 64 }, || false).unwrap().as_str().to_owned()
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

#[test]
fn backlink_navigation_uses_verified_archive_bytes_and_readers_survive_projection_retirement() {
    let fixture = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&saved, &fixture);
    let prepared = prepare(&saved, handle, 1);
    assert!(prepared.contains("\"evidence_precision\":\"whole-captured-markdown-document\""));
    assert!(prepared.contains("\"relationships_complete\":true"));
    assert_eq!(number(&prepared, "retained_links"), 3);
    let incoming = page(&saved, handle, 1);
    assert_eq!(incoming.matches("\"link_id\":").count(), 1);
    assert!(incoming.contains("src/lib.rs#entry"));
    let link = number(&incoming, "link_id");
    fs::rename(&fixture.archive, fixture.root.join("moved.fcbs")).unwrap();
    let source_reader = readers.create().unwrap(); let target_reader = readers.create().unwrap();
    let source = saved.open_reader(handle, &readers, source_reader,
        Selection::Relationship { generation: 1, id: link, side: RelationshipSide::Evidence }, || false).unwrap();
    assert!(source.as_str().contains("\"live_source_reopened\":false"));
    assert!(source.as_str().contains("src/lib.rs#entry"));
    let target = saved.open_reader(handle, &readers, target_reader,
        Selection::Relationship { generation: 1, id: link, side: RelationshipSide::Target }, || false).unwrap();
    assert!(target.as_str().contains("\"fragment_resolution\":\"unresolved\""));
    assert_eq!(number(target.as_str(), "offset"), 0);
    saved.execute(handle, Command::ClearRelationships { generation: 2 }, || false).unwrap();
    saved.close(handle).unwrap();
    let captured = readers.execute(target_reader, ReaderCommand::Window { offset: 0, bytes: 64 }, || false).unwrap();
    assert!(captured.as_str().contains(&hex(SOURCE)));
    let doc = readers.execute(source_reader, ReaderCommand::Window { offset: 0, bytes: 1024 }, || false).unwrap();
    assert!(doc.as_str().contains(&hex(DOC)));
    readers.close(source_reader).unwrap(); readers.close(target_reader).unwrap();
}

#[test]
fn canceled_replacement_preserves_old_backlinks_and_consumes_only_its_relationship_generation() {
    let fixture = fixture(); let saved = SavedSessions::new(); let handle = open(&saved, &fixture);
    prepare(&saved, handle, 1);
    saved.execute(handle, Command::Search { generation: 1, needle: "retained_entry", limit: 8 }, || false).unwrap();
    let mut polls = 0;
    let result = saved.execute(handle, Command::PrepareRelationships { generation: 2, max_links: 64, max_source_bytes: 4096 }, || {
        polls += 1; if polls == 4 { saved.cancel(handle).unwrap(); } false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(page(&saved, handle, 1).contains("src/lib.rs#entry"));
    assert!(saved.execute(handle, Command::PrepareRelationships { generation: 2, max_links: 64, max_source_bytes: 4096 },
        || false).is_err());
    assert!(saved.execute(handle, Command::Results { generation: 1, start: 0, limit: 8 }, || false).is_ok());
    prepare(&saved, handle, 3);
    assert!(saved.execute(handle, Command::Relationships { generation: 1, member: None, direction: RelationshipDirection::All,
        start: 0, limit: 8 }, || false).is_err());
    saved.close(handle).unwrap();
}

#[test]
fn reader_admission_and_unresolvable_targets_do_not_read_source_payloads() {
    let fixture = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&saved, &fixture); prepare(&saved, handle, 1);
    let reader = readers.create().unwrap();
    saved.open_reader(handle, &readers, reader,
        Selection::Relationship { generation: 1, id: 1, side: RelationshipSide::Target }, || false).unwrap();
    let before = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(saved.open_reader(handle, &readers, reader,
        Selection::Relationship { generation: 1, id: 1, side: RelationshipSide::Evidence }, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    let empty = readers.create().unwrap();
    for link in [2, 3] {
        assert!(saved.open_reader(handle, &readers, empty,
            Selection::Relationship { generation: 1, id: link, side: RelationshipSide::Target }, || false).is_err());
        assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    }
    let after = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    readers.close(empty).unwrap(); readers.close(reader).unwrap(); saved.close(handle).unwrap();
}

#[test]
fn damaged_archived_targets_cannot_install_a_reader_or_replace_with_a_live_file() {
    let fixture = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&saved, &fixture); prepare(&saved, handle, 1);
    let mut writer = fs::OpenOptions::new().write(true).open(&fixture.archive).unwrap();
    writer.seek(SeekFrom::Start(fixture.source_at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    let reader = readers.create().unwrap();
    assert!(saved.open_reader(handle, &readers, reader,
        Selection::Relationship { generation: 1, id: 1, side: RelationshipSide::Target }, || false).is_err());
    assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert!(page(&saved, handle, 1).contains("src/lib.rs#entry"));
    readers.close(reader).unwrap(); saved.close(handle).unwrap();
}
