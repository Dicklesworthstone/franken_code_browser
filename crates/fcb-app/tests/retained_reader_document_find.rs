#![forbid(unsafe_code)]

// fcb-1tar: rendered Markdown Find consumes the real shared flow and matcher.
mod support;
use support::{Json, parse};
use std::path::Path;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::document::reader::{DocumentReader, DocumentReadError};
use fcb_core::{DocumentGeneration, DocumentId, ResourceAllocationId, ResourceBudget};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, ReaderDocumentOptions,
    MAX_READER_DOCUMENT_FIND_NEEDLE_BYTES, MAX_READER_DOCUMENT_FIND_PAGE,
    MAX_READER_DOCUMENT_FIND_BYTES};

fn reader(source: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(8838).unwrap(), Path::new("README.md"),
        source, || false).unwrap()
}
fn json(response: &fcb_app::host::HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn options(width: u32) -> ReaderDocumentOptions {
    ReaderDocumentOptions { width_columns: width, max_source_bytes: 256 * 1024, ..Default::default() }
}
fn rendered(reader: &ReaderSession, options: ReaderDocumentOptions) -> (String, Vec<(usize, usize, usize)>) {
    let owner = reader.capture().request().file().owner();
    let budget = ResourceBudget::new(owner, ByteLength::new(128 * 1024 * 1024)).unwrap();
    let document = DocumentReader::prepare(reader.capture(), DocumentId::new(owner, 1).unwrap(),
        DocumentGeneration::new(owner, 1).unwrap(), options, &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let mut rows = Vec::new();
    let mut first = 0;
    while first < document.total_lines() {
        let page = document.window(first, 128).unwrap();
        for row in page.lines() { rows.push((row.rendered_range.start, row.rendered_range.end, row.line_index)); }
        first += page.lines().len();
    }
    (document.rendered_text().to_owned(), rows)
}
fn expected(text: &str, needle: &str) -> Vec<(usize, usize)> {
    text.as_bytes().windows(needle.len()).enumerate()
        .filter_map(|(start, bytes)| (bytes == needle.as_bytes()).then_some((start, start + needle.len()))).collect()
}
fn assert_page(page: &Json, needle: &str, first: usize, count: usize,
    text: &str, rows: &[(usize, usize, usize)]) {
    assert_eq!(page.get("command").text(), "document-find");
    assert_eq!(page.get("needle").text(), needle);
    assert_eq!(page.get("search_domain").text(), "rendered-text-utf8");
    assert!(page.get("case_sensitive").flag());
    assert_eq!(page.get("normalization").text(), "exact");
    assert!(page.get("overlapping").flag());
    assert!(page.get("match_count_complete").flag());
    assert!(!page.get("native_shaped").flag());
    assert_eq!(page.get("additional_source_bytes_read").number(), 0);
    assert_eq!(page.get("max_scan_bytes").number(), MAX_READER_DOCUMENT_FIND_BYTES as u64);
    assert_eq!(page.get("scanned_rendered_bytes").number(), text.len() as u64);
    assert_eq!(page.get("rendered_utf8_bytes").number(), text.len() as u64);
    let matches = expected(text, needle);
    assert_eq!(page.get("first_match").number(), first as u64);
    assert_eq!(page.get("matches_seen").number(), matches.len() as u64);
    let end = (first + count).min(matches.len());
    assert_eq!(page.get("matches").array().len(), end - first);
    for (offset, row) in page.get("matches").array().iter().enumerate() {
        let (start, end) = matches[first + offset];
        assert_eq!(row.get("match_id").number(), (first + offset + 1) as u64);
        assert_eq!(row.get("rendered_utf8_range").get("start").number(), start as u64);
        assert_eq!(row.get("rendered_utf8_range").get("end").number(), end as u64);
        assert_eq!(&text[start..end], needle);
        let line = rows.iter().find(|&&(_, b, _)| start < b).or_else(|| rows.last()).map_or(0, |row| row.2);
        assert_eq!(row.get("first_flow_line").number(), line as u64);
        let Json::Object(fields) = row else { panic!("match is not an object"); };
        assert_eq!(fields.len(), 3, "Find must not grant original-source or glyph ranges");
    }
    if end < matches.len() { assert_eq!(page.get("next_match").number(), end as u64); }
    else { assert_eq!(page.get("next_match"), &Json::Null); }
}

#[test]
fn logical_phrases_cross_markup_and_keep_rendered_unicode_offsets() {
    let source = "\u{feff}# Café\r\n\r\nA **bold** phrase with é😀 and e\u{301}.\r\n\r\nAnother **bold** phrase.\r\n\r\nA [linked](docs/target.md) phrase.\r\n";
    assert!(!source.contains("bold phrase"));
    assert!(!source.contains("linked phrase"));
    let mut reader = reader(source.as_bytes());
    reader.prepare_document(1, options(100), || false).unwrap();
    let (text, rows) = rendered(&reader, options(100));
    assert!(text.contains("linked phrase"), "upstream link labels join the rendered surrounding prose");
    for needle in ["bold phrase", "linked phrase", "é😀", "e\u{301}", "Bold phrase", "É😀", "**bold**"] {
        let response = reader.find_document(1, needle, 0, 64, || false).unwrap();
        assert_page(&json(&response), needle, 0, 64, &text, &rows);
    }
    let result = json(&reader.find_document(1, "bold phrase", 0, 64, || false).unwrap());
    assert_eq!(result.get("matches_seen").number(), 2);
    let row = &result.get("matches").array()[0];
    let start = row.get("rendered_utf8_range").get("start").number() as usize;
    let end = row.get("rendered_utf8_range").get("end").number() as usize;
    let shown = reader.document_window(1, row.get("first_flow_line").number() as usize, 4, || false).unwrap();
    assert!(shown.as_str().contains("bold phrase"));
    let source_page = json(&reader.document_selection_source(1, start, end, 64, || false).unwrap());
    let range = source_page.get("selection").get("original_range");
    let original = &source[range.get("start").number() as usize..range.get("end").number() as usize];
    assert!(original.contains("**bold** phrase"), "source action audits the enclosing Markdown, not the query's byte offsets");
    assert_eq!(reader.capture().bytes(), source.as_bytes());
}

#[test]
fn overlapping_pages_reach_beyond_the_ordinary_retained_hit_limit_and_replay_backwards() {
    let source = format!("# Dense\n\n{}\n", "a".repeat(12_000));
    let mut reader = reader(source.as_bytes());
    reader.prepare_document(1, options(100), || false).unwrap();
    let (text, rows) = rendered(&reader, options(100));
    let total = expected(&text, "aa").len();
    assert!(total > 4096);
    for first in [0, 37, 4096, total - 3, 37, 0, total] {
        let page = json(&reader.find_document(1, "aa", first, 37, || false).unwrap());
        assert_page(&page, "aa", first, 37, &text, &rows);
    }
    assert_eq!(reader.find_document(1, "aa", total + 1, 37, || false).err(), Some(ReaderSessionError::InvalidRange));
    assert_eq!(reader.document_generation(), Some(1));
}

#[test]
fn unqualified_source_mapping_keeps_rendered_find_and_flow_navigation_available() {
    let source = b"---\ntitle: metadata\n---\n\n# Target\n\nA **bold** phrase.\n";
    let mut reader = reader(source);
    reader.prepare_document(1, options(100), || false).unwrap();
    let (text, rows) = rendered(&reader, options(100));
    let page = json(&reader.find_document(1, "bold phrase", 0, 64, || false).unwrap());
    assert_page(&page, "bold phrase", 0, 64, &text, &rows);
    assert_eq!(page.get("matches_seen").number(), 1);
    let hit = &page.get("matches").array()[0];
    let start = hit.get("rendered_utf8_range").get("start").number() as usize;
    let end = hit.get("rendered_utf8_range").get("end").number() as usize;
    assert!(reader.document_window(1, hit.get("first_flow_line").number() as usize, 4, || false)
        .unwrap().as_str().contains("bold phrase"));
    assert_eq!(reader.document_selection_source(1, start, end, 16, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::InvalidOutput)));
    assert_eq!(reader.capture().bytes(), source);
}

#[test]
fn reflow_clear_and_refusals_never_relabel_document_or_source_query_state() {
    let mut reader = reader(b"# Heading\n\nA needle in this captured paragraph.\n");
    reader.search(1, "needle", 4, 4096, || false).unwrap();
    reader.prepare_document(1, options(100), || false).unwrap();
    let old = reader.find_document(1, "needle", 0, 64, || false).unwrap();
    reader.prepare_document(2, options(12), || false).unwrap();
    assert_eq!(reader.find_document(1, "needle", 0, 64, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
    let (text, rows) = rendered(&reader, options(12));
    assert_page(&json(&reader.find_document(2, "needle", 0, 64, || false).unwrap()),
        "needle", 0, 64, &text, &rows);
    let invalid = ReaderDocumentOptions { max_blocks: 0, ..options(100) };
    assert!(reader.prepare_document(3, invalid, || false).is_err());
    assert!(reader.find_document(2, "needle", 0, 64, || false).is_ok());
    reader.clear_document(4, || false).unwrap();
    assert_eq!(reader.find_document(2, "needle", 0, 64, || false).err(), Some(ReaderSessionError::MissingDocument));
    assert!(reader.copy_hit(1, 0, || false).unwrap().as_str().contains("6e6565646c65"));
    assert!(old.as_str().contains("\"document_generation\":\"1\""), "returned pages own their old identity");
}

#[test]
fn scan_and_publication_cancellation_preserve_the_accepted_capture_and_preview() {
    let paragraph = "This retained paragraph has ordinary words and one needle near its ending.\n\n";
    let source = format!("# Retained\n\n{}", paragraph.repeat(1100));
    let mut reader = reader(source.as_bytes());
    reader.prepare_document(1, options(100), || false).unwrap();
    let mut polls = 0;
    let accepted = reader.find_document(1, "absent", 0, 64, || { polls += 1; false }).unwrap();
    assert!(polls > 4, "scan must cross cooperative work quanta");
    for stop in [1, 2, 3, polls] {
        let mut calls = 0;
        assert_eq!(reader.find_document(1, "absent", 0, 64, || { calls += 1; calls == stop }).err(),
            Some(ReaderSessionError::Canceled));
        assert_eq!(reader.document_generation(), Some(1));
        assert_eq!(reader.capture().bytes(), source.as_bytes());
        assert_eq!(reader.find_document(1, "absent", 0, 64, || false).unwrap().as_str(), accepted.as_str());
    }
}

#[test]
fn query_page_and_generation_limits_fail_without_empty_successes() {
    let mut reader = reader(b"# Small\n\nbody\n");
    assert_eq!(reader.find_document(1, "body", 0, 1, || false).err(), Some(ReaderSessionError::MissingDocument));
    reader.prepare_document(1, options(100), || false).unwrap();
    for needle in ["".to_owned(), "a".repeat(MAX_READER_DOCUMENT_FIND_NEEDLE_BYTES + 1)] {
        assert_eq!(reader.find_document(1, &needle, 0, 1, || false).err(), Some(ReaderSessionError::InvalidLimits));
    }
    for count in [0, MAX_READER_DOCUMENT_FIND_PAGE + 1] {
        assert_eq!(reader.find_document(1, "body", 0, count, || false).err(), Some(ReaderSessionError::InvalidLimits));
    }
    assert_eq!(reader.find_document(1, "body", usize::MAX, 1, || false).err(), Some(ReaderSessionError::InvalidRange));
    assert!(reader.find_document(1, &"a".repeat(MAX_READER_DOCUMENT_FIND_NEEDLE_BYTES), 0, 1, || false).is_ok());
    let missing = json(&reader.find_document(1, "missing", 0, 1, || false).unwrap());
    assert_eq!(missing.get("matches_seen").number(), 0);
    assert!(missing.get("matches").array().is_empty());
    assert_eq!(missing.get("next_match"), &Json::Null);
    assert!(reader.document_window(1, 0, 4, || false).is_ok());
}

#[test]
fn layout_separators_are_findable_with_neighboring_or_empty_flow_context() {
    for source in [b"# Title\n\nA **bold** paragraph.\n\nLast paragraph.\n".as_slice(),
        b"```\n```\n".as_slice()] {
        let mut reader = reader(source);
        reader.prepare_document(1, options(100), || false).unwrap();
        let (text, rows) = rendered(&reader, options(100));
        assert!(!text.is_empty());
        if source.starts_with(b"```") { assert!(rows.is_empty(), "empty fenced code has separator-only logical text"); }
        for needle in [" ", "\n", "\n\n"] {
            let page = json(&reader.find_document(1, needle, 0, 128, || false).unwrap());
            assert_page(&page, needle, 0, 128, &text, &rows);
            for hit in page.get("matches").array() {
                let context = reader.document_window(1, hit.get("first_flow_line").number() as usize, 4, || false).unwrap();
                if rows.is_empty() {
                    assert_eq!(hit.get("first_flow_line").number(), 0);
                    assert!(json(&context).get("flow_lines").array().is_empty());
                }
            }
        }
    }
}
