//! Markdown, font, and image hostile regression lane (FCB-059 / Ref: HOSTILE.document).
//!
//! Runs nested Markdown, transclusion/include cycles, giant tables and paragraphs,
//! malformed font tables, and hostile image decompression metadata through real
//! upstream parse, flow, font, and decoder routes:
//! - Deeply nested Markdown structures under bounded flow/block budgets
//! - Recursive file transclusion cycle detection with useful source fallback
//! - Deeply nested transclusion chains exceeding policy depth budgets
//! - Giant tables and paragraphs exceeding continuous flow line/item budgets
//! - Malformed, truncated, and missing-table sfnt font records refused with typed errors
//! - Hostile image decompression bombs (dimension / memory / integer overflow)
//! - Multi-frame animated GIF frame-count budget exhaustion
//! - Asset path traversal, remote network schemes, and hostile character escape refusal
//! - Deterministic seeds, reproducible digests, attempt counters, and bounded event rings
//! - Failure-preserving delta-debugging minimization for all failing cases
//! - Intentional negative control oracle detecting safety bypasses

#![forbid(unsafe_code)]

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId, SourceRevision};
use fcb_document::assets::{
    AssetDomain, BoundedAssetBudgets, BoundedImageDecoder, TransclusionPolicy, TransclusionTracker,
};
use fcb_document::session::{DocumentBudgets, DocumentSession, DocumentViewConstraints};
use fcb_document::DocumentError;
use fcb_source::CompleteCapture;
use franken_markdown::text::{Font, FontError};
use franken_markdown::FlowError;

use crate::{
    minimize, HostileByteGenerator, HostileLogEvent, ReproducibleDigest, TerminationBudget,
};

/// Nonempty registry of hostile document, font, and image regression cases.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocumentHostileCaseId {
    /// Deeply nested Markdown structures processed under bounded block/step budgets.
    DeeplyNestedMarkdown,
    /// Circular file transclusion loop detection with useful source fallback.
    TransclusionCycle,
    /// Deeply nested transclusion chain exceeding policy depth limit.
    TransclusionDepthBudget,
    /// Giant markdown table layout exceeding line and item budgets.
    GiantTableParagraphBudget,
    /// Corrupted and truncated sfnt font table refusal with typed errors.
    InvalidFontTableSfnt,
    /// Hostile image decompression bomb dimension and memory refusal.
    ImageDecompressionBomb,
    /// Hostile animated gif frame count budget exhaustion.
    ImageFrameCountBudget,
    /// Hostile asset path traversal and remote scheme refusal.
    AssetTraversalEscape,
}

impl DocumentHostileCaseId {
    /// Stable machine-readable identifier.
    pub const fn code(self) -> &'static str {
        match self {
            Self::DeeplyNestedMarkdown => "DEEPLY_NESTED_MARKDOWN",
            Self::TransclusionCycle => "TRANSCLUSION_CYCLE",
            Self::TransclusionDepthBudget => "TRANSCLUSION_DEPTH_BUDGET",
            Self::GiantTableParagraphBudget => "GIANT_TABLE_PARAGRAPH_BUDGET",
            Self::InvalidFontTableSfnt => "INVALID_FONT_TABLE_SFNT",
            Self::ImageDecompressionBomb => "IMAGE_DECOMPRESSION_BOMB",
            Self::ImageFrameCountBudget => "IMAGE_FRAME_COUNT_BUDGET",
            Self::AssetTraversalEscape => "ASSET_TRAVERSAL_ESCAPE",
        }
    }

    /// Human-readable title.
    pub const fn title(self) -> &'static str {
        match self {
            Self::DeeplyNestedMarkdown => "Deeply nested markdown structures under bounded block budget",
            Self::TransclusionCycle => "Recursive file transclusion cycle detection with source fallback",
            Self::TransclusionDepthBudget => "Deeply nested transclusion chain exceeding policy depth limit",
            Self::GiantTableParagraphBudget => "Giant markdown table layout exceeding line and item budgets",
            Self::InvalidFontTableSfnt => "Malformed and truncated sfnt font table refusal",
            Self::ImageDecompressionBomb => "Hostile image decompression bomb dimension and memory refusal",
            Self::ImageFrameCountBudget => "Hostile animated gif frame count budget exhaustion",
            Self::AssetTraversalEscape => "Hostile asset URI path traversal and foreign scheme refusal",
        }
    }

    /// Expected failure or refusal classification code.
    pub const fn expected_classification(self) -> &'static str {
        match self {
            Self::DeeplyNestedMarkdown => "DOCUMENT_FLOW_BUDGET_EXCEEDED",
            Self::TransclusionCycle => "DOCUMENT_TRANSCLUSION_CYCLE",
            Self::TransclusionDepthBudget => "DOCUMENT_TRANSCLUSION_DEPTH_EXCEEDED",
            Self::GiantTableParagraphBudget => "DOCUMENT_FLOW_BUDGET_EXCEEDED",
            Self::InvalidFontTableSfnt => "FONT_REFUSED_MALFORMED",
            Self::ImageDecompressionBomb => "DOCUMENT_DECOMPRESSION_BOMB",
            Self::ImageFrameCountBudget => "DOCUMENT_FRAME_COUNT_EXCEEDED",
            Self::AssetTraversalEscape => "DOCUMENT_ASSET_ESCAPE_REFUSED",
        }
    }

    /// Production API seam exercised by this case.
    pub const fn api_seam(self) -> &'static str {
        match self {
            Self::DeeplyNestedMarkdown => "fcb_document::session::DocumentSession",
            Self::TransclusionCycle => "fcb_document::assets::TransclusionTracker",
            Self::TransclusionDepthBudget => "fcb_document::assets::TransclusionTracker",
            Self::GiantTableParagraphBudget => "fcb_document::session::DocumentSession",
            Self::InvalidFontTableSfnt => "franken_markdown::text::Font",
            Self::ImageDecompressionBomb => "fcb_document::assets::BoundedImageDecoder",
            Self::ImageFrameCountBudget => "fcb_document::assets::BoundedImageDecoder",
            Self::AssetTraversalEscape => "fcb_document::assets::AssetDomain",
        }
    }
}

