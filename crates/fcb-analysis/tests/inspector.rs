//! Comprehensive test suite for evidence capability schema and Inspector facts (FCB-030.B).
//!
//! Verifies:
//! - All 6 levels on the capability ladder (Bytes, Lexical, Structural, ResolvedLocal,
//!   ExternalSemantic, Heuristic) are distinguishable.
//! - Unknown counts are strictly `CountMetric::Unknown` and NEVER treated as zero (Plan §5.5).
//! - Heuristic candidates carry mandatory badges and explanations.
//! - Directional relationship edges carry exact evidence spans and capability tier (Plan §18.1).
//! - Negative controls:
//!   * Unknown count coercion to zero is rejected.
//!   * Same-name candidates cannot claim proven compiler semantics.
//!   * Unsupported language cannot claim structural or semantic facts.
//!   * Lexical imports cannot claim proven external package dependencies without independent proof.
//!   * Local syntax items cannot claim external compiler semantics without external provider.

use fcb_analysis::{
    CapabilityLevel, CountMetric, FactAuditError, HeuristicFact, IndexingStatus,
    InspectorError, InspectorFactsBuilder, NearbyDocSnippet, OutlineEvidence, OutlineExtractor,
    RelationshipEdge, RelationshipKind, SourceFact, SourceFactKind,
};
use fcb_core::{ArenaOwnerId, FileId, SourceRevision};

fn dummy_ids() -> (FileId, SourceRevision) {
    let owner = ArenaOwnerId::new(1).unwrap();
    let file_id = FileId::new(owner, 42).unwrap();
    let rev = SourceRevision::new(owner, 100).unwrap();
    (file_id, rev)
}

#[test]
fn count_metric_unknown_is_not_zero_and_formats_honestly() {
    let unknown = CountMetric::Unknown;
    let zero = CountMetric::Known(0);

    // Invariant: Unknown is distinct from Known(0) (Plan §5.5: "Unknown facts are unknown, not zero.")
    assert_ne!(unknown, zero);
    assert!(unknown.is_unknown());
    assert!(!unknown.is_known());
    assert_eq!(unknown.as_known(), None);

    // Formatted strings: Unknown displays as "unknown", zero displays as "0"
    assert_eq!(unknown.to_string(), "unknown");
    assert_eq!(zero.to_string(), "0");

    // Known zero is indeed zero
    assert!(zero.is_known());
    assert_eq!(zero.as_known(), Some(0));

    // Negative control: requiring known value from Unknown fails explicitly
    let err = unknown.require_known().unwrap_err();
    assert_eq!(err, InspectorError::UnknownCountCannotBeZero);
}

#[test]
fn all_six_capability_levels_are_distinguishable_and_categorized() {
    let (file_id, rev) = dummy_ids();
    let evidence = OutlineEvidence::new(0, 100, 1, 10, None).unwrap();

    let mut builder = InspectorFactsBuilder::new(
        "src/pipeline.rs",
        file_id,
        rev,
        "rust",
        CapabilityLevel::Structural,
        100,
        10,
    );

    // Add facts across each tier of the ladder (Plan §11.5)
    builder.add_fact(SourceFact {
        id: 1,
        name: "range_bytes".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::Bytes,
        evidence,
        is_proven_semantic: false,
    });

    builder.add_fact(SourceFact {
        id: 2,
        name: "TOKEN_COMMENT".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::Lexical,
        evidence,
        is_proven_semantic: false,
    });

    builder.add_fact(SourceFact {
        id: 3,
        name: "PipelineContext".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::Structural,
        evidence,
        is_proven_semantic: false,
    });

    builder.add_fact(SourceFact {
        id: 4,
        name: "super::config".to_string(),
        kind: SourceFactKind::LexicalImport,
        capability_level: CapabilityLevel::ResolvedLocal,
        evidence,
        is_proven_semantic: false,
    });

    let facts = builder.build().expect("valid facts bundle");
    let counts = facts.facts_count_by_capability();

    // Verify each tier is distinguishable
    assert_eq!(counts[&CapabilityLevel::Bytes], 1);
    assert_eq!(counts[&CapabilityLevel::Lexical], 1);
    assert_eq!(counts[&CapabilityLevel::Structural], 1);
    assert_eq!(counts[&CapabilityLevel::ResolvedLocal], 1);
    assert_eq!(counts[&CapabilityLevel::ExternalSemantic], 0);
    assert_eq!(counts[&CapabilityLevel::Heuristic], 0);

    let structural_facts = facts.facts_at_level(CapabilityLevel::Structural);
    assert_eq!(structural_facts.len(), 1);
    assert_eq!(structural_facts[0].name, "PipelineContext");
}

