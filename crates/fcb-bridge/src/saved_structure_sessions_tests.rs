#![forbid(unsafe_code)]

use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry};
use std::{fs, io::{Seek, SeekFrom, Write}, path::PathBuf};

const SOURCE: &[u8] = b"fn needle() { needle(); }\n";
const DOCUMENT: &[u8] = b"# Guide\n\n## Needle\n";
const SECOND: &[u8] = b"fn needle_extra() {}\n";

struct Fixture { path: PathBuf, source_at: u64 }
fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("fcb-structure-bridge-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    let owner = ArenaOwnerId::new(8993).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let entries = [
        SnapshotEntry { path: b"docs/guide.md", observed_bytes: DOCUMENT.len() as u64,
            data: SnapshotData::Captured(DOCUMENT) },
        SnapshotEntry { path: b"src/main.rs", observed_bytes: SOURCE.len() as u64,
            data: SnapshotData::Captured(SOURCE) },
        SnapshotEntry { path: b"src/second.rs", observed_bytes: SECOND.len() as u64,
            data: SnapshotData::Captured(SECOND) },
    ];
    let encoded = SnapshotBytes::encode(owner, true, "structure-test", &entries, Default::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let source_at = encoded.bytes().windows(SOURCE.len()).position(|bytes| bytes == SOURCE).unwrap() as u64;
    let path = root.join("source.fcbs"); fs::write(&path, encoded.bytes()).unwrap();
    Fixture { path, source_at }
}
fn open(table: &SavedSessions, fixture: &Fixture) -> u64 {
    let handle = table.create().unwrap();
    table.open(handle, &fixture.path, Default::default(), || false).unwrap(); handle
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn search(table: &SavedSessions, handle: u64, generation: u64,
    mode: StructureMode, name_mode: SymbolNameMode, needle: &str) -> String {
    table.execute(handle, Command::StructureSearch { generation, mode, name_mode, needle,
        max_hits: 10, max_source_bytes: 1024 * 1024 }, || false).unwrap().as_str().to_owned()
}
fn page(table: &SavedSessions, handle: u64, generation: u64) -> String {
    table.execute(handle, Command::StructureResults { generation, start: 0, limit: 10 }, || false)
        .unwrap().as_str().to_owned()
}

#[test]
fn structural_modes_and_metadata_pages_preserve_other_saved_searches() {
    let fixture = fixture(); let table = SavedSessions::new(); let handle = open(&table, &fixture);
    let initial = table.execute(handle, Command::Info, || false).unwrap();
    assert!(initial.as_str().contains("\"accepted_structure_generation\":null"));
    assert_eq!(number(initial.as_str(), "last_structure_attempt"), 0);
    table.execute(handle, Command::Search { generation: 1, needle: "needle", limit: 10 }, || false).unwrap();
    table.execute(handle, Command::ExpressionSearch { generation: 1, query: "needle path:src/",
        max_hits: 10, max_scan_bytes: 4096 }, || false).unwrap();
    let symbols = search(&table, handle, 1, StructureMode::Symbols, SymbolNameMode::Exact, "needle");
    assert_eq!(number(&symbols, "retained_hits"), 1);
    assert!(symbols.contains("\"selection_namespace\":\"saved-structure\""));
    assert!(symbols.contains("\"requested_mode\":\"symbols\""));
    assert!(symbols.contains("\"semantic_complete\":false"));
    let before = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 1);
    let after = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    let prefix = search(&table, handle, 2, StructureMode::Symbols, SymbolNameMode::Prefix, "needle");
    assert_eq!(number(&prefix, "retained_hits"), 2);
    let heading = search(&table, handle, 3, StructureMode::Headings, SymbolNameMode::Exact, "Guide");
    assert_eq!(number(&heading, "retained_hits"), 1);
    assert!(heading.contains("\"requested_mode\":\"headings\""));
    assert!(heading.contains("\"heading_slug\":\"guide\""));
    let occurrences = search(&table, handle, 4, StructureMode::Occurrences, SymbolNameMode::Exact, "needle");
    assert_eq!(number(&occurrences, "retained_hits"), 2);
    assert!(occurrences.contains("\"requested_mode\":\"occurrences\""));
    table.execute(handle, Command::ClearStructure { generation: 5 }, || false).unwrap();
    assert!(table.execute(handle, Command::StructureResults { generation: 4, start: 0, limit: 10 }, || false).is_err());
    assert!(table.execute(handle, Command::Results { generation: 1, start: 0, limit: 10 }, || false).is_ok());
    assert!(table.execute(handle, Command::ExpressionResults { generation: 1, start: 0, limit: 10 }, || false).is_ok());
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "accepted_query_generation"), 1);
    assert_eq!(number(info.as_str(), "accepted_expression_generation"), 1);
    assert_eq!(number(info.as_str(), "last_structure_attempt"), 5);
    assert!(info.as_str().contains("\"accepted_structure_generation\":null"));
    table.close(handle).unwrap();
}