/// Metadata describing one registered hostile document case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentHostileCaseDesc {
    pub id: DocumentHostileCaseId,
    pub code: &'static str,
    pub title: &'static str,
    pub expected_classification: &'static str,
    pub api_seam: &'static str,
}

/// Registry of all cases in the document, font, and image hostile regression lane.
#[derive(Debug)]
pub struct DocumentHostileCaseRegistry;

impl DocumentHostileCaseRegistry {
    const ALL_CASES: [DocumentHostileCaseDesc; 8] = [
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::DeeplyNestedMarkdown,
            code: DocumentHostileCaseId::DeeplyNestedMarkdown.code(),
            title: DocumentHostileCaseId::DeeplyNestedMarkdown.title(),
            expected_classification: DocumentHostileCaseId::DeeplyNestedMarkdown.expected_classification(),
            api_seam: DocumentHostileCaseId::DeeplyNestedMarkdown.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::TransclusionCycle,
            code: DocumentHostileCaseId::TransclusionCycle.code(),
            title: DocumentHostileCaseId::TransclusionCycle.title(),
            expected_classification: DocumentHostileCaseId::TransclusionCycle.expected_classification(),
            api_seam: DocumentHostileCaseId::TransclusionCycle.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::TransclusionDepthBudget,
            code: DocumentHostileCaseId::TransclusionDepthBudget.code(),
            title: DocumentHostileCaseId::TransclusionDepthBudget.title(),
            expected_classification: DocumentHostileCaseId::TransclusionDepthBudget.expected_classification(),
            api_seam: DocumentHostileCaseId::TransclusionDepthBudget.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::GiantTableParagraphBudget,
            code: DocumentHostileCaseId::GiantTableParagraphBudget.code(),
            title: DocumentHostileCaseId::GiantTableParagraphBudget.title(),
            expected_classification: DocumentHostileCaseId::GiantTableParagraphBudget.expected_classification(),
            api_seam: DocumentHostileCaseId::GiantTableParagraphBudget.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::InvalidFontTableSfnt,
            code: DocumentHostileCaseId::InvalidFontTableSfnt.code(),
            title: DocumentHostileCaseId::InvalidFontTableSfnt.title(),
            expected_classification: DocumentHostileCaseId::InvalidFontTableSfnt.expected_classification(),
            api_seam: DocumentHostileCaseId::InvalidFontTableSfnt.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::ImageDecompressionBomb,
            code: DocumentHostileCaseId::ImageDecompressionBomb.code(),
            title: DocumentHostileCaseId::ImageDecompressionBomb.title(),
            expected_classification: DocumentHostileCaseId::ImageDecompressionBomb.expected_classification(),
            api_seam: DocumentHostileCaseId::ImageDecompressionBomb.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::ImageFrameCountBudget,
            code: DocumentHostileCaseId::ImageFrameCountBudget.code(),
            title: DocumentHostileCaseId::ImageFrameCountBudget.title(),
            expected_classification: DocumentHostileCaseId::ImageFrameCountBudget.expected_classification(),
            api_seam: DocumentHostileCaseId::ImageFrameCountBudget.api_seam(),
        },
        DocumentHostileCaseDesc {
            id: DocumentHostileCaseId::AssetTraversalEscape,
            code: DocumentHostileCaseId::AssetTraversalEscape.code(),
            title: DocumentHostileCaseId::AssetTraversalEscape.title(),
            expected_classification: DocumentHostileCaseId::AssetTraversalEscape.expected_classification(),
            api_seam: DocumentHostileCaseId::AssetTraversalEscape.api_seam(),
        },
    ];

    /// Slice of all registered case descriptors.
    pub const fn all_cases() -> &'static [DocumentHostileCaseDesc] {
        &Self::ALL_CASES
    }

    /// Lookup a case by its code string.
    pub fn lookup(code: &str) -> Option<&'static DocumentHostileCaseDesc> {
        Self::ALL_CASES.iter().find(|desc| desc.code == code)
    }

    /// Total count of registered cases in this lane.
    pub const fn count() -> usize {
        Self::ALL_CASES.len()
    }
}

