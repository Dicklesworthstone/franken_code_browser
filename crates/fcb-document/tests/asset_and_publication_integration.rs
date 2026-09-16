#![forbid(unsafe_code)]

use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId, SourceRevision,
};
use fcb_document::{
    AssetDomain, AssetRequest, AssetRequestId, BoundedAssetBudgets, BoundedAssetRegistry,
    DisplayItem, DisplayList, DisplayRect, DisplayTextRun, DocumentBudgets,
    DocumentDisplayPlan, DocumentPublication, DocumentSession, DocumentViewConstraints,
    SourceSpan,
};
use fcb_source::{CaptureRequest, CompleteCapture, ObservationDigest};

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_file_id(owner: ArenaOwnerId, id: u64) -> FileId {
    FileId::new(owner, id).unwrap()
}

fn test_revision(owner: ArenaOwnerId, rev: u64) -> SourceRevision {
    SourceRevision::new(owner, rev).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

fn test_doc_id(owner: ArenaOwnerId, id: u64) -> DocumentId {
    DocumentId::new(owner, id).unwrap()
}

fn create_capture(bytes: &[u8]) -> CompleteCapture {
    let owner = test_owner();
    let file = test_file_id(owner, 1);
    let rev = test_revision(owner, 10);
    let request = CaptureRequest::new(file, rev).unwrap();
    let declared_len = ByteLength::new(bytes.len() as u64);
    CompleteCapture::new(request, declared_len, Arc::from(bytes)).unwrap()
}

#[test]
fn test_real_readme_headless_output_consumed_through_public_api() {
    let readme_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md");
    let readme_bytes = std::fs::read(readme_path).expect("README.md must be readable");
    let capture = create_capture(&readme_bytes);
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 1);
    let generation = test_generation(owner, 1);

    let session = DocumentSession::new(doc_id, &capture, generation).expect("session creation succeeds");
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
    assert!(
        output.semantic_fixture.contains("SEMANTIC LAYOUT FIXTURE"),
        "fixture must contain semantic header"
    );
}

#[test]
fn test_stale_document_request_refused_without_cloning_ast_per_frame() {
    let text = b"# Title\n\nSome paragraph text that should not be re-parsed.";
    let capture = create_capture(text);
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 101);
    let valid_gen = test_generation(owner, 10);
    let stale_gen = test_generation(owner, 9);

    let session = DocumentSession::new(doc_id, &capture, valid_gen).unwrap();

    let constraints = DocumentViewConstraints {
        viewport_width: 80,
        line_height: 16,
        char_width: 1,
        max_viewport_lines: None,
    };
    let budgets = DocumentBudgets::default();

    let err = session
        .consume_headless(stale_gen, constraints, budgets)
        .unwrap_err();
    assert_eq!(err.code(), "DOCUMENT_STALE_REQUEST");
}

#[test]
fn test_asset_domain_confinement_authorization_and_escapes() {
    // 1. Confined relative paths
    assert!(AssetDomain::classify("images/architecture.png").is_confined());
    assert!(AssetDomain::classify("./assets/diagram.svg").is_confined());
    assert!(AssetDomain::classify("sub/nested/file.txt").is_confined());
    assert!(AssetDomain::classify("file:///local/relative/path.png").is_confined());

    // 2. Traversal escapes
    assert!(!AssetDomain::classify("../escaped.png").is_confined());
    assert!(!AssetDomain::classify("assets/../../etc/passwd").is_confined());
    assert!(!AssetDomain::classify("assets/%2e%2e/secret").is_confined());

    // 3. Absolute escapes
    assert!(!AssetDomain::classify("/etc/passwd").is_confined());
    assert!(!AssetDomain::classify("C:\\Windows\\system32").is_confined());

    // 4. Remote and script schemes
    assert!(!AssetDomain::classify("http://example.com/test.png").is_confined());
    assert!(!AssetDomain::classify("https://example.com/test.png").is_confined());
    assert!(!AssetDomain::classify("ftp://files.org/asset.bin").is_confined());
    assert!(!AssetDomain::classify("javascript:alert(1)").is_confined());
    assert!(!AssetDomain::classify("data:image/png;base64,AAAA").is_confined());

    // 5. Hostile control characters & nulls
    assert!(!AssetDomain::classify("assets/test\0.png").is_confined());
    assert!(!AssetDomain::classify("assets/\u{202E}gnp.tset").is_confined());
    assert!(!AssetDomain::classify("   ").is_confined());
}

