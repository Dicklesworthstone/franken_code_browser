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

use std::{io::{Read, Seek}, mem::size_of, ops::Range};
use fcb_search::{SearchMode, UnicodeNormalization};
use fcb_search::normalized::{NormalizedCursor, SourceScalar, matched_text_capacity};
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
                { fcb_document::reader::MAX_HEADING_SEARCH_SOURCE_BYTES }
                #[cfg(not(feature = "markdown"))]
                { 0 }
            }
        }
    }
}

/// A frozen selection of raw FCBS member labels. Only slash separates
/// components. No case folding, text conversion or filesystem lookup occurs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureScope<'a> { Archive, Directory(&'a [u8]), Member(&'a [u8]) }

impl StructureScope<'_> {
    pub fn validate(self) -> Result<(), StructureError> {
        let path = match self { Self::Archive => return Ok(()), Self::Directory(path) | Self::Member(path) => path };
        if path.len() > 16_384 || path.contains(&0)
            || path.split(|&byte| byte == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
            return Err(StructureError::InvalidScope);
        }
        Ok(())
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
    IdentityExhausted, Pending, Canceled, StaleQuery, CapabilityUnavailable, InvalidHit, InvalidScope,
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
            Self::InvalidScope => f.write_str("STRUCTURE_INVALID_SCOPE"),
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
    skip_matches: u64,
    case_sensitive: bool,
    count_all_matches: bool,
    first_unretained: Option<u64>,
    remaining: Range<usize>,
    selected_members: usize,
    membership_complete: bool,
    metadata_limit: usize,
    occurrence_name_bytes: usize,
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
        Self::new_page(archive, needle, options, 0, budget, allocations)
    }
    /// Skip qualified extracted matches, then retain the next bounded window.
    /// Source/extractor budgets remain unchanged; a fresh generation identifies
    /// the new accepted result table over the same verified archive members.
    pub fn new_page(archive: &'archive mut PagedSnapshot<R>, needle: &str, options: StructureOptions,
        skip_matches: u64, budget: &ResourceBudget, allocations: [ResourceAllocationId; 5])
        -> Result<Self, StructureError> {
        Self::new_matching_page(archive, needle, options, skip_matches, true, false, budget, allocations)
    }
    /// Select the name-matching and counting policy without changing legacy
    /// constructors. Insensitive names and whole-token occurrences use Unicode
    /// scalar lowercase, not full case folding or normalization. Each result
    /// retains its original source spelling. Counting visits the admitted saved scope
    /// after the retained window fills; coverage and stored rows stay distinct.
    #[allow(clippy::too_many_arguments)]
    pub fn new_matching_page(archive: &'archive mut PagedSnapshot<R>, needle: &str, options: StructureOptions,
        skip_matches: u64, case_sensitive: bool, count_all_matches: bool,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 5]) -> Result<Self, StructureError> {
        Self::new_scoped_page(archive, needle, options, StructureScope::Archive, skip_matches,
            case_sensitive, count_all_matches, budget, allocations)
    }

    /// Freeze the selected directory or exact member before any extraction,
    /// hit skip or byte quota. The archive's original ordinals remain stable.
    #[allow(clippy::too_many_arguments)]
    pub fn new_scoped_page(archive: &'archive mut PagedSnapshot<R>, needle: &str, options: StructureOptions,
        scope: StructureScope<'_>, skip_matches: u64, case_sensitive: bool, count_all_matches: bool,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 5]) -> Result<Self, StructureError> {
        scope.validate()?;
        let directory = archive.directory();
        let remaining = match scope {
            StructureScope::Archive => 0..directory.len(),
            StructureScope::Directory(prefix) => directory.descendants(prefix),
            StructureScope::Member(path) => directory.find_path(path).map_or(0..0, |n| n..n + 1),
        };
        let selected_members = remaining.len();
        let membership_complete = match scope {
            StructureScope::Member(_) => selected_members == 1,
            _ => directory.discovery_complete(),
        };
        let owner = directory.owner();
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
        if !remaining.is_empty() {
            let last = (remaining.end - 1) as u64;
            options.first_file.get().checked_add(last).ok_or(StructureError::IdentityExhausted)?;
            options.first_revision.get().checked_add(last).ok_or(StructureError::IdentityExhausted)?;
        }
        let metadata_limit = options.max_matches.checked_mul(MAX_STRUCTURE_NAME_BYTES * 2)
            .ok_or(StructureError::ResourceDenied)?.min(MAX_STRUCTURE_METADATA_BYTES);
        // Lowercase equality can match a wider source spelling (K versus K).
        // Each original scalar contributes at least one lowercase scalar and
        // occupies at most four UTF-8 bytes. Admit one reusable source witness
        // independently of retained result metadata before loading any member.
        let occurrence_name_bytes = if options.mode == StructureMode::Occurrences {
            if case_sensitive { needle.len() } else {
                needle.chars().flat_map(char::to_lowercase).count().checked_mul(4)
                    .ok_or(StructureError::ResourceDenied)?
            }
        } else { 0 };
        // Contains reuses the shared incremental matcher one name at a time.
        // Reserve its needle-sized tables/ring and transient growth before any
        // allocation; never build a source-sized lowercase string.
        let matching_charge = if !case_sensitive && options.name_mode == SymbolNameMode::Contains {
            matched_text_capacity(needle, lowercase_mode()).map_err(|_| StructureError::ResourceDenied)?
                .checked_mul(size_of::<char>() + size_of::<usize>() + size_of::<SourceScalar>())
                .and_then(|n| n.checked_add(size_of::<NormalizedCursor>()))
                .ok_or(StructureError::ResourceDenied)?
        } else { 0 };
        let charge = options.max_matches.checked_mul(size_of::<StructureHit>())
            .and_then(|n| n.checked_add(metadata_limit + needle.len() + size_of::<Self>() + size_of::<StructureReport>()))
            .and_then(|n| n.checked_add(matching_charge).and_then(|n| n.checked_add(occurrence_name_bytes)))
            .ok_or(StructureError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(owner, allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| StructureError::ResourceDenied)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(options.max_matches).map_err(|_| StructureError::ResourceDenied)?;
        if hits.capacity() > options.max_matches { return Err(StructureError::ResourceDenied); }
        Ok(Self { archive, options, needle: copy_text(needle)?, allocations, hits, seen: 0, skip_matches,
            case_sensitive, count_all_matches, first_unretained: None, remaining, selected_members, membership_complete,
            metadata_limit, occurrence_name_bytes, state: StructureState::Pending, stats: StructureStats::default(), _lease: lease })
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
        if self.state == StructureState::Pending && self.remaining.is_empty() {
            self.state = StructureState::Complete;
        }
        Ok(self.state)
    }

    fn one_member(&mut self, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
        -> Result<(), StructureError> {
        let Some(ordinal) = self.remaining.next() else { return Ok(()); };
        let member = self.archive.directory().member(ordinal).ok_or(StructureError::InvalidHit)?;
        self.stats.members_visited += 1;
        if matches!(member.data, PagedMemberData::Unavailable(_)) {
            self.stats.unavailable_files += 1; return Ok(());
        }
        let language = SymbolLanguage::from_path(member.path);
        let supported = match self.options.mode {
            StructureMode::Symbols => language.is_some(),
            StructureMode::Headings => is_markdown_path(member.path),
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
                    if !matches_name(symbol.name(), &self.needle, self.options.name_mode, self.case_sensitive, canceled)? {
                        continue;
                    }
                    if !self.append(identity, symbol.name_range().unwrap_or(symbol.original_range()),
                        symbol.original_range(), symbol.name(), symbol.kind().label(), language.name(), symbol.id(), None)? { break; }
                }
            }
            StructureMode::Occurrences => {
                let remaining = self.options.max_matches - self.hits.len();
                let file_skip = self.skip_matches.saturating_sub(self.seen);
                let references = match CapturedReferences::build_matching_window(source.bytes(), request, self.options.generation,
                    &self.needle, None, remaining, file_skip, self.count_all_matches, self.case_sensitive,
                    budget, self.allocations[3], &mut *canceled) {
                    Ok(references) => references,
                    Err(ReferenceError::Canceled) => return Err(StructureError::Canceled),
                    Err(ReferenceError::ResourceDenied) => return Err(StructureError::ResourceDenied),
                    Err(ReferenceError::SourceLimit) => { self.stats.extraction_limited_files += 1; return Ok(()); }
                    Err(_) => { self.stats.unsupported_files += 1; return Ok(()); }
                };
                self.stats.files_searched += 1;
                let skipped = file_skip.min(references.total_matches_counted() as u64);
                self.seen = self.seen.checked_add(skipped).ok_or(StructureError::IdentityExhausted)?;
                let capacity = if references.candidates().is_empty() { 0 } else { self.occurrence_name_bytes };
                let mut source_name = String::new();
                source_name.try_reserve_exact(capacity).map_err(|_| StructureError::ResourceDenied)?;
                if source_name.capacity() > capacity { return Err(StructureError::ResourceDenied); }
                for occurrence in references.candidates() {
                    check(canceled)?;
                    let range = occurrence.original_range();
                    references.write_source_text(occurrence.id(), &mut source_name)?;
                    self.append(identity, range, range, &source_name, "identifier-occurrence", "text", occurrence.id(), None)?;
                }
                let later = (references.total_matches_counted() as u64)
                    .checked_sub(skipped).and_then(|n| n.checked_sub(references.candidates().len() as u64))
                    .ok_or(StructureError::InvalidHit)?;
                if later > 0 {
                    self.first_unretained.get_or_insert(self.seen);
                    self.seen = self.seen.checked_add(later).ok_or(StructureError::IdentityExhausted)?;
                    if !self.count_all_matches { self.state = StructureState::Truncated; }
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
            if !matches_name(&heading.title, &self.needle, self.options.name_mode, self.case_sensitive, canceled)? {
                continue;
            }
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
        if self.seen <= self.skip_matches { return Ok(true); }
        if self.hits.len() == self.options.max_matches {
            self.first_unretained.get_or_insert(self.seen - 1);
            if !self.count_all_matches { self.state = StructureState::Truncated; }
            return Ok(self.count_all_matches);
        }
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
                membership_complete: self.membership_complete, selected_members: self.selected_members, options: self.options,
                needle: self.needle, hits: self.hits, seen: self.seen, skip_matches: self.skip_matches,
                case_sensitive: self.case_sensitive, count_all_matches: self.count_all_matches,
                first_unretained: self.first_unretained, state: self.state, stats: self.stats, _lease: self._lease }),
        }
    }
}

