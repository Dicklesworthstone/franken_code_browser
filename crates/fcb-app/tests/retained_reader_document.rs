#![forbid(unsafe_code)]

use std::path::Path;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::document::{TextSelectionRange, reader::{DocumentReader, DocumentReadError}};
use fcb_core::{DocumentGeneration, DocumentId, ResourceAllocationId, ResourceBudget};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, ReaderDocumentOptions,
    ReaderOutlineOptions, DocumentCopyMode};
use fcb_app::EXIT_OK;

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn reader(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(owner(8831), Path::new("README.md"), bytes, || false).unwrap()
}
fn stale() -> ReaderSessionError { ReaderSessionError::Document(DocumentReadError::StaleGeneration) }
fn number(json: &str, key: &str) -> usize {
    json.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
// Expected selection comes from the public document adapter, not handwritten
// Markdown offsets. These tests verify host composition/ownership, not a second
// oracle for upstream parsing or native glyph geometry.
fn selected(reader: &ReaderSession, needle: &str) -> (usize, usize, Vec<u8>) {
    let owner = reader.capture().request().file().owner();
    let budget = ResourceBudget::new(owner, ByteLength::new(128 * 1024 * 1024)).unwrap();
    let document = DocumentReader::prepare(reader.capture(), DocumentId::new(owner, 1).unwrap(),
        DocumentGeneration::new(owner, 1).unwrap(), ReaderDocumentOptions::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let start = document.rendered_text().find(needle).unwrap();
    let end = start + needle.len();
    let selection = document.selection(TextSelectionRange::new(start, end)).unwrap();
    (start, end, selection.enclosing_original_bytes.to_vec())
}

#[test]
fn prepare_and_heading_navigation_render_the_retained_source_without_native_claims() {
    let mut reader = reader(b"# Overview\n\nA **bold** introduction.\n\n## Details\n\nMore text.\n");
    assert_eq!(reader.document_window(1, 0, 1, || false).err(), Some(ReaderSessionError::MissingDocument));
    let response = reader.prepare_document(1, Default::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    for field in ["\"document_ready\":true", "\"layout_complete\":true", "\"native_shaped\":false",
        "\"native_presented\":false", "\"additional_source_bytes_read\":\"0\"", "\"slug\":\"details\""] {
        assert!(response.as_str().contains(field), "{field}: {}", response.as_str());
    }
    assert!(!response.as_str().contains("**bold**"));
    let heading = reader.document_at_heading(1, "details", 2, || false).unwrap();
    assert!(heading.as_str().contains("Details"));
    assert!(number(heading.as_str(), "first_flow_line") > 0);
    assert!(reader.document_at_heading(1, "missing", 2, || false).is_err());
    assert_eq!(reader.document_generation(), Some(1));
}

#[test]
fn rendered_text_copy_and_enclosing_original_markdown_have_distinct_domains() {
    let mut reader = reader(b"# Copy\n\nA **bold** word and [link](https://invalid.example/inert).\n");
    let (start, end, expected) = selected(&reader, "bold");
    reader.prepare_document(1, Default::default(), || false).unwrap();
    let rendered = reader.copy_document_selection(1, start, end, DocumentCopyMode::RenderedText, || false).unwrap();
    assert!(rendered.as_str().contains("\"copy_domain\":\"rendered-text-utf8\""));
    assert!(rendered.as_str().contains("\"text\":\"bold\""));
    assert!(!rendered.as_str().contains("\"original_hex\":"));
    let original = reader.copy_document_selection(1, start, end, DocumentCopyMode::EnclosingMarkdown, || false).unwrap();
    assert!(original.as_str().contains("\"copy_domain\":\"enclosing-original-markdown\""));
    assert!(original.as_str().contains(&hex(&expected)));
    assert!(std::str::from_utf8(&expected).unwrap().contains("**bold**"));
    assert!(original.as_str().contains("enclosing-regions-not-glyph-exact"));
    assert!(!original.as_str().contains("\"query_generation\":"));
}

#[test]
fn preview_to_source_reuses_exact_decoded_selection_and_document_identity() {
    let mut reader = reader("\u{feff}# Café\r\n\r\nA **bold** word.\r\n".as_bytes());
    let (start, end, expected) = selected(&reader, "bold");
    reader.prepare_document(1, Default::default(), || false).unwrap();
    reader.search(1, "bold", 10, 1024, || false).unwrap();
    let source = reader.document_selection_source(1, start, end, 64, || false).unwrap();
    assert!(source.as_str().contains("\"selection_namespace\":\"document\""));
    assert!(source.as_str().contains("\"document_generation\":\"1\""));
    assert!(source.as_str().contains("\"window_utf8_range\":"));
    assert!(source.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&expected))));
    assert!(!source.as_str().contains("\"query_generation\":"));
    assert!(reader.copy_hit(1, 0, || false).is_ok());
}

