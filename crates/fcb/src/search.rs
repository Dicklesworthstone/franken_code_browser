#![forbid(unsafe_code)]

//! Headless search and exact result navigation through the public `fcb` facade.
//!
//! Content search uses retained captures, `EphemeralIndex`, and `IndexedQuery`.
//! Path navigation uses `PathIndex` and `PathSearch` without loading source.
//! Demand-read `ObservedExtent` values retain only requested source ranges;
//! `ExtentView` and `ExtentQuery` read/search those bytes without filling holes.
//! All worker operations take explicit bounds; none creates a runtime, window,
//! filesystem grant, or persistent store.
//!
//! A content hit opens its searched capture. A path result instead identifies
//! a file to capture explicitly; it does not pretend to pin unread source.
//! Native identities never pass through lossy strings or escaped labels.

pub mod reader;
pub mod reading_window;
pub mod extents;
pub mod extent_query;
pub mod extent_navigation;
pub use extent_navigation::ExtentActivationError;
pub use extents::{ExtentConsistency, ExtentError, ExtentReadState, ExtentReadStats,
    ExtentStepBudget, ExtentWindowRequest, FileExtentRead, FileRangeReader,
    ObservedExtent, ExtentView, ExtentViewError, ExtentText};
pub use extent_query::{ExtentMatch, ExtentQuery, ExtentQueryError, ExtentQueryInput,
    ExtentQueryOptions, ExtentQueryState};
pub use reader::{ReaderError, ReaderIndexProgress, ReaderLimits, ReadingAnchor,
    ReadingSeek, ReadingSeekState, ReadingTarget, SourceReader};
pub use reading_window::{LineEnding, ReadingLine, ReadingSelection, ReadingWindow, ReadingWindowOptions};

pub use fcb_search::*;
pub use fcb_search::paths::{IndexedPath, PathCase, PathEntry, PathIndex, PathIndexLimits,
    PathMatch, PathMatchKind, PathMatchMode, PathRank, PathSearch, PathSearchError,
    PathSearchOptions, PathSearchState, PathSelection, PathStepBudget, RawPath};
pub use fcb_core::{QueryGeneration, ResourceAllocationId, ResourceBudget, RootId};
pub use fcb_source::{CaptureRequest, CompleteCapture, DetectedEncoding};

use std::sync::Arc;
use crate::{BrowserSession, BrowserView, FcbError, FileId, SourceCapture};

/// Native identity supplied by an authorized host source provider. This is a
/// declaration to validate, NOT a grant and NOT permission to read a path.
#[derive(Clone, Copy, Debug)]
pub struct NativeSourceIdentity<'path> {
    pub root: RootId,
    pub path: &'path RawPath,
}

/// Selection captured from one query's visible or explicitly pinned result.
/// Index/query generations stay attached even if ordering subsequently changes.
#[derive(Clone, Copy, Debug)]
pub struct PathNavigationTarget<'index> {
    hit: PathMatch<'index>,
    index: SearchManifestId,
    generation: QueryGeneration,
}
impl<'index> PathNavigationTarget<'index> {
    pub fn from_search(search: &PathSearch<'index>, file: FileId) -> Result<Self, FcbError> {
        let hit = search.visible_matches().iter().find(|hit| hit.file_id() == file).copied()
            .or_else(|| match search.selection() {
                PathSelection::Matched(hit) if hit.file_id() == file => Some(hit),
                _ => None,
            }).ok_or(FcbError::SourceNotFound)?;
        Ok(Self { hit, index: search.index_id(), generation: search.generation() })
    }
    pub const fn file(&self) -> FileId { self.hit.file_id() }
    pub const fn root(&self) -> RootId { self.hit.root_id() }
    pub fn native_path(&self) -> &'index RawPath { self.hit.path().raw_path() }
    pub const fn index(&self) -> SearchManifestId { self.index }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub fn validate_delivery(&self, index: SearchManifestId, generation: QueryGeneration) -> Result<(), FcbError> {
        if self.index != index || self.generation != generation { return Err(FcbError::StaleGeneration); }
        Ok(())
    }
}

/// A facade source and the exact capture consumed by the search engine share
/// the same immutable byte allocation. Preparation computes the observation
/// digest, so it is explicit worker work rather than an interaction callback.
#[derive(Clone, Debug)]
pub struct PreparedSearchCapture {
    source: SourceCapture,
    capture: CompleteCapture,
}

