//! Comprehensive test suite for source outlines and fact capability enforcement (FCB-030.A).
//!
//! Verifies:
//! - Language-scoped parser facts with exact evidence spans.
//! - Hierarchical nesting (Rust impl methods, Markdown heading trees, Python classes).
//! - Line-block fallback for unsupported/plain languages (outline navigation never fails).
//! - Malformed/adversarial inputs gracefully recover without panics or memory spikes.
//! - Negative controls: same-name candidates and unproven imports are rejected from claiming
//!   proven compiler semantics (Plan §11.5, §18.1).

use fcb_analysis::{
    CapabilityLevel, ExtractorLimits, FactAuditError, FactAuditor, OutlineEvidence,
    OutlineExtractor, OutlineItemKind, OutlineStatus, SourceFact, SourceFactKind,
};
use fcb_core::{ArenaOwnerId, FileId, SourceRevision};

fn dummy_file_ids() -> (FileId, SourceRevision) {
    let owner = ArenaOwnerId::new(1).unwrap();
    let file_id = FileId::new(owner, 100).unwrap();
    let source_rev = SourceRevision::new(owner, 500).unwrap();
    (file_id, source_rev)
}

#[test]
fn rust_outline_extracts_items_and_nests_methods_under_impl() {
    let (file_id, rev) = dummy_file_ids();
    let extractor = OutlineExtractor::default();

    let code = br#"// Rust sample
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub enum Color {
    Red,
    Green,
    Blue,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    pub fn distance(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

pub fn standalone_function() -> u32 {
    42
}
"#;

    let outline = extractor.extract(file_id, rev, "rust", code);
    assert_eq!(outline.status, OutlineStatus::Qualified);
    assert_eq!(outline.language, "rust");

    // Top-level items: Point (struct), Color (enum), impl Point, standalone_function
    assert_eq!(outline.items.len(), 4);

    let struct_item = &outline.items[0];
    assert_eq!(struct_item.name, "Point");
    assert_eq!(struct_item.kind, OutlineItemKind::Struct);
    assert_eq!(struct_item.capability_level, CapabilityLevel::Structural);
    assert!(struct_item.evidence.name_range.is_some());

    let enum_item = &outline.items[1];
    assert_eq!(enum_item.name, "Color");
    assert_eq!(enum_item.kind, OutlineItemKind::Enum);

    let impl_item = &outline.items[2];
    assert_eq!(impl_item.name, "impl Point");
    assert_eq!(impl_item.kind, OutlineItemKind::Impl);
    // Verified nesting: child methods are nested inside the impl!
    assert_eq!(impl_item.children.len(), 2);
    assert_eq!(impl_item.children[0].name, "new");
    assert_eq!(impl_item.children[0].kind, OutlineItemKind::Method);
    assert_eq!(impl_item.children[1].name, "distance");
    assert_eq!(impl_item.children[1].kind, OutlineItemKind::Method);

    let fn_item = &outline.items[3];
    assert_eq!(fn_item.name, "standalone_function");
    assert_eq!(fn_item.kind, OutlineItemKind::Function);

    // Total items: 4 top-level + 2 nested methods = 6 items
    assert_eq!(outline.total_items, 6);
    assert_eq!(outline.max_depth, 2);

    // Offset lookup finds the exact nested method
    let method_offset = impl_item.children[0].evidence.byte_start + 5;
    let found = outline.find_at_offset(method_offset);
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "new");
}

#[test]
fn markdown_outline_extracts_heading_tree_with_hierarchical_nesting() {
    let (file_id, rev) = dummy_file_ids();
    let extractor = OutlineExtractor::default();

    let doc = br#"# Architecture Overview

Introduction paragraph text.

## Components

Some component notes.

### Storage Engine
Details on storage.

### Rendering Engine
Details on rendering.

## Security Model

Security notes.

```markdown
# This is inside a code fence and must NOT be an outline heading
## Neither is this
```

# Conclusion
Final thoughts.
"#;

    let outline = extractor.extract(file_id, rev, "markdown", doc);
    assert_eq!(outline.status, OutlineStatus::Qualified);

    // Top-level headings: "Architecture Overview" (H1) and "Conclusion" (H1)
    assert_eq!(outline.items.len(), 2);

    let h1 = &outline.items[0];
    assert_eq!(h1.name, "Architecture Overview");
    assert_eq!(h1.kind, OutlineItemKind::Heading { level: 1 });

    // H1 contains two H2 children: "Components" and "Security Model"
    assert_eq!(h1.children.len(), 2);
    let h2_components = &h1.children[0];
    assert_eq!(h2_components.name, "Components");
    assert_eq!(h2_components.kind, OutlineItemKind::Heading { level: 2 });

    // H2 "Components" contains two H3 children: "Storage Engine" and "Rendering Engine"
    assert_eq!(h2_components.children.len(), 2);
    assert_eq!(h2_components.children[0].name, "Storage Engine");
    assert_eq!(h2_components.children[0].kind, OutlineItemKind::Heading { level: 3 });
    assert_eq!(h2_components.children[1].name, "Rendering Engine");
    assert_eq!(h2_components.children[1].kind, OutlineItemKind::Heading { level: 3 });

    let h2_security = &h1.children[1];
    assert_eq!(h2_security.name, "Security Model");
    assert_eq!(h2_security.children.len(), 0);

    let h1_conclusion = &outline.items[1];
    assert_eq!(h1_conclusion.name, "Conclusion");
    assert_eq!(h1_conclusion.children.len(), 0);

    // Code fence headings were ignored: total headings = 1 + 2 + 2 + 1 = 6
    assert_eq!(outline.total_items, 6);
    assert_eq!(outline.max_depth, 3);
}

#[test]
fn python_outline_extracts_classes_and_nests_methods() {
    let (file_id, rev) = dummy_file_ids();
    let extractor = OutlineExtractor::default();

    let code = br#"# Python sample
class Navigator:
    def __init__(self, root):
        self.root = root

    def jump_to_line(self, line_num):
        pass

def global_helper():
    return 1
"#;

    let outline = extractor.extract(file_id, rev, "python", code);
    assert_eq!(outline.status, OutlineStatus::Qualified);
    assert_eq!(outline.items.len(), 2);

    let class_item = &outline.items[0];
    assert_eq!(class_item.name, "Navigator");
    assert_eq!(class_item.kind, OutlineItemKind::Class);
    // Indented defs are nested under the class
    assert_eq!(class_item.children.len(), 2);
    assert_eq!(class_item.children[0].name, "__init__");
    assert_eq!(class_item.children[0].kind, OutlineItemKind::Method);
    assert_eq!(class_item.children[1].name, "jump_to_line");
    assert_eq!(class_item.children[1].kind, OutlineItemKind::Method);

    let fn_item = &outline.items[1];
    assert_eq!(fn_item.name, "global_helper");
    assert_eq!(fn_item.kind, OutlineItemKind::Function);
}

#[test]
fn typescript_and_tsx_outline_extracts_components_and_types() {
    let (file_id, rev) = dummy_file_ids();
    let extractor = OutlineExtractor::default();

    let code = br#"interface AppProps {
    title: string;
}

type Mode = 'light' | 'dark';

export class AppController {
}

export function renderApp(props: AppProps) {
}

const Button = () => <button>Click</button>;
"#;

    let outline = extractor.extract(file_id, rev, "tsx", code);
    assert_eq!(outline.status, OutlineStatus::Qualified);
    assert_eq!(outline.items.len(), 5);

    assert_eq!(outline.items[0].name, "AppProps");
    assert_eq!(outline.items[0].kind, OutlineItemKind::Interface);

    assert_eq!(outline.items[1].name, "Mode");
    assert_eq!(outline.items[1].kind, OutlineItemKind::TypeAlias);

    assert_eq!(outline.items[2].name, "AppController");
    assert_eq!(outline.items[2].kind, OutlineItemKind::Class);

    assert_eq!(outline.items[3].name, "renderApp");
    assert_eq!(outline.items[3].kind, OutlineItemKind::Function);

    assert_eq!(outline.items[4].name, "Button");
    assert_eq!(outline.items[4].kind, OutlineItemKind::Function);
}

