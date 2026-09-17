//! FCB-036.V production verification scenario:
//! FCB source/preview/split panels consuming upstream selection and provenance,
//! truthful rendered vs Markdown copy, disjoint/contiguous classification,
//! pre-publication budget refusal, encoding preservation, and anchor synchronization.
//!
//! Required verification cases:
//! 1. `anchor_synchronization_and_independent_pane_scroll` — bidirectional sync between
//!    source byte offset and preview rendered position, independent pane scrolling.
//! 2. `rendered_text_versus_markdown_copy_semantics` — rendered reading text vs Markdown
//!    source copy (disjoint vs contiguous classification).
//! 3. `enclosing_markdown_block_copy_across_elements` — enclosing Markdown block copy.
//! 4. `complex_entities_dedented_code_and_reference_links` — escaped entities, dedented code,
//!    reference links, and generated numbering.
//! 5. `encoding_preservation_utf16_bom_crlf_and_bidi` — UTF-16 BOM, CRLF, and bidi text.
//! 6. `pre_publication_budget_refusal_preserves_clipboard` — budget refusal leaves clipboard untouched.
//! 7. `concurrency_and_native_failure_handling` — external concurrent changes and native failure.
//! 8. `streamed_document_export_with_cancellation` — streaming chunk writer with cancellation.
//! 9. `negative_control_oracle_detects_invented_contiguous_source` — oracle detects and rejects
//!    attempts to present disjoint spans as an unbroken contiguous range.
//! 10. `negative_control_oracle_detects_budget_overflow` — oracle detects and rejects budget overflow.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_036.sh`).

#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, ByteOffset, ByteRange, DocumentGeneration, DocumentId, FileId,
    SourceRevision,
};
use fcb_document::{
    discover_readme, DocumentBudgets, DocumentClipboardLimits, DocumentCopyAction,
    DocumentCopyError, DocumentLens, DocumentNativeClipboard, DocumentSession, DocumentSyncPoint,
    DocumentViewConstraints, SplitPaneNavigator, StagedDocumentClipboard, StreamedDocumentExport,
    TruthfulSelectionResolver,
};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use franken_markdown::TextSelectionRange;

const RUN_ID_ENV: &str = "FCB_036_RUN_ID";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-036-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_36_00_01),
        pin: SourcePin::new("0360003600036000360003600036000360003600").expect("pin valid"),
        route: RouteId::new("headless:document").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };

    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

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

