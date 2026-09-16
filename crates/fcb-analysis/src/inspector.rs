//! Evidence capability schema and Inspector facts (FCB-030.B).
//!
//! Implements Plan §5.5, §11.5, and §18.1:
//! - Complete evidence capability schema across the 6-level ladder:
//!   `Bytes`, `Lexical`, `Structural`, `ResolvedLocal`, `ExternalSemantic`, `Heuristic`.
//! - Inspector facts bundle: logical path, language capability, byte/line count,
//!   current revision, indexing status, outline summary, inbound/outbound relationships,
//!   and nearby documentation snippets.
//! - Unknown counts are explicitly `CountMetric::Unknown`, NEVER conflated with 0.
//! - Heuristic facts carry mandatory badges and explicit explanations.
//! - All relationship edges record exact evidence spans, capability tier, and extractor version.

use crate::facts::{FactAuditError, FactAuditor, SourceFact};
use crate::outline::{CapabilityLevel, OutlineEvidence, OutlineStatus, SourceOutline};
use fcb_core::{FileId, SourceRevision};
use std::collections::BTreeMap;
use std::fmt;

/// An integer metric that distinguishes known counts from unknown/indeterminate states.
///
/// Plan §5.5: "Unknown facts are unknown, not zero."
///
/// If relationship analysis or cross-file indexing has not executed, the count
/// MUST be `CountMetric::Unknown`. Conflating an unknown count with 0 produces
/// false claims about graph isolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CountMetric {
    /// The count is uncomputed, unavailable, or indeterminate.
    Unknown,
    /// The count is deterministically known.
    Known(u64),
}

impl CountMetric {
    /// Returns `true` if the count is unknown.
    #[must_use]
    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Returns `true` if the count is known.
    #[must_use]
    pub const fn is_known(self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// Retrieve the count if known, or `None` if unknown.
    #[must_use]
    pub const fn as_known(self) -> Option<u64> {
        match self {
            Self::Known(count) => Some(count),
            Self::Unknown => None,
        }
    }

    /// Require the count to be known, returning an error if it is unknown.
    ///
    /// This prevents accidentally treating an unknown metric as zero.
    pub fn require_known(self) -> Result<u64, InspectorError> {
        match self {
            Self::Known(count) => Ok(count),
            Self::Unknown => Err(InspectorError::UnknownCountCannotBeZero),
        }
    }
}

impl fmt::Display for CountMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::Known(count) => write!(f, "{count}"),
        }
    }
}

/// Indexing and analysis status of a source file in the repository index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexingStatus {
    /// The file has not been indexed yet.
    NotIndexed,
    /// Indexing is in progress or queued.
    Pending,
    /// Successfully indexed at the specified source revision.
    Indexed {
        /// Source revision at which indexing completed.
        revision: SourceRevision,
    },
    /// Indexing degraded or fell back due to malformed source syntax.
    DegradedMalformed,
}

impl IndexingStatus {
    /// Canonical text label for telemetry and display.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotIndexed => "not_indexed",
            Self::Pending => "pending",
            Self::Indexed { .. } => "indexed",
            Self::DegradedMalformed => "degraded_malformed",
        }
    }
}

/// A heuristic fact candidate with mandatory badge and explanation.
///
/// Plan §5.5: "Heuristic facts have an explicit badge and explanation."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeuristicFact {
    /// The underlying fact with `CapabilityLevel::Heuristic`.
    pub fact: SourceFact,
    /// Prominent badge label (e.g. `"[Heuristic Candidate]"`).
    pub badge: String,
    /// Human-readable explanation of why this fact is a candidate rather than proven.
    pub explanation: String,
}

impl HeuristicFact {
    /// Construct and validate a heuristic fact.
    ///
    /// Returns an error if the fact is not at `CapabilityLevel::Heuristic` or
    /// claims `is_proven_semantic == true`.
    pub fn new(
        fact: SourceFact,
        badge: impl Into<String>,
        explanation: impl Into<String>,
    ) -> Result<Self, FactAuditError> {
        if fact.capability_level != CapabilityLevel::Heuristic || fact.is_proven_semantic {
            return Err(FactAuditError::SameNameCandidateCannotBeProven {
                name: fact.name,
                claimed_level: fact.capability_level,
            });
        }
        Ok(Self {
            fact,
            badge: badge.into(),
            explanation: explanation.into(),
        })
    }
}

