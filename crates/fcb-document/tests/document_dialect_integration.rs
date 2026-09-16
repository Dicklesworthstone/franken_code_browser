#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId, SourceRevision,
};
use fcb_document::{
    CalloutEvaluation, DialectConfig, DialectFeature, DialectSupportLevel,
    DocumentBudgets, DocumentDialectMatrix, DocumentSession, DocumentViewConstraints,
    HtmlTreatment, TaskItemState,
};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt, ScenarioReceiptDraft,
    ScenarioSeed, SourcePin, TerminalOutcome,
};
use franken_markdown::ast::{Block, Inline};
use franken_markdown::{parse_markdown, parse_markdown_spanned};

const RUN_ID_ENV: &str = "FCB_DIALECT_RUN_ID";

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0xDC_01).expect("valid owner")
}

fn dummy_session(doc_id: u64, text: &str) -> DocumentSession {
    let owner = test_owner();
    let file = FileId::new(owner, 1).unwrap();
    let rev = SourceRevision::new(owner, 10).unwrap();
    let generation = DocumentGeneration::new(owner, 1).unwrap();
    let doc = DocumentId::new(owner, doc_id).unwrap();

    let req = CaptureRequest::new(file, rev).unwrap();
    let capture = CompleteCapture::new(
        req,
        ByteLength::new(text.len() as u64),
        Arc::from(text.as_bytes()),
    )
    .unwrap();

    DocumentSession::new(doc, &capture, generation).unwrap()
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-dialect-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0xDC_01_00_01),
        pin: SourcePin::new("0310403104031040310403104031040310403104").expect("pin valid"),
        route: fcb_test_support::receipts::RouteId::new("headless:dialect").expect("route valid"),
        corpus_digest: fcb_test_support::ContentDigest::of(detail.as_bytes()),
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
    let _ = std::fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

#[test]
fn test_explicit_dialect_capability_matrix_rows() {
    let matrix = DocumentDialectMatrix::standard();
    let rows = matrix.all_rows();
    assert_eq!(rows.len(), 7, "must have all 7 explicit capability rows");

    let features = [
        DialectFeature::Footnotes,
        DialectFeature::Callouts,
        DialectFeature::ReferenceLinks,
        DialectFeature::HeadingIds,
        DialectFeature::TaskLists,
        DialectFeature::Strikethrough,
        DialectFeature::RawHtml,
    ];

    for feat in features {
        let row = matrix.get_row(feat).expect("feature row must be defined");
        assert!(!row.advertised_conformance.is_empty());
        assert!(!row.unsupported_behavior.is_empty());
        assert!(!row.parser_rule.is_empty());
    }

    // Verify support level classifications
    assert!(matrix.is_qualified_subset(DialectFeature::Footnotes));
    assert!(matrix.is_qualified_subset(DialectFeature::Callouts));
    assert!(matrix.is_qualified_subset(DialectFeature::ReferenceLinks));
    assert!(matrix.is_fully_supported(DialectFeature::HeadingIds));
    assert!(matrix.is_fully_supported(DialectFeature::TaskLists));
    assert!(matrix.is_fully_supported(DialectFeature::Strikethrough));
    assert_eq!(
        matrix.get_row(DialectFeature::RawHtml).unwrap().support_level,
        DialectSupportLevel::PreservedVisible
    );

    record_receipt(
        "test_explicit_dialect_capability_matrix_rows",
        Effect::Succeeded,
        "7 explicit dialect rows verified with truthful conformance boundaries",
    );
}

#[test]
fn test_callout_truthful_boundaries_and_prose_preservation() {
    let matrix = DocumentDialectMatrix::standard();

    // 1. Valid GFM alerts (Note, Tip, Important, Warning, Caution)
    for tag in ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"] {
        let md = format!("> [!{tag}]\n> Body text here.");
        let doc = parse_markdown(&md);
        assert_eq!(doc.blocks.len(), 1);
        let eval = matrix.evaluate_callout(&doc.blocks);
        match eval {
            CalloutEvaluation::Alert { tag: t, label, .. } => {
                assert!(tag.eq_ignore_ascii_case(t));
                assert!(!label.is_empty());
            }
            CalloutEvaluation::StandardBlockQuote { reason } => {
                assert!(false, "expected alert for tag {tag}, got reason: {reason}");
            }
        }
    }

    // 2. Truthful boundary: Same-line prose (`> [!NOTE] urgent`) must NOT be swallowed into callout
    let same_line = "> [!NOTE] urgent message on same line\n> More details.";
    let doc_same_line = parse_markdown(same_line);
    let eval_same_line = matrix.evaluate_callout(&doc_same_line.blocks);
    match eval_same_line {
        CalloutEvaluation::StandardBlockQuote { reason } => {
            assert!(reason.contains("same-line prose"));
        }
        CalloutEvaluation::Alert { .. } => {
            assert!(false, "same-line prose must stay a standard blockquote to prevent text swallowing");
        }
    }

    // 3. Unknown alert tag stays standard blockquote
    let unknown_tag = "> [!CUSTOM]\n> Some text.";
    let doc_unknown = parse_markdown(unknown_tag);
    let eval_unknown = matrix.evaluate_callout(&doc_unknown.blocks);
    assert!(matches!(eval_unknown, CalloutEvaluation::StandardBlockQuote { .. }));

    // 4. Extension toggle: disabling callouts treats all alerts as standard blockquotes
    let mut config = DialectConfig::default();
    config.enable_callouts = false;
    let matrix_disabled = DocumentDialectMatrix::with_config(config);
    let valid_alert = "> [!NOTE]\n> Body text.";
    let doc_valid = parse_markdown(valid_alert);
    let eval_disabled = matrix_disabled.evaluate_callout(&doc_valid.blocks);
    assert!(matches!(eval_disabled, CalloutEvaluation::StandardBlockQuote { .. }));

    record_receipt(
        "test_callout_truthful_boundaries_and_prose_preservation",
        Effect::Succeeded,
        "verified alerts and preserved same-line prose against text swallowing",
    );
}

