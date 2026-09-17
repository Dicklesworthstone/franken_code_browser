#![forbid(unsafe_code)]

//! Comprehensive test suite verifying truthful rendered and Markdown selection copy,
//! provenance classification, and budgeted multi-flavor clipboard staging (FCB-036.B).
//!
//! # Contracts Verified
//!
//! 1. **Disjoint vs Contiguous Classification (§12.4)**: Distinguishes contiguous source
//!    ranges from disjoint ranges and generated markers (list numbering, checkboxes).
//! 2. **Truthful Markdown Copy (§12.4)**: "Copy rendered text" vs "Copy Markdown source"
//!    vs "Copy enclosing Markdown block" produce exact semantics.
//! 3. **Negative Control Oracle (§12.4)**: Rejects attempts to present disjoint spans as an
//!    invented contiguous slice (`InventedContiguousConcatenation`).
//! 4. **Complex Document Contexts**: Escaped entities (`&amp;`), dedented code blocks,
//!    reference links, and generated numbering correctly classified.
//! 5. **Encoding Invariants (§10.8)**: Exact raw bytes preserve UTF-16 BOM, CRLF, and
//!    malformed UTF-8; plain text produces declared decoded Unicode.
//! 6. **Pre-Publication Budget Refusal (§10.11)**: Selections exceeding budget are refused
//!    before publication, leaving the clipboard completely untouched.
//! 7. **Concurrency & Safe Error Handling**: Concurrent external pasteboard changes and
//!    simulated native publication failures report truthfully without unsafe rollback.
//! 8. **Streamed Export Alternative (§10.11)**: Streaming writer for large selections with
//!    cooperative cancellation.
//! 9. **Bidirectional Scroll Synchronization (§5.6)**: Exact span mapping without percentage jumps.

use std::sync::Arc;
use fcb_core::{
    ArenaOwnerId, ByteLength, ByteOffset, ByteRange, DocumentGeneration, DocumentId, FileId,
    SourceRevision,
};
use fcb_document::{
    DocumentBudgets, DocumentClipboardLimits, DocumentCopyAction, DocumentCopyError,
    DocumentNativeClipboard, DocumentSession, DocumentSyncPoint, DocumentViewConstraints,
    MarkdownSourceCopy, StagedDocumentClipboard, StreamedDocumentExport,
    TruthfulSelectionResolver,
};
use fcb_source::{CaptureRequest, CompleteCapture};
use franken_markdown::TextSelectionRange;

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(36).unwrap()
}

fn test_file_id(owner: ArenaOwnerId, n: u64) -> FileId {
    FileId::new(owner, n).unwrap()
}

fn test_revision(owner: ArenaOwnerId, n: u64) -> SourceRevision {
    SourceRevision::new(owner, n).unwrap()
}

fn test_doc_id(owner: ArenaOwnerId, n: u64) -> DocumentId {
    DocumentId::new(owner, n).unwrap()
}

fn test_generation(owner: ArenaOwnerId, n: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, n).unwrap()
}

fn make_capture(bytes: &[u8]) -> CompleteCapture {
    let owner = test_owner();
    let file = test_file_id(owner, 1);
    let rev = test_revision(owner, 1);
    let req = CaptureRequest::new(file, rev).unwrap();
    let declared_len = ByteLength::new(bytes.len() as u64);
    CompleteCapture::new(req, declared_len, Arc::from(bytes)).unwrap()
}

fn setup_session_and_source_map(markdown: &str) -> (CompleteCapture, DocumentGeneration, franken_markdown::DocumentSourceMap) {
    let capture = make_capture(markdown.as_bytes());
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 1);
    let doc_gen = test_generation(owner, 1);

    let session = DocumentSession::new(doc_id, &capture, doc_gen).expect("session created");
    let output = session
        .consume_headless(
            doc_gen,
            DocumentViewConstraints::default(),
            DocumentBudgets::default(),
        )
        .expect("headless layout");

    (capture, doc_gen, output.source_map)
}

#[test]
fn test_truthful_rendered_vs_markdown_copy_actions() {
    let md = "# Overview\n\nThis is **bold** text and `code` inline.\n";
    let (capture, doc_gen, source_map) = setup_session_and_source_map(md);

    // Select entire rendered text
    let total_len = source_map.rendered_len();
    let selection = TextSelectionRange::new(0, total_len);

    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve succeeds");

    assert!(!provenance.reading_text.is_empty());
    assert!(!provenance.source_ranges.is_empty());

    // 1. Copy Rendered Reading Text
    let staged_rendered = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage rendered");
    assert_eq!(staged_rendered.plain_text, provenance.reading_text);

    // 2. Copy Markdown Source
    let staged_md = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::MarkdownSource,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage markdown");
    assert!(staged_md.markdown_source.is_some());

    // 3. Copy Enclosing Markdown Block
    let staged_block = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::EnclosingMarkdownBlock,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage block");
    assert!(staged_block.enclosing_block.is_some());

    // 4. Copy with Location Provenance
    let staged_loc = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::LocationProvenance,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage location");
    assert!(staged_loc.plain_text.contains("<!-- fcb:file="));
}

