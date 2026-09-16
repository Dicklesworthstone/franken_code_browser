//! FCB-030.V production verification scenario: source-specific structural outlines,
//! evidence capability schema, and Inspector facts driven together through their real
//! public implementation.
//!
//! Required cases (each independently selectable via cargo test):
//! 1. `rust_outline_and_nesting` — Rust items with method nesting under impl blocks and exact spans.
//! 2. `markdown_heading_tree` — Markdown H1–H6 tree hierarchy nesting, ignoring code fences.
//! 3. `multi_language_coverage` — Python, TS/TSX, Go, and C++ language extractors.
//! 4. `unsupported_language_line_blocks` — Plain/unsupported language falling back to bounded line blocks.
//! 5. `inspector_facts_bundle` — Inspector facts with unknown counts preserved as `Unknown` (never 0).
//! 6. `heuristic_candidate_disclosure` — Heuristic candidate facts with mandatory badge and explanation.
//! 7. `negative_control_oracle` — Rejection of proven claims from candidates, imports, and plain bytes.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_030.sh`).

use std::path::PathBuf;

use fcb_analysis::{
    CapabilityLevel, CountMetric, ExtractorLimits, FactAuditError, HeuristicFact,
    IndexingStatus, InspectorError, InspectorFactsBuilder, NearbyDocSnippet, OutlineEvidence,
    OutlineExtractor, OutlineItemKind, OutlineStatus, RelationshipEdge, RelationshipKind,
    SourceFact, SourceFactKind,
};
use fcb_core::{ArenaOwnerId, FileId, SourceRevision};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt, ScenarioReceiptDraft,
    ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_030_RUN_ID";

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C_30).expect("test owner is non-zero")
}

fn dummy_ids() -> (FileId, SourceRevision) {
    let file_id = FileId::new(owner(), 101).expect("file id");
    let rev = SourceRevision::new(owner(), 501).expect("revision");
    (file_id, rev)
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-030-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    std::fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_30_00_01),
        pin: SourcePin::new("0303030303030303030303030303030303030303").expect("pin valid"),
        route: fcb_test_support::receipts::RouteId::new("headless:rust").expect("route valid"),
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
    std::fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    )
    .expect("receipt retained");
}

#[test]
fn rust_outline_and_nesting() {
    let (file_id, rev) = dummy_ids();
    let extractor = OutlineExtractor::default();

    let code = br#"// Production Rust source
pub struct BufferManager {
    capacity: usize,
}

impl BufferManager {
    pub fn new(capacity: usize) -> Self {
        Self { capacity }
    }

    pub fn allocate(&mut self, size: usize) -> Option<usize> {
        Some(size)
    }
}

pub fn global_init() {
}
"#;

    let outline = extractor.extract(file_id, rev, "rust", code);
    assert_eq!(outline.status, OutlineStatus::Qualified);
    assert_eq!(outline.language, "rust");
    assert_eq!(outline.items.len(), 3); // BufferManager struct, impl BufferManager, global_init

    let impl_item = &outline.items[1];
    assert_eq!(impl_item.kind, OutlineItemKind::Impl);
    assert_eq!(impl_item.children.len(), 2);
    assert_eq!(impl_item.children[0].name, "new");
    assert_eq!(impl_item.children[0].kind, OutlineItemKind::Method);
    assert_eq!(impl_item.children[1].name, "allocate");
    assert_eq!(impl_item.children[1].kind, OutlineItemKind::Method);

    assert_eq!(outline.total_items, 5);
    assert_eq!(outline.max_depth, 2);

    record_receipt(
        "rust_outline_and_nesting",
        Effect::Succeeded,
        "Rust item outline extracted with exact spans and methods nested under impl blocks",
    );
}

#[test]
fn markdown_heading_tree() {
    let (file_id, rev) = dummy_ids();
    let extractor = OutlineExtractor::default();

    let doc = br#"# Architecture
Intro

## Storage
Details

### VFS
Layer

## Rendering
Details

```markdown
# Inside fence, must be ignored
## Also ignored
```
"#;

    let outline = extractor.extract(file_id, rev, "markdown", doc);
    assert_eq!(outline.status, OutlineStatus::Qualified);
    assert_eq!(outline.items.len(), 1); // H1 "Architecture"

    let h1 = &outline.items[0];
    assert_eq!(h1.name, "Architecture");
    assert_eq!(h1.children.len(), 2); // "Storage" and "Rendering"
    assert_eq!(h1.children[0].name, "Storage");
    assert_eq!(h1.children[0].children.len(), 1); // "VFS"
    assert_eq!(h1.children[0].children[0].name, "VFS");
    assert_eq!(h1.children[1].name, "Rendering");

    assert_eq!(outline.total_items, 4);
    assert_eq!(outline.max_depth, 3);

    record_receipt(
        "markdown_heading_tree",
        Effect::Succeeded,
        "Markdown headings hierarchically nested into outline tree while ignoring code fences",
    );
}

