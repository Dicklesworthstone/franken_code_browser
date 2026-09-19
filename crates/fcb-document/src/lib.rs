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
pub mod gallery;
pub mod lens;
pub mod selection_copy;
pub mod split_navigation;
pub mod provenance;
pub mod publication;
pub mod session;
/// Bounded captured-document reading, heading navigation and explicit copy domains.
pub mod reader;

pub use gallery::{
    BackingScale, CharacterBoundary, ContrastOracle, FractionalOffset, GalleryCaret,
    GalleryGlyph, GalleryGlyphRun, GalleryRouteIdentity, GallerySectionId,
    GallerySelectionOverlay, GalleryTestCase, GalleryThemeMode, GalleryVisualTokens,
    ReadableRunInspection, TypographyGallery,
};

pub use assets::{
    AssetDomain, AssetKind, AuthorizedAssetRequest, BoundedAssetBudgets, BoundedAssetRegistry,
    BoundedImageDecoder, DecodedImage, ImageCacheKey, ImageCodecValidator, ImageFormat,
    ImageFormatCapabilities, PrivateImageCache, TransclusionPolicy, TransclusionTracker,
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
pub use selection_copy::{
    DocumentClipboardFlavor, DocumentClipboardLimits, DocumentCopyAction, DocumentCopyError,
    DocumentNativeClipboard, DocumentSyncPoint, MarkdownSourceCopy, RenderedSelectionKind,
    RenderedSelectionProvenance, StagedDocumentClipboard, StreamedDocumentExport,
    TruthfulSelectionResolver,
};
pub use session::{
    DocumentBudgets, DocumentFlowLine, DocumentSession, DocumentViewConstraints,
    HeadlessDocumentOutput, ResumableDocumentLayout,
};
pub use split_navigation::{discover_readme, SharedAnchor, SplitPaneNavigator};

// Curated re-exports from upstream FrankenMarkdown for downstream consumers
pub use franken_markdown::{
    AccessibleReadingNode, AccessibleReadingRole, AssetRequest, AssetRequestId,
    AssetResult, DisplayClip, DisplayImage, DisplayItem, DisplayList,
    DisplayRect, DisplaySemanticAnchor, DisplayTextRun, DisplayVectorPath,
    DocumentSourceMap, HeadingSourceAnchor, NestedProvenanceGraph,
    ProvenanceAuditReport, RenderedElement, SourceSpan, TextSelectionRange,
    UnresolvedAsset,
};

/// Upstream lexical roles translated for native source presentation.
pub mod source_highlight;
