#![forbid(unsafe_code)]

//! Real host/registry expression workflows, sharing existing query lifetimes.
use super::*;
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchPathKind};
use crate::reader_sessions::Command as ReaderCommand;
use std::{fs, path::PathBuf};

fn open(table: &AtlasSessions, files: &[(&str, &[u8])]) -> (u64, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-atlas-expression-registry-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    for (name, bytes) in files {
        let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let handle = table.create().unwrap();
    table.open(handle, &root, AtlasSessionOptions::default(), || false).unwrap();
    (handle, root)
}
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 100, max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536 }
}
fn begin(table: &AtlasSessions, handle: u64, generation: u64, expression: &str) -> HostResponse {
    table.execute(handle, Command::SearchBeginExpression { generation, expression,
        scope: &AtlasSearchScope::all(), options: options(), max_scan_bytes: 65536 }, || false).unwrap()
}
fn finish(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = table.execute(handle, Command::SearchStep { generation }, || false).unwrap();
        if step.as_str().contains("\"search_in_progress\":false") { return step.as_str().to_owned(); }
    }
    panic!("bounded expression did not reach a terminal state");
}
fn page(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::SearchPage { generation, start: 0, limit: 100 }, || false)
        .unwrap().as_str().to_owned()
}

#[test]
fn expression_predicates_and_ui_scope_intersect_before_exact_capture_activation() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let (handle, root) = open(&table, &[
        ("src/a.rs", b"needle required needle"), ("src/b.rs", b"needle required forbidden"),
        ("src/c.rs", b"needle only"), ("docs/d.rs", b"needle required"),
        ("src/e.py", b"needle required"),
    ]);
    let scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let admitted = table.execute(handle, Command::SearchBeginExpression { generation: 1,
        expression: "needle required -forbidden lang:rust path:src/", scope: &scope,
        options: options(), max_scan_bytes: 65536 }, || false).unwrap();
    assert!(admitted.as_str().contains("\"source_bytes_read\":\"0\""));
    assert!(admitted.as_str().contains("\"command\":\"begin-expression\""));
    let result = finish(&table, handle, 1);
    assert!(result.contains("\"retained_hits\":\"2\""), "{result}");
    assert!(result.contains("\"search_complete\":true"));
    assert!(result.contains("\"needle\":\"needle\""));
    assert!(result.contains("\"predicate_scope\":\"same-complete-capture\""));
    fs::write(root.join("src/a.rs"), b"replacement without evidence").unwrap();
    let reader = readers.create().unwrap();
    let selected = table.open_search_reader(handle, &readers, reader, 1, 1, || false).unwrap();
    assert!(selected.as_str().contains("\"source_reopened\":false"));
    table.close(handle).unwrap();
    let exact = readers.execute(reader, ReaderCommand::Window { offset: 0, bytes: 64 }, || false).unwrap();
    assert!(exact.as_str().contains("needle required needle"));
    assert!(!exact.as_str().contains("replacement"));
    readers.close(reader).unwrap();
}

#[test]
fn cancellation_and_parser_failures_keep_accepted_literal_query_and_consume_attempts() {
    let table = AtlasSessions::new();
    let (handle, _) = open(&table, &[("a.rs", b"needle required"), ("b.rs", b"needle other")]);
    table.execute(handle, Command::Search { generation: 1, needle: "needle", options: options() }, || false).unwrap();
    begin(&table, handle, 2, "needle required");
    table.execute(handle, Command::SearchStep { generation: 2 }, || false).unwrap();
    table.cancel(handle).unwrap();
    assert!(table.execute(handle, Command::SearchStep { generation: 2 }, || false).is_err());
    assert!(page(&table, handle, 1).contains("\"retained_hits\":\"2\""));
    let invalid = table.execute(handle, Command::SearchBeginExpression { generation: 3,
        expression: "needle regex:a.*", scope: &AtlasSearchScope::all(),
        options: options(), max_scan_bytes: 65536 }, || false);
    assert!(invalid.err().unwrap().to_string().contains("QUERY_REGEX_UNQUALIFIED"));
    assert!(page(&table, handle, 1).contains("\"retained_hits\":\"2\""));
    assert!(table.execute(handle, Command::SearchBeginExpression { generation: 3,
        expression: "needle required", scope: &AtlasSearchScope::all(),
        options: options(), max_scan_bytes: 65536 }, || false).is_err());
    begin(&table, handle, 4, "needle required");
    assert!(finish(&table, handle, 4).contains("\"retained_hits\":\"1\""));
    table.close(handle).unwrap();
}