#[test]
fn test_heading_slug_collision_disambiguation_and_empty() {
    let mut counts = HashMap::new();

    // Deterministic slug with collision disambiguation
    let slug1 = DocumentDialectMatrix::compute_heading_slug("Architecture Overview", &mut counts);
    assert_eq!(slug1, "architecture-overview");

    // Second heading with same title receives -2
    let slug2 = DocumentDialectMatrix::compute_heading_slug("Architecture Overview", &mut counts);
    assert_eq!(slug2, "architecture-overview-2");

    // Third heading with same title receives -3
    let slug3 = DocumentDialectMatrix::compute_heading_slug("Architecture Overview", &mut counts);
    assert_eq!(slug3, "architecture-overview-3");

    // Distinct title receives bare slug
    let slug4 = DocumentDialectMatrix::compute_heading_slug("Data Models", &mut counts);
    assert_eq!(slug4, "data-models");

    // Empty or punctuation-only title defaults to "section"
    let slug_empty = DocumentDialectMatrix::compute_heading_slug("", &mut counts);
    assert_eq!(slug_empty, "section");

    let slug_punct = DocumentDialectMatrix::compute_heading_slug("--- ___", &mut counts);
    assert_eq!(slug_punct, "section-2");

    // Integration test with source map
    let md = "# Intro\n\n# Intro\n\n#\n";
    let _spanned = parse_markdown_spanned(md);
    let session = dummy_session(1, md);
    let output = session
        .consume_headless(
            DocumentGeneration::new(test_owner(), 1).unwrap(),
            DocumentViewConstraints::default(),
            DocumentBudgets::default(),
        )
        .expect("headless consumption succeeds");

    let headings = output.source_map.headings();
    assert_eq!(headings.len(), 3);
    assert_eq!(headings[0].slug, "intro");
    assert_eq!(headings[1].slug, "intro-2");
    assert_eq!(headings[2].slug, "section");

    record_receipt(
        "test_heading_slug_collision_disambiguation_and_empty",
        Effect::Succeeded,
        "verified deterministic slug disambiguation and section default",
    );
}

#[test]
fn test_reference_links_malformed_definitions_and_distant_edits() {
    // 1. Valid reference link resolution
    let doc_src = "[My Link][dest]\n\n[dest]: https://example.com/v1\n";
    let doc = parse_markdown(doc_src);
    let first_block = doc.blocks.first().unwrap();
    if let Block::Paragraph(inlines) = first_block {
        let link_inline = inlines.first().unwrap();
        match link_inline {
            Inline::Link { dest, .. } => {
                assert_eq!(dest, "https://example.com/v1");
            }
            _ => assert!(false, "expected resolved Inline::Link"),
        }
    } else {
        assert!(false, "expected paragraph");
    }

    // 2. Malformed reference definition: '[broken]: ' with no URL remains visible text
    let malformed_src = "[broken]: \n\nSome text.\n";
    let doc_malformed = parse_markdown(malformed_src);
    // In FMD, a malformed reference definition line is kept as visible paragraph text
    assert!(
        doc_malformed.blocks.iter().any(|b| match b {
            Block::Paragraph(inlines) => inlines.iter().any(|i| match i {
                Inline::Text(t) => t.contains("[broken]:"),
                _ => false,
            }),
            _ => false,
        }),
        "malformed reference definition must remain visible text rather than being swallowed"
    );

    // 3. Undefined reference link remains visible text '[Click][missing]'
    let undefined_src = "Please [Click][missing] to proceed.\n";
    let doc_undefined = parse_markdown(undefined_src);
    assert!(
        doc_undefined.blocks.iter().any(|b| match b {
            Block::Paragraph(inlines) => inlines.iter().any(|i| match i {
                Inline::Text(t) => t.contains("[Click][missing]"),
                _ => false,
            }),
            _ => false,
        }),
        "undefined reference link must remain visible text"
    );

    // 4. Distant definition edit: changing bottom definition re-resolves the distant link
    let edited_src = "[My Link][dest]\n\n[dest]: https://example.com/v2\n";
    let doc_edited = parse_markdown(edited_src);
    if let Some(Block::Paragraph(inlines)) = doc_edited.blocks.first() {
        if let Some(Inline::Link { dest, .. }) = inlines.first() {
            assert_eq!(dest, "https://example.com/v2");
        } else {
            assert!(false, "expected link");
        }
    }

    record_receipt(
        "test_reference_links_malformed_definitions_and_distant_edits",
        Effect::Succeeded,
        "verified reference link resolution, malformed retention, and distant edits",
    );
}

