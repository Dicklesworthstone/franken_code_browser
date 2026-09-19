#![forbid(unsafe_code)]

//! Source-symbol navigation through the supported facade. Reuses fcb-analysis
//! and the ordinary reader, without source lookup, parser duplication or runtime.
//! Name lookup returns declaration candidates; reference lookup returns whole-token
//! text candidates. Neither route claims compiler name or reference resolution.

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
pub enum SymbolNavigationError { Symbol(SymbolError), Reader(ReaderError), EncodingMismatch }
impl std::fmt::Display for SymbolNavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Symbol(error) => write!(f, "{error}"), Self::Reader(error) => write!(f, "{error}"),
            Self::EncodingMismatch => f.write_str("SYMBOL_READER_ENCODING_MISMATCH") }
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
fn encoding_compatible(left: DetectedEncoding, right: DetectedEncoding) -> bool {
    left == right || matches!((left, right), (DetectedEncoding::Utf8 { .. }, DetectedEncoding::Utf8 { .. }))
}
impl<'a> SourceReader<'a> {
    /// Select the validated identifier span, or declaration evidence when the
    /// extractor supplies no exact name span. The ordinary resumable reader
    /// resolves the containing line. No guessed visual columns or new I/O.
    /// Declared BOM-less UTF-16 can produce valid symbol ranges, but cannot be
    /// silently activated in an auto-detected UTF-8 reader.
    pub fn seek_symbol(&self, symbols: &CapturedSymbols<'_>, id: u64,
        generation: QueryGeneration) -> Result<ReadingSeek<'a>, SymbolNavigationError> {
        let source = self.source();
        symbols.validate_source(source.bytes(), source.file(), source.revision(), generation)?;
        if !encoding_compatible(symbols.encoding(), self.encoding()) { return Err(SymbolNavigationError::EncodingMismatch); }
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
    /// Feed this target to the capture's auto-detected reader. The archive need
    /// not remain open. An incompatible declared encoding is refused explicitly.
    pub fn symbol_target(&self, symbols: &CapturedSymbols<'_>, id: u64,
        generation: QueryGeneration) -> Result<ReadingTarget, SymbolNavigationError> {
        symbols.validate_source(self.bytes(), self.file(), self.revision(), generation)?;
        let encoding = fcb_source::detect_encoding(&self.bytes()[..self.bytes().len().min(3)]);
        if !encoding_compatible(symbols.encoding(), encoding) { return Err(SymbolNavigationError::EncodingMismatch); }
        Ok(ReadingTarget::Range(selected_range(symbols, id)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArenaOwnerId, BrowserSession, ByteLength, FileId, SourceRevision};
    #[test]
    fn declared_bomless_utf16_is_not_silently_read_as_utf8() {
        let owner = ArenaOwnerId::new(861).unwrap();
        let bytes: Vec<u8> = "fn hello() {}\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let source = SourceCapture::from_bytes(owner, FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap(),
            "hello.rs", bytes).unwrap();
        let view = BrowserSession::new(owner).open_capture(source).unwrap();
        let generation = QueryGeneration::new(owner, 1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(16 * 1024 * 1024)).unwrap();
        let mut options = SymbolOptions::new(generation, SymbolLanguage::Rust);
        options.encoding = Some(DetectedEncoding::Utf16Le);
        let symbols = view.symbols(options, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        assert_eq!(symbols.candidates()[0].name(), "hello");
        assert_eq!(symbols.encoding(), DetectedEncoding::Utf16Le);
        let reader = view.source_reader(super::super::ReaderLimits::default(), &budget, ResourceAllocationId::new(2).unwrap()).unwrap();
        assert!(matches!(reader.seek_symbol(&symbols, 1, generation), Err(SymbolNavigationError::EncodingMismatch)));
    }
}

// Reference occurrences share the source navigation surface, but never inherit
// the outline extractor's declaration kinds or imply compiler-proven bindings.
pub use fcb_analysis::references::{CapturedReferences, ReferenceCandidate, ReferenceError,
    MAX_REFERENCE_ITEMS, MAX_REFERENCE_NAME_BYTES, MAX_REFERENCE_SOURCE_BYTES};

#[derive(Clone, Copy, Debug)]
pub struct ReferenceOptions {
    pub generation: QueryGeneration,
    pub encoding: Option<DetectedEncoding>,
    pub max_items: usize,
}
impl ReferenceOptions {
    pub fn new(generation: QueryGeneration) -> Self {
        Self { generation, encoding: None, max_items: MAX_REFERENCE_ITEMS }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceNavigationError {
    Reference(ReferenceError), Reader(ReaderError), EncodingMismatch,
}
impl std::fmt::Display for ReferenceNavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reference(error) => write!(f, "{error}"),
            Self::Reader(error) => write!(f, "{error}"),
            Self::EncodingMismatch => f.write_str("REFERENCE_READER_ENCODING_MISMATCH"),
        }
    }
}
impl std::error::Error for ReferenceNavigationError {}
impl From<ReferenceError> for ReferenceNavigationError {
    fn from(error: ReferenceError) -> Self { Self::Reference(error) }
}
impl From<ReaderError> for ReferenceNavigationError {
    fn from(error: ReaderError) -> Self { Self::Reader(error) }
}

impl BrowserView {
    /// Find exact whole-token text candidates, including comments/literals, in
    /// the retained observation. No filesystem or language-server execution.
    pub fn references(&self, name: &str, options: ReferenceOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedReferences<'_>, ReferenceError> {
        source_references(self.source(), name, options, budget, allocation, canceled)
    }
}
impl PreparedSearchCapture {
    pub fn references(&self, name: &str, options: ReferenceOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedReferences<'_>, ReferenceError> {
        source_references(self.source(), name, options, budget, allocation, canceled)
    }
}
fn source_references<'a>(source: &'a SourceCapture, name: &str, options: ReferenceOptions,
    budget: &ResourceBudget, allocation: ResourceAllocationId,
    canceled: impl FnMut() -> bool) -> Result<CapturedReferences<'a>, ReferenceError> {
    let request = CaptureRequest::new(source.file(), source.revision()).map_err(|_| ReferenceError::OwnerMismatch)?;
    CapturedReferences::build(source.bytes(), request, options.generation, name,
        options.encoding, options.max_items, budget, allocation, canceled)
}
impl<'a> SourceReader<'a> {
    /// Activate a retained occurrence through the ordinary source reader. A
    /// changed source, foreign owner, superseded query, or incompatible decoder
    /// is refused before the byte range can become a reading selection.
    pub fn seek_reference(&self, references: &CapturedReferences<'_>, id: u64,
        generation: QueryGeneration) -> Result<ReadingSeek<'a>, ReferenceNavigationError> {
        let source = self.source();
        references.validate_source(source.bytes(), source.file(), source.revision(), generation)?;
        if !encoding_compatible(references.encoding(), self.encoding()) {
            return Err(ReferenceNavigationError::EncodingMismatch);
        }
        let candidate = references.candidate(id).ok_or(ReferenceError::NotFound)?;
        Ok(self.seek(ReadingTarget::Range(candidate.original_range()), generation)?)
    }
}
#[cfg(feature = "snapshot")]
impl super::paged_snapshot::PagedCapture {
    /// Search this already-verified archive member, without consulting its old
    /// live root. The returned occurrences borrow the retained member bytes.
    pub fn references(&self, name: &str, options: ReferenceOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<CapturedReferences<'_>, ReferenceError> {
        let request = CaptureRequest::new(self.file(), self.revision()).map_err(|_| ReferenceError::OwnerMismatch)?;
        CapturedReferences::build(self.bytes(), request, options.generation, name,
            options.encoding, options.max_items, budget, allocation, canceled)
    }
    pub fn reference_target(&self, references: &CapturedReferences<'_>, id: u64,
        generation: QueryGeneration) -> Result<ReadingTarget, ReferenceNavigationError> {
        references.validate_source(self.bytes(), self.file(), self.revision(), generation)?;
        let encoding = fcb_source::detect_encoding(&self.bytes()[..self.bytes().len().min(3)]);
        if !encoding_compatible(references.encoding(), encoding) {
            return Err(ReferenceNavigationError::EncodingMismatch);
        }
        let candidate = references.candidate(id).ok_or(ReferenceError::NotFound)?;
        Ok(ReadingTarget::Range(candidate.original_range()))
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    use crate::{ArenaOwnerId, BrowserSession, ByteLength, FileId, SourceRevision};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(886).unwrap() }
    fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
    fn view(bytes: Vec<u8>) -> BrowserView {
        let source = SourceCapture::from_bytes(owner(), FileId::new(owner(), 1).unwrap(),
            SourceRevision::new(owner(), 1).unwrap(), "source.rs", bytes).unwrap();
        BrowserSession::new(owner()).open_capture(source).unwrap()
    }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(64 * 1024 * 1024)).unwrap() }
    #[test]
    fn retained_references_activate_exact_reader_ranges() {
        let view = view(b"foo foobar\nfoo".to_vec());
        let budget = budget();
        let references = view.references("foo", ReferenceOptions::new(generation()), &budget,
            ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        assert_eq!(references.candidates().len(), 2);
        assert_eq!(references.source_bytes(2).unwrap(), b"foo");
        assert_eq!(references.candidates()[1].original_range().start().get(), 11);
        let reader = view.source_reader(super::super::ReaderLimits::default(), &budget,
            ResourceAllocationId::new(2).unwrap()).unwrap();
        assert!(reader.seek_reference(&references, 2, generation()).is_ok());
        assert!(matches!(reader.seek_reference(&references, 0, generation()),
            Err(ReferenceNavigationError::Reference(ReferenceError::NotFound))));
        assert!(matches!(reader.seek_reference(&references, 1, QueryGeneration::new(owner(), 2).unwrap()),
            Err(ReferenceNavigationError::Reference(ReferenceError::StaleQuery))));
    }
    #[test]
    fn changed_bytes_with_reused_source_ids_cannot_receive_old_reference() {
        let old = view(b"foo".to_vec());
        let current = view(b"bar".to_vec());
        let budget = budget();
        let references = old.references("foo", ReferenceOptions::new(generation()), &budget,
            ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        let reader = current.source_reader(super::super::ReaderLimits::default(), &budget,
            ResourceAllocationId::new(2).unwrap()).unwrap();
        assert!(matches!(reader.seek_reference(&references, 1, generation()),
            Err(ReferenceNavigationError::Reference(ReferenceError::StaleSource))));
    }
    #[test]
    fn declared_bomless_utf16_reference_requires_compatible_reader() {
        let bytes: Vec<u8> = "foo foo".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let view = view(bytes);
        let budget = budget();
        let mut options = ReferenceOptions::new(generation());
        options.encoding = Some(DetectedEncoding::Utf16Le);
        let references = view.references("foo", options, &budget,
            ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        assert_eq!(references.candidates().len(), 2);
        let reader = view.source_reader(super::super::ReaderLimits::default(), &budget,
            ResourceAllocationId::new(2).unwrap()).unwrap();
        assert!(matches!(reader.seek_reference(&references, 1, generation()), Err(ReferenceNavigationError::EncodingMismatch)));
    }
    #[test]
    fn facade_owns_query_name_and_propagates_cancellation() {
        let view = view(b"foo".to_vec());
        let budget = budget();
        let references = view.references(&String::from("foo"), ReferenceOptions::new(generation()), &budget,
            ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        assert_eq!(references.name(), "foo");
        assert!(matches!(view.references("foo", ReferenceOptions::new(generation()), &budget,
            ResourceAllocationId::new(2).unwrap(), || true), Err(ReferenceError::Canceled)));
    }
}