#[test]
fn prepared_expression_keeps_utf16_evidence_after_source_change_and_index_clear() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let original: Vec<_> = "\u{feff}needle\r\nrequired\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (handle, root) = open(&table, &[("a.rs", &original), ("b.rs", b"needle forbidden")]);
    table.execute_index(handle, IndexCommand::Prepare { generation: 1, options: AtlasIndexOptions {
        max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536, max_index_grams: 65536,
    } }, || false).unwrap();
    fs::write(root.join("a.rs"), b"new current bytes").unwrap();
    let admitted = table.execute_index(handle, IndexCommand::BeginExpression { generation: 2,
        index_generation: 1, expression: "needle required -forbidden", scope: &AtlasSearchScope::all(),
        max_matches: 100, max_scan_bytes: 65536 }, || false).unwrap();
    assert!(admitted.as_str().contains("\"command\":\"begin-indexed-expression\""));
    let result = finish(&table, handle, 2);
    assert!(result.contains("\"retained_hits\":\"1\""), "{result}");
    assert!(result.contains("\"source_bytes_read\":\"0\""));
    assert!(result.contains("\"original_range\":{\"start\":\"2\",\"end\":\"14\"}"));
    let reader = readers.create().unwrap();
    table.open_search_reader(handle, &readers, reader, 2, 1, || false).unwrap();
    table.execute_index(handle, IndexCommand::Clear { generation: 3 }, || false).unwrap();
    table.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyRange { start: 2, end: 14 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6e006500650064006c006500\""));
    readers.close(reader).unwrap();
}

#[test]
fn zero_verification_work_never_proves_document_exclusion_or_empty_complete_result() {
    let table = AtlasSessions::new(); let (handle, _) = open(&table, &[("a.rs", b"needle required")]);
    table.execute(handle, Command::SearchBeginExpression { generation: 1, expression: "needle -forbidden",
        scope: &AtlasSearchScope::all(), options: options(), max_scan_bytes: 0 }, || false).unwrap();
    let result = finish(&table, handle, 1);
    assert!(result.contains("\"retained_hits\":\"0\""), "{result}");
    assert!(result.contains("\"search_complete\":false"));
    assert!(result.contains("verification-byte-limit"));
    begin(&table, handle, 2, "needle -forbidden");
    assert!(finish(&table, handle, 2).contains("\"retained_hits\":\"1\""));
    table.close(handle).unwrap();
}

#[test]
fn busy_expression_owner_does_not_block_another_owner_or_nonblocking_cancel() {
    let table = AtlasSessions::new();
    let (a, _) = open(&table, &[("a.rs", b"needle required")]);
    let (b, _) = open(&table, &[("b.rs", b"needle required")]);
    begin(&table, a, 1, "needle required"); begin(&table, b, 1, "needle required");
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute(a, Command::SearchStep { generation: 1 }, || false).err(), Some(AccessError::Busy));
    assert!(finish(&table, b, 1).contains("\"retained_hits\":\"1\""));
    table.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(table.execute(a, Command::SearchStep { generation: 1 }, || false).is_err());
    table.close(a).unwrap(); table.close(b).unwrap();
}

#[test]
fn close_during_expression_admission_keeps_capacity_until_worker_returns() {
    let table = AtlasSessions::new(); let (handle, _) = open(&table, &[("a.rs", b"needle required")]);
    let other: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let result = table.execute(handle, Command::SearchBeginExpression { generation: 1,
        expression: "needle required", scope: &AtlasSearchScope::all(), options: options(), max_scan_bytes: 65536 }, || {
        if !closed { closed = true; table.close(handle).unwrap(); assert_eq!(table.create().err(), Some(AccessError::Capacity)); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    let replacement = table.create().unwrap(); assert_ne!(replacement, handle); table.close(replacement).unwrap();
    for handle in other { table.close(handle).unwrap(); }
}