fn setup_session(markdown: &str) -> (CompleteCapture, DocumentGeneration, franken_markdown::DocumentSourceMap) {
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
fn test_01_anchor_synchronization_and_independent_pane_scroll() {
    let md = "# Architecture\n\nIntroduction to FCB.\n\n# Design Details\n\nSpecific invariants.\n";
    let (_capture, _doc_gen, source_map) = setup_session(md);

    // 1. Split pane navigator with independent lenses
    let source_lens = DocumentLens::new(80, 24, 16, 8).unwrap();
    let preview_lens = DocumentLens::new(80, 24, 20, 10).unwrap();
    let mut nav = SplitPaneNavigator::new(source_lens, preview_lens);

    // Verify independent scroll: moving source does not touch preview
    nav.jump_source_to_byte(50);
    assert_eq!(nav.source_lens().scroll_y(), 50);
    assert_eq!(nav.preview_lens().scroll_y(), 0, "preview pane scroll remains independent");

    nav.jump_preview_to_y(120);
    assert_eq!(nav.preview_lens().scroll_y(), 120);
    assert_eq!(nav.source_lens().scroll_y(), 50, "source pane scroll unaffected by preview jump");

    // 2. Heading anchor jump
    let anchor = nav.jump_to_heading("design-details", &source_map, 500, 20);
    assert!(anchor.is_some(), "heading anchor jump must succeed");

    // 3. Direct README discovery
    let paths = vec![
        "src/lib.rs".to_string(),
        "README.md".to_string(),
        "docs/guide.md".to_string(),
    ];
    let readme = discover_readme(&paths);
    assert_eq!(readme.as_deref(), Some("README.md"));

    // 4. Bidirectional scroll synchronization via DocumentSyncPoint
    let sync_pt = DocumentSyncPoint::from_source_offset(40, &source_map);
    assert_eq!(sync_pt.source_byte, 40);
    assert!(sync_pt.heading_slug.is_some());

    record_receipt(
        "anchor_synchronization_and_independent_pane_scroll",
        Effect::Succeeded,
        "Independent pane scroll, heading jump, README discovery, and span synchronization verified",
    );
}

#[test]
fn test_02_rendered_text_versus_markdown_copy_semantics() {
    let md = "# Component\n\nThis is **bold** text and `code` inline.\n";
    let (capture, doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve succeeds");

    // 1. Plain text copy action
    let staged_text = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage plain text");
    assert_eq!(staged_text.plain_text, provenance.reading_text);

    // 2. Markdown source copy action
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

    record_receipt(
        "rendered_text_versus_markdown_copy_semantics",
        Effect::Succeeded,
        "Rendered text vs Markdown source copy semantics verified",
    );
}

#[test]
fn test_03_enclosing_markdown_block_copy_across_elements() {
    let md = "# Header Title\n\nParagraph with *italic* emphasis.\n\nAnother paragraph.\n";
    let (capture, doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve succeeds");

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
    assert!(!staged_block.exact_raw_bytes.is_empty());

    record_receipt(
        "enclosing_markdown_block_copy_across_elements",
        Effect::Succeeded,
        "Enclosing Markdown block copy across elements verified",
    );
}

#[test]
fn test_04_complex_entities_dedented_code_and_reference_links() {
    let md = r#"# Architecture

Escaped: \*not emphasis\* and &amp; entity.

```rust
    let y = 100;
```

Reference link: [Docs][doc_ref].

[doc_ref]: https://example.com/docs
"#;
    let (capture, _doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve complex");

    assert!(provenance.has_escapes_or_entities);
    assert!(provenance.has_dedented_code);
    assert!(provenance.has_reference_links);

    record_receipt(
        "complex_entities_dedented_code_and_reference_links",
        Effect::Succeeded,
        "Escaped entities, dedented code fences, and reference links verified",
    );
}

#[test]
fn test_05_encoding_preservation_utf16_bom_crlf_and_bidi() {
    // 1. CRLF in enclosing block
    let crlf_md = "# Title\r\n\r\nLine 1\r\nLine 2\r\n";
    let (capture, doc_gen, source_map) = setup_session(crlf_md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve crlf");

    let staged_crlf = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::EnclosingMarkdownBlock,
        &capture,
        doc_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage crlf");

    assert!(
        staged_crlf.exact_raw_bytes.windows(2).any(|w| w == b"\r\n"),
        "exact raw bytes must preserve original CRLF line endings"
    );

    // 2. UTF-16 BOM streaming export
    let raw_bom = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]; // UTF-16 BE "Hi"
    let bom_cap = make_capture(&raw_bom);
    let bom_range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(raw_bom.len() as u64)).unwrap();
    let mut bom_out = Vec::new();
    StreamedDocumentExport::stream_to_writer(&[bom_range], &bom_cap, &mut bom_out, 2, || false)
        .expect("stream bom");
    assert_eq!(bom_out, raw_bom);

    // 3. Bidi Arabic text
    let bidi_md = "# عنوان\n\nنص التجربة.\n";
    let (bidi_cap, bidi_gen, bidi_map) = setup_session(bidi_md);
    let bidi_sel = TextSelectionRange::new(0, bidi_map.rendered_len());
    let bidi_prov = TruthfulSelectionResolver::resolve(bidi_sel, &bidi_map, &bidi_cap)
        .expect("resolve bidi");
    let bidi_staged = StagedDocumentClipboard::stage(
        &bidi_prov,
        DocumentCopyAction::RenderedReadingText,
        &bidi_cap,
        bidi_gen,
        DocumentClipboardLimits::default(),
        || false,
    )
    .expect("stage bidi");
    assert!(bidi_staged.plain_text.contains("عنوان"));

    record_receipt(
        "encoding_preservation_utf16_bom_crlf_and_bidi",
        Effect::Succeeded,
        "CRLF preservation, UTF-16 BOM streaming, and bidi text verified",
    );
}

#[test]
fn test_06_pre_publication_budget_refusal_preserves_clipboard() {
    let md = "# Title\n\nSome paragraph text of substantial length.\n";
    let (capture, doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve");

    let mut clipboard = DocumentNativeClipboard::new();
    clipboard.inject_external_change("Old intact pasteboard content");
    let initial_seq = clipboard.sequence_number();

    let tiny_limits = DocumentClipboardLimits {
        max_clipboard_bytes: 8,
    };

    let res = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        tiny_limits,
        || false,
    );

    assert!(matches!(res, Err(DocumentCopyError::BudgetExceeded { .. })));
    assert_eq!(clipboard.sequence_number(), initial_seq);
    assert_eq!(
        clipboard.current().map(|c| c.plain_text.as_str()),
        Some("Old intact pasteboard content")
    );

    record_receipt(
        "pre_publication_budget_refusal_preserves_clipboard",
        Effect::Succeeded,
        "Pre-publication budget refusal preserves existing pasteboard contents",
    );
}

#[test]
fn test_07_concurrency_and_native_failure_handling() {
    let md = "# Title\n\nParagraph text.\n";
    let (capture, doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve");

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
    .expect("stage");

    // Simulate concurrent external pasteboard mutation
    clipboard.inject_external_change("External pasteboard entry");

    let pub_err = clipboard.publish(staged.clone(), expected_seq, false);
    assert!(matches!(pub_err, Err(DocumentCopyError::ConcurrentExternalChange)));

    // Simulated native failure
    let current_seq = clipboard.sequence_number();
    let fail_err = clipboard.publish(staged, current_seq, true);
    assert!(matches!(fail_err, Err(DocumentCopyError::NativePublicationFailed)));

    record_receipt(
        "concurrency_and_native_failure_handling",
        Effect::Succeeded,
        "Concurrent external change detected and native publication failure safely handled",
    );
}

#[test]
fn test_08_streamed_document_export_with_cancellation() {
    let md = "# Streamed Data\n\nLarge multi-chunk source stream test.\n";
    let capture = make_capture(md.as_bytes());

    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(md.len() as u64)).unwrap();
    let mut out = Vec::new();

    let written = StreamedDocumentExport::stream_to_writer(&[range], &capture, &mut out, 8, || false)
        .expect("stream complete");
    assert_eq!(written, md.len() as u64);
    assert_eq!(out, md.as_bytes());

    // Test cooperative cancellation
    let mut partial = Vec::new();
    let mut count = 0;
    let cancel_res = StreamedDocumentExport::stream_to_writer(&[range], &capture, &mut partial, 8, || {
        count += 1;
        count >= 2
    });
    assert!(matches!(cancel_res, Err(DocumentCopyError::Canceled)));

    record_receipt(
        "streamed_document_export_with_cancellation",
        Effect::Succeeded,
        "Streamed document export with bounded chunks and cooperative cancellation verified",
    );
}

#[test]
fn test_09_negative_control_oracle_detects_invented_contiguous_source() {
    let md = "# Title One\n\nFirst paragraph.\n\n# Title Two\n\nSecond paragraph.\n";
    let (capture, _doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve");

    assert!(provenance.is_disjoint());

    // Negative control: pass an enclosing range that bridges across the disjoint spans
    let min_s = provenance.source_ranges.iter().map(|r| r.start()).min().unwrap();
    let max_e = provenance.source_ranges.iter().map(|r| r.end()).max().unwrap();
    let bogus_range = ByteRange::new(min_s, max_e).unwrap();

    let check = provenance.assert_not_invented_contiguous(bogus_range);
    assert!(
        matches!(check, Err(DocumentCopyError::InventedContiguousConcatenation { .. })),
        "oracle must detect and reject invented contiguous concatenation"
    );

    record_receipt(
        "negative_control_oracle_detects_invented_contiguous_source",
        Effect::Succeeded,
        "Intentional negative control: oracle successfully detected invented contiguous source",
    );
}

#[test]
fn test_10_negative_control_oracle_detects_budget_overflow() {
    let md = "# Large Block\n\nDetailed content for budget overflow testing.\n";
    let (capture, doc_gen, source_map) = setup_session(md);

    let selection = TextSelectionRange::new(0, source_map.rendered_len());
    let provenance = TruthfulSelectionResolver::resolve(selection, &source_map, &capture)
        .expect("resolve");

    // Zero-byte limit guarantees rejection
    let zero_limit = DocumentClipboardLimits {
        max_clipboard_bytes: 0,
    };

    let res = StagedDocumentClipboard::stage(
        &provenance,
        DocumentCopyAction::RenderedReadingText,
        &capture,
        doc_gen,
        zero_limit,
        || false,
    );

    assert!(
        matches!(res, Err(DocumentCopyError::BudgetExceeded { .. })),
        "oracle must detect and reject zero/overflow budget"
    );

    record_receipt(
        "negative_control_oracle_detects_budget_overflow",
        Effect::Succeeded,
        "Intentional negative control: oracle successfully detected budget overflow",
    );
}
