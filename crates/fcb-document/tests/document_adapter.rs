#![forbid(unsafe_code)]

use std::sync::Arc;

use fcb_core::{
    ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId, SourceRevision,
};
use fcb_document::{
    resolve_reading_selection, verify_provenance_truthfulness, DocumentBudgets,
    DocumentDisplayPlan, DocumentError, DocumentLens, DocumentSession,
    DocumentViewConstraints, TextSelectionRange,
};
use fcb_source::{CaptureRequest, CompleteCapture};

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
    let readme_bytes = include_bytes!("../../../README.md");
    let capture = create_capture(readme_bytes);
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 100);
    let generation = test_generation(owner, 1);

    let session = DocumentSession::new(doc_id, &capture, generation).expect("session creation succeeds");
    assert_eq!(session.id(), doc_id);
    assert_eq!(session.generation(), generation);
    assert_eq!(session.digest(), capture.digest());

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
    assert!(output.total_width > 0, "total width must be positive");
    assert!(output.consumed_blocks > 0, "consumed blocks must be positive");
    assert_eq!(output.consumed_bytes, readme_bytes.len(), "all bytes consumed");

    // Verify nested provenance truthfulness: zero invented contiguous slices
    let report = verify_provenance_truthfulness(
        output.source_map.provenance_graph(),
        session.source_text(),
    )
    .expect("provenance truthfulness check succeeds");

    assert!(
        report.zero_invented_contiguous_slices,
        "provenance must never invent contiguous slices"
    );
    assert!(report.total_nodes > 0, "provenance nodes must exist");

    // Verify semantic fixture serialization
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
    let future_gen = test_generation(owner, 11);

    let session = DocumentSession::new(doc_id, &capture, valid_gen).unwrap();

    // 1. Validate request identity checks
    let file = capture.request().file();
    let rev = capture.request().revision();
    let digest = capture.digest();

    // Valid request succeeds
    assert!(session.validate_request(file, rev, digest, valid_gen).is_ok());

    // Stale generation refused
    assert_eq!(
        session.validate_request(file, rev, digest, stale_gen),
        Err(DocumentError::StaleRequest {
            expected: valid_gen,
            actual: stale_gen,
        })
    );

    // Future generation also refused
    assert_eq!(
        session.validate_request(file, rev, digest, future_gen),
        Err(DocumentError::StaleRequest {
            expected: valid_gen,
            actual: future_gen,
        })
    );

    // Mismatched revision refused
    let wrong_rev = test_revision(owner, 99);
    assert_eq!(
        session.validate_request(file, wrong_rev, digest, valid_gen),
        Err(DocumentError::StaleRevision {
            expected: rev,
            actual: wrong_rev,
        })
    );

    // Mismatched file refused
    let wrong_file = test_file_id(owner, 99);
    assert_eq!(
        session.validate_request(wrong_file, rev, digest, valid_gen),
        Err(DocumentError::MismatchedFile {
            expected: file,
            actual: wrong_file,
        })
    );

    // 2. Headless consumption with stale generation refused with zero parsing
    let constraints = DocumentViewConstraints::default();
    let budgets = DocumentBudgets::default();

    let res = session.consume_headless(stale_gen, constraints, budgets);
    assert_eq!(
        res.err(),
        Some(DocumentError::StaleRequest {
            expected: valid_gen,
            actual: stale_gen,
        })
    );

    // 3. Resumable creation with stale generation refused
    let res_stepper = session.create_resumable(stale_gen, 2);
    assert!(matches!(res_stepper.err(), Some(DocumentError::StaleRequest { .. })));

    // 4. DisplayPlan delivery validation refuses stale generation
    let plan = DocumentDisplayPlan::new(valid_gen, franken_markdown::DisplayList::new(), Vec::new());
    assert_eq!(
        plan.validate_delivery(stale_gen),
        Err(DocumentError::StaleRequest {
            expected: stale_gen,
            actual: valid_gen,
        })
    );
}

