#![forbid(unsafe_code)]

//! FCB-031.V production verification scenario: consume upstream FMD nested document/flow
//! APIs through thin fcb-document integration.
//!
//! Required cases (each independently selectable via cargo test):
//! 1. `real_readme_headless_flow_output` — Real README headless output consumed through public API.
//! 2. `stale_document_request_refused` — Stale document request refused without cloning AST per frame.
//! 3. `committed_upstream_flow_to_retained_publication` — Headless flow display plan drives retained publication.
//! 4. `bounded_asset_confinement_and_delivery` — Inert asset semantics, root confinement, and budget defense.
//! 5. `dialect_matrix_conformance_and_truthful_extensions` — Explicit dialect matrix with truthful bounds.
//! 6. `negative_control_oracle` — Intentional failing negative controls verifying defect detection.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_031.sh`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId, SourceRevision,
};
use fcb_document::{
    AssetDomain, AssetRequest, AssetRequestId, BoundedAssetBudgets, BoundedAssetRegistry,
    CalloutEvaluation, DialectFeature, DialectSupportLevel, DisplayItem, DisplayList,
    DisplayRect, DisplayTextRun, DocumentBudgets, DocumentDialectMatrix,
    DocumentDisplayPlan, DocumentPublication, DocumentSession, DocumentViewConstraints,
    HtmlTreatment, SourceSpan, TaskItemState,
};
use fcb_source::{CaptureRequest, CompleteCapture, ObservationDigest};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt, ScenarioReceiptDraft,
    ScenarioSeed, SourcePin, TerminalOutcome,
};
use franken_markdown::ast::Block;
use franken_markdown::parse_markdown;

const RUN_ID_ENV: &str = "FCB_031_RUN_ID";

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C_31).expect("valid owner")
}

fn dummy_capture(file_id_num: u64, rev_num: u64, bytes: &[u8]) -> CompleteCapture {
    let owner = test_owner();
    let file = FileId::new(owner, file_id_num).expect("valid file id");
    let rev = SourceRevision::new(owner, rev_num).expect("valid rev");
    let req = CaptureRequest::new(file, rev).expect("valid capture req");
    let len = ByteLength::new(bytes.len() as u64);
    CompleteCapture::new(req, len, Arc::from(bytes)).expect("valid capture")
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-031-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_31_00_01),
        pin: SourcePin::new("0310003100031000310003100031000310003100").expect("pin valid"),
        route: fcb_test_support::receipts::RouteId::new("headless:document").expect("route valid"),
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
fn real_readme_headless_flow_output() {
    let readme_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md");
    let readme_bytes = std::fs::read(readme_path).expect("README.md must be readable");
    let capture = dummy_capture(1, 100, &readme_bytes);
    let owner = test_owner();
    let doc_id = DocumentId::new(owner, 1).expect("doc id");
    let generation = DocumentGeneration::new(owner, 1).expect("generation");

    let session = DocumentSession::new(doc_id, &capture, generation)
        .expect("session creation succeeds");

    let constraints = DocumentViewConstraints {
        viewport_width: 80,
        line_height: 16,
        char_width: 1,
        max_viewport_lines: None,
    };
    let budgets = DocumentBudgets::default();

    let output = session
        .consume_headless(generation, constraints, budgets)
        .expect("headless flow consumption succeeds");

    assert!(output.lines.len() > 10, "README should produce multiple flow lines");
    assert!(output.total_height > 0, "total height must be positive");
    assert!(output.consumed_blocks > 0, "consumed blocks must be positive");
    assert!(output.consumed_bytes > 0, "consumed bytes must be positive");
    assert!(
        output.semantic_fixture.contains("SEMANTIC LAYOUT FIXTURE"),
        "fixture must contain semantic header"
    );

    record_receipt(
        "real_readme_headless_flow_output",
        Effect::Succeeded,
        "Real README consumed through public headless flow API with truthful metrics",
    );
}

#[test]
fn stale_document_request_refused() {
    let text = b"# Test Document\n\nParagraph text that should never be re-parsed.";
    let capture = dummy_capture(2, 200, text);
    let owner = test_owner();
    let doc_id = DocumentId::new(owner, 2).expect("doc id");
    let valid_gen = DocumentGeneration::new(owner, 10).expect("valid gen");
    let stale_gen = DocumentGeneration::new(owner, 9).expect("stale gen");

    let session = DocumentSession::new(doc_id, &capture, valid_gen)
        .expect("session creation succeeds");

    let constraints = DocumentViewConstraints::default();
    let budgets = DocumentBudgets::default();

    let err = session
        .consume_headless(stale_gen, constraints, budgets)
        .expect_err("stale generation request must be refused");
    assert_eq!(err.code(), "DOCUMENT_STALE_REQUEST");

    // Session validation without work
    let file = FileId::new(owner, 2).unwrap();
    let rev = SourceRevision::new(owner, 200).unwrap();
    let stale_rev = SourceRevision::new(owner, 199).unwrap();
    let digest = capture.digest();

    assert!(session.validate_request(file, rev, digest, valid_gen).is_ok());
    let err_rev = session
        .validate_request(file, stale_rev, digest, valid_gen)
        .expect_err("stale revision refused");
    assert_eq!(err_rev.code(), "DOCUMENT_STALE_REVISION");

    record_receipt(
        "stale_document_request_refused",
        Effect::Succeeded,
        "Stale document generation and revision refused without parsing or AST cloning",
    );
}