#[derive(Clone, Copy)]
struct MemberIdentity { ordinal: usize, file: FileId, revision: SourceRevision, digest: Sha256Digest, bytes: usize }

/// No archive descriptor, source buffers, parser trees or live paths are retained.
pub struct StructureReport {
    archive: Sha256Digest,
    membership_complete: bool,
    selected_members: usize,
    options: StructureOptions,
    needle: String,
    hits: Vec<StructureHit>,
    seen: u64,
    skip_matches: u64,
    case_sensitive: bool,
    count_all_matches: bool,
    first_unretained: Option<u64>,
    state: StructureState,
    stats: StructureStats,
    _lease: ResourceLease,
}
impl StructureReport {
    /// Number of known members in the frozen scope, independent of work limits.
    pub const fn selected_members(&self) -> usize { self.selected_members }
    /// A known exact member establishes singleton membership even for a
    /// partial archive. An absent requested member is explicitly unfulfilled.
    pub const fn membership_complete(&self) -> bool { self.membership_complete }
    pub fn hits(&self) -> &[StructureHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.seen }
    pub const fn skip_matches(&self) -> u64 { self.skip_matches }
    pub fn skipped_matches(&self) -> u64 { self.skip_matches.min(self.seen) }
    pub const fn case_sensitive(&self) -> bool { self.case_sensitive }
    pub const fn count_all_matches(&self) -> bool { self.count_all_matches }
    /// Later matches omitted by the retained-row cap, independent of coverage.
    /// A skipped prefix alone does not set this flag.
    pub const fn truncated(&self) -> bool { self.first_unretained.is_some() }
    /// Zero-row probes/counts never offer a continuation that cannot progress.
    pub fn has_more_matches(&self) -> bool { self.options.max_matches > 0 && self.truncated() }
    /// Metadata omitted before the retained window fills still consumes match
    /// positions. Counting later matches never moves this first omitted offset.
    pub fn next_match_offset(&self) -> Option<u64> {
        if self.has_more_matches() { self.first_unretained } else { None }
    }
    pub const fn stats(&self) -> StructureStats { self.stats }
    pub const fn mode(&self) -> StructureMode { self.options.mode }
    pub const fn name_mode(&self) -> SymbolNameMode { self.options.name_mode }
    pub fn needle(&self) -> &str { &self.needle }
    pub const fn state(&self) -> StructureState { self.state }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    /// Whether the count covers all admitted extraction work. Row caps and
    /// metadata-only omissions do not make an otherwise complete count inexact.
    /// This never claims compiler semantic completeness.
    pub fn matches_seen_exact(&self) -> bool {
        self.state == StructureState::Complete && self.membership_complete
            && self.stats.unsupported_files == 0 && self.stats.unavailable_files == 0
            && self.stats.source_limited_files == 0 && self.stats.extraction_limited_files == 0
    }
    /// Extraction-processing completeness, never compiler semantic completeness.
    pub fn is_complete(&self) -> bool {
        self.matches_seen_exact() && self.stats.metadata_omitted == 0
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
fn lowercase_mode() -> SearchMode {
    SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact }
}
fn matches_name(name: &str, needle: &str, mode: SymbolNameMode, case_sensitive: bool,
    canceled: &mut impl FnMut() -> bool) -> Result<bool, StructureError> {
    if case_sensitive {
        return Ok(match mode { SymbolNameMode::Exact => name == needle,
            SymbolNameMode::Prefix => name.starts_with(needle), SymbolNameMode::Contains => name.contains(needle) });
    }
    if mode != SymbolNameMode::Contains {
        let mut source = name.chars().flat_map(char::to_lowercase);
        for unit in needle.chars().flat_map(char::to_lowercase) {
            if source.next() != Some(unit) { return Ok(false); }
        }
        return Ok(mode == SymbolNameMode::Prefix || source.next().is_none());
    }
    let mut cursor = NormalizedCursor::new(needle, lowercase_mode())
        .map_err(|_| StructureError::ResourceDenied)?.ok_or(StructureError::InvalidLimits)?;
    for (index, (start, ch)) in name.char_indices().enumerate() {
        if index % 256 == 0 { check(canceled)?; }
        let end = (start + ch.len_utf8()) as u64;
        let scalar = SourceScalar { ch, raw_start: start as u64, raw_end: end,
            decoded_start: start as u64, decoded_end: end };
        for unit in cursor.units(ch) {
            if cursor.push(unit, scalar).is_some() { return Ok(true); }
        }
    }
    Ok(false)
}
/// Filename eligibility for Markdown heading searches over repository membership.
pub fn is_markdown_path(path: &[u8]) -> bool {
    let name = path.rsplit(|&byte| byte == b'/').next().unwrap_or(path);
    let Some(dot) = name.iter().rposition(|&byte| byte == b'.') else { return false; };
    [b"md".as_slice(), b"markdown", b"mdown", b"mkd"].iter().any(|ext| name[dot + 1..].eq_ignore_ascii_case(ext))
}
