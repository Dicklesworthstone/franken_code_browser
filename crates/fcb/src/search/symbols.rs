#![forbid(unsafe_code)]

//! Source-symbol navigation through the supported facade. Reuses fcb-analysis
//! and the ordinary reader, without source lookup, parser duplication or runtime.
//! Name lookup returns declaration candidates, not semantic definition resolution.

pub use fcb_analysis::symbols::{CapturedSymbols, SymbolCandidate, SymbolError, SymbolLanguage,
    SymbolNameMode, MAX_SYMBOL_ITEMS, MAX_SYMBOL_SOURCE_BYTES};
pub use fcb_analysis::OutlineItemKind;
use fcb_core::{ByteRange, QueryGeneration, ResourceAllocationId, ResourceBudget};
use crate::{BrowserView, SourceCapture};
use super::{CaptureRequest, DetectedEncoding, PreparedSearchCapture, ReaderError,
    ReadingSeek, ReadingTarget, SourceReader};

#[derive(Clone, Copy, Debug)]
pub struct SymbolOptions {
    pub generation: QueryGeneration,
    pub language: SymbolLanguage,
    pub encoding: Option<DetectedEncoding>,
    pub max_items: usize,
}
impl SymbolOptions {
    pub fn new(generation: QueryGeneration, language: SymbolLanguage) -> Self {
        Self { generation, language, encoding: None, max_items: MAX_SYMBOL_ITEMS }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolNavigationError { Symbol(SymbolError), Reader(ReaderError) }
impl std::fmt::Display for SymbolNavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Symbol(error) => write!(f, "{error}"), Self::Reader(error) => write!(f, "{error}") }
    }
}
impl std::error::Error for SymbolNavigationError {}
impl From<SymbolError> for SymbolNavigationError { fn from(error: SymbolError) -> Self { Self::Symbol(error) } }
impl From<ReaderError> for SymbolNavigationError { fn from(error: ReaderError) -> Self { Self::Reader(error) } }

impl BrowserView {
    /// Explicit worker-side extraction from this already-retained view. No live
    /// provider access; both returned evidence and later selections retain the
    /// exact old bytes even if the host refreshes another view of the same path.
    pub fn symbols(&self, options: SymbolOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedSymbols<'_>, SymbolError> {
        source_symbols(self.source(), options, budget, allocation, canceled)
    }
}
impl PreparedSearchCapture {
    pub fn symbols(&self, options: SymbolOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedSymbols<'_>, SymbolError> {
        source_symbols(self.source(), options, budget, allocation, canceled)
    }
}
fn source_symbols<'a>(source: &'a SourceCapture, options: SymbolOptions, budget: &ResourceBudget,
    allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedSymbols<'a>, SymbolError> {
    let request = CaptureRequest::new(source.file(), source.revision()).map_err(|_| SymbolError::OwnerMismatch)?;
    CapturedSymbols::build(source.bytes(), request, options.generation, options.language,
        options.encoding, options.max_items, budget, allocation, canceled)
}
fn selected_range(symbols: &CapturedSymbols<'_>, id: u64) -> Result<ByteRange, SymbolError> {
    let candidate = symbols.candidate(id).ok_or(SymbolError::NotFound)?;
    Ok(candidate.name_range().unwrap_or(candidate.original_range()))
}
impl<'a> SourceReader<'a> {
    /// Select the validated identifier span, or declaration evidence when the
    /// extractor supplies no exact name span. The ordinary resumable reader
    /// resolves the containing line. No guessed visual columns or new I/O.
    pub fn seek_symbol(&self, symbols: &CapturedSymbols<'_>, id: u64,
        generation: QueryGeneration) -> Result<ReadingSeek<'a>, SymbolNavigationError> {
        let source = self.source();
        symbols.validate_source(source.bytes(), source.file(), source.revision(), generation)?;
        Ok(self.seek(ReadingTarget::Range(selected_range(symbols, id)?), generation)?)
    }
}

#[cfg(feature = "snapshot")]
impl super::paged_snapshot::PagedCapture {
    /// The paged capture was already digest-verified. Its lease and source bytes
    /// remain borrowed for the duration of the returned candidate collection.
    pub fn symbols(&self, options: SymbolOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedSymbols<'_>, SymbolError> {
        let request = CaptureRequest::new(self.file(), self.revision()).map_err(|_| SymbolError::OwnerMismatch)?;
        CapturedSymbols::build(self.bytes(), request, options.generation, options.language,
            options.encoding, options.max_items, budget, allocation, canceled)
    }
    /// Feed this target to the capture's reader. The archive need not remain
    /// open, and its member path is never interpreted as live filesystem access.
    pub fn symbol_target(&self, symbols: &CapturedSymbols<'_>, id: u64,
        generation: QueryGeneration) -> Result<ReadingTarget, SymbolError> {
        symbols.validate_source(self.bytes(), self.file(), self.revision(), generation)?;
        Ok(ReadingTarget::Range(selected_range(symbols, id)?))
    }
}