#[test]
fn test_resumable_flow_stepping_and_display_list_equivalence() {
    let markdown = "\
# Chapter 1

First paragraph of introductory prose.

## Section 1.1

Second paragraph discussing implementation details.

- List item alpha
- List item beta
- List item gamma

![Diagram](assets/arch.png)

Final concluding thoughts.
";
    let capture = create_capture(markdown.as_bytes());
    let owner = test_owner();
    let session = DocumentSession::new(
        test_doc_id(owner, 102),
        &capture,
        test_generation(owner, 1),
    )
    .unwrap();

    let mut stepper = session.create_resumable(test_generation(owner, 1), 2).unwrap();
    assert_eq!(stepper.steps_taken(), 0);
    assert!(!stepper.is_finished());

    let mut _total_blocks_stepped = 0;
    let mut step_count = 0;
    while let Some(step) = stepper.step().unwrap() {
        step_count += 1;
        _total_blocks_stepped += step.blocks.len();
    }

    assert!(stepper.is_finished());
    assert!(step_count >= 2, "must have taken multiple steps");
    assert_eq!(stepper.steps_taken(), step_count + 1);

    let display_list = stepper.to_display_list();
    let plan = DocumentDisplayPlan::new(session.generation(), display_list, Vec::new());

    assert!(plan.item_count() > 0, "materialized items must be present");
    assert!(plan.reading_node_count() > 0, "reading nodes must be present");
    assert!(plan.is_complete());
}

#[test]
fn test_selection_resolution_and_disjoint_provenance() {
    let markdown = "\
# Section A

First paragraph with **bold** text.

# Section B

Second paragraph with *italic* text.
";
    let capture = create_capture(markdown.as_bytes());
    let owner = test_owner();
    let session = DocumentSession::new(
        test_doc_id(owner, 103),
        &capture,
        test_generation(owner, 1),
    )
    .unwrap();

    let output = session
        .consume_headless(
            session.generation(),
            DocumentViewConstraints::default(),
            DocumentBudgets::default(),
        )
        .unwrap();

    // Select across multiple elements
    let selection = TextSelectionRange {
        start: 0,
        end: 50,
    };

    let resolution = resolve_reading_selection(selection, &output.source_map, &capture).unwrap();
    assert!(!resolution.reading_text.is_empty(), "reading text must be extracted");
    assert!(!resolution.source_ranges.is_empty(), "source ranges must be mapped");

    // Verify all source ranges point strictly to valid byte offsets within capture
    for range in &resolution.source_ranges {
        let (start, end) = range.as_usize_bounds().unwrap();
        assert!(start <= end);
        assert!(end <= capture.bytes().len());
        let slice = &capture.bytes()[start..end];
        assert!(!slice.is_empty());
    }
}

