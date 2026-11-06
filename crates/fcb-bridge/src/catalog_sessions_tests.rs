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

fn named_file(page: &str, name: &[u8]) -> u64 {
    let hex: String = name.iter().map(|byte| format!("{byte:02x}")).collect();
    page.split("\"file_id\":\"").skip(1).find_map(|row| {
        row.contains(&format!("\"hex\":\"{hex}\""))
            .then(|| row.split('"').next().unwrap().parse().unwrap())
    }).expect("fixture file is retained")
}

fn prepare_links(readers: &ReaderSessions, reader: u64) {
    readers.execute(reader, ReaderCommand::Document { generation: 1, options: Default::default() }, || false).unwrap();
    readers.execute(reader, ReaderCommand::DocumentLinksPrepare { generation: 1, max_links: 16 }, || false).unwrap();
}

fn supplied_reader(readers: &ReaderSessions, handle: u64, label: &str, bytes: &[u8]) {
    readers.initialize_prepared(handle, |owner, stop| {
        let mut reader = fcb_app::host::reader::ReaderSession::from_bytes(owner,
            Path::new(label), bytes, &mut *stop).map_err(ReaderError::from)?;
        let reply = reader.info(&mut *stop).map_err(ReaderError::from)?;
        Ok::<_, ReaderError>((reader, reply))
    }, || false).unwrap();
}

