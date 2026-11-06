#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! Production unit and boundary tests for source selections, grapheme hit-testing,
//! copy modes, bounded clipboard, and streamed export (FCB-019.B / fcb-gjo.2).
//!
//! Required verification cases:
//! 1. `cheap_anchored_giant_selections` — O(1) representation of 500 MB file selections without memory allocation.
//! 2. `grapheme_and_affinity_hit_testing` — leading vs trailing caret affinity, tab expansion, and end-of-line snapping.
//! 3. `exact_byte_copy_vs_decoded_unicode_with_utf16_bom` — UTF-16 LE/BE BOM retention in raw bytes vs decoded representation.
//! 4. `exact_byte_copy_vs_decoded_unicode_with_malformed_utf8` — raw invalid bytes preserved, decoded replacements explicitly flagged.
//! 5. `exact_byte_copy_crlf_and_bidi_logical_order` — CRLF and bidirectional text byte preservation.
//! 6. `markdown_markers_and_location_provenance` — code fence integrity and location provenance header formatting.
//! 7. `clipboard_budget_refusal_preserves_old_clipboard` — oversized selection refusal leaves clipboard untouched.
//! 8. `clipboard_cancellation_and_stale_capture_refusal` — pre-publication cancellation and stale source/generation refusal.
//! 9. `concurrent_external_clipboard_change_and_native_failure` — generation token mismatch prevents stale overwrite; publication failure is safe.
//! 10. `streamed_file_export_chunked_and_cancellation` — streaming alternative for oversized selections with cooperative cancellation.

use fcb::{ArenaOwnerId, ByteOffset, FileId, SourceCapture, SourceRevision};
use fcb::search::{
    hit_test_line, CaretAffinity, ClipboardError, ClipboardLimits, ExportOutcome,
    NativeClipboard, QueryGeneration, SourceAnchor, SourceSelection, StagedClipboardData,
    StreamedExportOptions, StreamedFileExport,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(920).unwrap()
}

fn file(id: u64) -> FileId {
    FileId::new(owner(), id).unwrap()
}

fn revision(id: u64) -> SourceRevision {
    SourceRevision::new(owner(), id).unwrap()
}

fn generation(id: u64) -> QueryGeneration {
    QueryGeneration::new(owner(), id).unwrap()
}

fn make_source(id: u64, rev: u64, path: &str, bytes: Vec<u8>) -> SourceCapture {
    SourceCapture::from_bytes(owner(), file(id), revision(rev), path, bytes).unwrap()
}

fn select_range(capture: &SourceCapture, start: usize, end: usize) -> SourceSelection {
    SourceSelection::new(
        capture.file(), capture.revision(), generation(1),
        SourceAnchor::new(ByteOffset::new(start as u64), 1, 0),
        SourceAnchor::new(ByteOffset::new(end as u64), 1, 0),
    ).unwrap()
}

fn utf16_source(text: &str, big_endian: bool) -> Vec<u8> {
    let mut bytes = if big_endian { vec![0xFE, 0xFF] } else { vec![0xFF, 0xFE] };
    for unit in text.encode_utf16() {
        let pair = if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() };
        bytes.extend_from_slice(&pair);
    }
    bytes
}

#[test]
fn cheap_anchored_giant_selections() {
    // A 500 MB selection descriptor
    let five_hundred_mb = 500 * 1024 * 1024;
    let start = SourceAnchor::new(ByteOffset::new(0), 1, 0);
    let end = SourceAnchor::new(ByteOffset::new(five_hundred_mb), 10_000_000, 42);

    let selection = SourceSelection::new(
        file(1),
        revision(1),
        generation(1),
        start,
        end,
    )
    .unwrap();

    // Must be O(1) and exact
    assert!(!selection.is_empty());
    assert_eq!(selection.byte_len(), five_hundred_mb);
    assert_eq!(selection.start.line, 1);
    assert_eq!(selection.end.line, 10_000_000);
    assert_eq!(selection.byte_range.len().get(), five_hundred_mb);

    // Empty selection
    let empty_anchor = SourceAnchor::new(ByteOffset::new(100), 5, 10);
    let empty_selection = SourceSelection::new(
        file(1),
        revision(1),
        generation(1),
        empty_anchor,
        empty_anchor,
    )
    .unwrap();
    assert!(empty_selection.is_empty());
    assert_eq!(empty_selection.byte_len(), 0);
}

