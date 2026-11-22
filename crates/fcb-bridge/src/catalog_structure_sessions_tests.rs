#![forbid(unsafe_code)]

use super::*;
use fcb_app::host::catalog_session::CatalogStructureSearchMode;
use std::{fs, sync::atomic::{AtomicU64, Ordering}};

fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-catalog-structure-registry-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn open(searches: &CatalogStructureSessions, root: &Path) -> u64 {
    let handle = searches.create().unwrap();
    let options = CatalogStructureSearchOptions::new(CatalogStructureSearchMode::Symbols);
    let initial = searches.open(handle, root, 7, "target", options, || false).unwrap();
    assert_eq!(number(initial.as_str(), "revision"), 1);
    assert_eq!(number(initial.as_str(), "payload_bytes_read"), 0);
    handle
}
fn info(searches: &CatalogStructureSessions, handle: u64) -> String {
    searches.execute(handle, Command::Info, || false).unwrap().as_str().to_owned()
}
fn drain(searches: &CatalogStructureSessions, handle: u64) -> String {
    let mut report = info(searches, handle);
    for _ in 0..10_000 {
        if report.contains("\"more\":false") { return report; }
        report = searches.execute(handle, Command::Advance { generation: 7,
            after_revision: number(&report, "revision"), max_steps: 16 }, || false).unwrap().as_str().to_owned();
    }
    panic!("bounded structural search did not finish: {report}");
}

#[test]
fn independent_reader_handoff_uses_captured_bytes_and_preserves_empty_destination_on_refusal() {
    let root = root(); let path = root.join("source.rs");
    fs::write(&path, b"fn target() {}\n").unwrap();
    let searches = CatalogStructureSessions::new();
    let readers = reader_sessions::ReaderSessions::new();
    let handle = open(&searches, &root);
    let report = drain(&searches, handle);
    let revision = number(&report, "revision");
    let reader = readers.create().unwrap();
    assert!(matches!(searches.open_hit_reader(handle, &readers, reader, 7, 1, 0, || false),
        Err(AccessError::Search(CatalogStructureSearchError::StaleRevision))));
    assert!(matches!(readers.execute(reader, reader_sessions::Command::Info, || false),
        Err(reader_sessions::AccessError::NotOpen)));
    fs::remove_file(path).unwrap();
    let receipt = searches.open_hit_reader(handle, &readers, reader, 7, revision, 0, || false).unwrap();
    assert!(receipt.as_str().contains("\"activation_source\":\"retained-search-capture\""));
    assert!(receipt.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    assert!(matches!(searches.open_hit_reader(handle, &readers, reader, 7, revision, 0, || false),
        Err(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen))));
    searches.close(handle).unwrap();
    let copied = readers.execute(reader, reader_sessions::Command::CopyRange {
        start: 0, end: b"fn target() {}\n".len() as u64 }, || false).unwrap();
    assert!(copied.as_str().contains("666e207461726765742829207b7d0a"));
    let outside = readers.execute(reader, reader_sessions::Command::CopyRange {
        start: 0, end: b"fn target() {}\n".len() as u64 + 1 }, || false);
    assert!(outside.is_err(), "an out-of-range request must remain refused");
    readers.close(reader).unwrap();
}