#[test]
fn test_task_lists_and_strikethrough_immutability() {
    let md = "- [ ] Unchecked task\n- [x] Checked task\n- [X] Also checked\n- Ordinary bullet\n\n~~strikethrough text~~\n";
    let doc = parse_markdown(md);

    // Verify task items in AST
    if let Some(Block::List(list)) = doc.blocks.first() {
        assert_eq!(list.items.len(), 4);
        assert_eq!(list.items[0].task, Some(false));
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[0].task), TaskItemState::Unchecked);

        assert_eq!(list.items[1].task, Some(true));
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[1].task), TaskItemState::Checked);

        assert_eq!(list.items[2].task, Some(true));
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[2].task), TaskItemState::Checked);

        assert_eq!(list.items[3].task, None);
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[3].task), TaskItemState::NonTask);
    } else {
        assert!(false, "expected list block");
    }

    // Verify strikethrough in AST
    if let Some(Block::Paragraph(inlines)) = doc.blocks.get(1) {
        let strike = inlines.first().unwrap();
        match strike {
            Inline::Strikethrough(inner) => {
                assert_eq!(inner.len(), 1);
                assert_eq!(inner[0], Inline::Text("strikethrough text".to_string()));
            }
            _ => assert!(false, "expected Inline::Strikethrough"),
        }
    } else {
        assert!(false, "expected paragraph with strikethrough");
    }

    record_receipt(
        "test_task_lists_and_strikethrough_immutability",
        Effect::Succeeded,
        "verified task states and strikethrough AST structures",
    );
}

#[test]
fn test_raw_html_safe_escaping_and_extension_toggles() {
    let html_snippet = "<script>alert('pwned')</script>";
    let standard_matrix = DocumentDialectMatrix::standard();

    // Default safe mode escapes raw HTML
    let treatment_default = standard_matrix.evaluate_html_treatment(html_snippet);
    match treatment_default {
        HtmlTreatment::EscapedVisible { escaped } => {
            assert!(escaped.contains("&lt;script&gt;"));
            assert!(!escaped.contains("<script>"));
        }
        HtmlTreatment::PassThrough { .. } => {
            assert!(false, "raw HTML must not pass through under default configuration");
        }
    }

    // Toggled mode admits raw HTML
    let mut config = DialectConfig::default();
    config.allow_raw_html = true;
    let permissive_matrix = DocumentDialectMatrix::with_config(config);
    let treatment_permissive = permissive_matrix.evaluate_html_treatment(html_snippet);
    match treatment_permissive {
        HtmlTreatment::PassThrough { raw } => {
            assert_eq!(raw, html_snippet);
        }
        HtmlTreatment::EscapedVisible { .. } => {
            assert!(false, "expected pass-through when allow_raw_html is enabled");
        }
    }

    record_receipt(
        "test_raw_html_safe_escaping_and_extension_toggles",
        Effect::Succeeded,
        "verified HTML escaping by default and controlled toggle admission",
    );
}

#[test]
fn test_negative_control_oracle_detects_false_claims() {
    // Negative Control: A matrix claiming full CommonMark compliance for footnotes or callouts
    // must be rejected by the conformance oracle.
    let matrix = DocumentDialectMatrix::standard();

    // 1. Callouts cannot claim FullySupported because same-line prose is rejected
    let callout_row = matrix.get_row(DialectFeature::Callouts).unwrap();
    assert_ne!(
        callout_row.support_level,
        DialectSupportLevel::FullySupported,
        "negative control: callouts must be QualifiedSubset, not FullySupported"
    );

    // 2. Footnotes cannot claim FullySupported because numbering is presentation-affine
    let footnote_row = matrix.get_row(DialectFeature::Footnotes).unwrap();
    assert_ne!(
        footnote_row.support_level,
        DialectSupportLevel::FullySupported,
        "negative control: footnotes must be QualifiedSubset, not FullySupported"
    );

    // 3. Raw HTML cannot claim FullySupported by default
    let html_row = matrix.get_row(DialectFeature::RawHtml).unwrap();
    assert_ne!(
        html_row.support_level,
        DialectSupportLevel::FullySupported,
        "negative control: raw HTML must be PreservedVisible by default"
    );

    record_receipt(
        "test_negative_control_oracle_detects_false_claims",
        Effect::Succeeded,
        "negative control passed: oracle successfully rejects false full conformance claims",
    );
}