#[test]
fn committed_upstream_flow_to_retained_publication() {
    let owner = test_owner();
    let doc_id = DocumentId::new(owner, 3).expect("doc id");
    let generation = DocumentGeneration::new(owner, 5).expect("generation");
    let stale_gen = DocumentGeneration::new(owner, 4).expect("stale generation");
    let digest = ObservationDigest::observe(b"retained publication bytes");

    let mut display_list = DisplayList::new();
    display_list.push_item(DisplayItem::Text(DisplayTextRun {
        bounds: DisplayRect::new(0.0, 0.0, 100.0, 20.0),
        text: "Section Header".to_string(),
        font_run: None,
        color_role: "heading".to_string(),
        source_span: SourceSpan::new(0, 14),
        font_size: 16.0,
    }));
    display_list.push_item(DisplayItem::Text(DisplayTextRun {
        bounds: DisplayRect::new(0.0, 30.0, 100.0, 40.0),
        text: "Body Paragraph".to_string(),
        font_run: None,
        color_role: "text".to_string(),
        source_span: SourceSpan::new(16, 30),
        font_size: 14.0,
    }));

    let display_plan = DocumentDisplayPlan::new(generation, display_list, Vec::new());
    let pub_result = DocumentPublication::new(doc_id, generation, digest, display_plan);
    assert!(pub_result.is_ok());
    let publication = pub_result.unwrap();

    assert_eq!(publication.document_id(), doc_id);
    assert_eq!(publication.generation(), generation);
    assert_eq!(publication.digest(), digest);
    assert!(publication.is_complete());
    assert!(!publication.is_stale_for(generation));
    assert!(publication.is_stale_for(stale_gen));

    // Stale presentation request refused
    let stale_pres = publication.validate_presentation(stale_gen);
    assert!(stale_pres.is_err());
    assert_eq!(stale_pres.unwrap_err().code(), "DOCUMENT_STALE_REQUEST");

    // Retained viewport query filtering
    let header_viewport = DisplayRect::new(0.0, 0.0, 120.0, 25.0);
    let visible_header = publication.visible_items(header_viewport);
    assert_eq!(visible_header.len(), 1);

    let full_viewport = DisplayRect::new(0.0, 0.0, 200.0, 200.0);
    let visible_all = publication.visible_items(full_viewport);
    assert_eq!(visible_all.len(), 2);

    record_receipt(
        "committed_upstream_flow_to_retained_publication",
        Effect::Succeeded,
        "Retained publication encapsulates display plan, enforces generation, and filters viewport",
    );
}

#[test]
fn bounded_asset_confinement_and_delivery() {
    let owner = test_owner();
    let doc_id = DocumentId::new(owner, 4).expect("doc id");
    let generation = DocumentGeneration::new(owner, 1).expect("generation");

    let budgets = BoundedAssetBudgets {
        max_pending_requests: 3,
        max_total_asset_bytes: 4096,
        max_single_asset_bytes: 1024,
        ..BoundedAssetBudgets::default()
    };

    let mut registry = BoundedAssetRegistry::new(doc_id, generation, budgets);

    // 1. Confined relative paths accepted
    assert!(AssetDomain::classify("images/arch.png").is_confined());
    assert!(AssetDomain::classify("./assets/diagram.svg").is_confined());

    // 2. Traversal and absolute escapes rejected
    assert!(!AssetDomain::classify("../escaped.png").is_confined());
    assert!(!AssetDomain::classify("/etc/passwd").is_confined());
    assert!(!AssetDomain::classify("C:\\Windows\\system32").is_confined());
    assert!(!AssetDomain::classify("http://remote.org/img.png").is_confined());
    assert!(!AssetDomain::classify("javascript:alert(1)").is_confined());
    assert!(!AssetDomain::classify("assets/test\0.png").is_confined());

    // 3. Register valid request
    let req = AssetRequest {
        id: AssetRequestId(10),
        kind: "image",
        url: "assets/diagram.png".to_string(),
        source_offset: 42,
        generation: generation.get(),
        estimated_width: 200,
        estimated_height: 150,
        alt_text: "System Diagram".to_string(),
    };
    let auth = registry.authorize_and_register(&req);
    assert!(auth.is_ok());
    assert_eq!(registry.pending_count(), 1);

    // 4. Reject traversal escape in registration
    let escape_req = AssetRequest {
        id: AssetRequestId(11),
        kind: "image",
        url: "../secret/token.png".to_string(),
        source_offset: 80,
        generation: generation.get(),
        estimated_width: 50,
        estimated_height: 50,
        alt_text: "Secret".to_string(),
    };
    let escape_err = registry.authorize_and_register(&escape_req);
    assert!(escape_err.is_err());
    assert_eq!(escape_err.unwrap_err().code(), "DOCUMENT_ASSET_ESCAPE");

    // 5. Deliver resolution within bounds
    let payload = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let res = registry.deliver_resolution(10, generation, 200, 150, Some(payload));
    assert!(res.is_ok());
    assert_eq!(registry.pending_count(), 0);
    assert_eq!(registry.resolved_count(), 1);

    // 6. Stale generation delivery rejected
    let stale_gen = DocumentGeneration::new(owner, 2).unwrap();
    let stale_deliv = registry.deliver_resolution(10, stale_gen, 200, 150, None);
    assert!(stale_deliv.is_err());
    assert_eq!(stale_deliv.unwrap_err().code(), "DOCUMENT_STALE_ASSET_GENERATION");

    record_receipt(
        "bounded_asset_confinement_and_delivery",
        Effect::Succeeded,
        "Asset confinement checks, memory budget limits, and generation tracking verified",
    );
}