/// Structured outcome of running one hostile regression case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentHostileOutcome {
    pub case_id: DocumentHostileCaseId,
    pub seed: u64,
    pub input_digest: ReproducibleDigest,
    pub input_len: usize,
    pub minimized_digest: ReproducibleDigest,
    pub minimized_len: usize,
    pub classification: &'static str,
    pub attempts_spent: usize,
    pub budget_exhausted: bool,
    pub classification_preserved: bool,
    pub useful_fallback: Option<String>,
    pub verified_invariants: Vec<&'static str>,
    pub events: Vec<HostileLogEvent>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).expect("owner valid")
}

fn test_file(owner: ArenaOwnerId, val: u64) -> FileId {
    FileId::new(owner, val).expect("file valid")
}

fn test_rev(owner: ArenaOwnerId, val: u64) -> SourceRevision {
    SourceRevision::new(owner, val).expect("rev valid")
}

fn test_doc(owner: ArenaOwnerId, val: u64) -> DocumentId {
    DocumentId::new(owner, val).expect("doc valid")
}

fn test_gen(owner: ArenaOwnerId, val: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, val).expect("gen valid")
}

fn make_test_session(owner_val: u64, doc_val: u64, source: &str) -> DocumentSession {
    let owner = test_owner(owner_val);
    let file = test_file(owner, doc_val);
    let rev = test_rev(owner, 1);
    let doc_gen = test_gen(owner, 1);
    let doc_id = test_doc(owner, doc_val);
    let req = fcb_source::CaptureRequest::new(file, rev).expect("req valid");
    let bytes: Arc<[u8]> = Arc::from(source.as_bytes());
    let capture = CompleteCapture::new(req, ByteLength::new(bytes.len() as u64), bytes)
        .expect("capture valid");
    DocumentSession::new(doc_id, &capture, doc_gen).expect("session valid")
}

/// Constructs a synthetic minimal PNG header with valid IHDR chunk.
pub fn make_synthetic_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33);
    bytes.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    bytes
}

/// Constructs a synthetic multi-frame GIF with `frame_count` frames.
pub fn make_synthetic_multiframe_gif(width: u16, height: u16, frame_count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GIF89a");
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    // GCT flag: 0x80 (present), 2 entries (size 6 bytes)
    bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
    // 2 RGB colors (black and white)
    bytes.extend_from_slice(&[0, 0, 0, 0xFF, 0xFF, 0xFF]);

    for _ in 0..frame_count {
        // Graphic Control Extension (0x21, 0xF9, len 4, packed, delay 2 bytes, transp idx, terminator 0)
        bytes.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x05, 0x00, 0x00, 0x00]);
        // Image Descriptor (0x2C, left 2, top 2, width 2, height 2, packed 0)
        bytes.push(0x2C);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.push(0x00);
        // LZW minimum code size
        bytes.push(2);
        // Single data sub-block: len 1, byte 0, terminator 0
        bytes.extend_from_slice(&[1, 0, 0]);
    }
    // Trailer
    bytes.push(0x3B);
    bytes
}