impl PreparedSearchCapture {
    pub fn source(&self) -> &SourceCapture { &self.source }
    pub fn capture(&self) -> &CompleteCapture { &self.capture }
    pub fn document(&self) -> SearchDocument<'_> {
        SearchDocument::new(self.source.file(), self.source.logical_path(), &self.capture)
    }
    /// Read the exact original bytes named by a verified hit. Decoded UTF-8
    /// ranges are deliberately not used for slicing UTF-16 or arbitrary bytes.
    pub fn hit_bytes(&self, hit: &SearchMatch) -> Result<&[u8], FcbError> {
        exact_hit_bytes(&self.source, hit)
    }
}

impl BrowserSession {
    /// Open bytes explicitly captured for a selected path. The caller validates
    /// its native grant and performs I/O on its worker before this operation.
    /// File, root, raw path and active index/query must still agree. No provider
    /// lookup is repeated here. The capture's UTF-8 logical key may be an escaped
    /// label; it is deliberately NOT used as native path authority.
    ///
    /// Path lookup permits a newer content revision of the same file. Unlike a
    /// content match, it never claimed to have searched that file's old bytes.
    pub fn open_path_target(
        &self, target: &PathNavigationTarget<'_>, active_index: SearchManifestId,
        active_generation: QueryGeneration, native: NativeSourceIdentity<'_>,
        capture: SourceCapture,
    ) -> Result<BrowserView, FcbError> {
        target.validate_delivery(active_index, active_generation)?;
        if target.file().owner() != self.owner() || native.root.owner() != self.owner()
            || capture.owner() != self.owner() || native.root != target.root() {
            return Err(FcbError::OwnerMismatch);
        }
        if capture.file() != target.file() { return Err(FcbError::SourceNotFound); }
        if native.path != target.native_path() { return Err(FcbError::StaleGeneration); }
        self.open_capture(capture)
    }

    /// Prepare source already supplied by the host, without another provider
    /// lookup or copying its payload. The session's owner must match the capture.
    pub fn prepare_search_capture(&self, source: SourceCapture) -> Result<PreparedSearchCapture, FcbError> {
        if source.owner() != self.owner() { return Err(FcbError::OwnerMismatch); }
        let request = CaptureRequest::new(source.file(), source.revision())
            .map_err(|_| FcbError::OwnerMismatch)?;
        let capture = CompleteCapture::new(request, source.byte_length()?, Arc::clone(&source.bytes))
            .map_err(|_| FcbError::CaptureTooLarge)?;
        Ok(PreparedSearchCapture { source, capture })
    }

    /// Explicitly ask the configured provider for one source and retain it for
    /// search and navigation. No root enumeration or implicit scope widening.
    pub fn capture_for_search(&self, logical_path: &str) -> Result<PreparedSearchCapture, FcbError> {
        let provider = self.provider.as_ref().ok_or(FcbError::ProviderUnavailable)?;
        self.prepare_search_capture(provider.capture(logical_path)?)
    }

    /// Open a renderer-neutral reading view on the *searched* source revision.
    /// Validate report generation/manifest at delivery time before calling this
    /// method to activate a current result. This operation validates source
    /// identity/range and never fetches live source under an old revision label.
    pub fn open_search_hit(
        &self, source: &PreparedSearchCapture, hit: &SearchMatch,
    ) -> Result<BrowserView, FcbError> {
        if source.source.owner() != self.owner() { return Err(FcbError::OwnerMismatch); }
        source.hit_bytes(hit)?;
        self.open_capture(source.source.clone())
    }
}

impl BrowserView {
    /// Resolve an occurrence against this view's exact capture. A hit from a
    /// newer/older source revision cannot silently select bytes in this view.
    pub fn search_hit_bytes(&self, hit: &SearchMatch) -> Result<&[u8], FcbError> {
        exact_hit_bytes(self.source(), hit)
    }
}

fn exact_hit_bytes<'a>(source: &'a SourceCapture, hit: &SearchMatch) -> Result<&'a [u8], FcbError> {
    if hit.file_id.owner() != source.owner() || hit.revision.owner() != source.owner() {
        return Err(FcbError::OwnerMismatch);
    }
    if hit.file_id != source.file() { return Err(FcbError::SourceNotFound); }
    if hit.revision != source.revision() { return Err(FcbError::StaleGeneration); }
    if hit.original_byte_range.is_empty() { return Err(FcbError::SourceNotFound); }
    let (start, end) = hit.original_byte_range.as_usize_bounds()
        .map_err(|_| FcbError::CaptureTooLarge)?;
    source.bytes().get(start..end).ok_or(FcbError::SourceNotFound)
}