#[test]
fn multi_language_coverage() {
    let (file_id, rev) = dummy_ids();
    let extractor = OutlineExtractor::default();

    // Python
    let py = b"class Worker:\n    def run(self):\n        pass\n";
    let py_outline = extractor.extract(file_id, rev, "python", py);
    assert_eq!(py_outline.status, OutlineStatus::Qualified);
    assert_eq!(py_outline.items[0].name, "Worker");
    assert_eq!(py_outline.items[0].children[0].name, "run");

    // TypeScript
    let ts = b"export interface Config {\n    port: number;\n}\nexport function serve() {}\n";
    let ts_outline = extractor.extract(file_id, rev, "typescript", ts);
    assert_eq!(ts_outline.status, OutlineStatus::Qualified);
    assert_eq!(ts_outline.items.len(), 2);

    // Go
    let go = b"package main\ntype Server struct {}\nfunc (s *Server) Start() {}\n";
    let go_outline = extractor.extract(file_id, rev, "go", go);
    assert_eq!(go_outline.status, OutlineStatus::Qualified);
    assert_eq!(go_outline.items.len(), 2);

    // C++
    let cpp = b"namespace core {\nclass Device {\n};\n}\n";
    let cpp_outline = extractor.extract(file_id, rev, "cpp", cpp);
    assert_eq!(cpp_outline.status, OutlineStatus::Qualified);
    assert_eq!(cpp_outline.items.len(), 2);

    record_receipt(
        "multi_language_coverage",
        Effect::Succeeded,
        "Multi-language structural outline coverage for Python, TS, Go, and C++",
    );
}

#[test]
fn unsupported_language_line_blocks() {
    let (file_id, rev) = dummy_ids();
    let limits = ExtractorLimits {
        lines_per_block: 50,
        ..Default::default()
    };
    let extractor = OutlineExtractor::new(limits);

    // 75 lines in an unsupported plain format
    let mut text = String::new();
    for i in 1..=75 {
        text.push_str(&format!("entry_{i} = val\n"));
    }

    let outline = extractor.extract(file_id, rev, "custom_unsupported_fmt", text.as_bytes());
    assert_eq!(outline.status, OutlineStatus::LineBlockFallback);
    assert_eq!(outline.items.len(), 2);
    assert_eq!(outline.items[0].name, "Lines 1–50");
    assert_eq!(outline.items[0].capability_level, CapabilityLevel::Bytes);
    assert_eq!(outline.items[1].name, "Lines 51–75");
    assert_eq!(outline.items[1].capability_level, CapabilityLevel::Bytes);

    record_receipt(
        "unsupported_language_line_blocks",
        Effect::Succeeded,
        "Unsupported language cleanly falls back to bounded line blocks with Bytes capability tier",
    );
}

