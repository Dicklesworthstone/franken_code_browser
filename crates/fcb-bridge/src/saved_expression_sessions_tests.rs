#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use std::{fs, io::{Seek, SeekFrom, Write}, path::PathBuf};

struct Fixture { path: PathBuf, source_at: u64 }
fn fixture(source: &[u8]) -> Fixture {
    let root = std::env::temp_dir().join(format!("fcb-expression-bridge-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(8991).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries = [
        SnapshotEntry { path: b"src/a.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source) },
        SnapshotEntry { path: b"src/b.rs", observed_bytes: 24, data: SnapshotData::Captured(b"needle required excluded") },
        SnapshotEntry { path: b"src/c.py", observed_bytes: 15, data: SnapshotData::Captured(b"needle required") },
        SnapshotEntry { path: b"tests/d.rs", observed_bytes: 15, data: SnapshotData::Captured(b"needle required") },
    ];
    let encoded = SnapshotBytes::encode(owner, true, "test", &entries, Default::default(), &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let source_at = encoded.bytes().windows(source.len()).position(|bytes| bytes == source).unwrap() as u64;
    let path = root.join("source.fcbs"); fs::write(&path, encoded.bytes()).unwrap();
    Fixture { path, source_at }
}
fn open(table: &SavedSessions, f: &Fixture) -> u64 {
    let handle = table.create().unwrap();
    table.open(handle, &f.path, Default::default(), || false).unwrap(); handle
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn search(table: &SavedSessions, handle: u64, generation: u64, query: &str) -> String {
    table.execute(handle, Command::ExpressionSearch { generation, query, max_hits: 10,
        max_scan_bytes: 1024 * 1024 }, || false).unwrap().as_str().to_owned()
}
fn page(table: &SavedSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::ExpressionResults { generation, start: 0, limit: 10 }, || false)
        .unwrap().as_str().to_owned()
}
const QUERY: &str = "needle required -excluded path:src/ lang:rust";
const SOURCE: &[u8] = b"needle\nrequired\nfn original() {}\n";

#[test]
fn expression_scopes_and_whole_member_predicates_do_not_replace_literal_rows() {
    let f = fixture(SOURCE); let table = SavedSessions::new(); let handle = open(&table, &f);
    let initial = table.execute(handle, Command::Info, || false).unwrap();
    assert!(initial.as_str().contains("\"accepted_expression_generation\":null"));
    assert_eq!(number(initial.as_str(), "last_expression_attempt"), 0);
    let literal = table.execute(handle, Command::Search { generation: 1, needle: "needle", limit: 10 }, || false).unwrap();
    assert_eq!(number(literal.as_str(), "retained_hits"), 4);
    let expression = search(&table, handle, 1, QUERY);
    assert_eq!(number(&expression, "retained_hits"), 1);
    assert!(expression.contains("\"selection_namespace\":\"saved-expression\""));
    assert!(expression.contains("\"predicate_scope\":\"whole-captured-member\""));
    assert!(expression.contains("\"index_used\":false"));
    assert_eq!(number(&expression, "predicate_rejections"), 1);
    let before = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "accepted_query_generation"), 1);
    assert_eq!(number(before.as_str(), "accepted_expression_generation"), 1);
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    let after = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    table.execute(handle, Command::ClearExpression { generation: 2 }, || false).unwrap();
    assert!(table.execute(handle, Command::ExpressionResults { generation: 1, start: 0, limit: 10 }, || false).is_err());
    let literal = table.execute(handle, Command::Results { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    assert_eq!(number(literal.as_str(), "retained_hits"), 4);
    table.close(handle).unwrap();
}

#[test]
fn failed_and_canceled_attempts_retire_generations_without_erasing_accepted_expression() {
    let f = fixture(SOURCE); let table = SavedSessions::new(); let handle = open(&table, &f);
    search(&table, handle, 1, QUERY);
    assert!(table.execute(handle, Command::ExpressionSearch { generation: 2, query: "\"unterminated",
        max_hits: 10, max_scan_bytes: 1024 }, || false).is_err());
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    assert!(table.execute(handle, Command::ExpressionSearch { generation: 2, query: QUERY,
        max_hits: 10, max_scan_bytes: 1024 }, || false).is_err());
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "accepted_expression_generation"), 1);
    assert_eq!(number(info.as_str(), "last_expression_attempt"), 2);
    let mut polls = 0;
    let canceled = table.execute(handle, Command::ExpressionSearch { generation: 3, query: QUERY,
        max_hits: 10, max_scan_bytes: 1024 }, || {
        polls += 1; if polls == 4 { table.cancel(handle).unwrap(); } false
    });
    assert_eq!(canceled.err(), Some(AccessError::Canceled));
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "last_expression_attempt"), 3);
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    assert_eq!(number(&search(&table, handle, 4, QUERY), "retained_hits"), 1);
    table.close(handle).unwrap();
}

