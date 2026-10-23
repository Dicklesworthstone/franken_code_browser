#![forbid(unsafe_code)]

use super::*;
use std::{fs, sync::atomic::{AtomicU64, Ordering}};
use super::super::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-catalog-registry-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Retain fixtures for diagnosis.
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn info(catalogs: &CatalogSessions, handle: u64) -> String {
    catalogs.execute(handle, Command::Info, || false).unwrap().as_str().to_owned()
}
fn opened(catalogs: &CatalogSessions, root: &Path) -> u64 {
    let handle = catalogs.create().unwrap();
    let response = catalogs.open(handle, root, || false).unwrap();
    assert!(response.as_str().contains("\"latest_page\":\"1\"")); handle
}

#[test]
fn consumed_pages_replay_without_discovery_and_evicted_rows_lose_authority() {
    let root = root();
    for i in 0..1300 { fs::write(root.join(format!("file-{i:04}.rs")), b"source").unwrap(); }
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let first = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let first_file = number(first.as_str(), "file_id");
    assert!(number(first.as_str(), "catalogued_files") <= 256);
    assert!(first.as_str().contains("\"more\":true"));
    let second = catalogs.execute(handle, Command::Advance { after_page: 1 }, || false).unwrap();
    let replay = catalogs.execute(handle, Command::Advance { after_page: 1 }, || false).unwrap();
    assert_eq!(second.as_str(), replay.as_str());
    let stopped = catalogs.execute(handle, Command::Stop, || false).unwrap();
    assert!(stopped.as_str().contains("\"state\":\"paused\""));
    assert!(stopped.as_str().contains("\"more\":true"));
    for after_page in 2..5 {
        let next = catalogs.execute(handle, Command::Advance { after_page }, || false).unwrap();
        assert_eq!(number(next.as_str(), "page_id"), after_page + 1);
        assert!(number(next.as_str(), "retained_pages") <= 4);
        assert!(number(next.as_str(), "retained_files") <= 1024);
    }
    assert_eq!(number(&info(&catalogs, handle), "first_retained_page"), 2);
    assert!(catalogs.execute(handle, Command::Page { page: 1 }, || false).is_err());
    assert!(catalogs.execute(handle, Command::Advance { after_page: u64::MAX }, || false).is_err());
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    assert!(catalogs.open_reader(handle, &readers, reader, 1, first_file, 1024, || false).is_err());
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    readers.close(reader).unwrap(); catalogs.close(handle).unwrap();
}

#[test]
fn page_rows_open_exact_independent_readers_with_destination_admission_first() {
    let root = root(); let path = root.join("one.rs");
    fs::write(&path, b"first\nsecond\n").unwrap();
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    let page = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let file = number(page.as_str(), "file_id");
    let receipt = catalogs.open_reader(handle, &readers, reader, 1, file, 1024, || false).unwrap();
    assert!(receipt.as_str().contains("\"source_observation\":\"new-capture-after-metadata-selection\""));
    assert!(receipt.as_str().contains(&format!("\"reader_owner\":\"{reader}\"")));
    assert!(receipt.as_str().contains("first\\nsecond\\n"));
    // Keep the fixture under a fresh name, leaving the selected path absent.
    // Destination errors must win before even attempting source resolution.
    fs::rename(&path, root.join("moved-after-capture.rs")).unwrap();
    assert!(matches!(catalogs.open_reader(handle, &readers, reader, 1, file, 1024, || false),
        Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    assert!(matches!(catalogs.open_reader(handle, &readers, u64::MAX, 1, file, 1024, || false),
        Err(AccessError::Reader(ReaderError::UnknownHandle))));
    let empty = readers.create().unwrap();
    assert!(catalogs.open_reader(handle, &readers, empty, 1, u64::MAX, 1024, || false).is_err());
    assert!(matches!(readers.execute(empty, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    catalogs.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 5 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6669727374\""));
    readers.close(empty).unwrap(); readers.close(reader).unwrap();
}

#[test]
fn canceled_work_preserves_recoverable_pages_and_empty_reader_destination() {
    let root = root();
    for i in 0..300 { fs::write(root.join(format!("file-{i}.rs")), b"retained").unwrap(); }
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let before = info(&catalogs, handle);
    let mut polls = 0;
    let canceled = catalogs.execute(handle, Command::Advance { after_page: 1 }, || {
        polls += 1;
        if polls == 2 { catalogs.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    let recovered = info(&catalogs, handle);
    let latest = number(&recovered, "latest_page");
    assert!((1..=2).contains(&latest));
    assert!(number(&recovered, "catalogued_files") >= number(&before, "catalogued_files"));
    let retry = catalogs.execute(handle, Command::Advance { after_page: 1 }, || false).unwrap();
    assert_eq!(number(retry.as_str(), "page_id"), 2);
    if latest == 2 { assert_eq!(number(retry.as_str(), "catalogued_files"), number(&recovered, "catalogued_files")); }
    let page = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    let file = number(page.as_str(), "file_id");
    assert!(catalogs.open_reader(handle, &readers, reader, 1, file, 1024, || true).is_err());
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    catalogs.open_reader(handle, &readers, reader, 1, file, 1024, || false).unwrap();
    readers.close(reader).unwrap(); catalogs.close(handle).unwrap();
}

#[test]
fn busy_and_retiring_cells_preserve_nonblocking_capacity_and_never_reuse_ids() {
    let catalogs = CatalogSessions::new();
    let handles: Vec<_> = (0..MAX_CATALOG_SESSIONS).map(|_| catalogs.create().unwrap()).collect();
    assert!(matches!(catalogs.create(), Err(AccessError::Capacity)));
    let held = catalogs.get(handles[0]).unwrap();
    let locked = held.state.lock().unwrap();
    assert!(matches!(catalogs.execute(handles[0], Command::Info, || false), Err(AccessError::Busy)));
    catalogs.cancel(handles[0]).unwrap();
    catalogs.close(handles[0]).unwrap();
    assert!(matches!(catalogs.create(), Err(AccessError::Capacity)));
    assert!(matches!(catalogs.execute(handles[0], Command::Info, || false), Err(AccessError::Unknown)));
    drop(locked); drop(held);
    let replacement = catalogs.create().unwrap();
    assert!(!handles.contains(&replacement));
    catalogs.close(replacement).unwrap();
    for handle in &handles[1..] { catalogs.close(*handle).unwrap(); }
}