#[test]
fn failed_and_canceled_structure_attempts_preserve_accepted_rows() {
    let fixture = fixture(); let table = SavedSessions::new(); let handle = open(&table, &fixture);
    search(&table, handle, 1, StructureMode::Occurrences, SymbolNameMode::Exact, "needle");
    assert!(table.execute(handle, Command::StructureSearch { generation: 2, mode: StructureMode::Occurrences,
        name_mode: SymbolNameMode::Prefix, needle: "needle", max_hits: 10, max_source_bytes: 4096 }, || false).is_err());
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 2);
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "last_structure_attempt"), 2);
    assert_eq!(number(info.as_str(), "accepted_structure_generation"), 1);
    assert!(table.execute(handle, Command::StructureSearch { generation: 2, mode: StructureMode::Occurrences,
        name_mode: SymbolNameMode::Exact, needle: "needle", max_hits: 10, max_source_bytes: 4096 }, || false).is_err());
    let mut polls = 0;
    let canceled = table.execute(handle, Command::StructureSearch { generation: 3, mode: StructureMode::Occurrences,
        name_mode: SymbolNameMode::Exact, needle: "needle", max_hits: 10, max_source_bytes: 4096 }, || {
        polls += 1; if polls == 4 { table.cancel(handle).unwrap(); } false
    });
    assert_eq!(canceled.err(), Some(AccessError::Canceled));
    let info = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(info.as_str(), "last_structure_attempt"), 3);
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 2);
    assert_eq!(number(&search(&table, handle, 4, StructureMode::Occurrences, SymbolNameMode::Exact, "needle"), "retained_hits"), 2);
    table.close(handle).unwrap();
}

#[test]
fn structural_work_and_hit_caps_publish_partial_results_and_stable_ids() {
    let fixture = fixture(); let table = SavedSessions::new(); let handle = open(&table, &fixture);
    let bounded = table.execute(handle, Command::StructureSearch { generation: 1, mode: StructureMode::Occurrences,
        name_mode: SymbolNameMode::Exact, needle: "needle", max_hits: 10, max_source_bytes: 1 }, || false).unwrap();
    assert!(bounded.as_str().contains("\"state\":\"work-limit\""));
    assert!(bounded.as_str().contains("\"complete\":false"));
    assert!(bounded.as_str().contains("\"matches_seen_exact\":false"));
    let limited = table.execute(handle, Command::StructureSearch { generation: 2, mode: StructureMode::Occurrences,
        name_mode: SymbolNameMode::Exact, needle: "needle", max_hits: 1, max_source_bytes: 4096 }, || false).unwrap();
    assert_eq!(number(limited.as_str(), "retained_hits"), 1);
    assert!(limited.as_str().contains("\"truncated\":true"));
    search(&table, handle, 3, StructureMode::Occurrences, SymbolNameMode::Exact, "needle");
    let second = table.execute(handle, Command::StructureResults { generation: 3, start: 1, limit: 1 }, || false).unwrap();
    assert!(second.as_str().contains("\"hit_id\":\"2\""));
    assert!(!second.as_str().contains("\"hit_id\":\"1\""));
    table.close(handle).unwrap();
}

#[test]
fn structural_reader_cancellation_and_retirement_keep_source_ownership_separate() {
    let fixture = fixture(); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &fixture);
    search(&table, handle, 1, StructureMode::Occurrences, SymbolNameMode::Exact, "needle");
    let reader = readers.create().unwrap(); let mut signaled = false;
    assert!(table.open_reader(handle, &readers, reader, Selection::StructureHit { generation: 1, id: 1 }, || {
        if !signaled { readers.cancel(reader).unwrap(); signaled = true; } false
    }).is_err());
    assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert_eq!(number(&page(&table, handle, 1), "retained_hits"), 2);
    let receipt = table.open_reader(handle, &readers, reader, Selection::StructureHit { generation: 1, id: 1 }, || false).unwrap();
    assert!(receipt.as_str().contains("\"command\":\"structure-open-reader\""));
    assert!(receipt.as_str().contains("\"selection_namespace\":\"saved-structure\""));
    assert!(receipt.as_str().contains("\"structure_generation\":\"1\""));
    assert_eq!(number(receipt.as_str(), "reader_owner"), reader);
    table.execute(handle, Command::ClearStructure { generation: 2 }, || false).unwrap();
    table.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyRange { start: 3, end: 9 }, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    readers.close(reader).unwrap();
}

#[test]
fn structural_reader_admission_and_archive_corruption_never_replace_source() {
    let fixture = fixture(); let table = SavedSessions::new(); let readers = ReaderSessions::new();
    let handle = open(&table, &fixture);
    search(&table, handle, 1, StructureMode::Symbols, SymbolNameMode::Exact, "needle");
    let reader = readers.create().unwrap();
    table.open_reader(handle, &readers, reader, Selection::Member(1), || false).unwrap();
    let before = table.execute(handle, Command::Info, || false).unwrap();
    assert!(table.open_reader(handle, &readers, reader, Selection::StructureHit { generation: 1, id: 1 }, || false).is_err());
    assert!(table.open_reader(handle, &readers, u64::MAX, Selection::StructureHit { generation: 1, id: 1 }, || false).is_err());
    let after = table.execute(handle, Command::Info, || false).unwrap();
    assert_eq!(number(before.as_str(), "member_bytes_read"), number(after.as_str(), "member_bytes_read"));
    let empty = readers.create().unwrap();
    search(&table, handle, 2, StructureMode::Symbols, SymbolNameMode::Exact, "needle");
    assert!(table.open_reader(handle, &readers, empty, Selection::StructureHit { generation: 1, id: 1 }, || false).is_err());
    let mut writer = fs::OpenOptions::new().write(true).open(&fixture.path).unwrap();
    writer.seek(SeekFrom::Start(fixture.source_at)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(table.open_reader(handle, &readers, empty, Selection::StructureHit { generation: 2, id: 1 }, || false).is_err());
    assert_eq!(readers.execute(empty, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    assert_eq!(number(&page(&table, handle, 2), "retained_hits"), 1);
    readers.close(empty).unwrap(); readers.close(reader).unwrap(); table.close(handle).unwrap();
}
