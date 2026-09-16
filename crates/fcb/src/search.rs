#![forbid(unsafe_code)]

//! Headless search and exact result navigation through the public `fcb` facade.
//!
//! Enable `fcb`'s `search` feature. A host explicitly prepares captures, forms a
//! `SearchManifest`, supplies a shared `ResourceBudget`, builds an
//! `EphemeralIndex`, and drives `IndexedQuery` on its worker. These operations
//! do not create a runtime, window, filesystem grant, or persistent store.
//!
//! A path is a provider lookup key, not a capture identity. Keep the returned
//! `PreparedSearchCapture` until navigation has finished. Opening a hit uses
//! that retained capture, never a second call to a potentially changed provider.

pub use fcb_search::*;
pub use fcb_core::{QueryGeneration, ResourceAllocationId, ResourceBudget};
pub use fcb_source::{CaptureRequest, CompleteCapture, DetectedEncoding};

use std::sync::Arc;
use crate::{BrowserSession, BrowserView, FcbError, SourceCapture};

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