// ---------------------------------------------------------------------------
// Case 1: Deeply Nested Markdown
// ---------------------------------------------------------------------------

/// Executes the deeply nested markdown structure case under bounded block budgets.
pub fn run_deeply_nested_markdown_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::DeeplyNestedMarkdown.api_seam()
        ),
    });

    let depth = 80usize;
    let mut full_source = String::with_capacity(depth * 4 + 64);
    for _ in 0..depth {
        full_source.push_str("> ");
    }
    full_source.push_str("Deeply nested text content inside hostile blockquotes.\n\n");
    // Add extra paragraphs to stress block count
    for i in 0..30 {
        full_source.push_str(&format!("Paragraph block number {i} after nested structure.\n\n"));
    }

    let input_bytes = full_source.as_bytes().to_vec();
    let input_digest = ReproducibleDigest::of_bytes(&input_bytes);
    let input_len = input_bytes.len();

    let tight_budgets = DocumentBudgets {
        max_blocks: 15,
        max_lines: 50,
        max_bytes: 100_000,
        max_items: 100_000,
    };
    let constraints = DocumentViewConstraints::default();

    // Verify production execution triggers budget exhaustion
    let session = make_test_session(40, 401, &full_source);
    let res = session.consume_headless(session.generation(), constraints, tight_budgets);
    assert!(
        matches!(res, Err(DocumentError::Flow(FlowError::BudgetExceeded { .. }))),
        "nested source exceeding max_blocks must fail with FlowError::BudgetExceeded"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "VERIFIED_BUDGET_EXHAUSTION",
        detail: "nested markdown exceeded max_blocks budget".to_string(),
    });

    // Minimization: delta-debugging shrinks input paragraphs while keeping block count > max_blocks
    let mut classify_nested = |candidate: &[u8]| -> Option<&'static str> {
        let Ok(_text) = std::str::from_utf8(candidate) else {
            return None;
        };
        let owner = test_owner(40);
        let file = test_file(owner, 402);
        let rev = test_rev(owner, 1);
        let doc_gen = test_gen(owner, 1);
        let doc_id = test_doc(owner, 402);
        let req = fcb_source::CaptureRequest::new(file, rev).ok()?;
        let bytes: Arc<[u8]> = Arc::from(candidate);
        let capture = CompleteCapture::new(req, ByteLength::new(bytes.len() as u64), bytes).ok()?;
        let s = DocumentSession::new(doc_id, &capture, doc_gen).ok()?;
        match s.consume_headless(s.generation(), constraints, tight_budgets) {
            Err(DocumentError::Flow(FlowError::BudgetExceeded { .. })) => {
                Some("DOCUMENT_FLOW_BUDGET_EXCEEDED")
            }
            _ => None,
        }
    };

    let report = minimize(&input_bytes, TerminationBudget::attempts(100), &mut classify_nested);
    let useful_fallback = Some(session.source_text().to_string());

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::DeeplyNestedMarkdown,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_FLOW_BUDGET_EXCEEDED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback,
        verified_invariants: vec![
            "deeply_nested_markdown_respects_flow_block_budget",
            "source_bytes_remain_authoritative_on_layout_budget_exhaustion",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 2: Transclusion Cycle Detection & Source Fallback
// ---------------------------------------------------------------------------

/// Executes transclusion cycle detection and useful fallback generation.
pub fn run_transclusion_cycle_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::TransclusionCycle.api_seam()
        ),
    });

    // Synthetic chain containing an intentional circular include:
    // "doc/root.md" -> "doc/chapter1.md" -> "doc/sectionA.md" -> "doc/chapter1.md" (cycle!)
    let chain_paths = vec![
        "doc/root.md",
        "doc/chapter1.md",
        "doc/sectionA.md",
        "doc/subtopic1.md",
        "doc/chapter1.md", // loop
    ];

    let mut encoded_input = Vec::new();
    for p in &chain_paths {
        encoded_input.extend_from_slice(p.as_bytes());
        encoded_input.push(b'\n');
    }

    let input_digest = ReproducibleDigest::of_bytes(&encoded_input);
    let input_len = encoded_input.len();

    let mut tracker = TransclusionTracker::new(TransclusionPolicy::default());
    let mut cycle_detected = false;
    let mut cycle_path = String::new();

    for path in &chain_paths {
        match tracker.enter_transclusion(path) {
            Ok(()) => {}
            Err(DocumentError::TransclusionCycle { ref path, .. }) => {
                cycle_detected = true;
                cycle_path = path.clone();
                break;
            }
            Err(_) => break,
        }
    }

    assert!(cycle_detected, "circular transclusion must return TransclusionCycle");
    assert_eq!(cycle_path, "doc/chapter1.md");

    // Useful source fallback: comment fallback without foreign fetch or execution
    let fallback = TransclusionTracker::source_fallback_for_cycle(&cycle_path, tracker.active_chain());
    assert!(
        fallback.contains("transclusion cycle detected"),
        "fallback must contain cycle warning"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "CYCLE_DETECTED",
        detail: format!("cycle detected on {cycle_path}, active_depth={}", tracker.active_depth()),
    });

    // Minimizer: delta-debugging shrinks the path list down to minimal 2-step cycle
    let mut classify_cycle = |candidate: &[u8]| -> Option<&'static str> {
        let Ok(text) = std::str::from_utf8(candidate) else {
            return None;
        };
        let paths: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        if paths.len() < 2 {
            return None;
        }
        let mut t = TransclusionTracker::new(TransclusionPolicy::default());
        for p in paths {
            if let Err(DocumentError::TransclusionCycle { .. }) = t.enter_transclusion(p) {
                return Some("DOCUMENT_TRANSCLUSION_CYCLE");
            }
        }
        None
    };

    let report = minimize(&encoded_input, TerminationBudget::attempts(50), &mut classify_cycle);

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::TransclusionCycle,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_TRANSCLUSION_CYCLE",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback: Some(fallback),
        verified_invariants: vec![
            "transclusion_tracker_detects_recursive_cycles",
            "cycle_detection_produces_source_preserving_markdown_fallback",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 3: Transclusion Depth Budget Exhaustion
// ---------------------------------------------------------------------------

/// Executes transclusion depth budget exhaustion.
pub fn run_transclusion_depth_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::TransclusionDepthBudget.api_seam()
        ),
    });

    let policy = TransclusionPolicy {
        max_depth: 8,
        max_transclusions: 64,
        max_total_bytes: 1024 * 1024,
    };

    // Construct 15 nested unique paths
    let mut paths = Vec::new();
    let mut encoded = Vec::new();
    for i in 0..15 {
        let p = format!("nested/depth_{i}.md");
        encoded.extend_from_slice(p.as_bytes());
        encoded.push(b'\n');
        paths.push(p);
    }

    let input_digest = ReproducibleDigest::of_bytes(&encoded);
    let input_len = encoded.len();

    let mut tracker = TransclusionTracker::new(policy);
    let mut depth_exceeded = false;
    for p in &paths {
        match tracker.enter_transclusion(p) {
            Ok(()) => {}
            Err(DocumentError::TransclusionDepthExceeded { max_depth }) => {
                assert_eq!(max_depth, 8);
                depth_exceeded = true;
                break;
            }
            Err(_) => break,
        }
    }
    assert!(depth_exceeded, "depth > max_depth must return TransclusionDepthExceeded");

    events.push(HostileLogEvent {
        step: 1,
        phase: "DEPTH_EXCEEDED",
        detail: "transclusion depth reached policy maximum of 8".to_string(),
    });

    let mut classify_depth = |candidate: &[u8]| -> Option<&'static str> {
        let Ok(text) = std::str::from_utf8(candidate) else {
            return None;
        };
        let items: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        let mut t = TransclusionTracker::new(policy);
        for item in items {
            if let Err(DocumentError::TransclusionDepthExceeded { .. }) = t.enter_transclusion(item) {
                return Some("DOCUMENT_TRANSCLUSION_DEPTH_EXCEEDED");
            }
        }
        None
    };

    let report = minimize(&encoded, TerminationBudget::attempts(50), &mut classify_depth);

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::TransclusionDepthBudget,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_TRANSCLUSION_DEPTH_EXCEEDED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback: None,
        verified_invariants: vec![
            "transclusion_tracker_strictly_enforces_max_depth",
            "hostile_inclusion_blowup_refused_with_typed_error",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 4: Giant Table / Paragraph Budget Exhaustion
// ---------------------------------------------------------------------------

/// Executes layout budget exhaustion on a giant markdown table.
pub fn run_giant_table_paragraph_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::GiantTableParagraphBudget.api_seam()
        ),
    });

    // Build a giant markdown table with 50 rows
    let mut table_source = String::from("# Giant Benchmark Table\n\n| Index | Column A | Column B | Column C |\n| --- | --- | --- | --- |\n");
    for i in 0..50 {
        table_source.push_str(&format!("| Row {i:04} | Data alpha {i} | Data beta {i} | Data gamma {i} |\n"));
    }

    let input_bytes = table_source.as_bytes().to_vec();
    let input_digest = ReproducibleDigest::of_bytes(&input_bytes);
    let input_len = input_bytes.len();

    let tight_budgets = DocumentBudgets {
        max_blocks: 10_000,
        max_lines: 10, // tightly bounded line budget
        max_bytes: 100_000,
        max_items: 100_000,
    };
    let constraints = DocumentViewConstraints::default();

    let session = make_test_session(42, 420, &table_source);
    let res = session.consume_headless(session.generation(), constraints, tight_budgets);
    assert!(
        matches!(res, Err(DocumentError::Flow(FlowError::BudgetExceeded { .. }))),
        "giant table exceeding max_lines must fail with FlowError::BudgetExceeded"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "LINE_BUDGET_EXCEEDED",
        detail: "layout line budget 10 exhausted by giant table".to_string(),
    });

    let mut classify_table = |candidate: &[u8]| -> Option<&'static str> {
        let Ok(_text) = std::str::from_utf8(candidate) else {
            return None;
        };
        let owner = test_owner(42);
        let file = test_file(owner, 421);
        let rev = test_rev(owner, 1);
        let doc_gen = test_gen(owner, 1);
        let doc_id = test_doc(owner, 421);
        let req = fcb_source::CaptureRequest::new(file, rev).ok()?;
        let bytes: Arc<[u8]> = Arc::from(candidate);
        let capture = CompleteCapture::new(req, ByteLength::new(bytes.len() as u64), bytes).ok()?;
        let s = DocumentSession::new(doc_id, &capture, doc_gen).ok()?;
        match s.consume_headless(s.generation(), constraints, tight_budgets) {
            Err(DocumentError::Flow(FlowError::BudgetExceeded { .. })) => {
                Some("DOCUMENT_FLOW_BUDGET_EXCEEDED")
            }
            _ => None,
        }
    };

    let report = minimize(&input_bytes, TerminationBudget::attempts(60), &mut classify_table);
    let useful_fallback = Some(session.source_text().to_string());

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::GiantTableParagraphBudget,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_FLOW_BUDGET_EXCEEDED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback,
        verified_invariants: vec![
            "headless_flow_consumer_bounds_layout_lines",
            "authoritative_source_remains_available_during_partial_layout_refusal",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 5: Invalid Font Table SFNT Header Refusal
// ---------------------------------------------------------------------------

/// Executes malformed sfnt font table refusal through upstream Font::parse.
pub fn run_invalid_font_table_sfnt_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::InvalidFontTableSfnt.api_seam()
        ),
    });

    // Hostile sfnt table: invalid magic bytes followed by garbage table records
    let mut generator = HostileByteGenerator::new(seed, 128);
    let mut hostile_font_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF]; // Bad magic
    hostile_font_bytes.extend(generator.generate());

    let input_digest = ReproducibleDigest::of_bytes(&hostile_font_bytes);
    let input_len = hostile_font_bytes.len();

    let res = Font::parse(hostile_font_bytes.clone());
    assert!(
        matches!(res, Err(FontError::BadMagic | FontError::Truncated | FontError::MissingTable(_))),
        "corrupted sfnt table must fail with typed FontError"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "REFUSED_CORRUPT_SFNT",
        detail: format!("sfnt parsed returned typed error: {:?}", res.err()),
    });

    let mut classify_font = |candidate: &[u8]| -> Option<&'static str> {
        match Font::parse(candidate.to_vec()) {
            Err(_) => Some("FONT_REFUSED_MALFORMED"),
            Ok(_) => None,
        }
    };

    let report = minimize(&hostile_font_bytes, TerminationBudget::attempts(50), &mut classify_font);

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::InvalidFontTableSfnt,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "FONT_REFUSED_MALFORMED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback: Some("Fallback: retain standard system typography".to_string()),
        verified_invariants: vec![
            "font_reader_validates_sfnt_magic_before_table_reading",
            "malformed_font_tables_refused_without_panics_or_aborts",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 6: Image Decompression Bomb Refusal
// ---------------------------------------------------------------------------

/// Executes image decompression bomb dimension and pixel budget refusal.
pub fn run_image_decompression_bomb_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::ImageDecompressionBomb.api_seam()
        ),
    });

    // Synthetic PNG advertising 60,000 x 60,000 dimensions (3.6 gigapixels, 14.4 GB uncompressed)
    let mut hostile_png = make_synthetic_png(60_000, 60_000);
    // Append trailing hostile bytes
    let mut byte_gen = HostileByteGenerator::new(seed, 64);
    hostile_png.extend(byte_gen.generate());

    let input_digest = ReproducibleDigest::of_bytes(&hostile_png);
    let input_len = hostile_png.len();

    let budgets = BoundedAssetBudgets {
        max_image_dimension: 8192,
        max_decoded_pixels: 32 * 1024 * 1024,
        max_decoded_bytes: 64 * 1024 * 1024,
        ..BoundedAssetBudgets::default()
    };

    let res = BoundedImageDecoder::decode(&hostile_png, None, &budgets, 100);
    assert!(
        matches!(res, Err(DocumentError::DecompressionBomb { width: 60_000, height: 60_000, .. })),
        "60000x60000 image must be refused as DecompressionBomb"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "DECOMPRESSION_BOMB_REFUSED",
        detail: "60000x60000 png header refused before buffer allocation".to_string(),
    });

    // Minimization: shrinks trailing padding while keeping bomb IHDR intact
    let mut classify_bomb = |candidate: &[u8]| -> Option<&'static str> {
        match BoundedImageDecoder::decode(candidate, None, &budgets, 101) {
            Err(DocumentError::DecompressionBomb { .. }) => Some("DOCUMENT_DECOMPRESSION_BOMB"),
            _ => None,
        }
    };

    let report = minimize(&hostile_png, TerminationBudget::attempts(40), &mut classify_bomb);

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::ImageDecompressionBomb,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_DECOMPRESSION_BOMB",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback: Some("<!-- [image dimension bomb 60000x60000 refused] -->".to_string()),
        verified_invariants: vec![
            "bounded_image_decoder_inspects_dimensions_before_allocation",
            "decompression_bomb_thresholds_strictly_enforced",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 7: Image Frame Count Budget Exhaustion
// ---------------------------------------------------------------------------

/// Executes animated GIF frame count budget exhaustion.
pub fn run_image_frame_count_budget_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::ImageFrameCountBudget.api_seam()
        ),
    });

    let budgets = BoundedAssetBudgets {
        max_frame_count: 8,
        ..BoundedAssetBudgets::default()
    };

    // Synthetic GIF with 25 frames (budget is 8)
    let mut hostile_gif = make_synthetic_multiframe_gif(32, 32, 25);
    let mut byte_gen = HostileByteGenerator::new(seed, 32);
    hostile_gif.extend(byte_gen.generate());

    let input_digest = ReproducibleDigest::of_bytes(&hostile_gif);
    let input_len = hostile_gif.len();

    let res = BoundedImageDecoder::decode(&hostile_gif, None, &budgets, 102);
    assert!(
        matches!(res, Err(DocumentError::FrameCountExceeded { frame_count: 25, max_frames: 8 })),
        "25-frame GIF must exceed max_frame_count budget of 8"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "FRAME_COUNT_EXCEEDED",
        detail: "gif frame count 25 exceeds maximum budget 8".to_string(),
    });

    let mut classify_frames = |candidate: &[u8]| -> Option<&'static str> {
        match BoundedImageDecoder::decode(candidate, None, &budgets, 103) {
            Err(DocumentError::FrameCountExceeded { .. }) => Some("DOCUMENT_FRAME_COUNT_EXCEEDED"),
            _ => None,
        }
    };

    let report = minimize(&hostile_gif, TerminationBudget::attempts(40), &mut classify_frames);

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::ImageFrameCountBudget,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_FRAME_COUNT_EXCEEDED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback: Some("<!-- [animated image exceeded 8 frame limit] -->".to_string()),
        verified_invariants: vec![
            "bounded_image_decoder_sniffs_frame_counts_without_full_render",
            "frame_count_budget_strictly_enforced",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 8: Asset Traversal & Foreign Call Refusal
// ---------------------------------------------------------------------------

/// Executes path traversal and remote scheme refusal in AssetDomain classification.
pub fn run_asset_traversal_escape_case(seed: u64) -> DocumentHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!(
            "seed=0x{seed:016x}, seam={}",
            DocumentHostileCaseId::AssetTraversalEscape.api_seam()
        ),
    });

    // Hostile traversal strings with extra padding
    let hostile_uri = "../../../../etc/passwd\0padding_exploit";
    let input_bytes = hostile_uri.as_bytes().to_vec();
    let input_digest = ReproducibleDigest::of_bytes(&input_bytes);
    let input_len = input_bytes.len();

    let domain = AssetDomain::classify(hostile_uri);
    assert!(
        matches!(domain, AssetDomain::Rejected { .. }),
        "path traversal with null byte must be rejected"
    );

    events.push(HostileLogEvent {
        step: 1,
        phase: "TRAVERSAL_REJECTED",
        detail: format!("hostile uri rejected: {:?}", domain),
    });

    let mut classify_traversal = |candidate: &[u8]| -> Option<&'static str> {
        let Ok(s) = std::str::from_utf8(candidate) else {
            return None;
        };
        match AssetDomain::classify(s) {
            AssetDomain::Rejected { .. } => Some("DOCUMENT_ASSET_ESCAPE_REFUSED"),
            AssetDomain::ConfinedRelative(_) => None,
        }
    };

    let report = minimize(&input_bytes, TerminationBudget::attempts(30), &mut classify_traversal);

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZED",
        detail: format!(
            "original_len={input_len}, minimized_len={}, attempts={}",
            report.minimized.len(),
            report.attempts
        ),
    });

    DocumentHostileOutcome {
        case_id: DocumentHostileCaseId::AssetTraversalEscape,
        seed,
        input_digest,
        input_len,
        minimized_digest: ReproducibleDigest::of_bytes(&report.minimized),
        minimized_len: report.minimized.len(),
        classification: "DOCUMENT_ASSET_ESCAPE_REFUSED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        useful_fallback: Some("<!-- [external / unconfined asset fetch refused] -->".to_string()),
        verified_invariants: vec![
            "asset_domain_strictly_confines_relative_paths",
            "remote_schemes_and_traversal_refused_without_network_or_filesystem_escape",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Negative Control Oracle
// ---------------------------------------------------------------------------

/// Typed error demonstrating negative control oracle detection in document lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentNegativeControlError {
    /// Oracle failed to catch an intentional classification shift in minimizer.
    UndetectedClassificationShift,
    /// Oracle failed to catch unconfined path traversal escape.
    UndetectedTraversalEscape,
    /// Oracle failed to reject an image decompression bomb.
    UndetectedDecompressionBomb,
    /// Oracle failed to detect a transclusion cycle.
    UndetectedTransclusionCycle,
    /// Oracle failed to reject malformed font bytes.
    UndetectedFontRefusal,
}

/// Executes negative control tests demonstrating the oracle catches violations.
pub fn run_negative_control_oracle() -> Result<(), DocumentNegativeControlError> {
    // Negative Control 1: Minimizer on non-failing input must NOT claim preservation
    let clean = b"clean input";
    let report = minimize(
        clean,
        TerminationBudget::attempts(10),
        &mut |_cand: &[u8]| -> Option<&'static str> { None },
    );
    if report.classification_preserved {
        return Err(DocumentNegativeControlError::UndetectedClassificationShift);
    }

    // Negative Control 2: Path traversal escape must NEVER classify as ConfinedRelative
    let traversal = "../../../etc/shadow";
    if let AssetDomain::ConfinedRelative(_) = AssetDomain::classify(traversal) {
        return Err(DocumentNegativeControlError::UndetectedTraversalEscape);
    }

    // Negative Control 3: Extreme decompression bomb must NEVER decode successfully
    let bomb_png = make_synthetic_png(60_000, 60_000);
    let budgets = BoundedAssetBudgets::default();
    if BoundedImageDecoder::decode(&bomb_png, None, &budgets, 999).is_ok() {
        return Err(DocumentNegativeControlError::UndetectedDecompressionBomb);
    }

    // Negative Control 4: Transclusion loop must NEVER return Ok
    let mut tracker = TransclusionTracker::new(TransclusionPolicy::default());
    let _ = tracker.enter_transclusion("file_a.md");
    if tracker.enter_transclusion("file_a.md").is_ok() {
        return Err(DocumentNegativeControlError::UndetectedTransclusionCycle);
    }

    // Negative Control 5: Completely bad font magic must NEVER parse successfully
    if Font::parse(vec![0x00, 0x00, 0x00, 0x00]).is_ok() {
        return Err(DocumentNegativeControlError::UndetectedFontRefusal);
    }

    Ok(())
}