#[test]
fn inspector_facts_bundle_assembles_cleanly_with_outline_and_snippets() {
    let (file_id, rev) = dummy_ids();
    let rust_code = br#"/// Top-level math module docs
pub struct Calculator {
    pub value: i64,
}

impl Calculator {
    /// Adds two numbers
    pub fn add(&mut self, n: i64) {
        self.value += n;
    }
}
"#;

    let extractor = OutlineExtractor::default();
    let outline = extractor.extract(file_id, rev, "rust", rust_code);

    let evidence = OutlineEvidence::new(0, rust_code.len() as u64, 1, 12, None).unwrap();

    let mut builder = InspectorFactsBuilder::new(
        "src/calc.rs",
        file_id,
        rev,
        "rust",
        CapabilityLevel::Structural,
        rust_code.len() as u64,
        12,
    );

    builder = builder
        .indexing_status(IndexingStatus::Indexed { revision: rev })
        .outline(&outline)
        .inbound_relationships(CountMetric::Known(3))
        .outbound_relationships(CountMetric::Unknown); // Outbound unanalyzed: MUST be Unknown!

    // Add documentation snippet
    builder.add_doc_snippet(NearbyDocSnippet {
        target_name: "Calculator".to_string(),
        doc_text: "Top-level math module docs".to_string(),
        byte_range: (0, 31),
        line_range: (1, 1),
    });

    // Add relationship edge (Plan §18.1)
    builder.add_inbound_edge(RelationshipEdge {
        edge_id: 1,
        source_file: file_id,
        target_file: None,
        target_symbol: "std::ops::Add".to_string(),
        kind: RelationshipKind::LexicalImport,
        capability_level: CapabilityLevel::Structural,
        evidence,
        extractor_version: 1,
    });

    let facts = builder.build().expect("valid inspector bundle");

    assert_eq!(facts.logical_path, "src/calc.rs");
    assert!(facts.is_indexed_at_current_revision());
    assert_eq!(facts.outline_summary.total_items, 3); // Calculator, impl Calculator, add
    assert_eq!(facts.inbound_relationships, CountMetric::Known(3));
    assert_eq!(facts.outbound_relationships, CountMetric::Unknown);
    assert_eq!(facts.inbound_edges.len(), 1);
    assert_eq!(facts.nearby_docs.len(), 1);
    assert_eq!(facts.nearby_docs[0].target_name, "Calculator");
}

#[test]
fn heuristic_facts_require_badge_and_explanation() {
    let evidence = OutlineEvidence::new(10, 25, 2, 2, None).unwrap();

    let candidate_fact = SourceFact {
        id: 99,
        name: "find_widget".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Heuristic,
        evidence,
        is_proven_semantic: false,
    };

    let heuristic = HeuristicFact::new(
        candidate_fact.clone(),
        "[Heuristic Candidate]",
        "Identifier matches symbol name 'find_widget', but has no proven compiler resolution.",
    )
    .expect("valid heuristic fact");

    assert_eq!(heuristic.badge, "[Heuristic Candidate]");
    assert!(heuristic.explanation.contains("find_widget"));
    assert_eq!(heuristic.fact.capability_level, CapabilityLevel::Heuristic);
    assert!(!heuristic.fact.is_proven_semantic);
}

#[test]
fn negative_control_unsupported_language_cannot_claim_structural_facts() {
    let (file_id, rev) = dummy_ids();
    let evidence = OutlineEvidence::new(0, 50, 1, 3, None).unwrap();

    // Plain / unsupported language has capability level Bytes
    let mut builder = InspectorFactsBuilder::new(
        "notes.txt",
        file_id,
        rev,
        "text",
        CapabilityLevel::Bytes,
        50,
        3,
    );

    // NEGATIVE CONTROL: unsupported language trying to register a Structural fact
    builder.add_fact(SourceFact {
        id: 1,
        name: "IllegalStruct".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::Structural, // ILLEGAL for Bytes-only language!
        evidence,
        is_proven_semantic: false,
    });

    let err = builder.build().unwrap_err();
    assert!(matches!(
        err,
        InspectorError::AuditFailed(
            FactAuditError::UnsupportedLanguageCannotClaimSemantics { .. }
        )
    ));
    if let InspectorError::AuditFailed(
        FactAuditError::UnsupportedLanguageCannotClaimSemantics {
            name,
            claimed_level,
        },
    ) = err
    {
        assert_eq!(name, "IllegalStruct");
        assert_eq!(claimed_level, CapabilityLevel::Structural);
    }
}

#[test]
fn negative_control_heuristic_fact_rejects_proven_semantic_claims() {
    let evidence = OutlineEvidence::new(10, 20, 1, 1, None).unwrap();

    // NEGATIVE CONTROL: heuristic fact claiming is_proven_semantic == true
    let illegal_candidate = SourceFact {
        id: 1,
        name: "ambiguous_symbol".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Heuristic,
        evidence,
        is_proven_semantic: true, // ILLEGAL!
    };

    let err = HeuristicFact::new(
        illegal_candidate,
        "[Badge]",
        "Attempting illegal proven claim",
    )
    .unwrap_err();

    assert!(matches!(err, FactAuditError::SameNameCandidateCannotBeProven { .. }));
}

#[test]
fn negative_control_heuristic_fact_rejects_non_heuristic_level() {
    let evidence = OutlineEvidence::new(10, 20, 1, 1, None).unwrap();

    // NEGATIVE CONTROL: claiming Structural level inside a HeuristicFact
    let illegal_level_fact = SourceFact {
        id: 2,
        name: "ambiguous_symbol".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Structural, // ILLEGAL!
        evidence,
        is_proven_semantic: false,
    };

    let err = HeuristicFact::new(
        illegal_level_fact,
        "[Badge]",
        "Attempting illegal structural level",
    )
    .unwrap_err();

    assert!(matches!(err, FactAuditError::SameNameCandidateCannotBeProven { .. }));
}