/// Category of an inbound or outbound relationship edge.
///
/// Plan §18.1: "A co-occurrence edge is not a call edge. A lexical import name
/// is not a resolved external package dependency until resolution rules support that conclusion."
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelationshipKind {
    /// Lexical import statement (e.g. `use std::path;`).
    LexicalImport,
    /// Resolved local module or relative import proven within project scope.
    ResolvedLocalImport,
    /// External compiler/LSP verified symbol reference.
    CompilerReference,
    /// Heuristic candidate link (e.g. same-name identifier match).
    HeuristicCandidate,
}

impl RelationshipKind {
    /// Canonical label for serialization and inspection.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LexicalImport => "lexical_import",
            Self::ResolvedLocalImport => "resolved_local_import",
            Self::CompilerReference => "compiler_reference",
            Self::HeuristicCandidate => "heuristic_candidate",
        }
    }
}

/// A directional relationship edge with exact evidence span and capability tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationshipEdge {
    /// Unique identifier for this relationship edge.
    pub edge_id: u64,
    /// Source file originating the relationship.
    pub source_file: FileId,
    /// Target file if resolved, or `None` if unproven/external.
    pub target_file: Option<FileId>,
    /// Target symbol or path string.
    pub target_symbol: String,
    /// Edge relationship category.
    pub kind: RelationshipKind,
    /// Capability classification on the 6-level ladder.
    pub capability_level: CapabilityLevel,
    /// Exact evidence byte and line span in the source file.
    pub evidence: OutlineEvidence,
    /// Extractor version that generated this edge.
    pub extractor_version: u32,
}

/// Summary of structural outline metrics for the Inspector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlineSummary {
    /// Outline status (Qualified, LineBlockFallback, DegradedMalformed).
    pub status: OutlineStatus,
    /// Total number of items across all levels.
    pub total_items: usize,
    /// Maximum nesting depth.
    pub max_depth: usize,
}

impl From<&SourceOutline> for OutlineSummary {
    fn from(outline: &SourceOutline) -> Self {
        Self {
            status: outline.status,
            total_items: outline.total_items,
            max_depth: outline.max_depth,
        }
    }
}

/// A documentation snippet located adjacent to a declared entity or file head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyDocSnippet {
    /// Symbol or section name documented.
    pub target_name: String,
    /// Extracted documentation text (e.g. doc comment content).
    pub doc_text: String,
    /// Byte range of the documentation in the source file.
    pub byte_range: (u64, u64),
    /// Line range (1-indexed) of the documentation.
    pub line_range: (u32, u32),
}

/// The unified source Inspector facts bundle (Plan §5.5).
///
/// Exposes path, language capability, byte/line count, current revision,
/// indexing status, outline summary, inbound/outbound relationships,
/// heuristic facts with explicit badges, and nearby documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectorFacts {
    /// Logical workspace-relative path of the file.
    pub logical_path: String,
    /// File identifier.
    pub file_id: FileId,
    /// Source revision observed.
    pub revision: SourceRevision,
    /// Language route identifier.
    pub language: String,
    /// Language capability level on the Plan §11.5 ladder.
    pub language_capability: CapabilityLevel,
    /// Exact byte count of the captured source.
    pub byte_count: u64,
    /// Line count of the captured source.
    pub line_count: u32,
    /// Current indexing status.
    pub indexing_status: IndexingStatus,
    /// Outline summary.
    pub outline_summary: OutlineSummary,
    /// Count of inbound relationships, or `CountMetric::Unknown`.
    pub inbound_relationships: CountMetric,
    /// Count of outbound relationships, or `CountMetric::Unknown`.
    pub outbound_relationships: CountMetric,
    /// Inbound relationship edges.
    pub inbound_edges: Vec<RelationshipEdge>,
    /// Outbound relationship edges.
    pub outbound_edges: Vec<RelationshipEdge>,
    /// Extracted source facts.
    pub facts: Vec<SourceFact>,
    /// Heuristic candidates with explicit badges and explanations.
    pub heuristic_facts: Vec<HeuristicFact>,
    /// Nearby documentation snippets.
    pub nearby_docs: Vec<NearbyDocSnippet>,
}

