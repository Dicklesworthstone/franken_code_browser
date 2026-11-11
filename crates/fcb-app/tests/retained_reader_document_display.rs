#![forbid(unsafe_code)]

use std::path::Path;
use fcb::ArenaOwnerId;
use fcb::document::{native_display::NativeDisplayError, reader::DocumentReadError};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, ReaderDocumentOptions};

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
