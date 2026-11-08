#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf, sync::mpsc, time::{SystemTime, UNIX_EPOCH}};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-observed-registry-{}-{}-{}",
        std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    fs::write(path.join("source.rs"), b"first observed source\n").unwrap();
    path
}

#[test]
fn every_slot_is_admitted_and_a_retiring_cell_keeps_its_permit() {
    let readers = ObservedReaderSessions::new();
    let handles: Vec<_> = (0..MAX_SESSIONS).map(|_| readers.create().unwrap()).collect();
    assert!(matches!(readers.create(), Err(AccessError::Capacity)));
    let retained = readers.get(handles[0]).unwrap();
    readers.close(handles[0]).unwrap();
    assert!(matches!(readers.get(handles[0]), Err(AccessError::Unknown)));
    assert!(matches!(readers.create(), Err(AccessError::Capacity)));
    drop(retained);
    let replacement = readers.create().unwrap();
    assert!(!handles.contains(&replacement));
    readers.close(replacement).unwrap();
    for handle in handles.into_iter().skip(1) { readers.close(handle).unwrap(); }
    assert_eq!(readers.live.load(Ordering::Acquire), 0);
}

#[test]
fn close_during_open_revokes_delivery_without_waiting_or_reusing_the_slot() {
    let readers = ObservedReaderSessions::new();
    let handles: Vec<_> = (0..MAX_SESSIONS).map(|_| readers.create().unwrap()).collect();
    let path = root();
    let (entered, receive_entered) = mpsc::sync_channel(0);
    let (release, receive_release) = mpsc::sync_channel(0);
    std::thread::scope(|scope| {
        let readers_ref = &readers;
        let first = handles[0];
        let worker = scope.spawn(move || {
            let mut sent = false;
            readers_ref.open(first, &path, b"source.rs", || {
                if !sent {
                    sent = true; entered.send(()).unwrap(); receive_release.recv().unwrap();
                }
                false
            })
        });
        receive_entered.recv().unwrap();
        assert!(matches!(readers.execute(first, Command::Info, || false), Err(AccessError::Busy)));
        readers.close(first).unwrap();
        assert!(matches!(readers.create(), Err(AccessError::Capacity)));
        release.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
    });
    let next = readers.create().unwrap();
    assert!(!handles.contains(&next));
    readers.close(next).unwrap();
    for handle in handles.into_iter().skip(1) { readers.close(handle).unwrap(); }
}

#[test]
fn cancellation_reuses_the_session_but_never_the_observation_or_handle_identity() {
    let readers = ObservedReaderSessions::new();
    let complete_readers = super::super::reader_sessions::ReaderSessions::new();
    let complete = complete_readers.create().unwrap();
    let handle = readers.create().unwrap();
    assert_ne!(handle, complete, "all reader kinds share the library owner allocator");
    assert!(matches!(readers.execute(complete, Command::Info, || false), Err(AccessError::Unknown)));
    let path = root();
    assert!(readers.open(handle, &path, b"source.rs", || true).is_err());
    assert!(matches!(readers.execute(handle, Command::Info, || false), Err(AccessError::NotOpen)));
    readers.open(handle, &path, b"source.rs", || false).unwrap();
    assert!(matches!(readers.open(handle, &path, b"source.rs", || false), Err(AccessError::AlreadyOpen)));
    readers.cancel(handle).unwrap();
    let first = readers.execute(handle, Command::Window { offset: 0, bytes: 8,
        encoding: ObservedReaderEncoding::Utf8 }, || false).unwrap();
    assert!(first.as_str().contains("\"observation\":\"1\""));
    assert!(readers.execute(handle, Command::Window { offset: 8, bytes: 8,
        encoding: ObservedReaderEncoding::Utf8 }, || true).is_err());
    let retained = readers.execute(handle, Command::Retained { observation: 1 }, || false).unwrap();
    assert!(retained.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    let cell = readers.get(handle).unwrap();
    cell.epoch.store(u64::MAX, Ordering::Release);
    assert!(matches!(readers.cancel(handle), Err(AccessError::IdentityExhausted)));
    assert!(matches!(readers.execute(handle, Command::Info, || false), Err(AccessError::Closed)));
    drop(cell);
    readers.close(handle).unwrap();
    complete_readers.close(complete).unwrap();
}
