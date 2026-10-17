#![forbid(unsafe_code)]

use super::*;
use std::{fs, os::unix::ffi::OsStringExt};

fn find(sessions: &ReaderSessions, handle: u64, generation: u64, needle: &str) {
    sessions.execute(handle, Command::Find { generation, needle, limit: 16, scan_bytes: 1024 }, || false).unwrap();
}
fn copy(sessions: &ReaderSessions, handle: u64, length: u64) -> String {
    sessions.execute(handle, Command::CopyRange { start: 0, end: length }, || false).unwrap().as_str().to_owned()
}

#[test]
fn fork_owns_exact_bytes_after_source_close_and_has_independent_query_authority() {
    let sessions = ReaderSessions::new();
    let source = sessions.create().unwrap(); let child = sessions.create().unwrap();
    sessions.supply(source, b"alpha beta alpha").unwrap();
    find(&sessions, source, 9, "alpha");
    let info = sessions.fork(source, child, || false).unwrap();
    assert!(info.as_str().contains(&format!("\"owner\":\"{child}\"")));
    for field in ["accepted_query_generation", "accepted_outline_generation", "accepted_document_generation"] {
        assert!(info.as_str().contains(&format!("\"{field}\":null")));
    }
    assert!(info.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    assert!(info.as_str().contains("\"initial_read_calls\":\"0\""));
    assert!(info.as_str().contains("\"capture_origin\":\"host-supplied\""));
    assert_eq!(sessions.execute(child, Command::CopyHit { generation: 9, index: 0 }, || false).err(),
        Some(AccessError::Reader(ReaderSessionError::MissingHit)));
    find(&sessions, child, 1, "beta");
    assert!(sessions.execute(source, Command::CopyHit { generation: 9, index: 0 }, || false)
        .unwrap().as_str().contains("616c706861"));
    sessions.cancel(source).unwrap(); sessions.close(source).unwrap();
    assert!(sessions.execute(child, Command::CopyHit { generation: 1, index: 0 }, || false)
        .unwrap().as_str().contains("62657461"));
    assert!(copy(&sessions, child, 16).contains("616c706861206265746120616c706861"));
    sessions.close(child).unwrap();
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}

#[test]
fn forks_preserve_empty_utf16_bom_nul_and_malformed_bytes() {
    for bytes in [&b""[..], &b"\xff\xfea\0\r\0\n\0b\0"[..], &b"\xfe\xff\0a\0\n"[..],
                  &b"\xef\xbb\xbfhi\0\xff"[..]] {
        let sessions = ReaderSessions::new();
        let source = sessions.create().unwrap(); let child = sessions.create().unwrap();
        sessions.supply(source, bytes).unwrap();
        let before = copy(&sessions, source, bytes.len() as u64);
        sessions.fork(source, child, || false).unwrap();
        sessions.close(source).unwrap();
        let expected: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert!(before.contains(&format!("\"original_hex\":\"{expected}\"")));
        assert!(copy(&sessions, child, bytes.len() as u64).contains(&format!("\"original_hex\":\"{expected}\"")));
        assert!(sessions.execute(child, Command::Window { offset: 0, bytes: 64 }, || false).is_ok());
        sessions.close(child).unwrap();
    }
}

#[test]
fn fork_never_reopens_changed_or_removed_file_and_preserves_native_path_bytes() {
    let sessions = ReaderSessions::new();
    let source = sessions.create().unwrap(); let child = sessions.create().unwrap();
    let directory = std::env::temp_dir().join(format!("fcb-reader-fork-{}-{source}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join(std::ffi::OsString::from_vec(b"name-\xff.rs".to_vec()));
    fs::write(&path, b"old bytes").unwrap();
    sessions.open(source, &path, 1024, || false).unwrap();
    fs::write(&path, b"new bytes").unwrap();
    fs::remove_file(&path).unwrap(); fs::remove_dir(&directory).unwrap();
    let info = sessions.fork(source, child, || false).unwrap();
    assert!(info.as_str().contains("6e616d652dff2e7273"));
    sessions.close(source).unwrap();
    assert!(copy(&sessions, child, 9).contains("6f6c64206279746573"));
    sessions.close(child).unwrap();
}

#[test]
fn invalid_busy_and_occupied_destinations_preserve_both_readers() {
    let sessions = ReaderSessions::new();
    let source = sessions.create().unwrap(); let child = sessions.create().unwrap();
    assert_eq!(sessions.fork(source, child, || false).err(), Some(AccessError::NotOpen));
    sessions.supply(source, b"parent").unwrap();
    assert_eq!(sessions.fork(source, source, || false).err(), Some(AccessError::InvalidArgument));
    assert_eq!(sessions.fork(0, child, || false).err(), Some(AccessError::UnknownHandle));
    assert_eq!(sessions.fork(source, 0, || false).err(), Some(AccessError::UnknownHandle));
    let source_cell = sessions.get(source).unwrap();
    let guard = source_cell.state.lock().unwrap();
    assert_eq!(sessions.fork(source, child, || false).err(), Some(AccessError::Busy));
    drop(guard); drop(source_cell);
    let child_cell = sessions.get(child).unwrap();
    let guard = child_cell.state.lock().unwrap();
    assert_eq!(sessions.fork(source, child, || false).err(), Some(AccessError::Busy));
    drop(guard); drop(child_cell);
    sessions.supply(child, b"other").unwrap();
    assert_eq!(sessions.fork(source, child, || false).err(), Some(AccessError::AlreadyOpen));
    assert!(copy(&sessions, source, 6).contains("706172656e74"));
    assert!(copy(&sessions, child, 5).contains("6f74686572"));
    sessions.close(source).unwrap(); sessions.close(child).unwrap();
}

#[test]
fn canceled_fork_is_retryable_and_never_revokes_parent_hits() {
    let sessions = ReaderSessions::new();
    let source = sessions.create().unwrap(); let child = sessions.create().unwrap();
    sessions.supply(source, b"needle").unwrap(); find(&sessions, source, 1, "needle");
    // Refuse before copying, after copying, and during info marshaling.
    for cancel_at in 1..=3 {
        let mut polls = 0;
        assert!(sessions.fork(source, child, || { polls += 1; polls == cancel_at })
            .err().expect("canceled fork").canceled());
        assert_eq!(sessions.execute(child, Command::Info, || false).err(), Some(AccessError::NotOpen));
    }
    let mut once = false;
    assert!(sessions.fork(source, child, || {
        if !once { once = true; sessions.cancel(child).unwrap(); }
        false
    }).err().expect("destination canceled").canceled());
    assert_eq!(sessions.execute(child, Command::Info, || false).err(), Some(AccessError::NotOpen));
    assert!(sessions.execute(source, Command::CopyHit { generation: 1, index: 0 }, || false).is_ok());
    sessions.fork(source, child, || false).unwrap();
    sessions.close(child).unwrap(); sessions.close(source).unwrap();
}

#[test]
fn close_during_fork_suppresses_delivery_and_keeps_retirement_permit_until_drain() {
    let sessions = ReaderSessions::new();
    let handles: Vec<_> = (0..MAX_READER_SESSIONS).map(|_| sessions.create().unwrap()).collect();
    let source = handles[0]; let child = handles[1];
    sessions.supply(source, b"retained").unwrap();
    let mut once = false;
    let result = sessions.fork(source, child, || {
        if !once {
            once = true;
            sessions.close(source).unwrap();
            assert_eq!(sessions.live.load(Ordering::Acquire), MAX_READER_SESSIONS);
            assert_eq!(sessions.create(), Err(AccessError::Capacity));
        }
        false
    });
    assert!(result.err().expect("source closed").canceled());
    assert_eq!(sessions.execute(child, Command::Info, || false).err(), Some(AccessError::NotOpen));
    assert_eq!(sessions.live.load(Ordering::Acquire), MAX_READER_SESSIONS - 1);
    for handle in &handles[1..] { sessions.close(*handle).unwrap(); }
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}

#[test]
fn pinned_readers_use_the_same_capacity_and_can_fork_again_without_parent() {
    let sessions = ReaderSessions::new();
    let handles: Vec<_> = (0..MAX_READER_SESSIONS).map(|_| sessions.create().unwrap()).collect();
    sessions.supply(handles[0], b"pinned").unwrap();
    for pair in handles.windows(2) { sessions.fork(pair[0], pair[1], || false).unwrap(); }
    assert_eq!(sessions.create(), Err(AccessError::Capacity));
    for handle in &handles[..MAX_READER_SESSIONS - 1] { sessions.close(*handle).unwrap(); }
    let last = handles[MAX_READER_SESSIONS - 1];
    let next = sessions.create().unwrap(); sessions.fork(last, next, || false).unwrap();
    sessions.close(last).unwrap();
    assert!(copy(&sessions, next, 6).contains("70696e6e6564"));
    sessions.close(next).unwrap();
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}
