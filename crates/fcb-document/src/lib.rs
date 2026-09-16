#![forbid(unsafe_code)]

//! FCB document session state, lens geometry, and FMD output integration (FCB-031.A).
//!
//! Plan §6.2 & §27.4:
//! "FCB lens/session state, source-ID translation, asset requests, scheduling
//!  and integration of FMD output. No Markdown parser, generic flow engine,
//!  typesetter, diagram engine, or copied highlighter."
//!
//! This crate serves as the thin, authoritative bridge between FCB's source capture
//! domain (`CompleteCapture`, `FileId`, `SourceRevision`, `DocumentGeneration`) and
//! `franken_markdown`'s continuous-flow display engine and headless consumer.
//! It performs zero parsing, layout, or font shaping locally, delegating 100% of
//! document processing to upstream FrankenMarkdown.

pub mod assets;
pub mod dialect;
pub mod display_adapter;
pub mod error;
pub mod lens;
pub mod provenance;
pub mod publication;
pub mod session;

pub use assets::{
    AssetDomain, AssetKind, AuthorizedAssetRequest, BoundedAssetBudgets, BoundedAssetRegistry,
    ImageCodecValidator, ImageFormat, TransclusionPolicy, TransclusionTracker,
};
pub use dialect::{
    CalloutEvaluation, DialectCapabilityRow, DialectConfig, DialectFeature,
    DialectSupportLevel, DocumentDialectMatrix, DocumentProfile, HtmlTreatment,
    TaskItemState,
};
pub use display_adapter::DocumentDisplayPlan;
pub use error::DocumentError;
pub use lens::DocumentLens;
pub use provenance::{
    resolve_reading_selection, verify_provenance_truthfulness, SelectionResolution,
};
pub use publication::{DocumentPublication, RetainedLayoutMetrics};
pub use session::{
    DocumentBudgets, DocumentFlowLine, DocumentSession, DocumentViewConstraints,
    HeadlessDocumentOutput, ResumableDocumentLayout,
};

// Curated re-exports from upstream FrankenMarkdown for downstream consumers
pub use franken_markdown::{
    AccessibleReadingNode, AccessibleReadingRole, AssetRequest, AssetRequestId,
    AssetResult, DisplayClip, DisplayImage, DisplayItem, DisplayList,
    DisplayRect, DisplaySemanticAnchor, DisplayTextRun, DisplayVectorPath,
    DocumentSourceMap, HeadingSourceAnchor, NestedProvenanceGraph,
    ProvenanceAuditReport, RenderedElement, SourceSpan, TextSelectionRange,
    UnresolvedAsset,
};