#[test]
fn test_lens_visibility_and_scrolling() {
    let lens = DocumentLens::new(80, 200, 16, 1).unwrap();
    assert_eq!(lens.viewport_width(), 80);
    assert_eq!(lens.viewport_height(), 200);
    assert_eq!(lens.scroll_y(), 0);

    let (top, bottom) = lens.visible_y_range(1000);
    assert_eq!(top, 0);
    assert_eq!(bottom, 200);

    let mut scrolled = lens;
    scrolled.set_scroll_y(300);
    let (top, bottom) = scrolled.visible_y_range(1000);
    assert_eq!(top, 300);
    assert_eq!(bottom, 500);

    // Test visible line slicing
    let lines = vec![
        fcb_document::DocumentFlowLine {
            line_index: 0,
            baseline_y: 16,
            rendered_text: "Line 1".to_string(),
            rendered_range: fcb_document::TextSelectionRange { start: 0, end: 6 },
            source_span: fcb_document::SourceSpan { start: 0, end: 6 },
            element_indices: vec![0],
        },
        fcb_document::DocumentFlowLine {
            line_index: 1,
            baseline_y: 32,
            rendered_text: "Line 2".to_string(),
            rendered_range: fcb_document::TextSelectionRange { start: 7, end: 13 },
            source_span: fcb_document::SourceSpan { start: 7, end: 13 },
            element_indices: vec![1],
        },
        fcb_document::DocumentFlowLine {
            line_index: 2,
            baseline_y: 350,
            rendered_text: "Line 3".to_string(),
            rendered_range: fcb_document::TextSelectionRange { start: 14, end: 20 },
            source_span: fcb_document::SourceSpan { start: 14, end: 20 },
            element_indices: vec![2],
        },
        fcb_document::DocumentFlowLine {
            line_index: 3,
            baseline_y: 366,
            rendered_text: "Line 4".to_string(),
            rendered_range: fcb_document::TextSelectionRange { start: 21, end: 27 },
            source_span: fcb_document::SourceSpan { start: 21, end: 27 },
            element_indices: vec![3],
        },
        fcb_document::DocumentFlowLine {
            line_index: 4,
            baseline_y: 800,
            rendered_text: "Line 5".to_string(),
            rendered_range: fcb_document::TextSelectionRange { start: 28, end: 34 },
            source_span: fcb_document::SourceSpan { start: 28, end: 34 },
            element_indices: vec![4],
        },
    ];

    // Scrolled to y=300, height=200 => range [300, 500]
    let visible = scrolled.visible_lines(&lines);
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].rendered_text, "Line 3");
    assert_eq!(visible[1].rendered_text, "Line 4");
}

#[test]
fn test_invalid_utf8_capture_rejected_cleanly() {
    let invalid_utf8: &[u8] = &[0xff, 0xfe, 0x80, 0x81];
    let capture = create_capture(invalid_utf8);
    let owner = test_owner();

    let res = DocumentSession::new(
        test_doc_id(owner, 104),
        &capture,
        test_generation(owner, 1),
    );
    assert_eq!(res.err(), Some(DocumentError::InvalidUtf8));
}

#[test]
fn test_budget_defense_on_adversarial_input() {
    let text = "# Heading\n\n".to_string() + &"Excessive length text ".repeat(100);
    let capture = create_capture(text.as_bytes());
    let owner = test_owner();
    let session = DocumentSession::new(
        test_doc_id(owner, 105),
        &capture,
        test_generation(owner, 1),
    )
    .unwrap();

    let restrictive_budgets = DocumentBudgets {
        max_blocks: 1000,
        max_bytes: 50, // Only 50 bytes allowed
        max_lines: 1000,
        max_items: 100,
    };

    let res = session.consume_headless(
        session.generation(),
        DocumentViewConstraints::default(),
        restrictive_budgets,
    );

    assert!(
        matches!(res.err(), Some(DocumentError::Flow(franken_markdown::FlowError::BudgetExceeded { .. }))),
        "exceeding max_bytes budget must return FlowError::BudgetExceeded"
    );
}

#[test]
fn test_owner_mismatch_rejected() {
    let owner_a = ArenaOwnerId::new(1).unwrap();
    let owner_b = ArenaOwnerId::new(2).unwrap();

    let file = test_file_id(owner_a, 1);
    let rev = test_revision(owner_a, 1);
    let request = CaptureRequest::new(file, rev).unwrap();
    let bytes = b"test";
    let declared_len = ByteLength::new(bytes.len() as u64);
    let capture = CompleteCapture::new(request, declared_len, Arc::from(&bytes[..])).unwrap();

    // DocumentId with owner_b, but capture has owner_a
    let doc_id = test_doc_id(owner_b, 106);
    let generation = test_generation(owner_a, 1);

    let res = DocumentSession::new(doc_id, &capture, generation);
    assert_eq!(res.err(), Some(DocumentError::OwnerMismatch));
}
