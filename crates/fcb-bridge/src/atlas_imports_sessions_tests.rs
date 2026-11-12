#![forbid(unsafe_code)]

//! Real captured-source integration through the shared atlas and reader
//! registries. Lexical import evidence and catalog candidates stay distinct.
use super::*;
use crate::reader_sessions::Command as ReaderCommand;
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasRelationshipDirection};
use std::{fs, path::PathBuf};

const DOC: &[u8] = b"[source](src/lib.rs)\n";
const SOURCE: &[u8] = b"// module evidence\nmod child;\nuse crate::child::Child;\n";
const CHILD: &[u8] = b"pub struct Child;\n";
fn open(table: &AtlasSessions, files: &[(&str, &[u8])]) -> (u64, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-atlas-imports-registry-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    for (name, bytes) in files {
        let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let handle = table.create().unwrap();
    table.open(handle, &root, AtlasSessionOptions::default(), || false).unwrap();
    table.execute_index(handle, IndexCommand::Prepare { generation: 1, options: AtlasIndexOptions {
        max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536, max_index_grams: 65536,
    } }, || false).unwrap();
    (handle, root)
}
fn standard(table: &AtlasSessions) -> (u64, PathBuf) {
    open(table, &[("README.md", DOC), ("src/lib.rs", SOURCE), ("src/child.rs", CHILD)])
}
fn begin(table: &AtlasSessions, handle: u64, generation: u64) -> HostResponse {
    table.execute_imports(handle, ImportsCommand::Begin { generation, index_generation: 1,
        max_imports: 100, max_parse_bytes: 65536 }, || false).unwrap()
}
fn finish(table: &AtlasSessions, handle: u64, generation: u64) -> String {
    for _ in 0..100 {
        let step = table.execute_imports(handle, ImportsCommand::Step { generation }, || false).unwrap();
        if step.as_str().contains("\"relationships_in_progress\":false") { return step.as_str().to_owned(); }
    }
    panic!("bounded import inventory did not finish");
}
fn page(table: &AtlasSessions, handle: u64, generation: u64,
    direction: AtlasImportDirection, path: Option<&[u8]>) -> Result<HostResponse, AccessError> {
    table.execute_imports(handle, ImportsCommand::Page { generation, direction,
        relative_path: path, start: 0, limit: 100 }, || false)
}
fn documentation(table: &AtlasSessions, handle: u64, generation: u64) {
    table.execute_links(handle, LinksCommand::Begin { generation, index_generation: 1,
        max_links: 100, max_parse_bytes: 65536 }, || false).unwrap();
    for _ in 0..100 {
        let step = table.execute_links(handle, LinksCommand::Step { generation }, || false).unwrap();
        if step.as_str().contains("\"relationships_in_progress\":false") { return; }
    }
    panic!("bounded documentation inventory did not finish");
}
fn number(text: &str, key: &str) -> u64 {
    text.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn import_navigation_retains_exact_statement_and_target_after_working_tree_edits() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let (handle, root) = standard(&table);
    begin(&table, handle, 1);
    let result = finish(&table, handle, 1);
    assert_eq!(number(&result, "retained_links"), 2);
    assert!(result.contains("\"relationship_kind\":\"lexical-import\"") && result.contains("\"compiler_resolved\":false"));
    let incoming = page(&table, handle, 1, AtlasImportDirection::Incoming, Some(b"src/child.rs")).unwrap();
    assert_eq!(incoming.as_str().matches("\"link_id\":").count(), 1);
    let id = number(incoming.as_str(), "link_id");
    fs::write(root.join("src/lib.rs"), b"mod changed;\n").unwrap();
    fs::write(root.join("src/child.rs"), b"pub struct Changed;\n").unwrap();
    let evidence = readers.create().unwrap(); let target = readers.create().unwrap();
    let offset = SOURCE.windows(b"mod child;".len()).position(|w| w == b"mod child;").unwrap() as u64;
    for (reader, side, expected) in [(evidence, AtlasImportSide::Evidence, offset), (target, AtlasImportSide::Target, 0)] {
        let receipt = table.open_import_reader(handle, &readers, reader, 1, id, side, || false).unwrap();
        assert_eq!(number(receipt.as_str(), "initial_original_offset"), expected);
        assert!(receipt.as_str().contains("\"source_reopened\":false"));
        assert!(receipt.as_str().contains("\"relationship\":{"));
        assert!(receipt.as_str().contains(&format!("\"evidence_range\":{{\"start\":\"{offset}\",\"end\":\"{}\"}}", offset + 10)));
    }
    table.execute_index(handle, IndexCommand::Clear { generation: 2 }, || false).unwrap();
    assert!(page(&table, handle, 1, AtlasImportDirection::All, None).is_err());
    table.close(handle).unwrap();
    for (reader, bytes) in [(evidence, SOURCE), (target, CHILD)] {
        let retained = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: bytes.len() as u64 }, || false).unwrap();
        assert!(retained.as_str().contains(&hex(bytes)));
        readers.close(reader).unwrap();
    }
}

