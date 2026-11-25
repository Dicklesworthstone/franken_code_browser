#![forbid(unsafe_code)]

//! The existing logical Markdown service supports a larger bounded capture
//! than the separately qualified rich display. Native fallback must preserve
//! exact source navigation and copy without expanding the rich-display limit.
//! Authored code-first; no local build or execution claims.

mod support;
use support::{Json, parse};
use std::path::Path;
use fcb::ArenaOwnerId;
use fcb::document::{native_display::{NativeDisplayError, MAX_DISPLAY_SOURCE_BYTES},
    reader::{DocumentReadError, MAX_DOCUMENT_SOURCE_BYTES}};
use fcb_app::host::{HostResponse, reader::{DocumentCopyMode, ReaderDocumentOptions,
    ReaderSession, ReaderSessionError}};

const PARAGRAPH: &[u8] = b"Regular paragraph with enough plain source text to exercise bounded document flow.\r\n\r\n";
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn reader(bytes: &[u8], id: u64) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(id).unwrap(), Path::new("large.md"), bytes, || false).unwrap()
}
fn options() -> ReaderDocumentOptions {
    ReaderDocumentOptions { max_source_bytes: MAX_DOCUMENT_SOURCE_BYTES, ..Default::default() }
}

#[test]
fn large_logical_preview_keeps_bom_crlf_tail_anchors_and_copy_after_rich_display_refuses() {
    let mut bytes = b"\xef\xbb\xbf# Large document\r\n\r\n".to_vec();
    while bytes.len() < 136 * 1024 { bytes.extend_from_slice(PARAGRAPH); }
    let heading_at = bytes.len();
    bytes.extend_from_slice(b"## Tail target\r\n\r\n");
    let paragraph_at = bytes.len();
    bytes.extend_from_slice("A **bold** tail paragraph with café.\r\n".as_bytes());
    assert!(MAX_DISPLAY_SOURCE_BYTES < bytes.len() && bytes.len() < MAX_DOCUMENT_SOURCE_BYTES);
    let mut reader = reader(&bytes, 8851);
    // Negative control: merely retaining the old 64 KiB option cannot expose
    // this document. The larger logical input bound is a real host capability.
    assert_eq!(reader.prepare_document(1, ReaderDocumentOptions::default(), || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::SourceLimit)));
    let prepared = json(&reader.prepare_document(2, options(), || false).unwrap());
    assert_eq!(prepared.get("rendering").text(), "logical-frankenmarkdown-flow");
    assert_eq!(prepared.get("parser_source_base").number(), 3);
    assert_eq!(prepared.get("total_headings").number(), 2);
    assert!(prepared.get("layout_complete").flag());
    assert!(!prepared.get("native_shaped").flag());
    assert_eq!(prepared.get("additional_source_bytes_read").number(), 0);
    let tail = prepared.get("headings").array().iter()
        .find(|heading| heading.get("title").text() == "Tail target").unwrap();
    assert_eq!(tail.get("original_range").get("start").number(), heading_at as u64,
        "BOM and every CRLF contribute their original bytes before the late heading");
    let slug = tail.get("slug").text().to_owned();
    let heading = json(&reader.document_at_heading(2, &slug, 4, || false).unwrap());
    assert!(heading.get("first_flow_line").number() > 64);
    assert_eq!(heading.get("heading_original_range"), tail.get("original_range"));

    assert_eq!(reader.prepare_document_display(2, 640, || false).err(),
        Some(ReaderSessionError::DocumentDisplay(NativeDisplayError::SourceLimit)));
    assert_eq!(reader.document_generation(), Some(2));
    assert_eq!(reader.document_display_page(2, 0, 1, || false).err(),
        Some(ReaderSessionError::DocumentDisplay(NativeDisplayError::MissingDisplay)));
    let page = json(&reader.document_at_source(2, paragraph_at as u64, 4, || false).unwrap());
    assert_eq!(page.get("original_anchor").number(), paragraph_at as u64);
    let row = page.get("flow_lines").array().iter()
        .find(|row| row.get("text").text().contains("bold")).unwrap();
    let rendered = row.get("rendered_utf8_range");
    let start = rendered.get("start").number() as usize;
    let end = start + row.get("text").text().len();
    let original = row.get("enclosing_original_range");
    let a = original.get("start").number() as usize;
    let b = original.get("end").number() as usize;
    assert_eq!(a, paragraph_at);
    assert!(bytes[a..b].starts_with(b"A **bold** tail"));
    let rendered_copy = json(&reader.copy_document_selection(2, start, end,
        DocumentCopyMode::RenderedText, || false).unwrap());
    assert_eq!(rendered_copy.get("text").text(), row.get("text").text());
    let markdown_copy = json(&reader.copy_document_selection(2, start, end,
        DocumentCopyMode::EnclosingMarkdown, || false).unwrap());
    assert_eq!(markdown_copy.get("original_hex").text(), hex(&bytes[a..b]));
    assert_eq!(markdown_copy.get("enclosing_original_range"), original);
    let source = json(&reader.document_selection_source(2, start, end, 64, || false).unwrap());
    assert_eq!(source.get("selection").get("original_range"), original);
    assert_eq!(source.get("selection").get("original_hex").text(), hex(&bytes[a..b]));
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn logical_preview_admits_its_exact_256_kib_bound_and_rejects_one_more_byte() {
    let mut bytes = b"# Exact logical limit\r\n\r\n".to_vec();
    while bytes.len() + PARAGRAPH.len() <= MAX_DOCUMENT_SOURCE_BYTES {
        bytes.extend_from_slice(PARAGRAPH);
    }
    bytes.resize(MAX_DOCUMENT_SOURCE_BYTES, b'x');
    let mut accepted = reader(&bytes, 8852);
    let prepared = json(&accepted.prepare_document(1, options(), || false).unwrap());
    assert!(prepared.get("document_ready").flag());
    assert!(prepared.get("layout_complete").flag());
    assert_eq!(accepted.document_generation(), Some(1));
    assert!(accepted.document_window(1, prepared.get("total_flow_lines").number() as usize, 1,
        || false).unwrap().as_str().contains("\"flow_lines\":[]"));
    bytes.push(b'x');
    let mut oversized = reader(&bytes, 8853);
    assert_eq!(oversized.prepare_document(1, options(), || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::SourceLimit)));
    assert_eq!(oversized.document_generation(), None);
    assert!(oversized.read_window(MAX_DOCUMENT_SOURCE_BYTES as u64, 4, || false).is_ok());
}