#[test]
fn reflow_rejects_old_rendered_offsets_and_restores_place_from_original_bytes() {
    let bytes = b"# Start\n\nA long paragraph with enough words to wrap differently at narrow widths.\n\n## Target\n\nneedle.\n";
    let offset = bytes.windows(6).position(|v| v == b"needle").unwrap() as u64;
    let mut reader = reader(bytes);
    reader.prepare_document(1, Default::default(), || false).unwrap();
    let wide = reader.document_at_source(1, offset, 2, || false).unwrap();
    reader.prepare_document(2, ReaderDocumentOptions { width_columns: 12, ..Default::default() }, || false).unwrap();
    assert_eq!(reader.document_window(1, 0, 1, || false).err(), Some(stale()));
    assert_eq!(reader.document_at_heading(1, "target", 1, || false).err(), Some(stale()));
    assert_eq!(reader.copy_document_selection(1, 0, 1, DocumentCopyMode::RenderedText, || false).err(), Some(stale()));
    let narrow = reader.document_at_source(2, offset, 2, || false).unwrap();
    assert!(narrow.as_str().contains("needle"));
    assert!(number(narrow.as_str(), "first_flow_line") >= number(wide.as_str(), "first_flow_line"));
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn paging_covers_every_flow_row_and_heading_without_rebuilding() {
    let bytes = (0..140).map(|n| format!("## h{n}\n\nBody {n}.\n\n")).collect::<String>();
    let mut reader = reader(bytes.as_bytes());
    let first = reader.prepare_document(1, Default::default(), || false).unwrap();
    let total = number(first.as_str(), "total_flow_lines");
    assert_eq!(number(first.as_str(), "total_headings"), 140);
    assert!(first.as_str().contains("\"next_heading\":\"64\""));
    let mut at = 0;
    while at < total {
        let page = reader.document_window(1, at, 17, || false).unwrap();
        let rows = page.as_str().matches("\"flow_line\":").count();
        assert_eq!(rows, 17.min(total - at));
        assert_eq!(number(page.as_str(), "first_flow_line"), at);
        at += rows;
    }
    assert!(reader.document_window(1, total, 1, || false).unwrap().as_str().contains("\"flow_lines\":[]"));
    let tail = reader.document_headings(1, 128, 128, || false).unwrap();
    assert_eq!(tail.as_str().matches("\"slug\":").count(), 12);
    assert!(tail.as_str().contains("\"next_heading\":null"));
    assert_eq!(reader.document_generation(), Some(1));
}

#[test]
fn failed_reflow_preserves_usable_preview_and_consumes_its_generation() {
    let mut reader = reader(b"# Old\n\n## Next\n\nBody.\n");
    reader.prepare_document(1, Default::default(), || false).unwrap();
    let options = ReaderDocumentOptions { max_flow_lines: 1, ..Default::default() };
    assert_eq!(reader.prepare_document(2, options, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::EngineBudget)));
    assert_eq!(reader.document_generation(), Some(1));
    assert!(reader.document_at_heading(1, "old", 1, || false).is_ok());
    assert_eq!(reader.prepare_document(2, Default::default(), || false).err(), Some(stale()));
}

#[test]
fn cancellation_pulses_at_preparation_and_publication_boundaries_keep_the_old_layout() {
    let mut reader = reader(b"# Kept\n\nA **bold** paragraph.\n");
    let mut polls = 0;
    reader.prepare_document(1, Default::default(), || { polls += 1; false }).unwrap();
    let mut generation = 1;
    for stop in [1, 2, 3, polls / 2, polls - 1, polls] {
        generation += 1;
        let mut calls = 0;
        assert!(reader.prepare_document(generation, Default::default(), || { calls += 1; calls == stop }).is_err());
        assert_eq!(reader.document_generation(), Some(1));
        assert!(reader.document_window(1, 0, 10, || false).unwrap().as_str().contains("Kept"));
    }
    assert!(reader.clear_document(generation + 1, || true).is_err());
    assert_eq!(reader.document_generation(), Some(1));
}

