#![forbid(unsafe_code)]

use super::*;
use fcb::search::{PathCase, PathMatchMode};
use std::{fs, sync::atomic::{AtomicU64, Ordering}};
use super::super::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-file-finder-registry-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Preserve fixtures for diagnosis.
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn options(max_results: usize) -> FileFinderOptions {
    FileFinderOptions { max_results, case: PathCase::UnicodeLowercase, mode: PathMatchMode::Fuzzy }
}
fn info(finders: &FileFinderSessions, handle: u64) -> String {
    finders.execute(handle, Command::Info, || false).unwrap().as_str().to_owned()
}
fn opened(finders: &FileFinderSessions, root: &Path, query: &[u8]) -> u64 {
    let handle = finders.create().unwrap();
    let reply = finders.open(handle, root, 7, query, options(32), || false).unwrap();
    assert_eq!(number(reply.as_str(), "revision"), 1);
    assert_eq!(number(reply.as_str(), "retained_hits"), 0);
    handle
}

#[test]
fn progressive_file_finder_replays_accepted_progress_and_rejects_stale_rows() {
    let root = root();
    for i in 0..1300 { fs::write(root.join(format!("file-{i:04}.rs")), b"source").unwrap(); }
    let finders = FileFinderSessions::new(); let handle = opened(&finders, &root, b"file");
    let first = finders.execute(handle, Command::Advance { generation: 7, after_revision: 1, max_batches: 1 }, || false).unwrap();
    let revision = number(first.as_str(), "revision");
    let file = number(first.as_str(), "file_id");
    assert_eq!(revision, 2);
    assert!(number(first.as_str(), "searched_files") <= 256);
    assert_eq!(number(first.as_str(), "retained_hits"), 32);
    assert!(first.as_str().contains("\"more\":true"));
    // A retry with a different work allowance must not consume another batch.
    let replay = finders.execute(handle, Command::Advance { generation: 7, after_revision: 1, max_batches: 16 }, || false).unwrap();
    assert_eq!(first.as_str(), replay.as_str());
    let stopped = finders.execute(handle, Command::Stop { generation: 7 }, || false).unwrap();
    assert!(stopped.as_str().contains("\"state\":\"paused\""));
    let complete = finders.execute(handle, Command::Advance { generation: 7, after_revision: revision, max_batches: 16 }, || false).unwrap();
    assert_eq!(number(complete.as_str(), "searched_files"), 1300);
    assert_eq!(number(complete.as_str(), "matches_seen"), 1300);
    assert_eq!(number(complete.as_str(), "retained_hits"), 32);
    assert!(complete.as_str().contains("\"search_complete\":true"));
    assert!(complete.as_str().contains("\"truncated\":true"));
    assert!(complete.as_str().contains("\"discovery_source_bytes_read\":\"0\""));
    assert!(finders.execute(handle, Command::Page { generation: 7, revision }, || false).is_err());
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    // Retain the private fixture elsewhere. A stale hit must fail before root
    // resolution, even when its old filename was once a legitimate result.
    let moved = root.with_extension("retained-after-move");
    fs::rename(&root, moved).unwrap();
    let error = finders.open_reader(handle, &readers, reader, 7, revision, file, 1024, || false)
        .err().expect("stale revision must refuse");
    assert!(error.to_string().contains("STALE_REVISION"), "{error}");
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    readers.close(reader).unwrap(); finders.close(handle).unwrap();
}