#[test]
fn utf16_import_evidence_uses_original_byte_offsets_through_the_reader_registry() {
    for little_endian in [true, false] {
        let table = AtlasSessions::new(); let readers = ReaderSessions::new();
        let prefix = "// π😀\n"; let text = format!("{prefix}mod child;\n");
        let mut bytes = if little_endian { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() {
            let encoded = if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() };
            bytes.extend_from_slice(&encoded);
        }
        let (handle, root) = open(&table, &[("src/lib.rs", bytes.as_slice()), ("src/child.rs", CHILD)]);
        begin(&table, handle, 1); finish(&table, handle, 1);
        let incoming = page(&table, handle, 1, AtlasImportDirection::Incoming, Some(b"src/child.rs")).unwrap();
        assert_eq!(number(incoming.as_str(), "matching_links"), 1);
        let id = number(incoming.as_str(), "link_id");
        let start = 2 + prefix.encode_utf16().count() as u64 * 2;
        let end = start + "mod child;".encode_utf16().count() as u64 * 2;
        fs::write(root.join("src/lib.rs"), b"mod changed;\n").unwrap();
        let reader = readers.create().unwrap();
        let receipt = table.open_import_reader(handle, &readers, reader, 1, id, AtlasImportSide::Evidence, || false).unwrap();
        assert_eq!(number(receipt.as_str(), "initial_original_offset"), start);
        assert!(receipt.as_str().contains(if little_endian { "\"encoding\":\"utf16le\"" } else { "\"encoding\":\"utf16be\"" }));
        let selected = readers.execute(reader, ReaderCommand::CopyRange { start, end }, || false).unwrap();
        assert!(selected.as_str().contains(&hex(&bytes[start as usize..end as usize])));
        readers.close(reader).unwrap(); table.close(handle).unwrap();
    }
}

#[test]
fn javascript_imports_keep_exact_occurrences_and_catalog_candidates_without_package_execution() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let source = b"// importing\nimport { Child } from './child.js';\nexport { Child } from './child.js';\nimport remote from 'external-package';\n";
    let child = b"export class Child {}\n";
    let (handle, root) = open(&table, &[("src/main.js", source), ("src/child.js", child)]);
    begin(&table, handle, 1); finish(&table, handle, 1);
    let incoming = page(&table, handle, 1, AtlasImportDirection::Incoming, Some(b"src/child.js")).unwrap();
    assert_eq!(number(incoming.as_str(), "matching_links"), 2);
    assert!(incoming.as_str().contains("\"lexical_import_kind\":\"javascript-import\""));
    assert!(incoming.as_str().contains("\"lexical_import_kind\":\"javascript-export\""));
    assert!(incoming.as_str().contains("\"resolution_basis\":\"exact-relative-path\""));
    let outgoing = page(&table, handle, 1, AtlasImportDirection::Outgoing, Some(b"src/main.js")).unwrap();
    assert_eq!(number(outgoing.as_str(), "matching_links"), 3);
    assert!(outgoing.as_str().contains("\"resolution\":\"external\""));
    let id = number(incoming.as_str(), "link_id");
    fs::write(root.join("src/child.js"), b"export class Changed {}\n").unwrap();
    let reader = readers.create().unwrap();
    table.open_import_reader(handle, &readers, reader, 1, id, AtlasImportSide::Target, || false).unwrap();
    table.close(handle).unwrap();
    let retained = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: child.len() as u64 }, || false).unwrap();
    assert!(retained.as_str().contains(&hex(child)));
    readers.close(reader).unwrap();
}

#[test]
fn markdown_import_and_search_generations_remain_independent_across_cancellation() {
    let table = AtlasSessions::new(); let (handle, _) = standard(&table);
    begin(&table, handle, 1); finish(&table, handle, 1); documentation(&table, handle, 1);
    table.execute(handle, Command::Search { generation: 2, needle: "Child",
        options: AtlasSearchOptions { max_matches: 10, max_files: 100, max_file_bytes: 8192, max_source_bytes: 65536 } },
        || false).unwrap();
    begin(&table, handle, 2);
    table.execute_imports(handle, ImportsCommand::Step { generation: 2 }, || false).unwrap();
    table.execute_links(handle, LinksCommand::Begin { generation: 2, index_generation: 1,
        max_links: 100, max_parse_bytes: 65536 }, || false).unwrap();
    table.cancel(handle).unwrap();
    table.execute_index(handle, IndexCommand::Info, || false).unwrap();
    let cell = table.get(handle).unwrap(); let state = lock(&cell.state).unwrap();
    let session = state.as_ref().unwrap();
    assert_eq!(session.search.pending_import_generation(), None);
    assert_eq!(session.search.pending_relationship_generation(), None);
    assert_eq!(session.search.import_generation(), Some(1));
    assert_eq!(session.search.relationship_generation(), Some(1));
    drop(state); drop(cell);
    assert!(table.execute_imports(handle, ImportsCommand::Step { generation: 2 }, || false).is_err());
    assert_eq!(number(page(&table, handle, 1, AtlasImportDirection::Incoming, Some(b"src/child.rs")).unwrap().as_str(), "matching_links"), 1);
    assert!(table.execute_imports(handle, ImportsCommand::Begin { generation: 2, index_generation: 1,
        max_imports: 100, max_parse_bytes: 65536 }, || false).is_err());
    assert!(table.execute_imports(handle, ImportsCommand::Begin { generation: 3, index_generation: 999,
        max_imports: 100, max_parse_bytes: 65536 }, || false).is_err());
    assert!(page(&table, handle, 1, AtlasImportDirection::All, None).is_ok());
    begin(&table, handle, 4); finish(&table, handle, 4);
    assert!(page(&table, handle, 1, AtlasImportDirection::All, None).is_err());
    let docs = table.execute_links(handle, LinksCommand::Page { generation: 1, direction: AtlasRelationshipDirection::Incoming,
        relative_path: Some(b"src/lib.rs"), start: 0, limit: 100 }, || false).unwrap();
    assert_eq!(number(docs.as_str(), "matching_links"), 1);
    assert!(table.execute(handle, Command::SearchPage { generation: 2, start: 0, limit: 10 }, || false).is_ok());
    table.close(handle).unwrap();
}

