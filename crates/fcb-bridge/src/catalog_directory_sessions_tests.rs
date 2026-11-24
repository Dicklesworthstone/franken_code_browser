#![forbid(unsafe_code)]

use super::*;
use std::{fs, sync::atomic::{AtomicU64, Ordering}};
use super::super::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-directory-catalog-registry-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Retain fixtures for diagnosis.
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn scope(text: &str, directory: &[u8]) {
    let hex: String = directory.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(text.contains("\"discovery_scope\":\"directory\""), "{text}");
    assert!(text.contains("\"path_basis\":\"project-root\""), "{text}");
    let payload = text.split_once("\"discovery_directory\":{").unwrap().1.split('}').next().unwrap();
    assert!(payload.contains(&format!("\"hex\":\"{hex}\"")), "{text}");
    assert_eq!(number(text, "discovery_source_bytes_read"), 0);
    assert_eq!(number(text, "discovery_source_read_calls"), 0);
}

#[test]
fn canceled_scoped_open_never_latches_a_target_and_the_empty_handle_can_retry() {
    let root = root();
    for directory in ["first", "second"] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("file.rs"), directory.as_bytes()).unwrap();
    }
    let catalogs = CatalogSessions::new(); let handle = catalogs.create().unwrap();
    let mut requested = false;
    let result = catalogs.open_directory_with_policy(handle, &root, b"first",
        CatalogDiscoveryPolicy::RepositoryRules, || {
            if !requested { requested = true; catalogs.cancel(handle).unwrap(); }
            false
        });
    assert!(requested); assert!(matches!(result, Err(AccessError::Canceled)));
    assert!(matches!(catalogs.execute(handle, Command::Info, || false), Err(AccessError::NotOpen)));
    assert!(catalogs.open_directory_with_policy(handle, &root, b"missing",
        CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err());
    assert!(matches!(catalogs.execute(handle, Command::Info, || false), Err(AccessError::NotOpen)));
    let opened = catalogs.open_directory_with_policy(handle, &root, b"second",
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    scope(opened.as_str(), b"second"); assert_eq!(number(opened.as_str(), "catalogued_files"), 1);
    assert!(matches!(catalogs.open_directory_with_policy(handle, &root, b"first",
        CatalogDiscoveryPolicy::RepositoryRules, || false), Err(AccessError::AlreadyOpen)));
    scope(catalogs.execute(handle, Command::Info, || false).unwrap().as_str(), b"second");
    catalogs.close(handle).unwrap();
}

#[test]
fn scoped_cancel_recovery_and_stop_replay_keep_the_same_target_and_metadata_prefix() {
    let root = root(); let selected = root.join("selected"); fs::create_dir(&selected).unwrap();
    let seed = selected.join("file-0000.rs"); fs::write(&seed, b"retained").unwrap();
    for ordinal in 1..800 { fs::hard_link(&seed, selected.join(format!("file-{ordinal:04}.rs"))).unwrap(); }
    fs::write(root.join("outside.rs"), b"not selected").unwrap();
    let catalogs = CatalogSessions::new(); let handle = catalogs.create().unwrap();
    let first = catalogs.open_directory_with_policy(handle, &root, b"selected",
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    scope(first.as_str(), b"selected"); assert!(first.as_str().contains("\"more\":true"));
    let mut polls = 0;
    let result = catalogs.execute(handle, Command::Advance { after_page: 1 }, || {
        polls += 1; if polls == 3 { catalogs.cancel(handle).unwrap(); } false
    });
    assert!(matches!(result, Err(AccessError::Canceled)));
    let info = catalogs.execute(handle, Command::Info, || false).unwrap();
    scope(info.as_str(), b"selected");
    assert_eq!(number(info.as_str(), "latest_page"), 2,
        "cancellation after consumption must preserve the recoverable successor");
    let second = catalogs.execute(handle, Command::Advance { after_page: 1 }, || false).unwrap();
    scope(second.as_str(), b"selected"); assert_eq!(number(second.as_str(), "page_id"), 2);
    let replay = catalogs.execute(handle, Command::Advance { after_page: 1 }, || false).unwrap();
    assert_eq!(second.as_str(), replay.as_str());
    let stopped = catalogs.execute(handle, Command::Stop, || false).unwrap();
    scope(stopped.as_str(), b"selected"); assert!(stopped.as_str().contains("\"state\":\"paused\""));
    let replay = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    scope(replay.as_str(), b"selected");
    assert_eq!(replay.as_str().split_once("\"files\":[").unwrap().1,
        first.as_str().split_once("\"files\":[").unwrap().1);
    let mut page = catalogs.execute(handle, Command::Advance { after_page: 2 }, || false).unwrap();
    for _ in 0..8 {
        scope(page.as_str(), b"selected");
        assert!(number(page.as_str(), "retained_pages") <= 4);
        assert!(number(page.as_str(), "retained_files") <= 1024);
        if page.as_str().contains("\"more\":false") { break; }
        page = catalogs.execute(handle, Command::Advance { after_page: number(page.as_str(), "page_id") }, || false).unwrap();
    }
    assert!(page.as_str().contains("\"discovery_complete\":true"));
    assert_eq!(number(page.as_str(), "catalogued_files"), 800);
    catalogs.close(handle).unwrap();
}

#[test]
fn scoped_reader_transfer_admits_destination_first_and_survives_catalog_closure() {
    let root = root(); fs::create_dir(root.join("selected")).unwrap();
    let path = root.join("selected/file.rs"); fs::write(&path, b"original bytes").unwrap();
    fs::write(root.join("outside.rs"), b"outside").unwrap();
    let catalogs = CatalogSessions::new(); let handle = catalogs.create().unwrap();
    let page = catalogs.open_directory_with_policy(handle, &root, b"selected",
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let file = number(page.as_str(), "file_id");
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    assert!(catalogs.open_reader(handle, &readers, reader, 1, file, 4096, || true).is_err());
    assert!(matches!(readers.execute(reader, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    let receipt = catalogs.open_reader(handle, &readers, reader, 1, file, 4096, || false).unwrap();
    scope(receipt.as_str(), b"selected");
    assert_eq!(number(receipt.as_str(), "source_bytes_read"), b"original bytes".len() as u64);
    fs::rename(&path, root.join("preserved.rs")).unwrap();
    assert!(matches!(catalogs.open_reader(handle, &readers, reader, 1, file, 4096, || false),
        Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    let destination = readers.create().unwrap();
    assert!(catalogs.open_reader(handle, &readers, destination, 1, file, 4096, || false).is_err());
    assert!(matches!(readers.execute(destination, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    catalogs.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 14 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6f726967696e616c206279746573\""));
    readers.close(destination).unwrap(); readers.close(reader).unwrap();
}
