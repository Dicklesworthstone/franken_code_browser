#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_document::{SourceSpan, TextSelectionRange};
use fcb_document::reader::{DocumentReader, DocumentReadError, DocumentReadOptions};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(803).unwrap() }
fn generation(n: u64) -> DocumentGeneration { DocumentGeneration::new(owner(), n).unwrap() }
fn id() -> DocumentId { DocumentId::new(owner(), 1).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn capture(bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 7).unwrap()).unwrap(), ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn read<'a>(source: &'a CompleteCapture, budget: &ResourceBudget) -> DocumentReader<'a> {
    DocumentReader::prepare(source, id(), generation(1), DocumentReadOptions::default(), budget, allocation(1), || false).unwrap()
}

#[test]
fn markdown_flows_through_the_real_engine_and_heading_navigation_reuses_the_layout() {
    let source = capture(b"# Overview\n\nA **bold** introduction.\n\n## Details\n\nRead the implementation.\n");
    let budget = budget(); let reader = read(&source, &budget);
    assert!(reader.rendered_text().contains("bold"));
    assert!(!reader.rendered_text().contains("**bold**"));
    assert_eq!(reader.headings().len(), 2);
    let heading = reader.heading("details").unwrap();
    assert_eq!(heading.title, "Details");
    let range = reader.original_span(heading.source_span).unwrap();
    let (a, b) = range.as_usize_bounds().unwrap();
    assert!(std::str::from_utf8(&source.bytes()[a..b]).unwrap().contains("## Details"));
    let page = reader.window_at_heading("details", 2).unwrap();
    assert!(page.lines()[0].rendered_text.contains("Details"));
    assert!(page.first_index() > 0);
    assert_eq!(reader.window_at_heading("absent", 1).err(), Some(DocumentReadError::HeadingNotFound));
}

#[test]
fn all_windows_are_borrowed_nonoverlapping_flow_rows_with_explicit_end() {
    let source = capture(b"# Title\n\nFirst paragraph with enough words to wrap into multiple rows.\n\nSecond paragraph.\n");
    let budget = budget();
    let reader = DocumentReader::prepare(&source, id(), generation(1),
        DocumentReadOptions { width_columns: 12, ..Default::default() }, &budget, allocation(1), || false).unwrap();
    let mut cursor = 0; let mut count = 0;
    loop {
        let page = reader.window(cursor, 2).unwrap();
        for line in page.lines() { assert_eq!(line.line_index, count); count += 1; }
        match page.next_index() { Some(next) => { assert!(next > cursor); cursor = next; }, None => break }
    }
    assert_eq!(count, reader.total_lines());
    let eof = reader.window(count, 2).unwrap(); assert!(eof.lines().is_empty());
    assert_eq!(eof.next_index(), None);
    for (first, size) in [(count + 1, 1), (0, 0), (0, 1025)] {
        assert_eq!(reader.window(first, size).err(), Some(DocumentReadError::InvalidRange));
    }
}

#[test]
fn utf8_bom_and_non_ascii_characters_keep_original_byte_coordinates() {
    let source = capture("\u{feff}# Café 😀\n\nText.\n".as_bytes());
    let budget = budget(); let reader = read(&source, &budget);
    assert_eq!(reader.source_base(), 3); assert_eq!(reader.headings().len(), 1);
    assert_eq!(reader.headings()[0].title, "Café 😀");
    assert_eq!(reader.original_span(reader.headings()[0].source_span).unwrap().start().get(), 3);
    assert_eq!(reader.original_span(SourceSpan { start: 0, end: 0 }).unwrap().start().get(), 3);
    assert_eq!(reader.capture().bytes(), source.bytes());
    let source_text = std::str::from_utf8(source.bytes()).unwrap();
    let within_e = source_text.find('é').unwrap() + 1 - 3;
    assert_eq!(reader.original_span(SourceSpan { start: within_e, end: within_e }).err(), Some(DocumentReadError::InvalidOutput));
}

#[test]
fn rendered_copy_and_original_enclosing_markdown_are_not_conflated() {
    let source = capture(b"# Copy\n\nA **bold** word and [link](https://invalid.example/never-fetch).\n");
    let budget = budget(); let reader = read(&source, &budget);
    let start = reader.rendered_text().find("bold").unwrap();
    let selection = reader.selection(TextSelectionRange::new(start, start + 4)).unwrap();
    assert_eq!(selection.rendered_text, "bold");
    let markdown = std::str::from_utf8(selection.enclosing_original_bytes).unwrap();
    assert!(markdown.contains("**bold**"));
    let (a, b) = selection.enclosing_original_range.as_usize_bounds().unwrap();
    assert_eq!(selection.enclosing_original_bytes, &source.bytes()[a..b]);
    assert_ne!(selection.enclosing_original_bytes, selection.rendered_text.as_bytes());
    assert!(reader.selection(TextSelectionRange::new(start, start)).is_err());
    assert!(reader.selection(TextSelectionRange::new(0, usize::MAX)).is_err());
}

#[test]
fn stale_generation_and_distinct_captures_with_equal_ids_cannot_replace_the_searched_source() {
    let source = capture(b"# Old\n\nOriginal document.\n");
    let same = capture(source.bytes()); let replacement = capture(b"# New\n\nDifferent document.\n");
    let budget = budget(); let reader = read(&source, &budget);
    reader.validate_delivery(&source, generation(1)).unwrap();
    assert_eq!(reader.validate_delivery(&same, generation(1)), Err(DocumentReadError::StaleCapture));
    assert_eq!(reader.validate_delivery(&replacement, generation(1)), Err(DocumentReadError::StaleCapture));
    assert_eq!(reader.validate_delivery(&source, generation(2)), Err(DocumentReadError::StaleGeneration));
    assert!(reader.rendered_text().contains("Original"));
}