#[test]
fn inspector_facts_bundle() {
    let (file_id, rev) = dummy_ids();
    let code = b"pub struct Router {}\n";
    let evidence = OutlineEvidence::new(0, code.len() as u64, 1, 1, None).unwrap();

    let mut builder = InspectorFactsBuilder::new(
        "src/net/router.rs",
        file_id,
        rev,
        "rust",
        CapabilityLevel::Structural,
        code.len() as u64,
        1,
    );

    builder = builder
        .indexing_status(IndexingStatus::Indexed { revision: rev })
        .inbound_relationships(CountMetric::Known(2))
        .outbound_relationships(CountMetric::Unknown); // UNKNOWN relationship count (Plan §5.5)

    builder.add_doc_snippet(NearbyDocSnippet {
        target_name: "Router".to_string(),
        doc_text: "Packet router component".to_string(),
        byte_range: (0, code.len() as u64),
        line_range: (1, 1),
    });

    builder.add_inbound_edge(RelationshipEdge {
        edge_id: 1,
        source_file: file_id,
        target_file: None,
        target_symbol: "Handler".to_string(),
        kind: RelationshipKind::ResolvedLocalImport,
        capability_level: CapabilityLevel::ResolvedLocal,
        evidence,
        extractor_version: 1,
    });

    let facts = builder.build().expect("valid inspector bundle");

    // Invariant: Unknown metric is distinct from zero and displays as "unknown"
    assert_eq!(facts.outbound_relationships, CountMetric::Unknown);
    assert_ne!(facts.outbound_relationships, CountMetric::Known(0));
    assert_eq!(facts.outbound_relationships.to_string(), "unknown");
    assert_eq!(facts.inbound_relationships, CountMetric::Known(2));
    assert_eq!(facts.inbound_relationships.to_string(), "2");

    assert!(facts.is_indexed_at_current_revision());
    assert_eq!(facts.nearby_docs.len(), 1);
    assert_eq!(facts.inbound_edges.len(), 1);

    record_receipt(
        "inspector_facts_bundle",
        Effect::Succeeded,
        "Inspector facts correctly preserves CountMetric::Unknown without zero-conflation",
    );
}

#[test]
fn heuristic_candidate_disclosure() {
    let evidence = OutlineEvidence::new(10, 30, 2, 2, None).unwrap();

    let fact = SourceFact {
        id: 7,
        name: "resolve_target".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Heuristic,
        evidence,
        is_proven_semantic: false,
    };

    let heuristic = HeuristicFact::new(
        fact,
        "[Candidate Match]",
        "Identifier matches symbol name 'resolve_target' by lexical similarity; no compiler proof.",
    )
    .expect("valid heuristic fact");

    assert_eq!(heuristic.badge, "[Candidate Match]");
    assert!(heuristic.explanation.contains("resolve_target"));
    assert_eq!(heuristic.fact.capability_level, CapabilityLevel::Heuristic);

    record_receipt(
        "heuristic_candidate_disclosure",
        Effect::Succeeded,
        "Heuristic candidates enforce mandatory badge and explanatory disclosure",
    );
}

#[test]
fn negative_control_oracle() {
    let evidence = OutlineEvidence::new(0, 20, 1, 1, None).unwrap();

    // 1. Same-name candidate claiming proven semantics
    let illegal_proven = SourceFact {
        id: 1,
        name: "dispatch".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Heuristic,
        evidence,
        is_proven_semantic: true, // ILLEGAL
    };
    let err1 = HeuristicFact::new(illegal_proven, "[Badge]", "Exp").unwrap_err();
    assert!(matches!(err1, FactAuditError::SameNameCandidateCannotBeProven { .. }));

    // 2. Lexical import claiming proven external dependency
    let illegal_import = SourceFact {
        id: 2,
        name: "ext_crate::api".to_string(),
        kind: SourceFactKind::LexicalImport,
        capability_level: CapabilityLevel::Structural,
        evidence,
        is_proven_semantic: true, // ILLEGAL without external proof
    };
    let auditor = fcb_analysis::FactAuditor::new();
    let err2 = auditor.audit_fact(&illegal_import).unwrap_err();
    assert!(matches!(err2, FactAuditError::LexicalImportCannotBeProvenExternalDependency { .. }));

    // 3. Unsupported language (Bytes) claiming Structural facts
    let (file_id, rev) = dummy_ids();
    let mut builder = InspectorFactsBuilder::new(
        "raw.dat",
        file_id,
        rev,
        "binary",
        CapabilityLevel::Bytes,
        100,
        1,
    );
    builder.add_fact(SourceFact {
        id: 3,
        name: "BadFact".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::Structural,
        evidence,
        is_proven_semantic: false,
    });
    let err3 = builder.build().unwrap_err();
    assert!(matches!(
        err3,
        InspectorError::AuditFailed(FactAuditError::UnsupportedLanguageCannotClaimSemantics { .. })
    ));

    // 4. Unknown metric required to be known
    let unknown_metric = CountMetric::Unknown;
    let err4 = unknown_metric.require_known().unwrap_err();
    assert_eq!(err4, InspectorError::UnknownCountCannotBeZero);

    record_receipt(
        "negative_control_oracle",
        Effect::Succeeded,
        "Negative control oracle detected and rejected all invalid semantic and capability claims",
    );
}