#[test]
fn grapheme_and_affinity_hit_testing() {
    let line = "let x = 42;\n";

    // Hit test at column 0 ('l') -> Leading affinity
    let hit0 = hit_test_line(line, 0, 4);
    assert_eq!(hit0.byte_offset, 0);
    assert_eq!(hit0.char_index, 0);
    assert_eq!(hit0.affinity, CaretAffinity::Leading);
    assert!(hit0.is_exact_boundary);

    // Hit test in middle of tab stop
    let line_tab = "a\tb";
    // 'a' is col 0, tab advances to col 4, 'b' is col 4
    let hit_tab_lead = hit_test_line(line_tab, 1, 4); // First half of tab
    assert_eq!(hit_tab_lead.char_index, 1);
    assert_eq!(hit_tab_lead.byte_offset, 1);
    assert_eq!(hit_tab_lead.affinity, CaretAffinity::Leading);

    let hit_tab_trail = hit_test_line(line_tab, 3, 4); // Second half of tab
    assert_eq!(hit_tab_trail.char_index, 1);
    assert_eq!(hit_tab_trail.byte_offset, 1);
    assert_eq!(hit_tab_trail.affinity, CaretAffinity::Trailing);

    // Hit test past end of line
    let hit_eol = hit_test_line(line, 100, 4);
    assert_eq!(hit_eol.char_index, line.chars().count());
    assert_eq!(hit_eol.affinity, CaretAffinity::Trailing);
    assert!(hit_eol.is_exact_boundary);
}

#[test]
fn exact_byte_copy_vs_decoded_unicode_with_utf16_bom() {
    let text = "Hi Ω🦀\r\n";
    for big_endian in [false, true] {
        let bytes = utf16_source(text, big_endian);
        let capture = make_source(2, 1, "utf16.txt", bytes.clone());
        let selection = select_range(&capture, 0, bytes.len());
        let staged = StagedClipboardData::stage(
            &selection, &capture, generation(1), ClipboardLimits::default(), || false,
        ).unwrap();

        assert_eq!(staged.exact_bytes, bytes);
        assert_eq!(staged.plain_text, text);
        assert!(!staged.has_replacements);
    }
}

#[test]
fn exact_byte_copy_vs_decoded_unicode_with_malformed_utf8() {
    // Valid text with invalid UTF-8 byte sequences: [0x41, 0xFF, 0xFE, 0x42] ("A", invalid, "B")
    let invalid_bytes = vec![0x41, 0xFF, 0xFE, 0x42];
    let capture = make_source(3, 1, "bad_utf8.bin", invalid_bytes.clone());

    let selection = SourceSelection::new(
        file(3),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(4), 1, 4),
    )
    .unwrap();

    let staged = StagedClipboardData::stage(
        &selection,
        &capture,
        generation(1),
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    // Exact bytes preserve the raw invalid bytes
    assert_eq!(staged.exact_bytes, invalid_bytes);
    // Decoded text discloses the presence of replacement characters
    assert!(staged.has_replacements);
    assert!(staged.plain_text.contains('\u{FFFD}'));
}

#[test]
fn exact_byte_copy_crlf_and_bidi_logical_order() {
    // 1. CRLF line endings
    let crlf = b"row 1\r\nrow 2\r\n";
    let capture_crlf = make_source(4, 1, "crlf.txt", crlf.to_vec());

    let sel_crlf = SourceSelection::new(
        file(4),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(crlf.len() as u64), 2, 5),
    )
    .unwrap();

    let staged_crlf = StagedClipboardData::stage(
        &sel_crlf,
        &capture_crlf,
        generation(1),
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    // Verbatim CRLF preserved, not normalized to LF!
    assert_eq!(staged_crlf.exact_bytes, crlf);
    assert_eq!(staged_crlf.plain_text, "row 1\r\nrow 2\r\n");
    assert!(!staged_crlf.has_replacements);

    // 2. Bidirectional Hebrew: "שלום עולם" (Hello World)
    let hebrew = "שלום עולם\n";
    let capture_hebrew = make_source(5, 1, "hebrew.txt", hebrew.as_bytes().to_vec());

    let sel_hebrew = SourceSelection::new(
        file(5),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(hebrew.len() as u64), 1, 10),
    )
    .unwrap();

    let staged_hebrew = StagedClipboardData::stage(
        &sel_hebrew,
        &capture_hebrew,
        generation(1),
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    // Logical source order preserved verbatim
    assert_eq!(staged_hebrew.exact_bytes, hebrew.as_bytes());
    assert_eq!(staged_hebrew.plain_text, hebrew);
}

