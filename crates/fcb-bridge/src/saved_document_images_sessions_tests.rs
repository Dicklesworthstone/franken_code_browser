#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use std::{fs, path::PathBuf};

const DOC: &[u8] = b"![Captured](logo.png)\n";
const ASSET: &[u8] = b"\x89PNG\r\n\x1a\nencoded-image-transport-fixture";
struct Fixture { root: PathBuf, path: PathBuf }
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }
fn fixture() -> Fixture {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-saved-image-handles-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let owner = ArenaOwnerId::new(58820).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries = [
        SnapshotEntry { path: b"README.md", observed_bytes: DOC.len() as u64, data: SnapshotData::Captured(DOC) },
        SnapshotEntry { path: b"logo.png", observed_bytes: ASSET.len() as u64, data: SnapshotData::Captured(ASSET) },
    ];
    let bytes = SnapshotBytes::encode(owner, true, "saved-images", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("saved.fcbs"); fs::write(&path, bytes.bytes()).unwrap();
    Fixture { root, path }
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn open(saved: &SavedSessions, readers: &ReaderSessions, f: &Fixture) -> (u64, u64, usize, u64) {
    let archive = saved.create().unwrap(); saved.open(archive, &f.path, Default::default(), || false).unwrap();
    let reader = readers.create().unwrap();
    saved.open_reader(archive, readers, reader, Selection::Member(0), || false).unwrap();
    readers.execute(reader, ReaderCommand::Document { generation: 1, options: Default::default() }, || false).unwrap();
    let display = readers.execute(reader, ReaderCommand::DocumentDisplay { generation: 1, width_points: 640 }, || false).unwrap();
    let item = display.as_str().split("\"item_index\":\"").skip(1)
        .find(|part| part.contains("\"kind\":\"image\"")).unwrap();
    let index = item.split('"').next().unwrap().parse().unwrap();
    (archive, reader, index, number(item, "request_id"))
}

#[test]
fn archive_close_revokes_acquisition_but_source_and_admitted_chunks_keep_independent_reader_lifetime() {
    let f = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let (archive, reader, item_index, request_id) = open(&saved, &readers, &f);
    let auth = saved.document_images(archive, &readers, reader, 0, 1, DocumentImageCommand::Authorize, || false).unwrap();
    assert!(auth.as_str().contains("\"source_authority\":\"verified-saved-member\""));
    let response = saved.document_images(archive, &readers, reader, 0, 1,
        DocumentImageCommand::Prepare { item_index, request_id }, || false).unwrap();
    assert!(response.as_str().contains(&hex(ASSET)));
    saved.close(archive).unwrap(); fs::remove_file(&f.path).unwrap();
    assert_eq!(saved.document_images(archive, &readers, reader, 0, 1,
        DocumentImageCommand::Prepare { item_index, request_id }, || false).err(), Some(AccessError::Unknown));
    let chunk = readers.execute(reader, ReaderCommand::DocumentImageChunk {
        generation: 1, item_index, request_id, offset: 0, count: 64 }, || false).unwrap();
    assert!(chunk.as_str().contains(&hex(ASSET)));
    let source = readers.execute(reader, ReaderCommand::Window { offset: 0, bytes: 64 }, || false).unwrap();
    assert!(source.as_str().contains(&hex(DOC)));
    readers.close(reader).unwrap();
    assert!(response.as_str().contains("\"image_observation\":\"verified-saved-member\""));
}

#[test]
fn reader_and_archive_epochs_each_cancel_image_work_without_poisoning_the_other_session() {
    for cancel_reader in [false, true] {
        let f = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
        let (archive, reader, item_index, request_id) = open(&saved, &readers, &f);
        saved.document_images(archive, &readers, reader, 0, 1, DocumentImageCommand::Authorize, || false).unwrap();
        let mut polls = 0;
        let result = saved.document_images(archive, &readers, reader, 0, 1,
            DocumentImageCommand::Prepare { item_index, request_id }, || {
                polls += 1;
                if polls == 3 {
                    if cancel_reader { readers.cancel(reader).unwrap(); }
                    else { saved.cancel(archive).unwrap(); }
                }
                false
            });
        let error = result.err().unwrap();
        assert!(error.canceled()); assert!(error.json(archive).contains("\"exit_code\":\"130\""));
        assert!(readers.execute(reader, ReaderCommand::Info, || false).is_ok());
        assert!(saved.execute(archive, Command::Info, || false).is_ok());
        let accepted = saved.document_images(archive, &readers, reader, 0, 1,
            DocumentImageCommand::Prepare { item_index, request_id }, || false).unwrap();
        assert!(accepted.as_str().contains(&hex(ASSET)));
        readers.close(reader).unwrap(); saved.close(archive).unwrap();
    }
}

#[test]
fn busy_empty_and_closed_readers_cannot_trigger_archive_payload_work() {
    let f = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let (archive, reader, item_index, request_id) = open(&saved, &readers, &f);
    let before = saved.execute(archive, Command::Info, || false).unwrap();
    let empty = readers.create().unwrap();
    assert_eq!(saved.document_images(archive, &readers, empty, 0, 1,
        DocumentImageCommand::Authorize, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::NotOpen)));
    let busy: Result<HostResponse, AccessError> = readers.with_retained(reader, |_, _|
        saved.document_images(archive, &readers, reader, 0, 1,
            DocumentImageCommand::Prepare { item_index, request_id }, || false), || false);
    assert_eq!(busy.err(), Some(AccessError::Reader(reader_sessions::AccessError::Busy)));
    readers.close(reader).unwrap();
    assert_eq!(saved.document_images(archive, &readers, reader, 0, 1,
        DocumentImageCommand::Authorize, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::UnknownHandle)));
    let after = saved.execute(archive, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    readers.close(empty).unwrap(); saved.close(archive).unwrap();
}