#[test]
fn progressive_file_finder_admits_empty_reader_before_source_and_preserves_capture() {
    let root = root(); let path = root.join("hit.rs");
    fs::write(&path, b"first\nsecond\n").unwrap();
    let finders = FileFinderSessions::new(); let handle = opened(&finders, &root, b"hit.rs");
    let report = finders.execute(handle, Command::Advance { generation: 7, after_revision: 1, max_batches: 16 }, || false).unwrap();
    let revision = number(report.as_str(), "revision"); let file = number(report.as_str(), "file_id");
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    assert!(finders.open_reader(handle, &readers, reader, 8, revision, file, 1024, || false).is_err());
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    let receipt = finders.open_reader(handle, &readers, reader, 7, revision, file, 1024, || false).unwrap();
    assert!(receipt.as_str().contains("\"source_observation\":\"new-capture-after-metadata-selection\""));
    assert!(receipt.as_str().contains(&format!("\"reader_owner\":\"{reader}\"")));
    fs::rename(path, root.join("moved-after-capture.rs")).unwrap();
    assert!(matches!(finders.open_reader(handle, &readers, reader, 7, revision, file, 1024, || false),
        Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    assert!(matches!(finders.open_reader(handle, &readers, u64::MAX, 7, revision, file, 1024, || false),
        Err(AccessError::Reader(ReaderError::UnknownHandle))));
    finders.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 5 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6669727374\""));
    readers.close(reader).unwrap();
}

#[test]
fn progressive_file_finder_cancel_and_lost_initial_delivery_leave_recoverable_state() {
    let root = root(); fs::write(root.join("retained.rs"), b"capture").unwrap();
    let finders = FileFinderSessions::new(); let first = finders.create().unwrap();
    let mut polls = 0;
    finders.open(first, &root, 7, b"retained", options(32), || { polls += 1; false }).unwrap();
    finders.close(first).unwrap();
    assert!(polls > 0);
    let handle = finders.create().unwrap(); let mut at = 0;
    let lost = finders.open(handle, &root, 7, b"retained", options(32), || {
        at += 1;
        if at == polls { finders.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(lost, Err(AccessError::Canceled)));
    let recovered = info(&finders, handle);
    assert_eq!(number(&recovered, "revision"), 1);
    assert!(recovered.contains("\"pending_batch\":true"));
    let mut visits = 0;
    let canceled = finders.execute(handle, Command::Advance { generation: 7, after_revision: 1, max_batches: 16 }, || {
        visits += 1;
        if visits == 2 { finders.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    let recovered = info(&finders, handle);
    let current = number(&recovered, "revision");
    let report = if recovered.contains("\"more\":true") {
        finders.execute(handle, Command::Advance { generation: 7, after_revision: current, max_batches: 16 }, || false).unwrap().as_str().to_owned()
    } else { recovered };
    assert_eq!(number(&report, "matches_seen"), 1);
    assert!(report.contains("\"search_complete\":true"));
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    let revision = number(&report, "revision"); let file = number(&report, "file_id");
    assert!(finders.open_reader(handle, &readers, reader, 7, revision, file, 1024, || true).is_err());
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    finders.open_reader(handle, &readers, reader, 7, revision, file, 1024, || false).unwrap();
    readers.close(reader).unwrap(); finders.close(handle).unwrap();
}

#[test]
fn progressive_file_finder_retiring_handles_keep_capacity_and_cancel_without_locking() {
    let finders = FileFinderSessions::new();
    let handles: Vec<_> = (0..MAX_FILE_FINDER_SESSIONS).map(|_| finders.create().unwrap()).collect();
    assert!(matches!(finders.create(), Err(AccessError::Capacity)));
    let held = finders.get(handles[0]).unwrap(); let locked = held.state.lock().unwrap();
    assert!(matches!(finders.execute(handles[0], Command::Info, || false), Err(AccessError::Busy)));
    finders.cancel(handles[0]).unwrap(); finders.close(handles[0]).unwrap();
    assert!(matches!(finders.create(), Err(AccessError::Capacity)));
    assert!(matches!(finders.execute(handles[0], Command::Info, || false), Err(AccessError::Unknown)));
    drop(locked); drop(held);
    let replacement = finders.create().unwrap(); assert!(!handles.contains(&replacement));
    finders.close(replacement).unwrap();
    for &handle in &handles[1..] { finders.close(handle).unwrap(); }
}