#[test]
fn markdown_markers_and_location_provenance() {
    let md = "```rust\nfn hello() {}\n```\n";
    let capture = make_source(6, 1, "doc.md", md.as_bytes().to_vec());

    let selection = SourceSelection::new(
        file(6),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(md.len() as u64), 3, 3),
    )
    .unwrap();

    let staged = StagedClipboardData::stage(
        &selection,
        &capture,
        generation(1),
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    // Code fences preserved exactly without phantom markers
    assert_eq!(staged.exact_bytes, md.as_bytes());
    assert_eq!(staged.plain_text, md);

    // Location provenance header formatting
    assert!(staged.provenance.contains("File: doc.md"));
    assert!(staged.provenance.contains("Lines: 1-3"));
    assert!(staged.provenance.contains(&format!("Range: 0..{}", md.len())));
}

#[test]
fn clipboard_budget_refusal_preserves_old_clipboard() {
    let mut clipboard = NativeClipboard::new();
    clipboard.simulate_external_change("preserved previous text");

    let initial_text = clipboard.plain_text().unwrap().to_string();
    let initial_sync_tag = clipboard.generation_seq();

    // Generate payload of 100 bytes
    let bytes = vec![b'X'; 100];
    let capture = make_source(7, 1, "test.txt", bytes);

    let selection = SourceSelection::new(
        file(7),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(100), 1, 100),
    )
    .unwrap();

    // Limit set to 50 bytes -> MUST be refused
    let limits = ClipboardLimits {
        max_clipboard_bytes: 50,
    };

    let result = StagedClipboardData::stage(&selection, &capture, generation(1), limits, || false);
    assert_eq!(
        result,
        Err(ClipboardError::BudgetExceeded {
            requested_bytes: 100,
            max_budget_bytes: 50,
        })
    );

    // Crucial invariant: existing clipboard remains completely untouched
    assert_eq!(clipboard.plain_text(), Some(initial_text.as_str()));
    assert_eq!(clipboard.generation_seq(), initial_sync_tag);
}

#[test]
fn clipboard_cancellation_and_stale_capture_refusal() {
    let clipboard = NativeClipboard::new();
    let capture = make_source(8, 1, "test.rs", b"let a = 1;".to_vec());

    let selection = SourceSelection::new(
        file(8),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(10), 1, 10),
    )
    .unwrap();

    // 1. Cancellation before staging
    let cancel_res = StagedClipboardData::stage(
        &selection,
        &capture,
        generation(1),
        ClipboardLimits::default(),
        || true, // Canceled!
    );
    assert_eq!(cancel_res, Err(ClipboardError::Canceled));
    assert_eq!(clipboard.plain_text(), None);

    // 2. Stale capture revision
    let capture_rev2 = make_source(8, 2, "test.rs", b"let a = 1;".to_vec());
    let stale_rev_res = StagedClipboardData::stage(
        &selection,
        &capture_rev2,
        generation(1),
        ClipboardLimits::default(),
        || false,
    );
    assert_eq!(stale_rev_res, Err(ClipboardError::StaleSource));

    // 3. Stale query generation
    let stale_gen_res = StagedClipboardData::stage(
        &selection,
        &capture,
        generation(2), // Active generation is 2, but selection is generation 1
        ClipboardLimits::default(),
        || false,
    );
    assert_eq!(stale_gen_res, Err(ClipboardError::StaleGeneration));
}

