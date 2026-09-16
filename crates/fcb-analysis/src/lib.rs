#![forbid(unsafe_code)]

//! Source-specific structural outlines, facts, and analysis coordination (FCB-030.A).
//!
//! Provides language-scoped structural outline extraction with exact evidence
//! spans, hierarchical nesting, line-block fallback for unsupported or uncolored
//! languages, and fact capability boundary enforcement ensuring highlighters and
//! heuristic candidates never masquerade as proven compiler semantics.

pub mod extractor;
pub mod facts;
pub mod outline;

pub use extractor::{ExtractorLimits, OutlineExtractor};
pub use facts::{FactAuditError, FactAuditor, SourceFact, SourceFactKind};
pub use outline::{
    CapabilityLevel, OutlineEvidence, OutlineItem, OutlineItemKind, OutlineStatus, SourceOutline,
};