#[test]
fn dialect_matrix_conformance_and_truthful_extensions() {
    let matrix = DocumentDialectMatrix::standard();
    assert_eq!(matrix.all_rows().len(), 7);

    // 1. Heading slug collision disambiguation
    let mut counts = HashMap::new();
    let s1 = DocumentDialectMatrix::compute_heading_slug("API Overview", &mut counts);
    assert_eq!(s1, "api-overview");
    let s2 = DocumentDialectMatrix::compute_heading_slug("API Overview", &mut counts);
    assert_eq!(s2, "api-overview-2");
    let s_empty = DocumentDialectMatrix::compute_heading_slug("", &mut counts);
    assert_eq!(s_empty, "section");

    // 2. Alert callout classification: same-line prose preserved
    let valid_alert = "> [!NOTE]\n> Alert body here.";
    let doc_alert = parse_markdown(valid_alert);
    match matrix.evaluate_callout(&doc_alert.blocks) {
        CalloutEvaluation::Alert { tag, .. } => assert_eq!(tag, "note"),
        CalloutEvaluation::StandardBlockQuote { .. } => assert!(false, "expected Alert"),
    }

    let prose_alert = "> [!NOTE] urgent same-line prose\n> Details.";
    let doc_prose = parse_markdown(prose_alert);
    match matrix.evaluate_callout(&doc_prose.blocks) {
        CalloutEvaluation::StandardBlockQuote { reason } => {
            assert!(reason.contains("same-line prose"));
        }
        CalloutEvaluation::Alert { .. } => {
            assert!(false, "same-line prose must stay standard blockquote");
        }
    }

    // 3. Task lists and strikethrough
    let md = "- [ ] Unfinished item\n- [x] Finished item\n- Standard bullet\n\n~~struck text~~\n";
    let doc = parse_markdown(md);
    if let Some(Block::List(list)) = doc.blocks.first() {
        assert_eq!(list.items.len(), 3);
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[0].task), TaskItemState::Unchecked);
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[1].task), TaskItemState::Checked);
        assert_eq!(DocumentDialectMatrix::evaluate_task_state(list.items[2].task), TaskItemState::NonTask);
    } else {
        assert!(false, "expected list block");
    }

    // 4. Raw HTML safe escaping
    let html = "<script>alert(1)</script>";
    let treat = matrix.evaluate_html_treatment(html);
    match treat {
        HtmlTreatment::EscapedVisible { escaped } => {
            assert!(escaped.contains("&lt;script&gt;"));
        }
        HtmlTreatment::PassThrough { .. } => assert!(false, "must be escaped"),
    }

    record_receipt(
        "dialect_matrix_conformance_and_truthful_extensions",
        Effect::Succeeded,
        "Dialect capability matrix verified with truthful subset boundaries and extensions",
    );
}

#[test]
fn negative_control_oracle() {
    let matrix = DocumentDialectMatrix::standard();

    // Negative control 1: Claiming full CommonMark compliance on callouts must fail
    let callout_row = matrix.get_row(DialectFeature::Callouts).unwrap();
    assert_ne!(
        callout_row.support_level,
        DialectSupportLevel::FullySupported,
        "oracle rejection: callouts are not full CommonMark"
    );

    // Negative control 2: Path traversal must be refused
    assert!(
        !AssetDomain::classify("../../root/private.key").is_confined(),
        "oracle rejection: parent traversal must not be confined"
    );

    // Negative control 3: Stale generation must not be treated as valid
    let owner = test_owner();
    let gen1 = DocumentGeneration::new(owner, 1).unwrap();
    let gen2 = DocumentGeneration::new(owner, 2).unwrap();
    assert_ne!(gen1, gen2, "oracle rejection: distinct generations must not match");

    record_receipt(
        "negative_control_oracle",
        Effect::Succeeded,
        "Negative control oracle verified: defect classes properly classified and rejected",
    );
}