#[test]
fn concurrent_external_clipboard_change_and_native_failure() {
    let mut clipboard = NativeClipboard::new();
    let capture = make_source(9, 1, "concurrent.rs", b"hello world".to_vec());

    let selection = SourceSelection::new(
        file(9),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(11), 1, 11),
    )
    .unwrap();

    let staged = StagedClipboardData::stage(
        &selection,
        &capture,
        generation(1),
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    let initial_sync_tag = clipboard.generation_seq();

    // Simulate external clipboard change before publication finishes
    clipboard.simulate_external_change("another application copied this!");
    let external_sync_tag = clipboard.generation_seq();
    assert_ne!(initial_sync_tag, external_sync_tag);

    // Attempting publication with old token MUST fail with ConcurrentExternalChange
    let err_conflict = clipboard.publish(staged.clone(), initial_sync_tag);
    assert_eq!(err_conflict, Err(ClipboardError::ConcurrentExternalChange));
    // The external application's text must NOT be overwritten!
    assert_eq!(
        clipboard.plain_text(),
        Some("another application copied this!")
    );

    // Successful publication with current token
    let ok = clipboard.publish(staged.clone(), external_sync_tag);
    assert!(ok.is_ok());
    assert_eq!(clipboard.plain_text(), Some("hello world"));

    // Native publication failure injection
    clipboard.inject_publication_failure(true);
    let curr_sync_tag = clipboard.generation_seq();
    let err_fail = clipboard.publish(staged, curr_sync_tag);
    assert_eq!(err_fail, Err(ClipboardError::PublicationFailed));
    // Clipboard contents preserved
    assert_eq!(clipboard.plain_text(), Some("hello world"));
}

#[test]
fn streamed_file_export_chunked_and_cancellation() {
    // 256 KiB content
    let size = 256 * 1024;
    let mut data = Vec::with_capacity(size);
    for i in 0..size {
        data.push((i % 251) as u8);
    }

    let capture = make_source(10, 1, "large.bin", data.clone());

    let selection = SourceSelection::new(
        file(10),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(size as u64), 1, size as u32),
    )
    .unwrap();

    // 1. Full streamed export in 32 KiB chunks
    let mut output = Vec::new();
    let options = StreamedExportOptions {
        chunk_size: 32 * 1024,
    };

    let outcome = StreamedFileExport::stream_to_writer(
        &selection,
        &capture,
        generation(1),
        options,
        &mut output,
        || false,
    )
    .unwrap();

    assert_eq!(outcome, ExportOutcome::Completed { total_bytes: size });
    assert_eq!(output, data);

    // 2. Cooperative cancellation mid-stream
    let mut partial_output = Vec::new();
    let mut count = 0;
    let cancel_outcome = StreamedFileExport::stream_to_writer(
        &selection,
        &capture,
        generation(1),
        options,
        &mut partial_output,
        || {
            count += 1;
            count > 2 // Cancel after 2 chunks
        },
    )
    .unwrap();

    if let ExportOutcome::Canceled { bytes_written } = cancel_outcome {
        assert!(bytes_written > 0 && bytes_written < size);
        assert_eq!(partial_output.len(), bytes_written);
    } else {
        assert!(matches!(cancel_outcome, ExportOutcome::Canceled { .. }));
    }
}

#[test]
fn selected_unicode_uses_capture_encoding_and_keeps_interior_bom() {
    for big_endian in [false, true] {
        let bytes = utf16_source("A\u{FEFF}🦀Z", big_endian);
        let capture = make_source(11, 1, "interior.txt", bytes.clone());
        let selection = select_range(&capture, 4, bytes.len() - 2);
        let staged = StagedClipboardData::stage(
            &selection, &capture, generation(1), ClipboardLimits::default(), || false,
        ).unwrap();
        assert_eq!(staged.plain_text, "\u{FEFF}🦀");
        assert_eq!(staged.exact_bytes, bytes[4..bytes.len() - 2]);
        assert!(!staged.has_replacements);
    }

    let bytes = b"\xEF\xBB\xBFA\xEF\xBB\xBFZ";
    let capture = make_source(12, 1, "utf8_bom.txt", bytes.to_vec());
    for (start, end, expected) in [(0, bytes.len(), "A\u{FEFF}Z"), (4, 7, "\u{FEFF}")] {
        let selection = select_range(&capture, start, end);
        let staged = StagedClipboardData::stage(
            &selection, &capture, generation(1), ClipboardLimits::default(), || false,
        ).unwrap();
        assert_eq!(staged.plain_text, expected);
        assert_eq!(staged.exact_bytes, bytes[start..end]);
        assert!(!staged.has_replacements);
    }
}

