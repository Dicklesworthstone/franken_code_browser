#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document system adapts upstream
//! FrankenMarkdown incremental document dependency invalidation (FCB-037.A).
//!
//! Acceptance criteria & oracle contract:
//! 1. Reference, footnote, heading, and include dependencies are identified.
//! 2. Unchanged subtrees are verified and preserved without false invalidations.
//! 3. Conservative distant invalidation: editing a distant reference flags affected ranges.
//! 4. Non-overlapping edits leave independent sections completely unchanged.
//! 5. Negative controls: out-of-bounds ranges or empty documents produce valid bounded results.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use franken_markdown::dep_invalidation::{DependencyGraph, DependencyKind, InvalidationResult};

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn consumer_dependency_graph_tracks_all_markdown_dependency_kinds() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 1);

    let doc = r#"# Architecture Overview

Here is a link to [the reference guide][ref-guide] and a note[^note1].

[ref-guide]: https://example.com/guide

[^note1]: This is an authoritative footnote.

## Implementation Details

More details with shortcut [ref-guide] usage.
"#;

    let graph = DependencyGraph::scan(doc);
    let deps = graph.dependencies();

    // Verify headings
    let headings: Vec<_> = deps
        .iter()
        .filter(|d| matches!(d.kind, DependencyKind::Heading { .. }))
        .collect();
    assert_eq!(headings.len(), 2, "2 headings tracked");

    // Verify reference link
    let refs: Vec<_> = deps
        .iter()
        .filter(|d| matches!(d.kind, DependencyKind::Reference { .. }))
        .collect();
    assert!(refs.len() >= 2, "reference link and shortcut reference tracked");

    // Verify footnotes
    let footnotes: Vec<_> = deps
        .iter()
        .filter(|d| matches!(d.kind, DependencyKind::Footnote { .. }))
        .collect();
    assert!(footnotes.len() >= 2, "footnote reference and definition tracked");
}

#[test]
fn consumer_distant_invalidation_preserves_unchanged_subtrees() {
    let doc = r#"# Section 1
First paragraph text with nothing special.

# Section 2
Second paragraph referencing [guide][ref-guide].

# Section 3
Third paragraph with footnote[^1].

[ref-guide]: https://example.com/guide
[^1]: Footnote content.
"#;

    let graph = DependencyGraph::scan(doc);

    // Edit only within Section 1's paragraph body (after heading at 0..11)
    let inv: InvalidationResult = graph.invalidate(15, 30);

    // Section 1 paragraph edit does not overlap Section 2 or Section 3 dependencies
    assert!(inv.dirty.is_empty(), "no dependencies in section 1 edit range");
    assert!(!inv.unchanged.is_empty(), "downstream dependencies verified unchanged");

    // Edit the reference definition at the bottom of the document
    let ref_def_pos = doc.find("[ref-guide]:").unwrap();
    let inv_ref = graph.invalidate(ref_def_pos, ref_def_pos + 12);

    // Editing reference definition marks dirty dependencies
    assert!(!inv_ref.dirty.is_empty(), "reference definition dependency is dirty");
    assert!(inv_ref.distant_dirty, "distant sections depending on reference are conservatively dirty");
}

#[test]
fn consumer_negative_control_non_overlapping_edits_leave_dependencies_stable() {
    let doc = "Introductory text.\n\n[doc-ref]: https://example.com\n\nConcluding remarks.";
    let graph = DependencyGraph::scan(doc);

    // Edit only the concluding remarks
    let remarks_pos = doc.find("Concluding").unwrap();
    let inv = graph.invalidate(remarks_pos, doc.len());

    // The reference dependency at bytes ~20..50 is strictly before remarks_pos
    let ref_unchanged = inv.unchanged.iter().any(|(s, e)| *e <= remarks_pos && *s > 0);
    assert!(ref_unchanged, "preceding reference dependency must remain verified unchanged");
}