#[test]
fn lost_open_reply_and_prework_cancellation_keep_the_accepted_query_recoverable() {
    let root = root(); fs::write(root.join("source.rs"), b"fn target() {}\n").unwrap();
    let searches = CatalogStructureSessions::new();
    let probe = searches.create().unwrap();
    let mut total_polls = 0;
    searches.open(probe, &root, 7, "target",
        CatalogStructureSearchOptions::new(CatalogStructureSearchMode::Symbols),
        || { total_polls += 1; false }).unwrap();
    searches.close(probe).unwrap();
    let handle = searches.create().unwrap();
    let mut polls = 0;
    let lost = searches.open(handle, &root, 7, "target",
        CatalogStructureSearchOptions::new(CatalogStructureSearchMode::Symbols), || {
            polls += 1;
            if polls == total_polls { searches.cancel(handle).unwrap(); }
            false
        });
    assert!(matches!(lost, Err(AccessError::Canceled)));
    assert_eq!(number(&info(&searches, handle), "revision"), 1);
    let interrupted = searches.execute(handle, Command::Advance { generation: 7, after_revision: 1, max_steps: 16 },
        || { searches.cancel(handle).unwrap(); false });
    assert!(matches!(interrupted, Err(AccessError::Canceled)));
    let report = drain(&searches, handle);
    assert_eq!(number(&report, "matches_seen"), 1);
    assert!(report.contains("\"search_complete\":true"));
    searches.close(handle).unwrap();
}

#[test]
fn in_flight_cancellation_keeps_prior_evidence_and_root_replacement_revokes_it() {
    let root = root();
    for index in 0..3 { fs::write(root.join(format!("file-{index}.rs")), b"fn target() {}\n").unwrap(); }
    let searches = CatalogStructureSessions::new(); let handle = open(&searches, &root);
    let mut report = info(&searches, handle);
    while number(&report, "retained_hits") == 0 {
        report = searches.execute(handle, Command::Advance { generation: 7,
            after_revision: number(&report, "revision"), max_steps: 1 }, || false).unwrap().as_str().to_owned();
    }
    let mut polls = 0;
    let interrupted = searches.execute(handle, Command::Advance { generation: 7,
        after_revision: number(&report, "revision"), max_steps: 16 }, || {
        polls += 1;
        if polls == 6 { searches.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(interrupted, Err(AccessError::Canceled)));
    let canceled = info(&searches, handle);
    assert!(canceled.contains("\"state\":\"canceled\""));
    assert_eq!(number(&canceled, "retained_hits"), 1);
    let hit = searches.execute(handle, Command::Hit { generation: 7,
        revision: number(&canceled, "revision"), position: 0 }, || false).unwrap();
    assert!(hit.as_str().contains("\"original_hex\":\"746172676574\""));
    let retained = root.with_extension("retained-after-move");
    fs::rename(&root, retained).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(searches.execute(handle, Command::Info, || false).is_err());
    let readers = reader_sessions::ReaderSessions::new(); let reader = readers.create().unwrap();
    assert!(searches.open_hit_reader(handle, &readers, reader, 7,
        number(&canceled, "revision"), 0, || false).is_err());
    assert!(matches!(readers.execute(reader, reader_sessions::Command::Info, || false),
        Err(reader_sessions::AccessError::NotOpen)));
    searches.close(handle).unwrap(); readers.close(reader).unwrap();
}

#[test]
fn retiring_work_keeps_capacity_and_cross_family_handles_never_alias() {
    let searches = CatalogStructureSessions::new();
    let readers = reader_sessions::ReaderSessions::new(); let reader = readers.create().unwrap();
    assert!(matches!(searches.execute(reader, Command::Info, || false), Err(AccessError::Unknown)));
    let handles: Vec<_> = (0..MAX_CATALOG_STRUCTURE_SESSIONS).map(|_| searches.create().unwrap()).collect();
    assert!(!handles.contains(&reader));
    assert!(matches!(searches.create(), Err(AccessError::Capacity)));
    let held = searches.get(handles[0]).unwrap(); let locked = held.state.lock().unwrap();
    assert!(matches!(searches.execute(handles[0], Command::Info, || false), Err(AccessError::Busy)));
    searches.cancel(handles[0]).unwrap(); searches.close(handles[0]).unwrap();
    assert!(matches!(searches.create(), Err(AccessError::Capacity)));
    drop(locked); drop(held);
    let replacement = searches.create().unwrap(); assert!(!handles.contains(&replacement));
    searches.close(replacement).unwrap();
    for &handle in &handles[1..] { searches.close(handle).unwrap(); }
    readers.close(reader).unwrap();
}