#[test]
fn clipboard_decoding_keeps_scalars_across_bounded_chunks() {
    // A high surrogate ends the first 4 KiB raw chunk; its low surrogate
    // belongs to the next. A later U+FEFF is content, not another BOM.
    let text = format!("{}🦀\u{FEFF}\u{FFFD}Z", "a".repeat(2046));
    for big_endian in [false, true] {
        let bytes = utf16_source(&text, big_endian);
        let capture = make_source(13, 1, "split_utf16.txt", bytes.clone());
        let selection = select_range(&capture, 0, bytes.len());
        let staged = StagedClipboardData::stage(
            &selection, &capture, generation(1), ClipboardLimits::default(), || false,
        ).unwrap();
        assert_eq!(staged.plain_text, text);
        assert_eq!(staged.exact_bytes, bytes);
        assert!(!staged.has_replacements, "literal U+FFFD is not a decoding failure");
    }

    let text = format!("{}🦀\u{FEFF}Z", "a".repeat(4095));
    let capture = make_source(14, 1, "split_utf8.txt", text.as_bytes().to_vec());
    let selection = select_range(&capture, 0, text.len());
    let staged = StagedClipboardData::stage(
        &selection, &capture, generation(1), ClipboardLimits::default(), || false,
    ).unwrap();
    assert_eq!(staged.plain_text, text);
    assert_eq!(staged.exact_bytes, text.as_bytes());
    assert!(!staged.has_replacements);
}

#[test]
fn selected_partial_scalars_are_explicit_and_unaligned_utf16_is_refused() {
    for big_endian in [false, true] {
        let bytes = utf16_source("🦀Z", big_endian);
        let capture = make_source(15, 1, "partial_utf16.txt", bytes.clone());
        // Select a high surrogate, a low surrogate, or one byte of a unit.
        // Excluded source bytes must not be borrowed to complete a scalar.
        for (start, end) in [(2, 4), (4, 6), (2, 3)] {
            let selection = select_range(&capture, start, end);
            let staged = StagedClipboardData::stage(
                &selection, &capture, generation(1), ClipboardLimits::default(), || false,
            ).unwrap();
            assert_eq!(staged.plain_text, "\u{FFFD}");
            assert_eq!(staged.exact_bytes, bytes[start..end]);
            assert!(staged.has_replacements);
        }

        let unaligned = select_range(&capture, 3, 6);
        assert_eq!(StagedClipboardData::stage(
            &unaligned, &capture, generation(1), ClipboardLimits::default(), || false,
        ), Err(ClipboardError::InvalidRange));
        let mut raw = Vec::new();
        assert_eq!(StreamedFileExport::stream_to_writer(
            &unaligned, &capture, generation(1), StreamedExportOptions::default(), &mut raw, || false,
        ).unwrap(), ExportOutcome::Completed { total_bytes: 3 });
        assert_eq!(raw, bytes[3..6]);
    }

    let capture = make_source(16, 1, "partial_utf8.txt", "é".as_bytes().to_vec());
    let selection = select_range(&capture, 1, 2);
    let staged = StagedClipboardData::stage(
        &selection, &capture, generation(1), ClipboardLimits::default(), || false,
    ).unwrap();
    assert_eq!(staged.plain_text, "\u{FFFD}");
    assert_eq!(staged.exact_bytes, [0xA9]);
    assert!(staged.has_replacements);
}