#[test]
fn test_bounded_asset_registry_budget_defense() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 200);
    let generation = test_generation(owner, 1);

    let budgets = BoundedAssetBudgets {
        max_pending_requests: 2,
        max_total_asset_bytes: 1024,
        max_single_asset_bytes: 512,
        ..BoundedAssetBudgets::default()
    };

    let mut registry = BoundedAssetRegistry::new(doc_id, generation, budgets);

    // Register asset 1
    let req1 = AssetRequest {
        id: AssetRequestId(1),
        kind: "image",
        url: "images/photo1.png".to_string(),
        source_offset: 10,
        generation: generation.get(),
        estimated_width: 100,
        estimated_height: 100,
        alt_text: "Photo 1".to_string(),
    };
    assert!(registry.authorize_and_register(&req1).is_ok());

    // Register asset 2
    let req2 = AssetRequest {
        id: AssetRequestId(2),
        kind: "image",
        url: "images/photo2.png".to_string(),
        source_offset: 20,
        generation: generation.get(),
        estimated_width: 100,
        estimated_height: 100,
        alt_text: "Photo 2".to_string(),
    };
    assert!(registry.authorize_and_register(&req2).is_ok());

    // Register asset 3: exceeds max_pending_requests (budget = 2)
    let req3 = AssetRequest {
        id: AssetRequestId(3),
        kind: "image",
        url: "images/photo3.png".to_string(),
        source_offset: 30,
        generation: generation.get(),
        estimated_width: 100,
        estimated_height: 100,
        alt_text: "Photo 3".to_string(),
    };
    let err = registry.authorize_and_register(&req3).unwrap_err();
    assert_eq!(err.code(), "DOCUMENT_ASSET_BUDGET_EXCEEDED");

    // Single asset byte limit defense (> 512 bytes)
    let large_single = vec![0u8; 600];
    let err = registry
        .deliver_resolution(1, generation, 100, 100, Some(large_single))
        .unwrap_err();
    assert_eq!(err.code(), "DOCUMENT_ASSET_BUDGET_EXCEEDED");

    // Resolve valid payload
    let valid_payload = vec![0u8; 400];
    assert!(registry.deliver_resolution(1, generation, 100, 100, Some(valid_payload)).is_ok());

    // Total byte limit defense (> 1024 bytes)
    let second_payload = vec![0u8; 500];
    assert!(registry.deliver_resolution(2, generation, 100, 100, Some(second_payload)).is_ok());
    assert_eq!(registry.total_resolved_bytes(), 900);

    // Unknown request ID rejection
    let err = registry.deliver_resolution(999, generation, 100, 100, None).unwrap_err();
    assert_eq!(err.code(), "DOCUMENT_UNKNOWN_ASSET_REQUEST");
}

#[test]
fn test_asset_registry_generation_tracking_and_advance() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 300);
    let generation1 = test_generation(owner, 1);
    let generation2 = test_generation(owner, 2);

    let mut registry = BoundedAssetRegistry::new(doc_id, generation1, BoundedAssetBudgets::default());

    let req = AssetRequest {
        id: AssetRequestId(1),
        kind: "image",
        url: "images/test.png".to_string(),
        source_offset: 5,
        generation: generation1.get(),
        estimated_width: 50,
        estimated_height: 50,
        alt_text: "Test".to_string(),
    };
    assert!(registry.authorize_and_register(&req).is_ok());

    // Stale delivery rejected
    let err = registry.deliver_resolution(1, generation2, 50, 50, None).unwrap_err();
    assert_eq!(err.code(), "DOCUMENT_STALE_ASSET_GENERATION");

    // Advance generation drains older pending requests
    registry.advance_generation(generation2);
    assert_eq!(registry.pending_count(), 0);
    assert_eq!(registry.resolved_count(), 0);
    assert_eq!(registry.current_generation(), generation2);
}

#[test]
fn test_document_publication_retained_renderer_integration() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 400);
    let generation = test_generation(owner, 5);
    let stale_generation = test_generation(owner, 4);
    let digest = ObservationDigest::observe(b"sample digest");

    let mut display_list = DisplayList::new();
    display_list.push_item(DisplayItem::Text(DisplayTextRun {
        bounds: DisplayRect::new(0.0, 0.0, 100.0, 20.0),
        text: "Line 1".to_string(),
        font_run: None,
        color_role: "text".to_string(),
        source_span: SourceSpan::new(0, 6),
        font_size: 14.0,
    }));
    display_list.push_item(DisplayItem::Text(DisplayTextRun {
        bounds: DisplayRect::new(0.0, 30.0, 100.0, 20.0),
        text: "Line 2".to_string(),
        font_run: None,
        color_role: "text".to_string(),
        source_span: SourceSpan::new(8, 14),
        font_size: 14.0,
    }));

    let display_plan = DocumentDisplayPlan::new(generation, display_list, Vec::new());
    let publication = DocumentPublication::new(doc_id, generation, digest, display_plan).unwrap();

    assert_eq!(publication.document_id(), doc_id);
    assert_eq!(publication.generation(), generation);
    assert_eq!(publication.digest(), digest);
    assert!(publication.is_complete());
    assert!(!publication.is_stale_for(generation));
    assert!(publication.is_stale_for(stale_generation));

    // Presentation generation validation
    assert!(publication.validate_presentation(generation).is_ok());
    let err = publication.validate_presentation(stale_generation).unwrap_err();
    assert_eq!(err.code(), "DOCUMENT_STALE_REQUEST");

    // Retained renderer viewport query: only items intersecting viewport are returned
    let upper_viewport = DisplayRect::new(0.0, 0.0, 200.0, 25.0);
    let visible_upper = publication.visible_items(upper_viewport);
    assert_eq!(visible_upper.len(), 1);

    let lower_viewport = DisplayRect::new(0.0, 25.0, 200.0, 30.0);
    let visible_lower = publication.visible_items(lower_viewport);
    assert_eq!(visible_lower.len(), 1);

    let full_viewport = DisplayRect::new(0.0, 0.0, 200.0, 100.0);
    let visible_all = publication.visible_items(full_viewport);
    assert_eq!(visible_all.len(), 2);

    let outside_viewport = DisplayRect::new(500.0, 500.0, 50.0, 50.0);
    let visible_none = publication.visible_items(outside_viewport);
    assert_eq!(visible_none.len(), 0);
}