#[test]
fn catalog_link_transfer_admits_destination_first_and_keeps_both_captures_independent() {
    let root = root();
    fs::write(root.join("README.md"), b"# Origin\n\n[Next](next.md#details)\n").unwrap();
    fs::write(root.join("next.md"), b"# Details\n\nTarget before replacement.\n").unwrap();
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let page = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let source_file = named_file(page.as_str(), b"README.md");
    let readers = ReaderSessions::new(); let origin = readers.create().unwrap();
    catalogs.open_reader(handle, &readers, origin, 1, source_file, 4096, || false).unwrap();
    readers.execute(origin, ReaderCommand::Document { generation: 1, options: Default::default() }, || false).unwrap();
    readers.execute(origin, ReaderCommand::DocumentLinksPrepare { generation: 1, max_links: 16 }, || false).unwrap();
    let destination = readers.create().unwrap();
    // The Markdown destination remains the one in the origin capture.
    fs::write(root.join("README.md"), b"[Changed](unobserved.md)").unwrap();
    let receipt = catalogs.open_document_link(handle, &readers, origin, destination, 1, source_file, 1, 1, 4096, || false).unwrap();
    assert!(receipt.as_str().contains("\"command\":\"open-document-link\""));
    assert!(receipt.as_str().contains("\"fragment_status\":\"not-applied\""));
    assert!(receipt.as_str().contains("Target before replacement."));
    fs::rename(root.join("next.md"), root.join("moved.md")).unwrap();
    assert!(matches!(catalogs.open_document_link(handle, &readers, origin, destination,
        1, source_file, 1, u64::MAX, 4096, || false), Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    assert!(matches!(catalogs.open_document_link(handle, &readers, origin, origin,
        1, source_file, 1, 1, 4096, || false), Err(AccessError::InvalidArgument)));
    catalogs.close(handle).unwrap();
    assert!(readers.execute(origin, ReaderCommand::DocumentLinks { generation: 1, first: 0, count: 16 }, || false)
        .unwrap().as_str().contains("next.md#details"));
    assert!(readers.execute(destination, ReaderCommand::Window { offset: 0, bytes: 4096 }, || false)
        .unwrap().as_str().contains("Target before replacement."));
    readers.close(origin).unwrap(); readers.close(destination).unwrap();
}

#[test]
fn catalog_link_transfer_observes_origin_cancellation_before_destination_acceptance() {
    let root = root();
    fs::write(root.join("README.md"), b"[Next](next.md)\n").unwrap();
    fs::write(root.join("next.md"), b"target\n").unwrap();
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let page = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let source_file = named_file(page.as_str(), b"README.md");
    let readers = ReaderSessions::new(); let origin = readers.create().unwrap(); let destination = readers.create().unwrap();
    catalogs.open_reader(handle, &readers, origin, 1, source_file, 4096, || false).unwrap();
    readers.execute(origin, ReaderCommand::Document { generation: 1, options: Default::default() }, || false).unwrap();
    readers.execute(origin, ReaderCommand::DocumentLinksPrepare { generation: 1, max_links: 16 }, || false).unwrap();
    let mut canceled = false;
    let result = catalogs.open_document_link(handle, &readers, origin, destination,
        1, source_file, 1, 1, 4096, || {
            if !canceled { canceled = true; readers.cancel(origin).unwrap(); }
            false
        });
    assert!(result.err().unwrap().canceled());
    assert!(matches!(readers.execute(destination, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    catalogs.open_document_link(handle, &readers, origin, destination, 1, source_file, 1, 1, 4096, || false).unwrap();
    readers.close(destination).unwrap(); readers.close(origin).unwrap(); catalogs.close(handle).unwrap();
}

#[test]
fn rejected_catalog_reader_cannot_leave_a_receipt_or_evict_an_accepted_origin() {
    let root = root();
    let source_bytes = b"# Origin\n\n[Next](next.md)\n";
    fs::write(root.join("README.md"), source_bytes).unwrap();
    fs::write(root.join("next.md"), b"# Target\n").unwrap();
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let page = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let source_file = named_file(page.as_str(), b"README.md");
    let target_file = named_file(page.as_str(), b"next.md");
    let readers = ReaderSessions::new(); let origin = readers.create().unwrap();
    let mut polls = 0;
    catalogs.open_reader(handle, &readers, origin, 1, source_file, 4096,
        || { polls += 1; false }).unwrap();
    prepare_links(&readers, origin);
    assert!(polls > 1);
    // Leave this live origin's receipt at the oldest retained position. Failed
    // destination admission must not consume another position and evict it.
    for _ in 1..fcb_app::host::catalog_session::MAX_CATALOG_READER_RECEIPTS {
        let filler = readers.create().unwrap();
        catalogs.open_reader(handle, &readers, filler, 1, target_file, 4096, || false).unwrap();
        readers.close(filler).unwrap();
    }
    let rejected = readers.create().unwrap(); let mut rejection_polls = 0;
    let result = catalogs.open_reader(handle, &readers, rejected, 1, source_file, 4096, || {
        rejection_polls += 1;
        rejection_polls == polls // initialize_prepared's final pre-install poll.
    });
    assert!(result.err().unwrap().canceled());
    assert_eq!(rejection_polls, polls);
    assert!(matches!(readers.execute(rejected, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    // Same owner, request, raw label and digest do not upgrade a later supplied
    // reader when the catalog's actual candidate was never installed.
    supplied_reader(&readers, rejected, "README.md", source_bytes);
    prepare_links(&readers, rejected);
    let destination = readers.create().unwrap();
    assert!(matches!(catalogs.open_document_link(handle, &readers, rejected, destination,
        1, source_file, 1, 1, 4096, || false),
        Err(AccessError::Catalog(CatalogSessionError::StaleReaderReceipt))));
    catalogs.open_document_link(handle, &readers, origin, destination,
        1, source_file, 1, 1, 4096, || false).unwrap();
    readers.close(destination).unwrap(); readers.close(rejected).unwrap();
    readers.close(origin).unwrap(); catalogs.close(handle).unwrap();
}

#[test]
fn linked_capture_receipts_follow_installation_across_both_final_cancellation_boundaries() {
    let root = root();
    fs::write(root.join("README.md"), b"# Origin\n\n[Next](next.md)\n").unwrap();
    let target_bytes = b"# Target\n\n[Back](README.md)\n";
    fs::write(root.join("next.md"), target_bytes).unwrap();
    let catalogs = CatalogSessions::new(); let handle = opened(&catalogs, &root);
    let page = catalogs.execute(handle, Command::Page { page: 1 }, || false).unwrap();
    let source_file = named_file(page.as_str(), b"README.md");
    let target_file = named_file(page.as_str(), b"next.md");
    let readers = ReaderSessions::new(); let origin = readers.create().unwrap();
    catalogs.open_reader(handle, &readers, origin, 1, source_file, 4096, || false).unwrap();
    prepare_links(&readers, origin);
    let measured = readers.create().unwrap(); let mut polls = 0;
    catalogs.open_document_link(handle, &readers, origin, measured,
        1, source_file, 1, 1, 4096, || { polls += 1; false }).unwrap();
    readers.close(measured).unwrap();
    assert!(polls > 2);

    let rejected = readers.create().unwrap(); let mut rejection_polls = 0;
    let result = catalogs.open_document_link(handle, &readers, origin, rejected,
        1, source_file, 1, 1, 4096, || {
            rejection_polls += 1;
            rejection_polls == polls - 1 // pre-install, before with_retained's final poll.
        });
    assert!(result.err().unwrap().canceled());
    assert!(matches!(readers.execute(rejected, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    supplied_reader(&readers, rejected, "next.md", target_bytes);
    prepare_links(&readers, rejected);
    let back = readers.create().unwrap();
    assert!(matches!(catalogs.open_document_link(handle, &readers, rejected, back,
        1, target_file, 1, 1, 4096, || false),
        Err(AccessError::Catalog(CatalogSessionError::StaleReaderReceipt))));
    readers.close(back).unwrap(); readers.close(rejected).unwrap();

    let accepted = readers.create().unwrap(); let mut accepted_polls = 0;
    let result = catalogs.open_document_link(handle, &readers, origin, accepted,
        1, source_file, 1, 1, 4096, || {
            accepted_polls += 1;
            accepted_polls == polls // with_retained's post-install delivery poll.
        });
    assert!(result.err().unwrap().canceled());
    assert_eq!(accepted_polls, polls);
    assert!(readers.execute(accepted, ReaderCommand::Info, || false).is_ok());
    prepare_links(&readers, accepted);
    let back = readers.create().unwrap();
    // Delivery suppression after acceptance keeps the installed capture's
    // valid receipt, so recovery can still navigate from its retained source.
    catalogs.open_document_link(handle, &readers, accepted, back,
        1, target_file, 1, 1, 4096, || false).unwrap();
    readers.close(back).unwrap(); readers.close(accepted).unwrap();
    readers.close(origin).unwrap(); catalogs.close(handle).unwrap();
}