#[test]
fn clipboard_budget_counts_all_flavors_at_the_exact_byte_boundary() {
    for (bytes, text, replacements) in [
        (Vec::new(), "", false),
        (b"ab".to_vec(), "ab", false),
        (vec![0xFF], "\u{FFFD}", true),
        (utf16_source("Ω🦀", false), "Ω🦀", false),
    ] {
        let capture = make_source(17, 1, "café.rs", bytes.clone());
        let selection = select_range(&capture, 0, bytes.len());
        let provenance = format!(
            "File: café.rs\nRevision: 1\nLines: 1-1\nRange: 0..{}\n", bytes.len(),
        );
        let expected_total = bytes.len() + text.len() + provenance.len();
        // This used to overflow the limit * 3 comparison even for tiny input.
        let staged = StagedClipboardData::stage(
            &selection, &capture, generation(1),
            ClipboardLimits { max_clipboard_bytes: usize::MAX }, || false,
        ).unwrap();
        assert_eq!(staged.total_staged_bytes, expected_total);
        assert_eq!(staged.plain_text, text);
        assert_eq!(staged.exact_bytes, bytes);
        assert_eq!(staged.provenance, provenance);
        assert_eq!(staged.has_replacements, replacements);

        assert_eq!(StagedClipboardData::stage(
            &selection, &capture, generation(1),
            ClipboardLimits { max_clipboard_bytes: expected_total }, || false,
        ).unwrap(), staged);
        assert_eq!(StagedClipboardData::stage(
            &selection, &capture, generation(1),
            ClipboardLimits { max_clipboard_bytes: expected_total - 1 }, || false,
        ), Err(ClipboardError::BudgetExceeded {
            requested_bytes: expected_total,
            max_budget_bytes: expected_total - 1,
        }));
    }
}

#[test]
fn clipboard_decode_and_materialization_remain_cancelable() {
    let capture = make_source(18, 1, "cancel.txt", vec![b'x'; 16 * 1024]);
    let selection = select_range(&capture, 0, capture.bytes().len());
    for cancel_at in [3, 8, 14] {
        let checks = std::cell::Cell::new(0);
        let result = StagedClipboardData::stage(
            &selection, &capture, generation(1), ClipboardLimits::default(), || {
                checks.set(checks.get() + 1);
                checks.get() >= cancel_at
            },
        );
        assert_eq!(result, Err(ClipboardError::Canceled));
        assert_eq!(checks.get(), cancel_at);
    }
}

struct RecordingWriter {
    bytes: Vec<u8>,
    flushes: usize,
    largest_request: usize,
    max_write: usize,
    fail_after: Option<usize>,
}

impl Default for RecordingWriter {
    fn default() -> Self {
        Self {
            bytes: Vec::new(), flushes: 0, largest_request: 0,
            max_write: usize::MAX, fail_after: None,
        }
    }
}

impl std::io::Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.fail_after.is_some_and(|limit| self.bytes.len() >= limit) {
            return Err(std::io::ErrorKind::Other.into());
        }
        self.largest_request = self.largest_request.max(bytes.len());
        let remaining = self.fail_after.map_or(usize::MAX, |limit| limit - self.bytes.len());
        let count = bytes.len().min(self.max_write).min(remaining);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn invalid_and_inconsistent_selections_never_reach_the_export_writer() {
    let capture = make_source(19, 1, "bounds.bin", vec![b'x'; 4096]);
    let mut wrong_start = select_range(&capture, 0, 4096);
    wrong_start.start.byte_offset = ByteOffset::new(1);
    let mut wrong_end = select_range(&capture, 0, 4096);
    wrong_end.end.byte_offset = ByteOffset::new(4095);
    for selection in [
        select_range(&capture, 1, 4097),
        select_range(&capture, 4097, 4097),
        wrong_start,
        wrong_end,
    ] {
        let mut writer = RecordingWriter::default();
        assert_eq!(StreamedFileExport::stream_to_writer(
            &selection, &capture, generation(1),
            StreamedExportOptions { chunk_size: 1024 }, &mut writer, || false,
        ), Err(ClipboardError::InvalidRange));
        assert!(writer.bytes.is_empty());
        assert_eq!(writer.largest_request, 0);
        assert_eq!(writer.flushes, 0);
        assert_eq!(StagedClipboardData::stage(
            &selection, &capture, generation(1), ClipboardLimits::default(), || false,
        ), Err(ClipboardError::InvalidRange));
    }
}