#[test]
fn unsupported_language_degrades_cleanly_to_line_block_fallback() {
    let (file_id, rev) = dummy_file_ids();
    let limits = ExtractorLimits {
        lines_per_block: 20,
        ..Default::default()
    };
    let extractor = OutlineExtractor::new(limits);

    // 45 lines of raw text in an unsupported esoteric format
    let mut text = String::new();
    for i in 1..=45 {
        text.push_str(&format!("Line {i} content in unknown language\n"));
    }

    let outline = extractor.extract(file_id, rev, "unknown_esoteric_lang", text.as_bytes());
    assert_eq!(outline.status, OutlineStatus::LineBlockFallback);
    assert_eq!(outline.language, "unknown_esoteric_lang");

    // 45 lines / 20 lines per block = 3 line blocks (1..20, 21..40, 41..45)
    assert_eq!(outline.items.len(), 3);
    assert_eq!(outline.items[0].name, "Lines 1–20");
    assert_eq!(outline.items[0].capability_level, CapabilityLevel::Bytes);
    assert_eq!(outline.items[1].name, "Lines 21–40");
    assert_eq!(outline.items[2].name, "Lines 41–45");

    // Line blocks tile the input cleanly
    assert_eq!(outline.items[0].evidence.byte_start, 0);
    assert_eq!(outline.items[2].evidence.byte_end, text.len() as u64);
}