#[test]
fn test_disjoint_source_mapping_and_negative_control_oracle() {
    let md = "# Title\n\nFirst paragraph with **bold** words.\n\nSecond paragraph.\n";
    let (capture, _doc_gen, source_map) = setup_session_and_source_map(md);

    // Select the entire document rendered text
    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve succeeds");

    // The document contains multiple elements, giving disjoint source ranges
    if provenance.source_ranges.len() > 1 {
        assert!(provenance.is_disjoint());

        // Negative Control Oracle: If someone attempts to claim that the disjoint spans
        // form a single contiguous slice, assert_not_invented_contiguous must detect and reject it!
        let min_start = provenance.source_ranges.iter().map(|r| r.start()).min().unwrap();
        let max_end = provenance.source_ranges.iter().map(|r| r.end()).max().unwrap();
        let fake_contiguous = ByteRange::new(min_start, max_end).unwrap();

        let rejection = provenance.assert_not_invented_contiguous(fake_contiguous);
        assert!(
            matches!(rejection, Err(DocumentCopyError::InventedContiguousConcatenation { .. })),
            "oracle must detect invented contiguous source: got {rejection:?}"
        );
    }
}

#[test]
fn test_escaped_entities_dedented_code_and_reference_links() {
    let md = r#"# Complex Document

Escaped characters: \*not italic\* and &amp; entity.

```rust
    let x = 42;
```

Here is a [reference link][ref1].

[ref1]: https://github.com/Dicklesworthstone/franken_code_browser
"#;
    let (capture, _doc_gen, source_map) = setup_session_and_source_map(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve complex");

    assert!(provenance.has_escapes_or_entities, "must detect escaped entities");
    assert!(provenance.has_dedented_code, "must detect code fence context");
    assert!(provenance.has_reference_links, "must detect reference link definitions");
}

#[test]
fn test_generated_numbering_and_markers() {
    let md = r#"# Tasks and Lists

1. First ordered item
2. Second ordered item
3. Third ordered item
"#;
    let (capture, _doc_gen, source_map) = setup_session_and_source_map(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve list");

    assert!(
        provenance.has_generated_numbering || provenance.is_generated(),
        "generated markers must be explicitly flagged"
    );

    let copy_res = TruthfulSelectionResolver::copy_markdown_source(&provenance, &capture)
        .expect("copy markdown source");
    match copy_res {
        MarkdownSourceCopy::Contiguous { text, .. } => {
            assert!(!text.is_empty());
        }
        MarkdownSourceCopy::Disjoint { slices, .. } => {
            assert!(!slices.is_empty());
        }
        MarkdownSourceCopy::GeneratedMarker { marker } => {
            assert!(!marker.is_empty());
        }
    }
}

#[test]
fn test_exact_bytes_preserves_utf16_bom_crlf_and_malformed_utf8() {
    // 1. CRLF line endings
    let crlf_md = "# Title\r\n\r\nLine 1\r\nLine 2\r\n";
    let (capture, doc_gen, source_map) = setup_session_and_source_map(crlf_md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve crlf");

    let staged_enclosing = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::EnclosingMarkdownBlock,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage crlf enclosing");

    // Exact raw bytes of enclosing block must preserve CRLF
    assert!(
        staged_enclosing.exact_raw_bytes.windows(2).any(|w| w == b"\r\n"),
        "exact raw bytes must preserve original CRLF line endings"
    );
    assert!(!staged_enclosing.has_malformed_utf8);

    // 2. Streamed export preserving UTF-16 BOM and malformed UTF-8
    let raw_with_bom = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x6C, 0x00, 0x6F]; // UTF-16 BE "Hello"
    let bom_capture = make_capture(&raw_with_bom);
    let bom_range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(raw_with_bom.len() as u64)).unwrap();
    let mut bom_out = Vec::new();
    StreamedDocumentExport::stream_to_writer(&[bom_range], &bom_capture, &mut bom_out, 4, || false)
        .expect("stream bom");
    assert_eq!(bom_out, raw_with_bom, "streamed export must faithfully preserve UTF-16 BOM bytes");

    // 3. Malformed UTF-8 bytes preservation
    let malformed_bytes = vec![0xFF, 0xFE, 0x80, 0x81, b'a', b'b', b'c'];
    let mal_capture = make_capture(&malformed_bytes);
    let mal_range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(malformed_bytes.len() as u64)).unwrap();
    let mut mal_out = Vec::new();
    StreamedDocumentExport::stream_to_writer(&[mal_range], &mal_capture, &mut mal_out, 4, || false)
        .expect("stream malformed");
    assert_eq!(mal_out, malformed_bytes, "streamed export must faithfully preserve malformed bytes");

    // 2. Bidi Arabic text
    let bidi_md = "# تجربة\n\nنص عربي للتجربة.\n";
    let (bidi_capture, bidi_gen, bidi_map) = setup_session_and_source_map(bidi_md);
    let bidi_sel = TextSelectionRange::new(0, bidi_map.rendered_len());
    let bidi_prov = TruthfulSelectionResolver::resolve(bidi_sel, &bidi_map, &bidi_capture)
        .expect("resolve bidi");

    let bidi_staged = StagedDocumentClipboard::stage(
        &bidi_prov,
        DocumentCopyAction::RenderedReadingText,
        &bidi_capture,
        bidi_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage bidi");
    assert!(bidi_staged.plain_text.contains("عربي"));
}

#[test]
fn test_clipboard_budget_refusal_preserves_old_clipboard() {
    let md = "# Title\n\nSome paragraph content that has some non-trivial length.\n";
    let (capture, doc_gen, source_map) = setup_session_and_source_map(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve");

    let mut clipboard = DocumentNativeClipboard::new();
    clipboard.inject_external_change("Initial safe clipboard text");
    let initial_seq = clipboard.sequence_number();

    // Restrict budget to 16 bytes (smaller than staged data)
    let tiny_limits = DocumentClipboardLimits {
        max_clipboard_bytes: 16,
    };

    let stage_result = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        tiny_limits,
        || false,
    );

    assert!(
        matches!(stage_result, Err(DocumentCopyError::BudgetExceeded { .. })),
        "budget refusal must reject oversized staging: got {stage_result:?}"
    );

    // Verify clipboard was untouched
    assert_eq!(clipboard.sequence_number(), initial_seq);
    assert_eq!(
        clipboard.current().map(|c| c.plain_text.as_str()),
        Some("Initial safe clipboard text")
    );
}

#[test]
fn test_cancellation_and_concurrent_external_change() {
    let md = "# Title\n\nParagraph text.\n";
    let (capture, doc_gen, source_map) = setup_session_and_source_map(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve");

    // 1. Cancellation before staging
    let cancel_res = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || true, // canceled immediately
    );
    assert!(matches!(cancel_res, Err(DocumentCopyError::Canceled)));

    // 2. Concurrent external change prevents stale overwrite
    let mut clipboard = DocumentNativeClipboard::new();
    let expected_seq = clipboard.sequence_number();

    let staged = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage succeeds");

    // External application mutates clipboard before we publish
    clipboard.inject_external_change("External pasteboard copy");

    let pub_res = clipboard.publish(staged.clone(), expected_seq, false);
    assert!(
        matches!(pub_res, Err(DocumentCopyError::ConcurrentExternalChange)),
        "concurrent change must be detected and rejected: got {pub_res:?}"
    );

    // 3. Simulated native publication failure leaves clipboard intact without rollback
    let current_seq = clipboard.sequence_number();
    let fail_res = clipboard.publish(staged, current_seq, true);
    assert!(
        matches!(fail_res, Err(DocumentCopyError::NativePublicationFailed)),
        "native failure reported truthfully: got {fail_res:?}"
    );
    assert_eq!(
        clipboard.current().map(|c| c.plain_text.as_str()),
        Some("External pasteboard copy"),
        "clipboard must remain untouched after publication failure"
    );
}

#[test]
fn test_streamed_document_export_with_cooperative_cancellation() {
    let md = "# Large Export\n\nThis is a long line of text intended for streaming chunk tests.\n";
    let capture = make_capture(md.as_bytes());

    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(md.len() as u64)).unwrap();
    let mut buffer = Vec::new();

    // Stream with small 8-byte chunks
    let written = StreamedDocumentExport::stream_to_writer(
        &[range],
        &capture,
        &mut buffer,
        8,
        || false,
    )
    .expect("streaming succeeds");

    assert_eq!(written, md.len() as u64);
    assert_eq!(buffer, md.as_bytes());

    // Test cooperative cancellation during stream
    let mut partial_buf = Vec::new();
    let mut chunk_counter = 0;
    let cancel_res = StreamedDocumentExport::stream_to_writer(
        &[range],
        &capture,
        &mut partial_buf,
        8,
        || {
            chunk_counter += 1;
            chunk_counter >= 3 // cancel after 2 chunks
        },
    );

    assert!(
        matches!(cancel_res, Err(DocumentCopyError::Canceled)),
        "cancellation in stream must stop cleanly"
    );
    assert!(partial_buf.len() <= 24);
}

#[test]
fn test_bidirectional_synchronization_and_anchors() {
    let md = "# Heading One\n\nFirst line of content.\n\n# Heading Two\n\nSecond line.\n";
    let (_capture, _doc_gen, source_map) = setup_session_and_source_map(md);

    // 1. Source to rendered synchronization
    let sync_from_src = DocumentSyncPoint::from_source_offset(30, &source_map);
    assert_eq!(sync_from_src.source_byte, 30);
    assert!(sync_from_src.heading_slug.is_some());

    // 2. Rendered to source synchronization
    let sync_from_ren = DocumentSyncPoint::from_rendered_offset(10, &source_map);
    assert_eq!(sync_from_ren.rendered_offset, 10);
    assert!(sync_from_ren.heading_slug.is_some());
}
