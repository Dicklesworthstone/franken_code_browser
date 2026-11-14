#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::{ByteOffset, ByteRange};
use fcb::search::{snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry}, trail::{TrailBytes, TrailEntry}};
use fcb::store::Sha256;
use std::{fs, io::{Seek, SeekFrom, Write}, path::PathBuf};

const SOURCE: &[u8] = b"Exact retained trail member, never the live pathname.\n";
struct Fixture { archive: PathBuf, trail: PathBuf, source_at: u64 }
fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("fcb-trail-handles-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"unrelated live source").unwrap();
    let owner = ArenaOwnerId::new(9020).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, true, "trail-handles", &[SnapshotEntry {
        path: b"a.rs", observed_bytes: SOURCE.len() as u64, data: SnapshotData::Captured(SOURCE) }],
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let source_at = bytes.bytes().windows(SOURCE.len()).position(|window| window == SOURCE).unwrap() as u64;
    let archive = root.join("saved.fcbs"); fs::write(&archive, bytes.bytes()).unwrap();
    let item = TrailEntry { archive: bytes.digest(), source: Sha256::digest(SOURCE), source_length: SOURCE.len() as u64,
        path: b"a.rs", range: ByteRange::new(ByteOffset::new(6), ByteOffset::new(14)).unwrap(), rationale: "First exact visit" };
    let trail = TrailBytes::encode(owner, "Trail handle lifetime", None,
        &[item, TrailEntry { archive: Sha256Digest::new([7; 32]), rationale: "Other archive", ..item }],
        &budget, ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    let path = root.join("tour.fcbt"); fs::write(&path, trail.bytes()).unwrap();
    Fixture { archive, trail: path, source_at }
}
fn open(saved: &SavedSessions, fixture: &Fixture) -> u64 {
    let handle = saved.create().unwrap();
    saved.open(handle, &fixture.archive, Default::default(), || false).unwrap(); handle
}
fn attach(saved: &SavedSessions, handle: u64, path: &Path, generation: u64) -> Result<HostResponse, AccessError> {
    saved.execute(handle, Command::AttachTrail { generation, path }, || false)
}
fn page(saved: &SavedSessions, handle: u64, generation: u64) -> Result<HostResponse, AccessError> {
    saved.execute(handle, Command::TrailPage { generation, start: 0, limit: 32 }, || false)
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}

#[test]
fn canceled_attachment_preserves_old_rows_and_a_lost_accepted_reply_can_be_reconciled() {
    let fixture = fixture(); let saved = SavedSessions::new(); let handle = open(&saved, &fixture);
    let mut complete_polls = 0;
    saved.execute(handle, Command::AttachTrail { generation: 1, path: &fixture.trail }, || {
        complete_polls += 1; false
    }).unwrap();
    let mut polls = 0;
    let canceled = saved.execute(handle, Command::AttachTrail { generation: 2, path: &fixture.trail }, || {
        polls += 1; if polls == 4 { saved.cancel(handle).unwrap(); } false
    });
    assert_eq!(canceled.err(), Some(AccessError::Canceled));
    assert!(page(&saved, handle, 1).unwrap().as_str().contains("First exact visit"));
    let info = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "accepted_trail_generation"), 1);
    assert_eq!(number(info.as_str(), "last_trail_attempt"), 2);
    assert!(attach(&saved, handle, &fixture.trail, 2).is_err());

    // Revoke during the final host delivery poll, after that poll's epoch
    // check. Host acceptance can finish, then the registry suppresses delivery.
    // This deliberately distinguishes an unknown outcome from rollback.
    let mut polls = 0;
    let lost = saved.execute(handle, Command::AttachTrail { generation: 3, path: &fixture.trail }, || {
        polls += 1; if polls == complete_polls { saved.cancel(handle).unwrap(); } false
    });
    assert_eq!(polls, complete_polls);
    assert_eq!(lost.err(), Some(AccessError::Canceled));
    let info = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "accepted_trail_generation"), 3);
    assert_eq!(number(info.as_str(), "last_trail_attempt"), 3);
    assert!(page(&saved, handle, 1).is_err());
    assert!(page(&saved, handle, 3).is_ok());
    saved.close(handle).unwrap();
}

