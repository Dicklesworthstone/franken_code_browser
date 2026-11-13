#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use std::{fs, io::{Seek, SeekFrom, Write}, path::PathBuf};

const SOURCE: &[u8] = b"from .target import Value\n";
const TARGET: &[u8] = b"Value = 42\n";
struct Fixture { archive: PathBuf, target_at: u64 }
fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("fcb-import-handles-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(root.join("pkg/target.py"), b"unrelated live source").unwrap();
    let owner = ArenaOwnerId::new(9013).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries = [
        SnapshotEntry { path: b"pkg/source.py", observed_bytes: SOURCE.len() as u64, data: SnapshotData::Captured(SOURCE) },
        SnapshotEntry { path: b"pkg/target.py", observed_bytes: TARGET.len() as u64, data: SnapshotData::Captured(TARGET) },
    ];
    let bytes = SnapshotBytes::encode(owner, true, "import-handles", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let target_at = bytes.bytes().windows(TARGET.len()).position(|bytes| bytes == TARGET).unwrap() as u64;
    let archive = root.join("saved.fcbs"); fs::write(&archive, bytes.bytes()).unwrap();
    Fixture { archive, target_at }
}
fn open(saved: &SavedSessions, fixture: &Fixture) -> u64 {
    let handle = saved.create().unwrap();
    saved.open(handle, &fixture.archive, Default::default(), || false).unwrap(); handle
}
fn prepare(saved: &SavedSessions, handle: u64, generation: u64) -> String {
    saved.execute(handle, Command::PrepareImports { generation, max_links: 64, max_source_bytes: 4096 },
        || false).unwrap().as_str().to_owned()
}
fn page(saved: &SavedSessions, handle: u64, generation: u64) -> Result<HostResponse, AccessError> {
    saved.execute(handle, Command::Imports { generation, member: Some(1), direction: SavedImportDirection::Incoming,
        start: 0, limit: 64 }, || false)
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}

#[test]
fn canceling_import_replacement_preserves_the_accepted_inventory_and_other_query_generations() {
    let fixture = fixture(); let saved = SavedSessions::new(); let handle = open(&saved, &fixture);
    assert!(prepare(&saved, handle, 1).contains("\"imports_complete\":true"));
    saved.execute(handle, Command::Search { generation: 1, needle: "Value", limit: 8 }, || false).unwrap();
    let mut polls = 0;
    let result = saved.execute(handle, Command::PrepareImports { generation: 2, max_links: 64, max_source_bytes: 4096 }, || {
        polls += 1;
        if polls == 4 { saved.cancel(handle).unwrap(); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(page(&saved, handle, 1).unwrap().as_str().contains(".target"));
    let info = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "accepted_import_generation"), 1);
    assert_eq!(number(info.as_str(), "last_import_attempt"), 2);
    assert!(saved.execute(handle, Command::PrepareImports { generation: 2, max_links: 64, max_source_bytes: 4096 },
        || false).is_err());
    assert!(saved.execute(handle, Command::Results { generation: 1, start: 0, limit: 8 }, || false).is_ok());
    prepare(&saved, handle, 3);
    assert!(page(&saved, handle, 1).is_err());
    assert!(page(&saved, handle, 3).is_ok());
    saved.close(handle).unwrap();
}

#[test]
fn import_reader_admission_precedes_reads_and_corruption_cannot_install_or_replace_a_reader() {
    let fixture = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&saved, &fixture); prepare(&saved, handle, 1);
    let occupied = readers.create().unwrap();
    saved.open_reader(handle, &readers, occupied,
        Selection::Import { generation: 1, id: 1, side: SavedImportSide::Evidence }, || false).unwrap();
    let before = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(saved.open_reader(handle, &readers, occupied,
        Selection::Import { generation: 1, id: 1, side: SavedImportSide::Target }, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    let after = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    let empty = readers.create().unwrap();
    let mut writer = fs::OpenOptions::new().write(true).open(&fixture.archive).unwrap();
    writer.seek(SeekFrom::Start(fixture.target_at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.open_reader(handle, &readers, empty,
        Selection::Import { generation: 1, id: 1, side: SavedImportSide::Target }, || false).is_err());
    assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert!(page(&saved, handle, 1).is_ok(), "accepted metadata survives an unavailable target read");
    saved.close(handle).unwrap();
    assert!(readers.execute(occupied, ReaderCommand::Window { offset: 0, bytes: 128 }, || false)
        .unwrap().as_str().contains("from .target import Value"));
    readers.close(empty).unwrap(); readers.close(occupied).unwrap();
}