impl InspectorFacts {
    /// Retrieve facts matching a specific capability level.
    #[must_use]
    pub fn facts_at_level(&self, level: CapabilityLevel) -> Vec<&SourceFact> {
        self.facts
            .iter()
            .filter(|f| f.capability_level == level)
            .collect()
    }

    /// Compute counts of facts categorized by each level on the capability ladder.
    #[must_use]
    pub fn facts_count_by_capability(&self) -> BTreeMap<CapabilityLevel, usize> {
        let mut counts = BTreeMap::new();
        for level in &[
            CapabilityLevel::Bytes,
            CapabilityLevel::Lexical,
            CapabilityLevel::Structural,
            CapabilityLevel::ResolvedLocal,
            CapabilityLevel::ExternalSemantic,
            CapabilityLevel::Heuristic,
        ] {
            counts.insert(*level, 0);
        }
        for fact in &self.facts {
            *counts.entry(fact.capability_level).or_insert(0) += 1;
        }
        counts
    }

    /// Whether this file has any heuristic facts that require candidate disclosure.
    #[must_use]
    pub fn has_heuristic_facts(&self) -> bool {
        !self.heuristic_facts.is_empty()
    }

    /// Whether this file is indexed at the current revision.
    #[must_use]
    pub fn is_indexed_at_current_revision(&self) -> bool {
        match self.indexing_status {
            IndexingStatus::Indexed { revision } => revision == self.revision,
            _ => false,
        }
    }
}

/// Errors occurring during Inspector facts construction or auditing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectorError {
    /// An unknown count metric was illegally treated as zero.
    UnknownCountCannotBeZero,
    /// A source fact violated the capability oracle.
    AuditFailed(FactAuditError),
    /// A heuristic fact made an illegal semantic claim.
    HeuristicIllegalClaim(String),
}

impl fmt::Display for InspectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCountCannotBeZero => {
                write!(f, "unknown count metric cannot be treated as zero (Plan §5.5)")
            }
            Self::AuditFailed(e) => write!(f, "fact audit failed: {e}"),
            Self::HeuristicIllegalClaim(msg) => write!(f, "heuristic illegal claim: {msg}"),
        }
    }
}

impl std::error::Error for InspectorError {}

impl From<FactAuditError> for InspectorError {
    fn from(e: FactAuditError) -> Self {
        Self::AuditFailed(e)
    }
}

/// Builder for constructing verified `InspectorFacts`.
#[derive(Clone, Debug)]
pub struct InspectorFactsBuilder {
    logical_path: String,
    file_id: FileId,
    revision: SourceRevision,
    language: String,
    language_capability: CapabilityLevel,
    byte_count: u64,
    line_count: u32,
    indexing_status: IndexingStatus,
    outline_summary: OutlineSummary,
    inbound_relationships: CountMetric,
    outbound_relationships: CountMetric,
    inbound_edges: Vec<RelationshipEdge>,
    outbound_edges: Vec<RelationshipEdge>,
    facts: Vec<SourceFact>,
    heuristic_facts: Vec<HeuristicFact>,
    nearby_docs: Vec<NearbyDocSnippet>,
}

impl InspectorFactsBuilder {
    /// Initialize a new builder with core file identity and source measurements.
    pub fn new(
        logical_path: impl Into<String>,
        file_id: FileId,
        revision: SourceRevision,
        language: impl Into<String>,
        language_capability: CapabilityLevel,
        byte_count: u64,
        line_count: u32,
    ) -> Self {
        Self {
            logical_path: logical_path.into(),
            file_id,
            revision,
            language: language.into(),
            language_capability,
            byte_count,
            line_count,
            indexing_status: IndexingStatus::NotIndexed,
            outline_summary: OutlineSummary {
                status: OutlineStatus::LineBlockFallback,
                total_items: 0,
                max_depth: 0,
            },
            inbound_relationships: CountMetric::Unknown,
            outbound_relationships: CountMetric::Unknown,
            inbound_edges: Vec::new(),
            outbound_edges: Vec::new(),
            facts: Vec::new(),
            heuristic_facts: Vec::new(),
            nearby_docs: Vec::new(),
        }
    }