#[test]
fn unresolved_imports_are_readable_evidence_and_occupied_receivers_preserve_their_source() {
    let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let (handle, _) = standard(&table); begin(&table, handle, 1); finish(&table, handle, 1);
    let reader = readers.create().unwrap();
    for id in [0, 2, 999] {
        assert!(table.open_import_reader(handle, &readers, reader, 1, id, AtlasImportSide::Target, || false).is_err());
        assert_eq!(readers.execute(reader, ReaderCommand::Info, || false).err(), Some(reader_sessions::AccessError::NotOpen));
    }
    let receipt = table.open_import_reader(handle, &readers, reader, 1, 2, AtlasImportSide::Evidence, || false).unwrap();
    assert!(receipt.as_str().contains("\"resolution_basis\":\"unresolved-rust-use\""));
    assert_eq!(table.open_import_reader(handle, &readers, reader, 1, 1, AtlasImportSide::Target, || false).err(),
        Some(AccessError::Reader(reader_sessions::AccessError::AlreadyOpen)));
    let retained = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: SOURCE.len() as u64 }, || false).unwrap();
    assert!(retained.as_str().contains(&hex(SOURCE)));
    readers.close(reader).unwrap(); table.close(handle).unwrap();
}

#[test]
fn zero_parse_allowance_is_partial_and_retry_can_find_the_import() {
    let table = AtlasSessions::new(); let (handle, _) = standard(&table);
    table.execute_imports(handle, ImportsCommand::Begin { generation: 1, index_generation: 1,
        max_imports: 100, max_parse_bytes: 0 }, || false).unwrap();
    let partial = finish(&table, handle, 1);
    assert!(partial.contains("\"relationships_complete\":false"));
    assert_eq!(number(&partial, "retained_links"), 0);
    let incoming = page(&table, handle, 1, AtlasImportDirection::Incoming, Some(b"src/child.rs")).unwrap();
    assert!(incoming.as_str().contains("\"relationships_complete\":false"));
    begin(&table, handle, 2); finish(&table, handle, 2);
    assert_eq!(number(page(&table, handle, 2, AtlasImportDirection::Incoming, Some(b"src/child.rs")).unwrap().as_str(), "matching_links"), 1);
    table.close(handle).unwrap();
}

#[test]
fn busy_import_worker_isolated_from_other_atlases_and_cancellation() {
    let table = AtlasSessions::new();
    let (a, _) = standard(&table); let (b, _) = standard(&table);
    begin(&table, a, 1); begin(&table, b, 1);
    let cell = table.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(table.execute_imports(a, ImportsCommand::Step { generation: 1 }, || false).err(), Some(AccessError::Busy));
    assert_eq!(number(&finish(&table, b, 1), "retained_links"), 2);
    table.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(table.execute_imports(a, ImportsCommand::Step { generation: 1 }, || false).is_err());
    table.close(a).unwrap(); table.close(b).unwrap();
}

#[test]
fn close_during_import_admission_keeps_capacity_reserved_until_worker_drain() {
    let table = AtlasSessions::new(); let (handle, _) = standard(&table);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| table.create().unwrap()).collect();
    let mut closed = false;
    let result = table.execute_imports(handle, ImportsCommand::Begin { generation: 1, index_generation: 1,
        max_imports: 100, max_parse_bytes: 65536 }, || {
        if !closed {
            closed = true; table.close(handle).unwrap();
            assert_eq!(table.create().err(), Some(AccessError::Capacity));
        }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    let replacement = table.create().unwrap(); assert_ne!(replacement, handle);
    table.close(replacement).unwrap(); for handle in others { table.close(handle).unwrap(); }
}