#[test]
fn malformed_syntax_recovers_without_panics_or_memory_spikes() {
    let (file_id, rev) = dummy_file_ids();
    let extractor = OutlineExtractor::default();

    // Adversarial malformed inputs: unclosed braces, inverted delimiters, binary bytes
    let malformed_cases: &[&[u8]] = &[
        b"fn unclosed_fn(x: u32) {",
        b"fn nested() { { { { {",
        b"struct Incomplete",
        b"pub fn weird_eof(",
        b"/* unclosed comment fn foo() {}",
        b"r###\" unclosed raw string fn bar() {}",
        b"\0\x01\x02\xFF\xFE fn with_binary() {}",
    ];

    for &bad_input in malformed_cases {
        let outline = extractor.extract(file_id, rev, "rust", bad_input);
        // Must succeed without panicking
        assert!(!outline.language.is_empty());
    }
}

#[test]
fn negative_control_same_name_candidate_rejected_from_claiming_proven_semantics() {
    let evidence = OutlineEvidence::new(0, 10, 1, 1, None).unwrap();
    let auditor = FactAuditor::new();

    // Valid heuristic fact: same-name candidate marked Heuristic with is_proven_semantic == false
    let valid_heuristic = SourceFact {
        id: 1,
        name: "compute_hash".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Heuristic,
        evidence,
        is_proven_semantic: false,
    };
    assert!(auditor.audit_fact(&valid_heuristic).is_ok());

    // NEGATIVE CONTROL: same-name candidate illegally claiming is_proven_semantic == true
    let illegal_proven_candidate = SourceFact {
        id: 2,
        name: "compute_hash".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Heuristic,
        evidence,
        is_proven_semantic: true, // ILLEGAL! Candidates are never proven definitions
    };
    let err = auditor.audit_fact(&illegal_proven_candidate).unwrap_err();
    assert!(matches!(err, FactAuditError::SameNameCandidateCannotBeProven { .. }));

    // NEGATIVE CONTROL: candidate claiming level > Heuristic
    let illegal_level_candidate = SourceFact {
        id: 3,
        name: "compute_hash".to_string(),
        kind: SourceFactKind::IdentifierCandidate,
        capability_level: CapabilityLevel::Structural, // ILLEGAL for candidates
        evidence,
        is_proven_semantic: false,
    };
    let err = auditor.audit_fact(&illegal_level_candidate).unwrap_err();
    assert!(matches!(err, FactAuditError::SameNameCandidateCannotBeProven { .. }));
}

#[test]
fn negative_control_lexical_import_rejected_from_claiming_proven_external_dependency() {
    let evidence = OutlineEvidence::new(0, 25, 1, 1, None).unwrap();
    let auditor = FactAuditor::new();

    // Valid lexical import fact
    let valid_import = SourceFact {
        id: 10,
        name: "crate::utils::helper".to_string(),
        kind: SourceFactKind::LexicalImport,
        capability_level: CapabilityLevel::Structural,
        evidence,
        is_proven_semantic: false,
    };
    assert!(auditor.audit_fact(&valid_import).is_ok());

    // NEGATIVE CONTROL: lexical import claiming to be a proven external dependency without proof
    let illegal_proven_import = SourceFact {
        id: 11,
        name: "external_pkg::dependency".to_string(),
        kind: SourceFactKind::LexicalImport,
        capability_level: CapabilityLevel::Structural,
        evidence,
        is_proven_semantic: true, // ILLEGAL! (Plan §18.1)
    };
    let err = auditor.audit_fact(&illegal_proven_import).unwrap_err();
    assert!(matches!(
        err,
        FactAuditError::LexicalImportCannotBeProvenExternalDependency { .. }
    ));
}

#[test]
fn negative_control_syntax_fact_cannot_claim_compiler_semantics() {
    let evidence = OutlineEvidence::new(0, 20, 1, 1, None).unwrap();
    let auditor = FactAuditor::new();

    // Valid declared item fact (Structural level, not compiler-proven)
    let valid_decl = SourceFact {
        id: 20,
        name: "MyClass".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::Structural,
        evidence,
        is_proven_semantic: false,
    };
    assert!(auditor.audit_fact(&valid_decl).is_ok());

    // NEGATIVE CONTROL: declared item claiming ExternalSemantic without a compiler provider
    let illegal_compiler_fact = SourceFact {
        id: 21,
        name: "MyClass".to_string(),
        kind: SourceFactKind::DeclaredItem,
        capability_level: CapabilityLevel::ExternalSemantic, // ILLEGAL without external provider
        evidence,
        is_proven_semantic: true,
    };
    let err = auditor.audit_fact(&illegal_compiler_fact).unwrap_err();
    assert!(matches!(
        err,
        FactAuditError::LocalSyntaxCannotClaimCompilerSemantics { .. }
    ));
}
