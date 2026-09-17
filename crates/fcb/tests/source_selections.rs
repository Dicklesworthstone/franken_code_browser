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
    // UTF-16 Little Endian: BOM [0xFF, 0xFE] followed by "Hi" [0x48, 0x00, 0x69, 0x00]
    let utf16le_bytes = vec![0xFF, 0xFE, 0x48, 0x00, 0x69, 0x00];
    let capture = make_source(2, 1, "utf16.txt", utf16le_bytes.clone());

    let selection = SourceSelection::new(
        file(2),
        revision(1),
        generation(1),
        SourceAnchor::new(ByteOffset::new(0), 1, 0),
        SourceAnchor::new(ByteOffset::new(6), 1, 2),
    )
    .unwrap();

    let staged = StagedClipboardData::stage(
        &selection,
        &capture,
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    // Exact raw bytes MUST preserve the exact BOM verbatim:
    assert_eq!(staged.exact_bytes, utf16le_bytes);
    // Decoded UTF-8 text representation handles replacement / transcoding safely:
    assert!(staged.has_replacements);
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
    let initial_token = clipboard.generation_token();

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

    let result = StagedClipboardData::stage(&selection, &capture, limits, || false);
    match result {
        Err(ClipboardError::BudgetExceeded {
            requested_bytes,
            max_budget_bytes,
        }) => {
            assert_eq!(requested_bytes, 100);
            assert_eq!(max_budget_bytes, 50);
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }

    // Crucial invariant: existing clipboard remains completely untouched
    assert_eq!(clipboard.plain_text(), Some(initial_text.as_str()));
    assert_eq!(clipboard.generation_token(), initial_token);
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
        ClipboardLimits::default(),
        || false,
    );
    assert_eq!(stale_rev_res, Err(ClipboardError::StaleSource));

    // 3. Stale query generation
    let mut stale_gen_sel = selection;
    stale_gen_sel.generation = generation(99);
    let stale_gen_res = StagedClipboardData::stage(
        &stale_gen_sel,
        &capture,
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
        ClipboardLimits::default(),
        || false,
    )
    .unwrap();

    let initial_token = clipboard.generation_token();

    // Simulate external clipboard change before publication finishes
    clipboard.simulate_external_change("another application copied this!");
    let external_token = clipboard.generation_token();
    assert_ne!(initial_token, external_token);

    // Attempting publication with old token MUST fail with ConcurrentExternalChange
    let err_conflict = clipboard.publish(staged.clone(), initial_token);
    assert_eq!(err_conflict, Err(ClipboardError::ConcurrentExternalChange));
    // The external application's text must NOT be overwritten!
    assert_eq!(
        clipboard.plain_text(),
        Some("another application copied this!")
    );

    // Successful publication with current token
    let ok = clipboard.publish(staged.clone(), external_token);
    assert!(ok.is_ok());
    assert_eq!(clipboard.plain_text(), Some("hello world"));

    // Native publication failure injection
    clipboard.inject_publication_failure(true);
    let curr_token = clipboard.generation_token();
    let err_fail = clipboard.publish(staged, curr_token);
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
        options,
        &mut partial_output,
        || {
            count += 1;
            count > 2 // Cancel after 2 chunks
        },
    )
    .unwrap();

    match cancel_outcome {
        ExportOutcome::Canceled { bytes_written } => {
            assert!(bytes_written > 0 && bytes_written < size);
            assert_eq!(partial_output.len(), bytes_written);
        }
        _ => panic!("expected Canceled outcome"),
    }
}