#[test]
fn streamed_export_bounds_extreme_chunks_and_checks_empty_cancellation() {
    let bytes: Vec<u8> = (0..131_077).map(|value| (value % 251) as u8).collect();
    let capture = make_source(20, 1, "chunks.bin", bytes.clone());
    let selection = select_range(&capture, 3, bytes.len() - 2);
    let mut writer = RecordingWriter::default();
    assert_eq!(StreamedFileExport::stream_to_writer(
        &selection, &capture, generation(1),
        StreamedExportOptions { chunk_size: usize::MAX }, &mut writer, || false,
    ).unwrap(), ExportOutcome::Completed { total_bytes: bytes.len() - 5 });
    assert_eq!(writer.bytes, bytes[3..bytes.len() - 2]);
    assert!(writer.largest_request <= 64 * 1024);
    assert_eq!(writer.flushes, 1);

    let selection = select_range(&capture, 3, 11);
    let mut writer = RecordingWriter::default();
    assert_eq!(StreamedFileExport::stream_to_writer(
        &selection, &capture, generation(1),
        StreamedExportOptions { chunk_size: 0 }, &mut writer, || false,
    ).unwrap(), ExportOutcome::Completed { total_bytes: 8 });
    assert_eq!(writer.bytes, bytes[3..11]);
    assert_eq!(writer.largest_request, 1);

    let empty = select_range(&capture, bytes.len(), bytes.len());
    let mut writer = RecordingWriter::default();
    assert_eq!(StreamedFileExport::stream_to_writer(
        &empty, &capture, generation(1), StreamedExportOptions::default(),
        &mut writer, || true,
    ).unwrap(), ExportOutcome::Canceled { bytes_written: 0 });
    assert!(writer.bytes.is_empty());
    assert_eq!(writer.flushes, 0);
}

#[test]
fn streamed_export_counts_short_writes_before_cancellation_or_io_failure() {
    let bytes = b"0123456789abcdef";
    let capture = make_source(21, 1, "short.bin", bytes.to_vec());
    let selection = select_range(&capture, 1, 15);
    let mut writer = RecordingWriter { max_write: 3, ..RecordingWriter::default() };
    let mut checks = 0;
    assert_eq!(StreamedFileExport::stream_to_writer(
        &selection, &capture, generation(1), StreamedExportOptions::default(),
        &mut writer, || { checks += 1; checks > 2 },
    ).unwrap(), ExportOutcome::Canceled { bytes_written: 6 });
    assert_eq!(writer.bytes, bytes[1..7]);
    assert_eq!(writer.flushes, 0);

    let mut writer = RecordingWriter { fail_after: Some(3), ..RecordingWriter::default() };
    assert_eq!(StreamedFileExport::stream_to_writer(
        &selection, &capture, generation(1), StreamedExportOptions::default(),
        &mut writer, || false,
    ), Err(ClipboardError::PublicationFailed));
    assert_eq!(writer.bytes, bytes[1..4]);
    assert_eq!(writer.flushes, 0);
}

#[derive(Clone, Copy)]
enum WriteFault { Interrupted, Zero, TooLong }

struct FaultyWriter { fault: WriteFault, attempts: usize, flushes: usize }

impl std::io::Write for FaultyWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.attempts += 1;
        match self.fault {
            WriteFault::Interrupted => Err(std::io::ErrorKind::Interrupted.into()),
            WriteFault::Zero => Ok(0),
            WriteFault::TooLong => Ok(bytes.len() + 1),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn streamed_export_refuses_nonprogressing_writers_and_polls_interrupts() {
    let capture = make_source(22, 1, "writer.bin", b"data".to_vec());
    let selection = select_range(&capture, 0, 4);
    for (fault, attempts) in [
        (WriteFault::Interrupted, 16), (WriteFault::Zero, 1), (WriteFault::TooLong, 1),
    ] {
        let mut writer = FaultyWriter { fault, attempts: 0, flushes: 0 };
        assert_eq!(StreamedFileExport::stream_to_writer(
            &selection, &capture, generation(1), StreamedExportOptions::default(),
            &mut writer, || false,
        ), Err(ClipboardError::PublicationFailed));
        assert_eq!(writer.attempts, attempts);
        assert_eq!(writer.flushes, 0);
    }

    let mut writer = FaultyWriter { fault: WriteFault::Interrupted, attempts: 0, flushes: 0 };
    let mut checks = 0;
    assert_eq!(StreamedFileExport::stream_to_writer(
        &selection, &capture, generation(1), StreamedExportOptions::default(),
        &mut writer, || { checks += 1; checks > 2 },
    ).unwrap(), ExportOutcome::Canceled { bytes_written: 0 });
    assert_eq!(writer.attempts, 2);
    assert_eq!(writer.flushes, 0);
}