#[test]
fn owner_source_limits_invalid_encoding_and_denied_memory_fail_before_publication() {
    let source = capture(b"# small"); let budget = budget();
    let foreign = DocumentId::new(ArenaOwnerId::new(804).unwrap(), 1).unwrap();
    assert_eq!(DocumentReader::prepare(&source, foreign, generation(1), Default::default(), &budget, allocation(1), || false).err(),
        Some(DocumentReadError::OwnerMismatch));
    assert_eq!(DocumentReader::prepare(&source, id(), generation(1),
        DocumentReadOptions { max_source_bytes: 2, ..Default::default() }, &budget, allocation(1), || false).err(), Some(DocumentReadError::SourceLimit));
    let invalid = capture(&[0xff, 0xfe, b'#', 0]);
    assert_eq!(DocumentReader::prepare(&invalid, id(), generation(1), Default::default(), &budget, allocation(1), || false).err(),
        Some(DocumentReadError::InvalidUtf8));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(DocumentReader::prepare(&source, id(), generation(1), Default::default(), &tiny, allocation(1), || false).err(),
        Some(DocumentReadError::ResourceDenied));
    assert!(DocumentReadOptions { width_columns: 0, ..Default::default() }.validate().is_err());
}

#[test]
fn layout_budget_failure_is_not_a_truncated_document_claim() {
    let source = capture(b"# first\n\n## second\n\nthird paragraph\n"); let budget = budget();
    let result = DocumentReader::prepare(&source, id(), generation(1),
        DocumentReadOptions { max_flow_lines: 1, ..Default::default() }, &budget, allocation(1), || false);
    assert_eq!(result.err(), Some(DocumentReadError::EngineBudget));
}

#[test]
fn cancellation_can_discard_preparation_without_affecting_an_older_reader() {
    let source = capture(b"# Retained\n\nStill readable.\n"); let budget = budget(); let old = read(&source, &budget);
    let mut polls = 0;
    let result = DocumentReader::prepare(&source, id(), generation(2), Default::default(), &budget, allocation(2), || {
        polls += 1; polls >= 3
    });
    assert_eq!(result.err(), Some(DocumentReadError::Canceled));
    assert!(old.window(0, 10).unwrap().whole_document_visible());
    old.validate_delivery(&source, generation(1)).unwrap();
}

#[test]
fn empty_and_bom_only_documents_have_valid_empty_views() {
    for bytes in [b"".as_slice(), &[0xef, 0xbb, 0xbf]] {
        let source = capture(bytes); let budget = budget(); let reader = read(&source, &budget);
        assert_eq!(reader.total_lines(), 0); assert!(reader.headings().is_empty());
        assert!(reader.window(0, 10).unwrap().whole_document_visible());
    }
}

#[test]
fn rendered_offset_navigation_uses_the_flow_domain_and_checks_scalar_boundaries() {
    let source = capture("\u{feff}# Intro\n\nA **Café 😀** paragraph with enough text to wrap.\n".as_bytes());
    let budget = budget();
    let reader = DocumentReader::prepare(&source, id(), generation(1),
        DocumentReadOptions { width_columns: 14, ..Default::default() },
        &budget, allocation(1), || false).unwrap();
    let mut visited = 0;
    for first in 0..reader.total_lines() {
        let page = reader.window(first, 1).unwrap();
        let line = &page.lines()[0];
        if line.rendered_range.is_empty() { continue; }
        let target = reader.window_at_rendered(line.rendered_range.start, 1).unwrap();
        assert_eq!(target.first_index(), first);
        assert_eq!(target.lines()[0].rendered_text, line.rendered_text);
        visited += 1;
    }
    assert!(visited > 2);
    let middle = reader.rendered_text().find('é').unwrap() + 1;
    assert_eq!(reader.window_at_rendered(middle, 1).err(), Some(DocumentReadError::InvalidRange));
    assert_eq!(reader.window_at_rendered(usize::MAX, 1).err(), Some(DocumentReadError::InvalidRange));
    for count in [0, 1025] {
        assert_eq!(reader.window_at_rendered(0, count).err(), Some(DocumentReadError::InvalidRange));
    }
    let eof = reader.window_at_rendered(reader.rendered_text().len(), 1).unwrap();
    assert_eq!(eof.first_index(), reader.total_lines());
    assert!(eof.lines().is_empty());
    assert_eq!(reader.capture().bytes(), source.bytes());
}

#[test]
fn every_rendered_scalar_boundary_has_flow_context_even_in_layout_separator_gaps() {
    for bytes in [b"# Heading\n\nA paragraph.\n\nAnother paragraph.\n".as_slice(), b"```\n```\n".as_slice()] {
        let source = capture(bytes); let budget = budget(); let reader = read(&source, &budget);
        let mut rows = Vec::new();
        for first in 0..reader.total_lines() {
            let page = reader.window(first, 1).unwrap();
            rows.push((page.lines()[0].rendered_range.end, first));
        }
        assert!(!reader.rendered_text().is_empty());
        for (offset, _) in reader.rendered_text().char_indices() {
            let window = reader.window_at_rendered(offset, 1).unwrap();
            let expected = rows.iter().find(|&&(end, _)| offset < end)
                .or_else(|| rows.last()).map_or(0, |row| row.1);
            assert_eq!(window.first_index(), expected);
            if rows.is_empty() { assert!(window.lines().is_empty()); }
        }
        assert!(reader.window_at_rendered(reader.rendered_text().len(), 1).unwrap().lines().is_empty());
    }
}
