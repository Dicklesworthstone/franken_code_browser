#![forbid(unsafe_code)]

//! Structural navigation over immutable saved repositories. The same symbol,
//! reference and FrankenMarkdown engines used by live readers own extraction.
//! This adapter owns archive iteration, admission, coverage and exact witnesses.
//!
//! A step visits at most one member, loading only that member's verified bytes.
//! Extraction is bounded synchronous worker work, with cancellation around the
//! underlying calls; it is not a fixed-duration UI callback. Completed reports
//! retain metadata only. Activating a hit verifies its archive member again and
//! never consults a path from the archived workspace.

use std::{io::{Read, Seek}, mem::size_of};
use fcb_core::{ByteLength, ByteRange, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, ResourceLease, SourceRevision};
use super::{CaptureRequest, SymbolLanguage, SymbolNameMode};
use super::symbols::{CapturedReferences, CapturedSymbols, ReferenceError, SymbolError,
    MAX_REFERENCE_SOURCE_BYTES, MAX_SYMBOL_ITEMS, MAX_SYMBOL_SOURCE_BYTES, validate_reference_name};
use super::paged_snapshot::{PagedCapture, PagedMemberData, PagedSearchError,
    PagedSnapshot, PagedSnapshotError, Sha256Digest};

pub const MAX_STRUCTURE_HITS: usize = 4096;
pub const MAX_STRUCTURE_NEEDLE_BYTES: usize = 1024;
pub const MAX_STRUCTURE_NAME_BYTES: usize = 4096;
pub const MAX_STRUCTURE_METADATA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STRUCTURE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureMode { Symbols, Headings, Occurrences }
impl StructureMode {
    pub const fn requested(self) -> &'static str {
        match self { Self::Symbols => "symbols", Self::Headings => "headings", Self::Occurrences => "occurrences" }
    }
    pub const fn realized(self) -> &'static str {
        match self { Self::Symbols => "heuristic-outline-candidate", Self::Headings => "fmd-heading",
            Self::Occurrences => "whole-token-text-candidate" }
    }
    pub const fn source_limit(self) -> usize {
        match self {
            Self::Symbols => MAX_SYMBOL_SOURCE_BYTES,
            Self::Occurrences => MAX_REFERENCE_SOURCE_BYTES,
            Self::Headings => {
                #[cfg(feature = "markdown")]
                { fcb_document::reader::MAX_DOCUMENT_SOURCE_BYTES }
                #[cfg(not(feature = "markdown"))]
                { 0 }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructureOptions {
    pub generation: QueryGeneration,
    pub first_file: FileId,
    pub first_revision: SourceRevision,
    pub mode: StructureMode,
    pub name_mode: SymbolNameMode,
    /// Zero is an existence probe. One additional match proves truncation.
    pub max_matches: usize,
    /// Aggregate original member bytes loaded, separate from extractor work.
    pub max_source_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureError {
    Search(PagedSearchError), Reference(ReferenceError), InvalidLimits, ResourceDenied,
    IdentityExhausted, Pending, Canceled, StaleQuery, CapabilityUnavailable, InvalidHit,
}
impl StructureError {
    pub fn is_canceled(self) -> bool {
        matches!(self, Self::Canceled | Self::Search(PagedSearchError::Canceled)
            | Self::Search(PagedSearchError::Archive(PagedSnapshotError::Canceled))
            | Self::Reference(ReferenceError::Canceled))
    }
}
impl std::fmt::Display for StructureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Search(error) => write!(f, "{error}"), Self::Reference(error) => write!(f, "{error}"),
            Self::InvalidLimits => f.write_str("STRUCTURE_QUERY_INVALID_LIMITS"),
            Self::ResourceDenied => f.write_str("STRUCTURE_RESOURCE_DENIED"),
            Self::IdentityExhausted => f.write_str("STRUCTURE_IDENTITY_EXHAUSTED"),
            Self::Pending => f.write_str("STRUCTURE_QUERY_PENDING"),
            Self::Canceled => f.write_str("STRUCTURE_CANCELED"),
            Self::StaleQuery => f.write_str("STRUCTURE_STALE_QUERY"),
            Self::CapabilityUnavailable => f.write_str("STRUCTURE_CAPABILITY_UNAVAILABLE"),
            Self::InvalidHit => f.write_str("STRUCTURE_INVALID_HIT"),
        }
    }
}
impl std::error::Error for StructureError {}
impl From<PagedSearchError> for StructureError { fn from(error: PagedSearchError) -> Self { Self::Search(error) } }
impl From<PagedSnapshotError> for StructureError {
    fn from(error: PagedSnapshotError) -> Self { Self::Search(PagedSearchError::Archive(error)) }
}
impl From<ReferenceError> for StructureError { fn from(error: ReferenceError) -> Self { Self::Reference(error) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureState { Pending, Complete, Truncated, WorkLimit, Canceled, Failed(StructureError) }

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StructureStats {
    pub members_visited: usize,
    pub members_loaded: usize,
    /// Files with a successful extractor result, including limited inventories.
    pub files_searched: usize,
    /// Unsupported language, decoding, or unqualified source provenance.
    pub unsupported_files: usize,
    pub unavailable_files: usize,
    pub source_limited_files: usize,
    pub extraction_limited_files: usize,
    pub metadata_omitted: usize,
    pub loaded_bytes: u64,
    pub peak_source_bytes: usize,
    pub metadata_bytes: usize,
}

/// A private-construction witness to an exact extracted occurrence. IDs are
/// response-local; file/revision domains are supplied explicitly by the host.
#[derive(Debug)]
pub struct StructureHit {
    ordinal: usize,
    file: FileId,
    revision: SourceRevision,
    generation: QueryGeneration,
    archive: Sha256Digest,
    source: Sha256Digest,
    range: ByteRange,
    evidence: ByteRange,
    name: String,
    kind: &'static str,
    language: &'static str,
    candidate: u64,
    slug: Option<String>,
}
impl StructureHit {
    pub const fn ordinal(&self) -> usize { self.ordinal }
    pub const fn file(&self) -> FileId { self.file }
    pub const fn revision(&self) -> SourceRevision { self.revision }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn source_digest(&self) -> Sha256Digest { self.source }
    pub const fn original_range(&self) -> ByteRange { self.range }
    pub const fn evidence_range(&self) -> ByteRange { self.evidence }
    pub fn name(&self) -> &str { &self.name }
    pub const fn kind(&self) -> &'static str { self.kind }
    pub const fn language(&self) -> &'static str { self.language }
    pub const fn candidate_id(&self) -> u64 { self.candidate }
    pub fn heading_slug(&self) -> Option<&str> { self.slug.as_deref() }

    /// Verify the original archive and full member content before exposing the
    /// selected range. Retained readers may then outlive this query and archive.
    pub fn open<R: Read + Seek>(&self, archive: &mut PagedSnapshot<R>, generation: QueryGeneration,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2], canceled: impl FnMut() -> bool)
        -> Result<PagedCapture, StructureError> {
        if generation != self.generation || archive.directory().digest() != self.archive {
            return Err(StructureError::StaleQuery);
        }
        let capture = PagedCapture::load(archive, self.ordinal, self.file, self.revision,
            budget, allocations, canceled)?;
        if capture.source_digest() != self.source || self.range.is_empty() || self.evidence.is_empty()
            || self.evidence.start() > self.range.start() || self.evidence.end() < self.range.end()
            || self.evidence.end().get() > capture.bytes().len() as u64 {
            return Err(StructureError::InvalidHit);
        }
        Ok(capture)
    }
}

/// Iterator-like bounded worker operation. No filesystem paths or source root
/// access are created; the archive handle is an explicit caller-owned service.
pub struct StructureQuery<'archive, R: Read + Seek> {
    archive: &'archive mut PagedSnapshot<R>,
    options: StructureOptions,
    needle: String,
    allocations: [ResourceAllocationId; 5],
    hits: Vec<StructureHit>,
    seen: u64,
    next: usize,
    metadata_limit: usize,
    state: StructureState,
    stats: StructureStats,
    _lease: ResourceLease,
}
impl<'archive, R: Read + Seek> StructureQuery<'archive, R> {
    /// allocations = [result metadata, verified member, heading source copy,
    /// extractor, heading provenance]. All leases are distinct; callers must
    /// also keep them disjoint from the archive's retained directory lease.
    pub fn new(archive: &'archive mut PagedSnapshot<R>, needle: &str, options: StructureOptions,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 5]) -> Result<Self, StructureError> {
        let owner = archive.directory().owner();
        if options.generation.owner() != owner || options.first_file.owner() != owner
            || options.first_revision.owner() != owner { return Err(PagedSearchError::OwnerMismatch.into()); }
        if needle.is_empty() || needle.len() > MAX_STRUCTURE_NEEDLE_BYTES || needle.as_bytes().contains(&0)
            || options.max_matches > MAX_STRUCTURE_HITS || options.max_source_bytes > MAX_STRUCTURE_SOURCE_BYTES
            || allocations.iter().enumerate().any(|(i, id)| allocations[..i].contains(id)) {
            return Err(StructureError::InvalidLimits);
        }
        if options.mode == StructureMode::Occurrences {
            if options.name_mode != SymbolNameMode::Exact { return Err(StructureError::InvalidLimits); }
            validate_reference_name(needle)?;
        }
        #[cfg(not(feature = "markdown"))]
        if options.mode == StructureMode::Headings { return Err(StructureError::CapabilityUnavailable); }
        let last = archive.directory().len().saturating_sub(1) as u64;
        options.first_file.get().checked_add(last).ok_or(StructureError::IdentityExhausted)?;
        options.first_revision.get().checked_add(last).ok_or(StructureError::IdentityExhausted)?;
        let metadata_limit = options.max_matches.checked_mul(MAX_STRUCTURE_NAME_BYTES * 2)
            .ok_or(StructureError::ResourceDenied)?.min(MAX_STRUCTURE_METADATA_BYTES);
        let charge = options.max_matches.checked_mul(size_of::<StructureHit>())
            .and_then(|n| n.checked_add(metadata_limit + needle.len() + size_of::<Self>() + size_of::<StructureReport>()))
            .ok_or(StructureError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(owner, allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| StructureError::ResourceDenied)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(options.max_matches).map_err(|_| StructureError::ResourceDenied)?;
        if hits.capacity() > options.max_matches { return Err(StructureError::ResourceDenied); }
        Ok(Self { archive, options, needle: copy_text(needle)?, allocations, hits, seen: 0, next: 0,
            metadata_limit, state: StructureState::Pending, stats: StructureStats::default(), _lease: lease })
    }
    pub const fn state(&self) -> StructureState { self.state }
    pub const fn stats(&self) -> StructureStats { self.stats }
    pub fn cancel(&mut self) { self.state = StructureState::Canceled; }

    pub fn step(&mut self, generation: QueryGeneration, budget: &ResourceBudget,
        mut canceled: impl FnMut() -> bool) -> Result<StructureState, StructureError> {
        if generation != self.options.generation { self.cancel(); return Err(StructureError::StaleQuery); }
        if canceled() { self.cancel(); return Err(StructureError::Canceled); }
        if self.state != StructureState::Pending { return Ok(self.state); }
        if let Err(error) = self.one_member(budget, &mut canceled) {
            self.state = if error.is_canceled() { StructureState::Canceled } else { StructureState::Failed(error) };
            return Err(error);
        }
        if canceled() { self.cancel(); return Err(StructureError::Canceled); }
        if self.state == StructureState::Pending && self.next == self.archive.directory().len() {
            self.state = StructureState::Complete;
        }
        Ok(self.state)
    }

    fn one_member(&mut self, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
        -> Result<(), StructureError> {
        if self.next == self.archive.directory().len() { return Ok(()); }
        let ordinal = self.next;
        let member = self.archive.directory().member(ordinal).ok_or(StructureError::InvalidHit)?;
        self.next += 1; self.stats.members_visited += 1;
        if matches!(member.data, PagedMemberData::Unavailable(_)) {
            self.stats.unavailable_files += 1; return Ok(());
        }
        let language = SymbolLanguage::from_path(member.path);
        let supported = match self.options.mode {
            StructureMode::Symbols => language.is_some(),
            StructureMode::Headings => is_markdown(member.path),
            StructureMode::Occurrences => true,
        };
        if !supported { self.stats.unsupported_files += 1; return Ok(()); }
        if member.observed_bytes > self.options.mode.source_limit() as u64 {
            self.stats.source_limited_files += 1; return Ok(());
        }
        if member.observed_bytes > self.options.max_source_bytes - self.stats.loaded_bytes {
            self.state = StructureState::WorkLimit; return Ok(());
        }
        let owner = self.options.generation.owner();
        let file = FileId::new(owner, self.options.first_file.get() + ordinal as u64)
            .map_err(|_| StructureError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, self.options.first_revision.get() + ordinal as u64)
            .map_err(|_| StructureError::IdentityExhausted)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| StructureError::InvalidHit)?;
        let source = self.archive.load(ordinal, budget, self.allocations[1], &mut *canceled)?;
        self.stats.members_loaded += 1;
        self.stats.loaded_bytes += source.bytes().len() as u64;
        self.stats.peak_source_bytes = self.stats.peak_source_bytes.max(source.bytes().len());
        let identity = MemberIdentity { ordinal, file, revision, digest: source.source_digest(), bytes: source.bytes().len() };
        match self.options.mode {
            StructureMode::Symbols => {
                let language = language.ok_or(StructureError::InvalidHit)?;
                let symbols = match CapturedSymbols::build(source.bytes(), request, self.options.generation,
                    language, None, MAX_SYMBOL_ITEMS, budget, self.allocations[3], &mut *canceled) {
                    Ok(symbols) => symbols,
                    Err(SymbolError::Canceled) => return Err(StructureError::Canceled),
                    Err(SymbolError::ResourceDenied) => return Err(StructureError::ResourceDenied),
                    Err(SymbolError::SourceLimit | SymbolError::ComplexityLimit) => {
                        self.stats.extraction_limited_files += 1; return Ok(());
                    }
                    Err(_) => { self.stats.unsupported_files += 1; return Ok(()); }
                };
                self.stats.files_searched += 1;
                if symbols.output_limited() { self.stats.extraction_limited_files += 1; }
                for symbol in symbols.candidates() {
                    check(canceled)?;
                    if !symbol.matches_name(&self.needle, self.options.name_mode) { continue; }
                    if !self.append(identity, symbol.name_range().unwrap_or(symbol.original_range()),
                        symbol.original_range(), symbol.name(), symbol.kind().label(), language.name(), symbol.id(), None)? { break; }
                }
            }
            StructureMode::Occurrences => {
                let remaining = self.options.max_matches - self.hits.len();
                let references = match CapturedReferences::build(source.bytes(), request, self.options.generation,
                    &self.needle, None, remaining, budget, self.allocations[3], &mut *canceled) {
                    Ok(references) => references,
                    Err(ReferenceError::Canceled) => return Err(StructureError::Canceled),
                    Err(ReferenceError::ResourceDenied) => return Err(StructureError::ResourceDenied),
                    Err(ReferenceError::SourceLimit) => { self.stats.extraction_limited_files += 1; return Ok(()); }
                    Err(_) => { self.stats.unsupported_files += 1; return Ok(()); }
                };
                self.stats.files_searched += 1;
                for occurrence in references.candidates() {
                    check(canceled)?;
                    let range = occurrence.original_range();
                    self.append(identity, range, range, references.name(), "identifier-occurrence", "text", occurrence.id(), None)?;
                }
                if references.output_limited() {
                    self.seen = self.seen.checked_add(1).ok_or(StructureError::IdentityExhausted)?;
                    self.state = StructureState::Truncated;
                }
            }
            StructureMode::Headings => self.headings(identity, request, source.bytes(), budget, canceled)?,
        }
        Ok(())
    }

    #[cfg(feature = "markdown")]
    fn headings(&mut self, identity: MemberIdentity, request: CaptureRequest, bytes: &[u8],
        budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<(), StructureError> {
        use std::sync::Arc;
        use fcb_core::{DocumentGeneration, DocumentId};
        use fcb_document::reader::{DocumentReadOptions, DocumentReader};
        use super::CompleteCapture;
        let owner = request.file().owner();
        let _source_lease = budget.try_reserve_managed(owner, self.allocations[2],
            ByteLength::new((bytes.len() + size_of::<CompleteCapture>() + 64) as u64))
            .map_err(|_| StructureError::ResourceDenied)?;
        let capture = CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
            .map_err(|_| StructureError::InvalidHit)?;
        let options = DocumentReadOptions { max_source_bytes: self.options.mode.source_limit(), ..DocumentReadOptions::default() };
        let prepared = DocumentReader::prepare(&capture,
            DocumentId::new(owner, request.file().get()).map_err(|_| StructureError::IdentityExhausted)?,
            DocumentGeneration::new(owner, self.options.generation.get()).map_err(|_| StructureError::IdentityExhausted)?,
            options, budget, self.allocations[3], &mut *canceled);
        let document = match prepared {
            Ok(document) => document,
            Err(error) => return self.document_failure(error),
        };
        // A bounded parsed heading is not sufficient source-coordinate proof.
        // Qualification rejects upstream spans shifted by earlier blocks.
        if let Err(error) = document.qualify_heading_source_ranges(budget, self.allocations[4], &mut *canceled) {
            return self.document_failure(error);
        }
        self.stats.files_searched += 1;
        for (ordinal, heading) in document.headings().iter().enumerate() {
            check(canceled)?;
            if !matches_name(&heading.title, &self.needle, self.options.name_mode) { continue; }
            let range = document.original_span(heading.source_span).map_err(|_| StructureError::InvalidHit)?;
            if !self.append(identity, range, range, &heading.title, "heading", "markdown",
                ordinal as u64 + 1, Some(&heading.slug))? { break; }
        }
        Ok(())
    }
    #[cfg(feature = "markdown")]
    fn document_failure(&mut self, error: fcb_document::reader::DocumentReadError) -> Result<(), StructureError> {
        use fcb_document::reader::DocumentReadError;
        match error {
            DocumentReadError::Canceled => Err(StructureError::Canceled),
            DocumentReadError::ResourceDenied => Err(StructureError::ResourceDenied),
            DocumentReadError::EngineBudget | DocumentReadError::SourceLimit => {
                self.stats.extraction_limited_files += 1; Ok(())
            }
            _ => { self.stats.unsupported_files += 1; Ok(()) }
        }
    }
    #[cfg(not(feature = "markdown"))]
    fn headings(&mut self, _: MemberIdentity, _: CaptureRequest, _: &[u8], _: &ResourceBudget,
        _: &mut impl FnMut() -> bool) -> Result<(), StructureError> { Err(StructureError::CapabilityUnavailable) }

    #[allow(clippy::too_many_arguments)]
    fn append(&mut self, identity: MemberIdentity, selected: ByteRange, evidence: ByteRange,
        name: &str, kind: &'static str, language: &'static str, candidate: u64, slug: Option<&str>)
        -> Result<bool, StructureError> {
        if selected.is_empty() || evidence.is_empty() || evidence.start() > selected.start()
            || evidence.end() < selected.end() || evidence.end().get() > identity.bytes as u64 || candidate == 0 {
            return Err(StructureError::InvalidHit);
        }
        self.seen = self.seen.checked_add(1).ok_or(StructureError::IdentityExhausted)?;
        if self.hits.len() == self.options.max_matches { self.state = StructureState::Truncated; return Ok(false); }
        let bytes = name.len().checked_add(slug.map_or(0, str::len)).ok_or(StructureError::ResourceDenied)?;
        if name.is_empty() || name.len() > MAX_STRUCTURE_NAME_BYTES
            || slug.is_some_and(|s| s.is_empty() || s.len() > MAX_STRUCTURE_NAME_BYTES)
            || bytes > self.metadata_limit - self.stats.metadata_bytes {
            self.stats.metadata_omitted += 1; return Ok(true);
        }
        let row = StructureHit { ordinal: identity.ordinal, file: identity.file, revision: identity.revision,
            generation: self.options.generation, archive: self.archive.directory().digest(), source: identity.digest,
            range: selected, evidence, name: copy_text(name)?, kind, language, candidate,
            slug: slug.map(copy_text).transpose()? };
        self.stats.metadata_bytes += bytes;
        self.hits.push(row);
        Ok(true)
    }

    pub fn finish(self) -> Result<StructureReport, StructureError> {
        match self.state {
            StructureState::Pending => Err(StructureError::Pending),
            StructureState::Canceled => Err(StructureError::Canceled),
            StructureState::Failed(error) => Err(error),
            _ => Ok(StructureReport { archive: self.archive.directory().digest(),
                discovery_complete: self.archive.directory().discovery_complete(), options: self.options,
                needle: self.needle, hits: self.hits, seen: self.seen, state: self.state,
                stats: self.stats, _lease: self._lease }),
        }
    }
}

#[derive(Clone, Copy)]
struct MemberIdentity { ordinal: usize, file: FileId, revision: SourceRevision, digest: Sha256Digest, bytes: usize }

/// No archive descriptor, source buffers, parser trees or live paths are retained.
pub struct StructureReport {
    archive: Sha256Digest,
    discovery_complete: bool,
    options: StructureOptions,
    needle: String,
    hits: Vec<StructureHit>,
    seen: u64,
    state: StructureState,
    stats: StructureStats,
    _lease: ResourceLease,
}
impl StructureReport {
    pub fn hits(&self) -> &[StructureHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.seen }
    pub const fn stats(&self) -> StructureStats { self.stats }
    pub const fn mode(&self) -> StructureMode { self.options.mode }
    pub const fn name_mode(&self) -> SymbolNameMode { self.options.name_mode }
    pub fn needle(&self) -> &str { &self.needle }
    pub const fn state(&self) -> StructureState { self.state }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    /// Extraction-processing completeness, never compiler semantic completeness.
    pub fn is_complete(&self) -> bool {
        self.state == StructureState::Complete && self.discovery_complete
            && self.stats.unsupported_files == 0 && self.stats.unavailable_files == 0
            && self.stats.source_limited_files == 0 && self.stats.extraction_limited_files == 0
            && self.stats.metadata_omitted == 0
    }
    pub fn validate_delivery(&self, archive: Sha256Digest, generation: QueryGeneration) -> Result<(), StructureError> {
        if archive != self.archive || generation != self.options.generation { return Err(StructureError::StaleQuery); }
        Ok(())
    }
}

fn copy_text(text: &str) -> Result<String, StructureError> {
    let mut out = String::new();
    out.try_reserve_exact(text.len()).map_err(|_| StructureError::ResourceDenied)?;
    if out.capacity() > text.len() { return Err(StructureError::ResourceDenied); }
    out.push_str(text); Ok(out)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), StructureError> {
    if canceled() { Err(StructureError::Canceled) } else { Ok(()) }
}
#[cfg(feature = "markdown")]
fn matches_name(name: &str, needle: &str, mode: SymbolNameMode) -> bool {
    match mode { SymbolNameMode::Exact => name == needle, SymbolNameMode::Prefix => name.starts_with(needle),
        SymbolNameMode::Contains => name.contains(needle) }
}
fn is_markdown(path: &[u8]) -> bool {
    let name = path.rsplit(|&byte| byte == b'/').next().unwrap_or(path);
    let Some(dot) = name.iter().rposition(|&byte| byte == b'.') else { return false; };
    [b"md".as_slice(), b"markdown", b"mdown", b"mkd"].iter().any(|ext| name[dot + 1..].eq_ignore_ascii_case(ext))
}
