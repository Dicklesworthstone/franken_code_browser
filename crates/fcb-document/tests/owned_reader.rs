#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, DocumentGeneration, DocumentId,
    FileId, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_document::TextSelectionRange;
use fcb_document::reader::{DocumentReader, DocumentReadError, DocumentReadOptions};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8031).unwrap() }
fn generation(n: u64) -> DocumentGeneration { DocumentGeneration::new(owner(), n).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn capture(bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 1).unwrap()).unwrap(), ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn read<'a>(source: &'a CompleteCapture, budget: &ResourceBudget, n: u64) -> DocumentReader<'a> {
    DocumentReader::prepare(source, DocumentId::new(owner(), 1).unwrap(), generation(n),
        DocumentReadOptions::default(), budget, allocation(n), || false).unwrap()
}

#[test]
fn owned_layout_outlives_original_capture_and_reuses_source_and_flow_allocations() {
    let budget = budget();
    let (owned, source_pointer, flow_pointer) = {
        let source = capture(b"# Kept\n\nA **bold** document.\n");
        let source_pointer = source.bytes().as_ptr();
        let reader = read(&source, &budget, 1);
        let flow_pointer = reader.rendered_text().as_ptr();
        (reader.into_owned(&budget, allocation(2)).unwrap(), source_pointer, flow_pointer)
    };
    assert_eq!(owned.capture().bytes().as_ptr(), source_pointer);
    assert_eq!(owned.rendered_text().as_ptr(), flow_pointer);
    assert_eq!(owned.heading("kept").unwrap().title, "Kept");
    assert!(owned.window_at_heading("kept", 10).unwrap().whole_document_visible());
    let start = owned.rendered_text().find("bold").unwrap();
    let selected = owned.selection(TextSelectionRange::new(start, start + 4)).unwrap();
    assert_eq!(selected.rendered_text, "bold");
    assert!(std::str::from_utf8(selected.enclosing_original_bytes).unwrap().contains("**bold**"));
}

#[test]
fn owned_delivery_uses_its_capture_object_and_never_rebinds_equal_external_ids() {
    let budget = budget(); let source = capture(b"# Same\n");
    let borrowed = read(&source, &budget, 1);
    borrowed.validate_delivery(&source, generation(1)).unwrap();
    let owned = borrowed.into_owned(&budget, allocation(2)).unwrap();
    owned.validate_delivery(owned.capture(), generation(1)).unwrap();
    assert_eq!(owned.validate_delivery(&source, generation(1)), Err(DocumentReadError::StaleCapture));
    assert_eq!(owned.validate_delivery(owned.capture(), generation(2)), Err(DocumentReadError::StaleGeneration));
}

#[test]
fn ownership_admission_can_fail_without_destroying_another_accepted_layout() {
    let budget = budget(); let source = capture(b"# Retained\n");
    let old = read(&source, &budget, 1).into_owned(&budget, allocation(2)).unwrap();
    let next = read(&source, &budget, 3);
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(next.into_owned(&denied, allocation(4)).err(), Some(DocumentReadError::ResourceDenied));
    assert_eq!(old.heading("retained").unwrap().title, "Retained");
}

#[test]
fn source_navigation_uses_upstream_region_mapping_in_original_bom_coordinates() {
    let budget = budget();
    let source = capture("\u{feff}# Start\n\nParagraph with café and **needle**.\n\n## Later\n".as_bytes());
    let reader = read(&source, &budget, 1);
    let text = std::str::from_utf8(source.bytes()).unwrap();
    let offset = text.find("needle").unwrap();
    let page = reader.window_at_source(ByteOffset::new(offset as u64), 1).unwrap();
    assert!(!page.lines().is_empty());
    let mapped = reader.original_span(page.lines()[0].source_span).unwrap();
    assert!(mapped.start().get() <= offset as u64 && mapped.end().get() > offset as u64);
    assert!(reader.window_at_source(ByteOffset::new(source.bytes().len() as u64), 1).unwrap().lines().is_empty());
    for bad in [0, 1, text.find('é').unwrap() + 1, source.bytes().len() + 1] {
        assert_eq!(reader.window_at_source(ByteOffset::new(bad as u64), 1).err(), Some(DocumentReadError::InvalidRange));
    }
}

#[test]
fn source_navigation_remains_valid_after_owned_layout_moves() {
    let budget = budget(); let source = capture(b"# A\n\n## B\n\nBody.\n");
    let reader = read(&source, &budget, 1).into_owned(&budget, allocation(2)).unwrap();
    let moved = vec![reader];
    let reader = &moved[0];
    let range = reader.original_span(reader.heading("b").unwrap().source_span).unwrap();
    let page = reader.window_at_source(range.start(), 1).unwrap();
    assert!(page.lines()[0].rendered_text.contains('B'));
    reader.validate_delivery(reader.capture(), generation(1)).unwrap();
}