#[test]
fn clearing_document_does_not_clear_source_search_or_outline() {
    let mut reader = reader(b"fn kept() {}\n");
    reader.search(1, "kept", 10, 1024, || false).unwrap();
    reader.prepare_outline(1, ReaderOutlineOptions {
        language: Some(fcb::search::SymbolLanguage::Rust), ..Default::default()
    }, || false).unwrap();
    reader.prepare_document(1, Default::default(), || false).unwrap();
    reader.clear_document(2, || false).unwrap();
    assert_eq!(reader.document_generation(), None);
    assert_eq!(reader.outline_generation(), Some(1));
    assert_eq!(reader.accepted_generation(), Some(1));
    assert!(reader.copy_hit(1, 0, || false).is_ok());
    assert!(reader.copy_symbol(1, 1, false, || false).is_ok());
    assert_eq!(reader.document_window(1, 0, 1, || false).err(), Some(ReaderSessionError::MissingDocument));
    assert_eq!(reader.prepare_document(2, Default::default(), || false).err(), Some(stale()));
    reader.prepare_document(3, Default::default(), || false).unwrap();
}

#[test]
fn unsupported_document_encoding_and_size_leave_ordinary_source_reading_available() {
    let utf16: Vec<u8> = "\u{feff}# Header\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    for bytes in [utf16, vec![0xff, 0x61]] {
        let mut reader = reader(&bytes);
        assert_eq!(reader.prepare_document(1, Default::default(), || false).err(),
            Some(ReaderSessionError::Document(DocumentReadError::InvalidUtf8)));
        assert!(reader.copy_range(0, bytes.len() as u64, || false).unwrap().as_str().contains(&hex(&bytes)));
    }
    let mut reader = reader(&vec![b'a'; 256 * 1024 + 1]);
    assert_eq!(reader.prepare_document(1, ReaderDocumentOptions { max_source_bytes: 256 * 1024, ..Default::default() }, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::SourceLimit)));
    assert!(reader.read_window(0, 64, || false).is_ok());
}

#[test]
fn hostile_offsets_and_interior_utf8_positions_never_create_a_selection() {
    let mut reader = reader("# Café\n\nText.\n".as_bytes());
    let (start, end, _) = selected(&reader, "é");
    reader.prepare_document(1, Default::default(), || false).unwrap();
    for (a, b) in [(0, 0), (2, 1), (start + 1, end), (0, usize::MAX)] {
        assert!(reader.copy_document_selection(1, a, b, DocumentCopyMode::RenderedText, || false).is_err());
    }
    assert!(reader.document_window(1, usize::MAX, 1, || false).is_err());
    assert!(reader.document_window(1, 0, 129, || false).is_err());
    assert!(reader.document_headings(1, usize::MAX, 1, || false).is_err());
    assert!(reader.document_at_source(1, u64::MAX, 1, || false).is_err());
    assert!(reader.document_selection_source(1, start, end, 16 * 1024 + 1, || false).is_err());
    assert_eq!(reader.document_generation(), Some(1));
}

#[test]
fn empty_documents_and_exhausted_generation_are_explicit() {
    for bytes in [b"".as_slice(), b"\xef\xbb\xbf"] {
        let mut reader = reader(bytes);
        let result = reader.prepare_document(u64::MAX, Default::default(), || false).unwrap();
        assert!(result.as_str().contains("\"total_flow_lines\":\"0\""));
        assert!(result.as_str().contains("\"flow_lines\":[]"));
        assert!(reader.document_at_source(u64::MAX, bytes.len() as u64, 1, || false).is_ok());
        assert_eq!(reader.clear_document(1, || false).err(), Some(stale()));
        assert_eq!(reader.prepare_document(0, Default::default(), || false).err(), Some(stale()));
    }
}

#[cfg(unix)]
#[test]
fn live_replacement_and_raw_filename_cannot_change_document_bytes() {
    use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt, time::{SystemTime, UNIX_EPOCH}};
    let root = std::env::temp_dir().join(format!("fcb-doc-reader-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&root).unwrap();
    let path = root.join(OsString::from_vec(b"raw-\xff\n.md".to_vec()));
    fs::write(&path, b"# Original\n\n**needle**.\n").unwrap();
    let mut reader = ReaderSession::open(owner(8832), &path, 1024, || false).unwrap();
    fs::write(&path, b"# Changed\n\nNew bytes.\n").unwrap();
    let result = reader.prepare_document(1, Default::default(), || false).unwrap();
    assert!(result.as_str().contains("Original"));
    assert!(!result.as_str().contains("Changed"));
    assert!(result.as_str().contains("7261772dff0a2e6d64"));
    assert!(reader.document_at_heading(1, "original", 1, || false).is_ok());
    assert!(reader.document_at_heading(1, "changed", 1, || false).is_err());
}