#[test]
fn reader_destination_admission_and_exact_reference_checks_precede_payload_reads() {
    let fixture = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&saved, &fixture); attach(&saved, handle, &fixture.trail, 1).unwrap();
    let occupied = readers.create().unwrap();
    saved.open_reader(handle, &readers, occupied, Selection::Trail { generation: 1, item: 0 }, || false).unwrap();
    let before = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(saved.open_reader(handle, &readers, occupied,
        Selection::Trail { generation: 1, item: 0 }, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    let empty = readers.create().unwrap();
    for selection in [Selection::Trail { generation: 2, item: 0 }, Selection::Trail { generation: 1, item: 1 },
        Selection::Trail { generation: 1, item: 2 }] {
        assert!(saved.open_reader(handle, &readers, empty, selection, || false).is_err());
        assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    }
    let after = saved.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    assert_eq!(number(before.as_str(), "member_read_calls"), number(after.as_str(), "member_read_calls"));

    let mut writer = fs::OpenOptions::new().write(true).open(&fixture.archive).unwrap();
    writer.seek(SeekFrom::Start(fixture.source_at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.open_reader(handle, &readers, empty, Selection::Trail { generation: 1, item: 0 }, || false).is_err());
    assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    saved.execute(handle, Command::DetachTrail { generation: 2 }, || false).unwrap();
    saved.close(handle).unwrap();
    assert!(readers.execute(occupied, ReaderCommand::Window { offset: 0, bytes: 256 }, || false)
        .unwrap().as_str().contains("Exact retained trail member"));
    readers.close(empty).unwrap(); readers.close(occupied).unwrap();
}

#[test]
fn canceling_reader_preparation_leaves_an_empty_destination_and_a_reusable_attachment() {
    let fixture = fixture(); let saved = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&saved, &fixture); attach(&saved, handle, &fixture.trail, 1).unwrap();
    let reader = readers.create().unwrap(); let mut polls = 0;
    let result = saved.open_reader(handle, &readers, reader, Selection::Trail { generation: 1, item: 0 }, || {
        polls += 1; if polls == 4 { readers.cancel(reader).unwrap(); } false
    });
    assert!(result.is_err());
    assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert!(page(&saved, handle, 1).is_ok());
    assert!(saved.open_reader(handle, &readers, reader, Selection::Trail { generation: 1, item: 0 }, || false).is_ok());
    readers.close(reader).unwrap(); saved.close(handle).unwrap();
}

fn write_request(destination: &Path) -> SavedTrailWriteRequest<'_> {
    SavedTrailWriteRequest { operation: 1, parent: None, member: 0,
        selection: ByteRange::new(ByteOffset::new(6), ByteOffset::new(14)).unwrap(),
        title: "Authored trail", rationale: "Exact saved source", destination }
}

#[test]
fn cancel_during_trail_publication_preserves_effect_and_recovery_without_replay() {
    let fixture = fixture(); let saved = SavedSessions::new(); let handle = open(&saved, &fixture);
    attach(&saved, handle, &fixture.trail, 1).unwrap();
    let destination = fixture.archive.with_extension("canceled.fcbt"); let mut signaled = false;
    let response = saved.write_trail(handle, write_request(&destination), || {
        if !signaled && destination.exists() { saved.cancel(handle).unwrap(); signaled = true; return true; }
        false
    }).unwrap();
    assert!(signaled);
    assert!(response.as_str().contains("\"effect\":\"destination-created-incomplete\""));
    assert!(response.as_str().contains("SAVED_TRAIL_WRITE_CANCELED"));
    assert_eq!(number(response.as_str(), "bytes_written"), 0);
    assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
    fs::write(&destination, b"changed externally after writer drained").unwrap();
    assert_eq!(saved.execute(handle, Command::TrailWriteReceipt { operation: 1 }, || false).unwrap().as_str(), response.as_str());
    assert_eq!(fs::read(&destination).unwrap(), b"changed externally after writer drained");
    assert_eq!(number(saved.execute(handle, Command::Info, || false).unwrap().as_str(), "accepted_trail_generation"), 1);
    assert!(page(&saved, handle, 1).is_ok());
    assert_eq!(saved.write_trail(handle, write_request(&destination), || false).err(), Some(AccessError::Trail(SavedTrailError::StaleWrite)));
    saved.close(handle).unwrap();
}

#[test]
fn closing_during_trail_write_keeps_its_effect_response_and_capacity_through_drain() {
    let fixture = fixture(); let saved = SavedSessions::new(); let handle = open(&saved, &fixture);
    let rest: Vec<_> = (1..MAX_SAVED_SESSIONS).map(|_| saved.create().unwrap()).collect();
    let destination = fixture.archive.with_extension("closed.fcbt"); let mut closed = false;
    let response = saved.write_trail(handle, write_request(&destination), || {
        if !closed && destination.exists() {
            saved.close(handle).unwrap(); closed = true;
            assert_eq!(saved.create().err(), Some(AccessError::Capacity)); return true;
        }
        false
    }).unwrap();
    assert!(closed);
    assert!(response.as_str().contains("\"effect\":\"destination-created-incomplete\""));
    assert_eq!(number(response.as_str(), "bytes_written"), 0);
    assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
    assert_eq!(saved.execute(handle, Command::TrailWriteReceipt { operation: 1 }, || false).err(), Some(AccessError::Unknown));
    let fresh = saved.create().unwrap(); assert_ne!(fresh, handle);
    saved.close(fresh).unwrap(); for handle in rest { saved.close(handle).unwrap(); }
}