    /// Set the indexing status.
    #[must_use]
    pub fn indexing_status(mut self, status: IndexingStatus) -> Self {
        self.indexing_status = status;
        self
    }

    /// Set outline summary from a `SourceOutline`.
    #[must_use]
    pub fn outline(mut self, outline: &SourceOutline) -> Self {
        self.outline_summary = OutlineSummary::from(outline);
        self
    }

    /// Set inbound relationship metric.
    #[must_use]
    pub fn inbound_relationships(mut self, metric: CountMetric) -> Self {
        self.inbound_relationships = metric;
        self
    }

    /// Set outbound relationship metric.
    #[must_use]
    pub fn outbound_relationships(mut self, metric: CountMetric) -> Self {
        self.outbound_relationships = metric;
        self
    }

    /// Add an inbound relationship edge.
    pub fn add_inbound_edge(&mut self, edge: RelationshipEdge) {
        self.inbound_edges.push(edge);
    }

    /// Add an outbound relationship edge.
    pub fn add_outbound_edge(&mut self, edge: RelationshipEdge) {
        self.outbound_edges.push(edge);
    }

    /// Add a verified source fact.
    pub fn add_fact(&mut self, fact: SourceFact) {
        self.facts.push(fact);
    }

    /// Add a heuristic fact candidate with badge and explanation.
    pub fn add_heuristic_fact(&mut self, heuristic_fact: HeuristicFact) {
        self.heuristic_facts.push(heuristic_fact);
    }

    /// Add nearby documentation snippet.
    pub fn add_doc_snippet(&mut self, snippet: NearbyDocSnippet) {
        self.nearby_docs.push(snippet);
    }

    /// Build and audit the complete `InspectorFacts`.
    ///
    /// Validates:
    /// - All facts pass the `FactAuditor` capability rules.
    /// - Unsupported languages (capability `Bytes`) do not contain structural or semantic claims.
    /// - Heuristic facts do not claim proven status.
    pub fn build(self) -> Result<InspectorFacts, InspectorError> {
        let auditor = FactAuditor::new();
        for fact in &self.facts {
            auditor.audit_fact(fact)?;

            // If file language capability is Bytes, local facts cannot claim Structural+
            if self.language_capability == CapabilityLevel::Bytes
                && fact.capability_level > CapabilityLevel::Bytes
            {
                return Err(InspectorError::AuditFailed(
                    FactAuditError::UnsupportedLanguageCannotClaimSemantics {
                        name: fact.name.clone(),
                        claimed_level: fact.capability_level,
                    },
                ));
            }
        }

        for h in &self.heuristic_facts {
            if h.fact.is_proven_semantic || h.fact.capability_level != CapabilityLevel::Heuristic {
                return Err(InspectorError::HeuristicIllegalClaim(format!(
                    "heuristic fact '{}' illegally claimed proven status or non-heuristic level",
                    h.fact.name
                )));
            }
        }

        Ok(InspectorFacts {
            logical_path: self.logical_path,
            file_id: self.file_id,
            revision: self.revision,
            language: self.language,
            language_capability: self.language_capability,
            byte_count: self.byte_count,
            line_count: self.line_count,
            indexing_status: self.indexing_status,
            outline_summary: self.outline_summary,
            inbound_relationships: self.inbound_relationships,
            outbound_relationships: self.outbound_relationships,
            inbound_edges: self.inbound_edges,
            outbound_edges: self.outbound_edges,
            facts: self.facts,
            heuristic_facts: self.heuristic_facts,
            nearby_docs: self.nearby_docs,
        })
    }
}
