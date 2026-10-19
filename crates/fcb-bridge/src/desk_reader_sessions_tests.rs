//! Real registry handoff, including capacity and competing cancellation. These
//! tests use independent registries, never fabricated native capture receipts.
use super::*;
use std::{fs, path::PathBuf, sync::atomic::AtomicU64};
use crate::reader_sessions::Command as ReaderCommand;

static FIXTURE: AtomicU64 = AtomicU64::new(1);
fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!("fcb-desk-reader-{}-{}",
        std::process::id(), FIXTURE.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn field(text: &str, key: &str) -> u64 {
    let marker = format!("\"{key}\":\"");
    text.split_once(&marker).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn open_desk(desks: &DeskSessions, source: &Path) -> (u64, u64) {
    let desk = desks.create().unwrap(); desks.start(desk, || false).unwrap();
    let response = desks.execute(desk, Command::Open { expected: 0, attempt: 1, path: source }, || false).unwrap();
    (desk, field(response.as_str(), "pane"))
}
fn info(readers: &ReaderSessions, reader: u64) -> Result<HostResponse, reader_sessions::AccessError> {
    readers.execute(reader, ReaderCommand::Info, || false)
}

#[test]
fn restored_offline_source_gains_full_reader_tools_and_outlives_the_desk() {
    let root = directory(); let source = root.join("source.rs"); let checkpoint = root.join("saved.fcbdesk");
    fs::write(&source, b"pub fn retained_name() {}\r\n").unwrap();
    let desks = DeskSessions::new(); let (first, pane) = open_desk(&desks, &source);
    desks.execute(first, Command::Apply { expected: 1, attempt: 2, kind: 2,
        subject: pane, start: 7, end: 20, label: "" }, || false).unwrap();
    desks.save(first, 2, &checkpoint, 4096, || false).unwrap();
    desks.close(first).unwrap();
    // Deliberate negative control: the old source pathname now names a directory.
    // Any attempted live-file reopen must fail rather than yield retained code.
    fs::rename(&source, root.join("original-captured-source.rs")).unwrap();
    fs::create_dir(&source).unwrap();
    let restored = desks.create().unwrap(); desks.start(restored, || false).unwrap();
    let restored_state = desks.execute(restored, Command::Restore {
        expected: 0, attempt: 1, path: &checkpoint }, || false).unwrap();
    let pane = field(restored_state.as_str(), "pane");
    let readers = ReaderSessions::new(); let destination = readers.create().unwrap();
    let receipt = desks.open_reader(restored, &readers, 1, pane, destination, || false).unwrap();
    assert!(receipt.as_str().contains("\"command\":\"open-reader\""));
    assert!(receipt.as_str().contains("\"source_reopened\":false"));
    assert_eq!(field(receipt.as_str(), "owner"), restored);
    assert_eq!(field(receipt.as_str(), "offset"), 7);
    assert!(receipt.as_str().contains("\"selection\":{\"start\":\"7\",\"end\":\"20\"}"));
    let transferred_info = info(&readers, destination).unwrap();
    assert_eq!(field(transferred_info.as_str(), "owner"), destination);
    assert!(transferred_info.as_str().contains("\"capture_origin\":\"host-supplied\""));
    assert_eq!(field(transferred_info.as_str(), "initial_source_bytes_read"), 0);
    assert_eq!(field(transferred_info.as_str(), "initial_read_calls"), 0);
    assert_eq!(desks.execute(restored, Command::State, || false).unwrap().as_str(), restored_state.as_str());
    desks.close(restored).unwrap();
    let found = readers.execute(destination, ReaderCommand::Find { generation: 1,
        needle: "retained_name", limit: 8, scan_bytes: 4096 }, || false).unwrap();
    assert_eq!(field(found.as_str(), "matches_seen"), 1);
    let copied = readers.execute(destination, ReaderCommand::CopyHit { generation: 1, index: 0 }, || false).unwrap();
    assert!(copied.as_str().contains("72657461696e65645f6e616d65"));
    readers.execute(destination, ReaderCommand::Outline { generation: 1,
        options: fcb_app::host::reader::ReaderOutlineOptions::default() }, || false).unwrap();
    let symbols = readers.execute(destination, ReaderCommand::Symbols { generation: 1,
        needle: "retained_name", mode: fcb::search::SymbolNameMode::Exact, start: 0, limit: 8 }, || false).unwrap();
    assert!(symbols.as_str().contains("\"name\":\"retained_name\""));
    assert_eq!(field(copied.as_str(), "additional_source_bytes_read"), 0);
    readers.close(destination).unwrap();
}

#[test]
fn stale_revision_bad_handle_and_nonempty_destination_cannot_substitute_another_capture() {
    let root = directory(); let source = root.join("source.txt"); let other = root.join("other.txt");
    fs::write(&source, b"intended capture").unwrap(); fs::write(&other, b"preexisting destination").unwrap();
    let desks = DeskSessions::new(); let (desk, pane) = open_desk(&desks, &source);
    let readers = ReaderSessions::new(); let destination = readers.create().unwrap();
    assert!(matches!(desks.open_reader(desk, &readers, 0, pane, destination, || false),
        Err(AccessError::ReaderTransfer(DeskReaderError::Desk(DeskSessionError::Desk(fcb::ui::reading_panes::desk::DeskError::StaleRevision))))));
    assert!(matches!(info(&readers, destination), Err(reader_sessions::AccessError::NotOpen)));
    assert!(desks.open_reader(desk, &readers, 1, u64::MAX, destination, || false).is_err());
    assert!(desks.open_reader(desk, &readers, 1, pane, desk, || false).is_err());
    assert!(desks.open_reader(desk, &readers, 1, pane, u64::MAX, || false).is_err());
    readers.open(destination, &other, 4096, || false).unwrap();
    assert!(matches!(desks.open_reader(desk, &readers, 1, pane, destination, || false),
        Err(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen))));
    let copy = readers.execute(destination, ReaderCommand::CopyRange { start: 0, end: 23 }, || false).unwrap();
    assert!(copy.as_str().contains("7072656578697374696e672064657374696e6174696f6e"));
    assert_eq!(field(desks.execute(desk, Command::State, || false).unwrap().as_str(), "model_revision"), 1);
    desks.close(desk).unwrap();
    assert!(matches!(desks.open_reader(desk, &readers, 1, pane, destination, || false), Err(AccessError::Unknown)));
}

#[test]
fn source_and_destination_cancellation_revoke_inflight_handoffs_but_allow_fresh_attempts() {
    let root = directory(); let source = root.join("large.txt"); fs::write(&source, vec![b'x'; 512 * 1024]).unwrap();
    for cancel_source in [true, false] {
        let desks = DeskSessions::new(); let (desk, pane) = open_desk(&desks, &source);
        let readers = ReaderSessions::new(); let destination = readers.create().unwrap();
        let mut checks = 0;
        let result = desks.open_reader(desk, &readers, 1, pane, destination, || {
            checks += 1;
            if checks == 4 {
                if cancel_source { desks.cancel(desk).unwrap(); }
                else { readers.cancel(destination).unwrap(); }
            }
            false
        });
        assert!(result.err().unwrap().canceled());
        assert_eq!(checks, 4, "negative control reaches retained capture preparation");
        assert!(matches!(info(&readers, destination), Err(reader_sessions::AccessError::NotOpen)));
        assert_eq!(field(desks.execute(desk, Command::State, || false).unwrap().as_str(), "model_revision"), 1);
        desks.open_reader(desk, &readers, 1, pane, destination, || false).unwrap();
        assert_eq!(field(info(&readers, destination).unwrap().as_str(), "captured_bytes"), 512 * 1024);
    }
}

#[test]
fn close_and_busy_source_reject_delivery_without_deadlock_or_leaking_an_extra_reader_slot() {
    let root = directory(); let source = root.join("source.txt"); fs::write(&source, b"retained bytes").unwrap();
    let desks = DeskSessions::new(); let (desk, pane) = open_desk(&desks, &source);
    let readers = ReaderSessions::new(); let destination = readers.create().unwrap();
    let cell = desks.get(desk).unwrap();
    let held = lock(&cell.state).unwrap();
    assert!(matches!(desks.open_reader(desk, &readers, 1, pane, destination, || false), Err(AccessError::Busy)));
    drop(held);
    let mut closed = false;
    let result = desks.open_reader(desk, &readers, 1, pane, destination, || {
        if !closed { desks.close(desk).unwrap(); closed = true; }
        false
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    assert!(matches!(info(&readers, destination), Err(reader_sessions::AccessError::NotOpen)));
    let replacement = desks.create().unwrap(); assert_ne!(replacement, desk);
    readers.close(destination).unwrap();
}

#[test]
fn transfer_requires_a_slot_in_the_existing_eight_reader_admission_limit() {
    let root = directory(); let source = root.join("source.txt"); fs::write(&source, b"bounded capture").unwrap();
    let desks = DeskSessions::new(); let (desk, pane) = open_desk(&desks, &source);
    let readers = ReaderSessions::new();
    let handles: Vec<_> = (0..reader_sessions::MAX_READER_SESSIONS).map(|_| readers.create().unwrap()).collect();
    assert!(matches!(readers.create(), Err(reader_sessions::AccessError::Capacity)));
    desks.open_reader(desk, &readers, 1, pane, handles[0], || false).unwrap();
    assert!(matches!(readers.create(), Err(reader_sessions::AccessError::Capacity)));
    readers.close(handles[0]).unwrap();
    let next = readers.create().unwrap(); assert!(!handles.contains(&next));
    desks.open_reader(desk, &readers, 1, pane, next, || false).unwrap();
    let info = info(&readers, next).unwrap();
    assert_eq!(field(info.as_str(), "captured_bytes"), 15);
    assert_eq!(field(info.as_str(), "initial_read_calls"), 0);
}