#[test]
fn work_and_result_caps_remain_explicit_and_do_not_invent_complete_counts() {
    let source = b"needle needle needle needle required";
    let f = fixture(source); let table = SavedSessions::new(); let handle = open(&table, &f);
    let work = table.execute(handle, Command::ExpressionSearch { generation: 1, query: QUERY,
        max_hits: 10, max_scan_bytes: 1 }, || false).unwrap();
    assert!(work.as_str().contains("\"state\":\"work-limit\""));
    assert!(work.as_str().contains("\"complete\":false"));
    assert!(work.as_str().contains("\"matches_seen_exact\":false"));
    let limited = table.execute(handle, Command::ExpressionSearch { generation: 2, query: QUERY,
        max_hits: 2, max_scan_bytes: 1024 }, || false).unwrap();
    assert!(limited.as_str().contains("\"state\":\"truncated\""));
    assert_eq!(number(limited.as_str(), "retained_hits"), 2);
    let second = table.execute(handle, Command::ExpressionResults { generation: 2, start: 1, limit: 1 }, || false).unwrap();
    assert!(second.as_str().contains("\"hit_id\":\"2\""));
    assert!(!second.as_str().contains("\"hit_id\":\"1\""));
    table.close(handle).unwrap();
}

#[test]
fn expression_reader_owns_exact_source_after_query_and_archive_retirement() {
    let f = fixture(SOURCE); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); search(&table, handle, 1, QUERY);
    let reader = readers.create().unwrap();
    let mut signaled = false;
    assert!(table.open_reader(handle, &readers, reader, Selection::ExpressionHit { generation: 1, id: 1 }, || {
        if !signaled { readers.cancel(reader).unwrap(); signaled = true; } false
    }).is_err());
    assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    let receipt = table.open_reader(handle, &readers, reader,
        Selection::ExpressionHit { generation: 1, id: 1 }, || false).unwrap();
    assert!(receipt.as_str().contains("\"command\":\"expression-open-reader\""));
    assert!(receipt.as_str().contains("\"selection_namespace\":\"saved-expression\""));
    assert!(receipt.as_str().contains("\"primary_needle\":\"needle\""));
    assert_eq!(number(receipt.as_str(), "reader_owner"), reader);
    table.execute(handle, Command::ClearExpression { generation: 2 }, || false).unwrap();
    table.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 6 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    readers.close(reader).unwrap();
}

#[test]
fn stale_hits_and_busy_destinations_cannot_read_or_replace_retained_sources() {
    let f = fixture(SOURCE); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &f); search(&table, handle, 1, QUERY);
    let reader = readers.create().unwrap();
    table.open_reader(handle, &readers, reader, Selection::Member(0), || false).unwrap();
    let before = table.execute(handle, Command::Info, || false).unwrap();
    assert!(table.open_reader(handle, &readers, reader, Selection::ExpressionHit { generation: 1, id: 1 }, || false).is_err());
    assert!(table.open_reader(handle, &readers, u64::MAX, Selection::ExpressionHit { generation: 1, id: 1 }, || false).is_err());
    let after = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    let empty = readers.create().unwrap(); search(&table, handle, 2, QUERY);
    assert!(table.open_reader(handle, &readers, empty, Selection::ExpressionHit { generation: 1, id: 1 }, || false).is_err());
    let mut writer = fs::OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(f.source_at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(table.open_reader(handle, &readers, empty, Selection::ExpressionHit { generation: 2, id: 1 }, || false).is_err());
    assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert_eq!(number(&page(&table, handle, 2), "retained_hits"), 1);
    readers.close(empty).unwrap(); readers.close(reader).unwrap(); table.close(handle).unwrap();
}
