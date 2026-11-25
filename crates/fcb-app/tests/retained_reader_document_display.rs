#![forbid(unsafe_code)]

mod support;

use std::path::Path;
use fcb::ArenaOwnerId;
use fcb::document::{native_display::{NativeDisplayError, MAX_DISPLAY_SOURCE_BYTES}, reader::DocumentReadError};
use fcb_app::host::reader::{DocumentCopyMode, ReaderSession, ReaderSessionError, ReaderDocumentOptions};

fn new_reader(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(8841).unwrap(), Path::new("README.md"), bytes, || false).unwrap()
}
fn number(json: &str, key: &str) -> usize {
    json.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn display_error(error: NativeDisplayError) -> ReaderSessionError { ReaderSessionError::DocumentDisplay(error) }

#[test]
fn retained_display_and_font_packets_keep_logical_layout_and_capture_identity() {
    let bytes = b"# Heading\n\nA **bold** word and [link](#heading).\n";
    let mut reader = new_reader(bytes);
    let logical = reader.prepare_document(1, Default::default(), || false).unwrap();
    let logical_rows = number(logical.as_str(), "total_flow_lines");
    let prepared = reader.prepare_document_display(1, 640, || false).unwrap();
    for field in ["\"command\":\"document-display-prepare\"", "\"document_generation\":\"1\"",
        "\"display_width_points\":\"640\"", "\"rendering\":\"bundled-frankenmarkdown-display\"",
        "\"glyph_shaped\":true", "\"native_shaped\":false", "\"native_presented\":false",
        "\"additional_source_bytes_read\":\"0\"", "\"source_ranges_qualified\":true",
        "\"fonts\":[{\"font_index\":\"0\"", "\"kind\":\"text\"", "\"glyph_id\":"] {
        assert!(prepared.as_str().contains(field), "{field}: {}", prepared.as_str());
    }
    assert!(!prepared.as_str().contains("rendered_utf8_range"), "rich glyphs must not be mislabeled logical selection offsets");
    let items = number(prepared.as_str(), "total_display_items");
    let page = reader.document_display_page(1, 0, 1, || false).unwrap();
    assert_eq!(page.as_str().matches("\"item_index\":").count(), 1);
    let eof = reader.document_display_page(1, items, 1, || false).unwrap();
    assert!(eof.as_str().contains("\"display_items\":[],\"next_display_item\":null"));
    let font = reader.document_display_font(1, 0, 0, 16, || false).unwrap();
    assert!(font.as_str().contains("\"font_bytes\":\"16\""));
    assert!(font.as_str().contains("\"next_font_offset\":\"16\""));
    let hex = font.as_str().split_once("\"font_hex\":\"").unwrap().1.split('"').next().unwrap();
    assert_eq!(hex.len(), 32); assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let total_font = number(font.as_str(), "total_font_bytes");
    let font_eof = reader.document_display_font(1, 0, total_font, 16, || false).unwrap();
    assert!(font_eof.as_str().contains("\"font_hex\":\"\",\"next_font_offset\":null"));
    let after = reader.document_window(1, 0, 64, || false).unwrap();
    assert_eq!(number(after.as_str(), "total_flow_lines"), logical_rows);
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn only_successful_logical_reflow_retires_the_rich_width_and_old_font_authority() {
    let mut reader = new_reader(b"# Title\n\nFirst paragraph.\n\nSecond paragraph.\n");
    reader.prepare_document(1, Default::default(), || false).unwrap();
    reader.prepare_document_display(1, 640, || false).unwrap();
    assert_eq!(reader.prepare_document_display(1, 320, || false).err(),
        Some(display_error(NativeDisplayError::StaleGeneration)));
    assert!(reader.prepare_document_display(1, 640, || false).is_ok());
    assert!(reader.prepare_document(2, ReaderDocumentOptions { max_flow_lines: 1,
        ..Default::default() }, || false).is_err());
    assert!(reader.document_display_font(1, 0, 0, 16, || false).is_ok());
    reader.prepare_document(3, ReaderDocumentOptions { width_columns: 40,
        ..Default::default() }, || false).unwrap();
    assert_eq!(reader.document_display_page(1, 0, 1, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
    assert_eq!(reader.document_display_font(3, 0, 0, 16, || false).err(),
        Some(display_error(NativeDisplayError::MissingDisplay)));
    reader.prepare_document_display(3, 320, || false).unwrap();
    reader.clear_document(4, || false).unwrap();
    assert!(reader.document_display_page(3, 0, 1, || false).is_err());
    assert!(reader.read_window(0, 4096, || false).unwrap().as_str().contains("# Title"));
}

#[test]
fn refusal_and_cancellation_do_not_publish_partial_display_or_poison_logical_work() {
    let mut reader = new_reader("# Unsupported 😀\n\nRetained source.\n".as_bytes());
    reader.prepare_document(1, Default::default(), || false).unwrap();
    assert_eq!(reader.prepare_document_display(1, 640, || false).err(),
        Some(display_error(NativeDisplayError::ShapingUnsupported)));
    assert_eq!(reader.document_display_page(1, 0, 1, || false).err(),
        Some(display_error(NativeDisplayError::MissingDisplay)));
    assert!(reader.document_window(1, 0, 64, || false).unwrap().as_str().contains("Retained source."));
    let mut reader = new_reader(b"# Supported\n\nRetained source.\n");
    reader.prepare_document(1, Default::default(), || false).unwrap();
    let mut polls = 0;
    assert!(reader.prepare_document_display(1, 640, || { polls += 1; polls == 5 }).is_err());
    assert_eq!(reader.document_display_page(1, 0, 1, || false).err(),
        Some(display_error(NativeDisplayError::MissingDisplay)));
    reader.prepare_document_display(1, 640, || false).unwrap();
    assert_eq!(reader.document_display_page(1, 0, 129, || false).err(),
        Some(display_error(NativeDisplayError::InvalidLimits)));
    assert_eq!(reader.document_display_font(1, 0, 0, 65537, || false).err(),
        Some(display_error(NativeDisplayError::InvalidLimits)));
    assert!(reader.document_display_page(1, 0, 1, || false).is_ok());
}

#[test]
fn source_anchored_display_pages_restore_place_and_refuse_unqualified_or_unmapped_bytes() {
    let bytes = b"\n\n# First\n\nEarly paragraph.\n\n## Later\n\nDestination paragraph.\n";
    let mut reader = new_reader(bytes);
    reader.prepare_document(1, Default::default(), || false).unwrap();
    reader.prepare_document_display(1, 320, || false).unwrap();
    let offset = bytes.windows(b"Destination".len()).position(|part| part == b"Destination").unwrap() as u64;
    let page = reader.document_display_from_source(1, offset, 4, || false).unwrap();
    assert!(page.as_str().contains("\"command\":\"document-display-from-source\""));
    assert_eq!(number(page.as_str(), "original_anchor"), offset as usize);
    assert!(number(page.as_str(), "first_display_item") > 0);
    assert!(page.as_str().contains("Destination"));
    let eof = reader.document_display_from_source(1, bytes.len() as u64, 4, || false).unwrap();
    assert_eq!(number(eof.as_str(), "first_display_item"), number(eof.as_str(), "total_display_items"));
    assert!(eof.as_str().contains("\"display_items\":[],\"next_display_item\":null"));
    assert_eq!(reader.document_display_from_source(1, 0, 4, || false).err(),
        Some(display_error(NativeDisplayError::InvalidRange)));
    assert_eq!(reader.document_display_from_source(2, offset, 4, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
    let mut unqualified = new_reader(b"---\ntitle: metadata\n---\n\n# Target\n\nBody.\n");
    unqualified.prepare_document(1, Default::default(), || false).unwrap();
    unqualified.prepare_document_display(1, 320, || false).unwrap();
    assert_eq!(unqualified.document_display_from_source(1, 0, 4, || false).err(),
        Some(display_error(NativeDisplayError::InvalidOutput)));
    assert!(unqualified.document_display_page(1, 0, 4, || false).is_ok());
}

#[test]
fn large_rich_preview_keeps_heading_source_and_copy_actions_in_host_budget() {
    let mut bytes = b"# Large retained document\n\n".to_vec();
    let paragraph = b"A plain paragraph remains readable through bounded native pages and exact source anchors.\n\n";
    while bytes.len() < 112 * 1024 { bytes.extend_from_slice(paragraph); }
    let late_heading_offset = bytes.len();
    bytes.extend_from_slice(b"## Late target\n\n");
    let late_source_offset = bytes.len();
    bytes.extend_from_slice(b"Late **source** anchor remains visible.\n");
    assert!(bytes.len() > 100 * 1024 && bytes.len() < MAX_DISPLAY_SOURCE_BYTES);
    let mut reader = new_reader(&bytes);
    reader.prepare_document(1, ReaderDocumentOptions {
        max_source_bytes: MAX_DISPLAY_SOURCE_BYTES, ..Default::default()
    }, || false).unwrap();
    let prepared = reader.prepare_document_display(1, 640, || false).unwrap();
    assert!(prepared.as_str().contains("\"source_ranges_qualified\":true"));
    assert!(prepared.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    assert!(prepared.as_str().contains("\"glyph_shaped\":true"));
    let total = number(prepared.as_str(), "total_display_items");
    assert!(total > 128);
    drop(prepared);
    let late = reader.document_display_from_source(1, late_source_offset as u64, 4, || false).unwrap();
    assert!(number(late.as_str(), "first_display_item") > 128);
    assert_eq!(number(late.as_str(), "original_anchor"), late_source_offset);
    assert!(late.as_str().contains("anchor remains visible."));
    assert!(late.as_str().contains("\"source_ranges_qualified\":true"));
    drop(late);

    // Native heading/source commands go through the logical navigation API
    // before restoring rich geometry. They must reuse the same completed
    // provenance audit; a fresh full audit cannot coexist with this display.
    let heading = support::parse(reader.document_at_heading(1, "late-target", 4, || false).unwrap()
        .as_str().as_bytes()).unwrap();
    assert!(heading.get("first_flow_line").number() > 64);
    assert_eq!(heading.get("heading_original_range").get("start").number(), late_heading_offset as u64);
    let page = support::parse(reader.document_at_source(1, late_source_offset as u64, 4, || false).unwrap()
        .as_str().as_bytes()).unwrap();
    assert_eq!(page.get("original_anchor").number(), late_source_offset as u64);
    let row = page.get("flow_lines").array().iter()
        .find(|row| row.get("text").text().contains("anchor remains visible.")).unwrap();
    let text = row.get("text").text();
    assert_eq!(text, "Late source anchor remains visible.");
    let start = row.get("rendered_utf8_range").get("start").number() as usize;
    let end = start + text.len();
    let original = row.get("enclosing_original_range");
    let a = original.get("start").number() as usize;
    let b = original.get("end").number() as usize;
    assert_eq!(a, late_source_offset);
    assert!(bytes[a..b].starts_with(b"Late **source**"));
    let rendered_copy = support::parse(reader.copy_document_selection(1, start, end,
        DocumentCopyMode::RenderedText, || false).unwrap().as_str().as_bytes()).unwrap();
    assert_eq!(rendered_copy.get("text").text(), text);
    assert_eq!(rendered_copy.get("enclosing_original_range"), original);
    let markdown_copy = support::parse(reader.copy_document_selection(1, start, end,
        DocumentCopyMode::EnclosingMarkdown, || false).unwrap().as_str().as_bytes()).unwrap();
    let expected_hex: String = bytes[a..b].iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(markdown_copy.get("original_hex").text(), expected_hex);
    assert_eq!(markdown_copy.get("enclosing_original_range"), original);
    let source = support::parse(reader.document_selection_source(1, start, end, 32, || false).unwrap()
        .as_str().as_bytes()).unwrap();
    assert_eq!(source.get("selection").get("original_range"), original);
    assert_eq!(source.get("selection").get("original_hex").text(), expected_hex);

    assert_eq!(reader.document_at_heading(2, "late-target", 4, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
    assert_eq!(reader.document_at_source(2, late_source_offset as u64, 4, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
    assert_eq!(reader.document_at_heading(1, "late-target", 4, || true).err(),
        Some(ReaderSessionError::Canceled));
    assert_eq!(reader.document_at_source(1, late_source_offset as u64, 4, || true).err(),
        Some(ReaderSessionError::Canceled));
    for mode in [DocumentCopyMode::RenderedText, DocumentCopyMode::EnclosingMarkdown] {
        assert_eq!(reader.copy_document_selection(2, start, end, mode, || false).err(),
            Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
        assert_eq!(reader.copy_document_selection(1, start, end, mode, || true).err(),
            Some(ReaderSessionError::Canceled));
    }
    let eof = reader.document_display_page(1, total, 128, || false).unwrap();
    assert!(eof.as_str().contains("\"display_items\":[],\"next_display_item\":null"));
    assert!(reader.document_display_font(1, 0, 0, 16, || false).unwrap()
        .as_str().contains("\"font_bytes\":\"16\""));
    assert!(reader.document_window(1, 0, 4, || false).unwrap()
        .as_str().contains("Large retained document"));
    assert_eq!(reader.document_generation(), Some(1));
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn unqualified_or_failed_rich_preview_cannot_bypass_logical_source_qualification() {
    let bytes = b"---\ntitle: metadata\n---\n\n# Target\n\nBody.\n";
    let body_at = bytes.windows(b"Body".len()).position(|part| part == b"Body").unwrap() as u64;
    let mut reader = new_reader(bytes);
    let logical = support::parse(reader.prepare_document(1, Default::default(), || false).unwrap()
        .as_str().as_bytes()).unwrap();
    let row = logical.get("flow_lines").array().iter()
        .find(|row| row.get("text").text().contains("Body.")).unwrap();
    let start = row.get("rendered_utf8_range").get("start").number() as usize;
    let end = start + row.get("text").text().len();
    let rich = reader.prepare_document_display(1, 640, || false).unwrap();
    assert!(rich.as_str().contains("\"source_ranges_qualified\":false"));
    let unqualified = Some(ReaderSessionError::Document(DocumentReadError::InvalidOutput));
    assert_eq!(reader.document_at_heading(1, "target", 4, || false).err(), unqualified);
    assert_eq!(reader.document_at_source(1, body_at, 4, || false).err(), unqualified);
    assert_eq!(reader.document_selection_source(1, start, end, 16, || false).err(), unqualified);
    for mode in [DocumentCopyMode::RenderedText, DocumentCopyMode::EnclosingMarkdown] {
        assert_eq!(reader.copy_document_selection(1, start, end, mode, || false).err(), unqualified);
    }
    assert!(reader.document_display_page(1, 0, 4, || false).is_ok());
    assert!(reader.document_window(1, 0, 4, || false).is_ok());

    let mut failed = new_reader("---\ntitle: metadata\n---\n\n# Target\n\nBody 😀.\n".as_bytes());
    failed.prepare_document(1, Default::default(), || false).unwrap();
    assert_eq!(failed.prepare_document_display(1, 640, || false).err(),
        Some(display_error(NativeDisplayError::ShapingUnsupported)));
    assert_eq!(failed.document_display_page(1, 0, 1, || false).err(),
        Some(display_error(NativeDisplayError::MissingDisplay)));
    assert_eq!(failed.document_at_heading(1, "target", 4, || false).err(), unqualified);
    assert!(failed.document_window(1, 0, 4, || false).is_ok());
}
