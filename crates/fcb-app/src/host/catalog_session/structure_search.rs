#![forbid(unsafe_code)]

//! Qualified names over a progressively discovered project, independent of the
//! atlas's retained file prefix. One worker step admits metadata or examines one
//! bounded complete capture. Existing public symbol and document engines own all
//! parsing, decoding and source mapping.
//!
//! Only counters grow with visited files. Retained rows, complete source
//! witnesses, paths and diagnostics have fixed bounds. A completed traversal is
//! an observation, never an atomic workspace snapshot or compiler completeness.

mod wire;
#[cfg(all(test, unix))]
mod tests;

use std::{mem::size_of, path::Path, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, ByteRange, FileId, QueryGeneration, SourceRevision};
use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader, MAX_HEADING_SEARCH_SOURCE_BYTES};
use fcb::search::{CapturedSymbols, CompleteCapture, SymbolError, SymbolLanguage,
    SearchMode, SymbolNameMode, UnicodeNormalization, MAX_SYMBOL_ITEMS, MAX_SYMBOL_SOURCE_BYTES};
use fcb::search::normalized::{NormalizedCursor, SourceScalar, matched_text_capacity};
use fcb::source::{CaptureRequest, NormalizedPath, RootGrant, SourceError};
use fcb::store::{Sha256, Sha256Digest};
use fcb_core::{DocumentGeneration, DocumentId, ResourceLease};
use crate::{AppError, workspace};
use crate::output::OutputError;
use super::{CatalogDiscoveryPolicy, CatalogSession, CatalogSessionError};
use super::super::{HostResponse, atlas_search::{AtlasSearchError, AtlasSearchScope},
    reader::{ReaderSession, ReaderSessionError}};

pub const MAX_CATALOG_STRUCTURE_HITS: usize = 256;
pub const MAX_CATALOG_STRUCTURE_PAGE: usize = 32;
pub const MAX_CATALOG_STRUCTURE_STEPS: usize = 16;
pub const MAX_CATALOG_STRUCTURE_QUERY_BYTES: usize = 1024;
pub const MAX_CATALOG_STRUCTURE_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_CATALOG_STRUCTURE_READ_CALLS: u64 = 1_048_576;
pub const MAX_CATALOG_STRUCTURE_RETAINED_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_NAME_BYTES: usize = 4096;
const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 32;
// Includes <=32 MiB retained captures, <=256 bounded raw paths (including their
// component storage), <=2 MiB retained name/slug data, one staged file/result
// batch, hashing and capture-copy overlap, and vector/string bookkeeping.
// Capture-reader, extractor and output leases come from this SAME catalog
// budget separately. Visiting another file never increases this reservation.
const STATE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogStructureSearchMode { Symbols, Headings }
impl CatalogStructureSearchMode {
    pub const fn requested(self) -> &'static str {
        match self { Self::Symbols => "symbols", Self::Headings => "headings" }
    }
    pub const fn realized(self) -> &'static str {
        match self { Self::Symbols => "heuristic-outline-candidate", Self::Headings => "fmd-heading" }
    }
    pub const fn source_limit(self) -> usize {
        match self { Self::Symbols => MAX_SYMBOL_SOURCE_BYTES, Self::Headings => MAX_HEADING_SEARCH_SOURCE_BYTES }
    }
}

/// Matching uses extracted names/titles with the scope's immutable case policy.
/// Case-insensitive matching uses Unicode scalar lowercase, without full case
/// folding, canonical normalization or regex interpretation. Original source
/// ranges and spelling are preserved. Every query counts through its I/O bounds
/// even after row storage fills. start_after selects a bounded ordinal window;
/// another window starts a new observation on a new session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogStructureSearchOptions {
    pub mode: CatalogStructureSearchMode,
    pub name_mode: SymbolNameMode,
    pub scope: AtlasSearchScope,
    pub max_matches: usize,
    pub start_after: u64,
    pub max_scan_bytes: u64,
    pub max_read_calls: u64,
}
impl CatalogStructureSearchOptions {
    pub fn new(mode: CatalogStructureSearchMode) -> Self {
        Self { mode, name_mode: SymbolNameMode::Contains, scope: AtlasSearchScope::all(),
            max_matches: MAX_CATALOG_STRUCTURE_HITS, start_after: 0,
            max_scan_bytes: 256 * 1024 * 1024, max_read_calls: MAX_CATALOG_STRUCTURE_READ_CALLS }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogStructureSearchError {
    Catalog(CatalogSessionError), App(AppError), Scope(AtlasSearchError), Reader(ReaderSessionError),
    InvalidLimits, StaleQuery, StaleRevision, MissingHit, NoMoreResults, Canceled, IdentityExhausted,
}
impl CatalogStructureSearchError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Scope(AtlasSearchError::Canceled) => true,
            Self::Catalog(error) => error.is_canceled(), Self::App(error) => error.is_canceled(),
            Self::Reader(error) => CatalogSessionError::Reader(error).is_canceled(), _ => false,
        }
    }
}
impl std::fmt::Display for CatalogStructureSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog(error) => write!(f, "{error}"), Self::App(error) => write!(f, "{error}"),
            Self::Scope(error) => write!(f, "{error}"), Self::Reader(error) => write!(f, "{error}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "CATALOG_STRUCTURE_INVALID_LIMITS",
                Self::StaleQuery => "CATALOG_STRUCTURE_STALE_QUERY",
                Self::StaleRevision => "CATALOG_STRUCTURE_STALE_REVISION",
                Self::MissingHit => "CATALOG_STRUCTURE_MISSING_HIT",
                Self::NoMoreResults => "CATALOG_STRUCTURE_NO_MORE_RESULTS",
                Self::Canceled => "CATALOG_STRUCTURE_CANCELED",
                Self::IdentityExhausted => "CATALOG_STRUCTURE_IDENTITY_EXHAUSTED",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for CatalogStructureSearchError {}
impl From<CatalogSessionError> for CatalogStructureSearchError { fn from(e: CatalogSessionError) -> Self { Self::Catalog(e) } }
impl From<AppError> for CatalogStructureSearchError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<AtlasSearchError> for CatalogStructureSearchError { fn from(e: AtlasSearchError) -> Self { Self::Scope(e) } }
impl From<ReaderSessionError> for CatalogStructureSearchError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for CatalogStructureSearchError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

struct Candidate { file: FileId, path: NormalizedPath, observed_bytes: u64 }
struct RetainedSource { path: NormalizedPath, capture: CompleteCapture, digest: Sha256Digest }
struct Row {
    occurrence: u64, candidate_id: u64, selected: ByteRange, evidence: ByteRange,
    name: String, kind: &'static str, language: &'static str, heading_slug: Option<String>,
}
struct Hit { source: Arc<RetainedSource>, row: Row }
#[derive(Clone, Copy)]
struct Diagnostic { file: FileId, code: &'static str }
#[derive(Clone, Copy)]
enum Problem { Unsupported, Oversized, Unavailable, AnalysisLimited }
#[derive(Clone, Copy)]
enum Stop { ByteLimit, CallLimit, UserStopped, DiscoveryUnavailable, WorkFailed }
impl Stop {
    const fn code(self) -> &'static str {
        match self { Self::ByteLimit => "source-byte-limit", Self::CallLimit => "read-call-limit",
            Self::UserStopped => "user-stopped", Self::DiscoveryUnavailable => "discovery-unavailable",
            Self::WorkFailed => "work-failed" }
    }
}
struct Extracted {
    rows: Vec<Row>, matches: u64, window_matches: u64, metadata_bytes: usize,
    metadata_omitted: u64, limited: Option<&'static str>,
}

pub struct CatalogStructureSearchSession {
    catalog: CatalogSession,
    generation: QueryGeneration,
    options: CatalogStructureSearchOptions,
    needle: String,
    revision: u64,
    replay_after_revision: Option<u64>,
    next_entry: usize,
    candidate: Option<Candidate>,
    hits: Vec<Hit>,
    diagnostics: Vec<Diagnostic>,
    eligible: u64, excluded: u64, region_catalogued: u64,
    examined: u64, scanned: u64, incomplete: u64,
    unsupported: u64, oversized: u64, unavailable: u64, analysis_limited: u64,
    matches_seen: u64, matching_files: u64, retained_matching_files: u64,
    retained_source_bytes: usize, metadata_bytes: usize, metadata_omitted: u64,
    source_retention_omitted: u64,
    io: workspace::IoCounts,
    stop_reason: Option<Stop>,
    discovery_error: Option<CatalogSessionError>,
    work_error: Option<CatalogStructureSearchError>,
    canceled: bool,
    _lease: ResourceLease,
}
impl CatalogStructureSearchSession {
    pub fn open(owner: ArenaOwnerId, root: &Path, generation: u64, query: &str,
        options: CatalogStructureSearchOptions, canceled: impl FnMut() -> bool)
        -> Result<Self, CatalogStructureSearchError> {
        Self::open_with_policy(owner, root, generation, query, options,
            CatalogDiscoveryPolicy::RepositoryRules, canceled)
    }
    /// Open only fixes the immutable query and first discovery batch. Source
    /// capture begins on advance; no retained atlas or full file manifest exists.
    pub fn open_with_policy(owner: ArenaOwnerId, root: &Path, generation: u64, query: &str,
        options: CatalogStructureSearchOptions, policy: CatalogDiscoveryPolicy,
        mut canceled: impl FnMut() -> bool) -> Result<Self, CatalogStructureSearchError> {
        if query.is_empty() || query.len() > MAX_CATALOG_STRUCTURE_QUERY_BYTES || query.as_bytes().contains(&0)
            || !(1..=MAX_CATALOG_STRUCTURE_HITS).contains(&options.max_matches)
            || !(1..=MAX_CATALOG_STRUCTURE_SCAN_BYTES).contains(&options.max_scan_bytes)
            || !(1..=MAX_CATALOG_STRUCTURE_READ_CALLS).contains(&options.max_read_calls)
            || options.start_after.checked_add(options.max_matches as u64).and_then(|n| n.checked_add(1)).is_none() {
            return Err(CatalogStructureSearchError::InvalidLimits);
        }
        check(&mut canceled)?;
        let generation = QueryGeneration::new(owner, generation).map_err(|_| CatalogStructureSearchError::InvalidLimits)?;
        let mut catalog = CatalogSession::open_with_policy(owner, root, policy, &mut canceled)?;
        let id = catalog.next_id()?;
        // Contains uses the shared normalized cursor with needle-sized tables
        // and source-scalar provenance. Admit that scratch before allocation;
        // lowercasing never creates another copy of a complete source/name.
        let matching_charge = if !options.scope.case_sensitive() && options.name_mode == SymbolNameMode::Contains {
            matched_text_capacity(query, lowercase_mode()).map_err(|_| AppError::Admission)?
                .checked_mul(size_of::<char>() + size_of::<usize>() + size_of::<SourceScalar>())
                .and_then(|bytes| bytes.checked_add(size_of::<NormalizedCursor>()))
                .ok_or(AppError::Admission)?
        } else { 0 };
        let state_charge = STATE_BYTES.checked_add(size_of::<Self>() as u64)
            .and_then(|bytes| bytes.checked_add(matching_charge as u64)).ok_or(AppError::Admission)?;
        let lease = catalog.budget.try_reserve_managed(owner, id,
            ByteLength::new(state_charge)).map_err(|_| AppError::Admission)?;
        let hits = reserve(options.max_matches)?;
        let diagnostics = reserve(MAX_DIAGNOSTICS)?;
        let needle = query.to_owned();
        check(&mut canceled)?; catalog.validate_active()?;
        Ok(Self { catalog, generation, options, needle, revision: 1, replay_after_revision: None,
            next_entry: 0, candidate: None, hits, diagnostics, eligible: 0, excluded: 0,
            region_catalogued: 0, examined: 0, scanned: 0, incomplete: 0, unsupported: 0,
            oversized: 0, unavailable: 0, analysis_limited: 0, matches_seen: 0, matching_files: 0,
            retained_matching_files: 0, retained_source_bytes: 0, metadata_bytes: 0,
            metadata_omitted: 0, source_retention_omitted: 0, io: workspace::IoCounts::default(),
            stop_reason: None, discovery_error: None, work_error: None, canceled: false, _lease: lease })
    }
    pub fn owner(&self) -> ArenaOwnerId { self.catalog.owner() }
    pub fn grant(&self) -> &RootGrant { self.catalog.grant() }
    pub fn validate_active(&self) -> Result<(), CatalogStructureSearchError> { Ok(self.catalog.validate_active()?) }
    pub fn info(&mut self, canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogStructureSearchError> {
        self.snapshot("info", 0, MAX_CATALOG_STRUCTURE_PAGE, canceled)
    }
    pub fn page(&mut self, generation: u64, revision: u64, start: usize, count: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogStructureSearchError> {
        self.validate_revision(generation, revision)?; self.snapshot("page", start, count, canceled)
    }
    pub fn hit(&mut self, generation: u64, revision: u64, position: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogStructureSearchError> {
        self.validate_revision(generation, revision)?; self.witness(position, canceled)
    }
    /// The current revision authorizes work. Retrying its accepted predecessor
    /// replays progress without further discovery, capture, counting or parsing.
    /// Each file publishes its counts and rows together after qualification.
    pub fn advance(&mut self, generation: u64, after_revision: u64, max_steps: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogStructureSearchError> {
        self.advance_work(generation, after_revision, max_steps, &mut canceled)?;
        self.snapshot("advance", 0, MAX_CATALOG_STRUCTURE_PAGE, canceled)
    }
    fn advance_work(&mut self, generation: u64, after_revision: u64, max_steps: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogStructureSearchError> {
        self.validate_generation(generation)?;
        if !(1..=MAX_CATALOG_STRUCTURE_STEPS).contains(&max_steps) { return Err(CatalogStructureSearchError::InvalidLimits); }
        if self.replay_after_revision == Some(after_revision) && after_revision < self.revision { return Ok(()); }
        if after_revision != self.revision { return Err(CatalogStructureSearchError::StaleRevision); }
        if !self.more() { return Err(CatalogStructureSearchError::NoMoreResults); }
        check(canceled)?; self.validate_active()?;
        self.revision = add(self.revision, 1)?;
        self.replay_after_revision = Some(after_revision);
        let result: Result<(), CatalogStructureSearchError> = (|| {
            for _ in 0..max_steps {
                check(canceled)?; self.validate_active()?;
                if !self.more() { break; }
                self.step(canceled)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.candidate = None;
            if error.is_canceled() { self.canceled = true; }
            else { self.stop_reason = Some(Stop::WorkFailed); self.work_error = Some(error); }
        }
        result
    }
    pub fn stop(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, CatalogStructureSearchError> {
        self.validate_generation(generation)?; check(&mut canceled)?; self.validate_active()?;
        if self.more() {
            self.revision = add(self.revision, 1)?;
            self.replay_after_revision = None;
            self.candidate = None;
            self.stop_reason = Some(Stop::UserStopped);
        }
        self.snapshot("stop", 0, MAX_CATALOG_STRUCTURE_PAGE, canceled)
    }
    fn validate_generation(&self, generation: u64) -> Result<(), CatalogStructureSearchError> {
        if generation != self.generation.get() { Err(CatalogStructureSearchError::StaleQuery) } else { Ok(()) }
    }
    fn validate_revision(&self, generation: u64, revision: u64) -> Result<(), CatalogStructureSearchError> {
        self.validate_generation(generation)?;
        if revision != self.revision { Err(CatalogStructureSearchError::StaleRevision) } else { Ok(()) }
    }
    fn pending_entries(&self) -> bool {
        self.catalog.pages.back().is_some_and(|page| self.next_entry < page.files.len())
    }
    fn more(&self) -> bool {
        !self.canceled && self.stop_reason.is_none()
            && (self.candidate.is_some() || self.pending_entries() || self.catalog.discovery.is_some())
    }
    fn complete(&self) -> bool {
        !self.canceled && !self.more() && self.stop_reason.is_none() && self.catalog.complete
            && self.incomplete == 0 && self.examined == self.eligible
            && self.options.scope.region_files().is_none_or(|files| files as u64 == self.region_catalogued)
    }
    fn step(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogStructureSearchError> {
        if self.candidate.is_some() { return self.examine(canceled); }
        if !self.pending_entries() {
            if self.catalog.discovery.is_some() {
                let consumed = self.catalog.consume_page(canceled);
                self.next_entry = 0;
                if let Err(error) = consumed {
                    if error.is_canceled() { return Err(error.into()); }
                    self.stop_reason = Some(Stop::DiscoveryUnavailable); self.discovery_error = Some(error);
                }
            }
            return Ok(());
        }
        let page = self.catalog.pages.back().ok_or(CatalogStructureSearchError::InvalidLimits)?;
        let file = page.files.get(self.next_entry).ok_or(CatalogStructureSearchError::InvalidLimits)?;
        let in_region = self.options.scope.region_includes_path(file.path.as_bytes());
        let admitted = self.options.scope.includes_path(file.path.as_bytes());
        let candidate = admitted.then(|| Candidate { file: file.id, path: file.path.clone(), observed_bytes: file.observed_bytes });
        if in_region { self.region_catalogued = add(self.region_catalogued, 1)?; }
        if admitted { self.eligible = add(self.eligible, 1)?; } else { self.excluded = add(self.excluded, 1)?; }
        self.next_entry += 1;
        self.candidate = candidate;
        Ok(())
    }
    fn examine(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogStructureSearchError> {
        let candidate = self.candidate.take().ok_or(CatalogStructureSearchError::InvalidLimits)?;
        let language = SymbolLanguage::from_path(candidate.path.as_bytes());
        let supported = match self.options.mode {
            CatalogStructureSearchMode::Symbols => language.is_some(),
            CatalogStructureSearchMode::Headings => is_markdown(candidate.path.as_bytes()),
        };
        if !supported { return self.problem(candidate.file, Problem::Unsupported, "STRUCTURE_UNSUPPORTED_LANGUAGE"); }
        let source_limit = self.options.mode.source_limit();
        if candidate.observed_bytes > source_limit as u64 {
            return self.problem(candidate.file, Problem::Oversized, "STRUCTURE_FILE_BYTE_LIMIT");
        }
        if self.io.bytes >= self.options.max_scan_bytes {
            self.stop_reason = Some(Stop::ByteLimit); return Ok(());
        }
        if self.io.calls >= self.options.max_read_calls {
            self.stop_reason = Some(Stop::CallLimit); return Ok(());
        }
        let owner = self.owner();
        let revision = SourceRevision::new(owner, self.generation.get()).map_err(|_| CatalogStructureSearchError::IdentityExhausted)?;
        let request = CaptureRequest::new(candidate.file, revision).map_err(CatalogSessionError::from)?;
        let root = self.grant().root_path().to_path_buf();
        let allocation = self.catalog.next_id()?;
        let catalog = &self.catalog;
        let captured = workspace::read_capture_with_call_limit(&root, request, &candidate.path,
            source_limit, self.options.max_scan_bytes, self.options.max_read_calls,
            &mut self.io, &catalog.budget, allocation,
            &mut || canceled() || catalog.validate_active().is_err());
        self.validate_active()?; check(canceled)?;
        let capture = match captured {
            Ok(capture) => capture,
            Err(SourceError::Canceled) => return Err(CatalogStructureSearchError::Canceled),
            Err(SourceError::PayloadTooLarge) if self.options.max_scan_bytes - self.io.bytes < source_limit as u64 => {
                self.stop_reason = Some(Stop::ByteLimit); return Ok(());
            }
            Err(SourceError::PayloadTooLarge) => return self.problem(candidate.file, Problem::Oversized, "STRUCTURE_FILE_BYTE_LIMIT"),
            Err(_) if self.io.calls >= self.options.max_read_calls => {
                self.stop_reason = Some(Stop::CallLimit); return Ok(());
            }
            Err(error) => return self.problem(candidate.file, Problem::Unavailable, error.code()),
        };
        let can_retain_source = capture.bytes().len()
            <= MAX_CATALOG_STRUCTURE_RETAINED_SOURCE_BYTES - self.retained_source_bytes;
        let remaining = if can_retain_source { self.options.max_matches - self.hits.len() } else { 0 };
        let mut extracted = Extracted { rows: reserve(remaining)?, matches: 0, window_matches: 0,
            metadata_bytes: 0, metadata_omitted: 0, limited: None };
        let extract_id = self.catalog.next_id()?;
        let qualification_id = self.catalog.next_id()?;
        let catalog = &self.catalog;
        let extraction = match self.options.mode {
            CatalogStructureSearchMode::Symbols => {
                let language = language.ok_or(CatalogStructureSearchError::InvalidLimits)?;
                match CapturedSymbols::build(capture.bytes(), *capture.request(), self.generation, language,
                    None, MAX_SYMBOL_ITEMS, &catalog.budget, extract_id,
                    || canceled() || catalog.validate_active().is_err()) {
                    Ok(symbols) => {
                        if symbols.output_limited() { extracted.limited = Some("STRUCTURE_EXTRACTION_ITEM_LIMIT"); }
                        for symbol in symbols.candidates() {
                            check(canceled)?;
                            if !matches_name(symbol.name(), &self.needle, self.options.name_mode,
                                self.options.scope.case_sensitive(), canceled)? { continue; }
                            self.stage_row(&mut extracted, remaining, capture.bytes().len(), symbol.id(),
                                symbol.name_range().unwrap_or(symbol.original_range()), symbol.original_range(),
                                symbol.name(), symbol.kind().label(), language.name(), None)?;
                        }
                        Ok(())
                    }
                    Err(SymbolError::Canceled) => return Err(CatalogStructureSearchError::Canceled),
                    Err(error) => Err((if matches!(error, SymbolError::SourceLimit | SymbolError::ComplexityLimit
                        | SymbolError::ResourceDenied) { Problem::AnalysisLimited } else { Problem::Unsupported }, error.code())),
                }
            }
            CatalogStructureSearchMode::Headings => {
                let document_id = DocumentId::new(owner, candidate.file.get()).map_err(|_| CatalogStructureSearchError::IdentityExhausted)?;
                let document_generation = DocumentGeneration::new(owner, self.generation.get()).map_err(|_| CatalogStructureSearchError::IdentityExhausted)?;
                match DocumentReader::prepare(&capture, document_id, document_generation,
                    DocumentReadOptions { max_source_bytes: source_limit, ..DocumentReadOptions::default() },
                    &catalog.budget, extract_id, || canceled() || catalog.validate_active().is_err()) {
                    Ok(document) => {
                        match document.qualify_heading_source_ranges(&catalog.budget, qualification_id,
                            || canceled() || catalog.validate_active().is_err()) {
                            Ok(()) => {
                                for (ordinal, heading) in document.headings().iter().enumerate() {
                                    check(canceled)?;
                                    if !matches_name(&heading.title, &self.needle, self.options.name_mode,
                                        self.options.scope.case_sensitive(), canceled)? { continue; }
                                    let range = document.original_span(heading.source_span)
                                        .map_err(|_| CatalogStructureSearchError::InvalidLimits)?;
                                    self.stage_row(&mut extracted, remaining, capture.bytes().len(), ordinal as u64 + 1,
                                        range, range, &heading.title, "heading", "markdown", Some(&heading.slug))?;
                                }
                                Ok(())
                            }
                            Err(DocumentReadError::Canceled) => return Err(CatalogStructureSearchError::Canceled),
                            Err(error) => Err((document_problem(error), error.code())),
                        }
                    }
                    Err(DocumentReadError::Canceled) => return Err(CatalogStructureSearchError::Canceled),
                    Err(error) => Err((document_problem(error), error.code())),
                }
            }
        };
        self.validate_active()?; check(canceled)?;
        if let Err((problem, code)) = extraction { return self.problem(candidate.file, problem, code); }
        let source = if extracted.rows.is_empty() { None } else {
            let mut hash = Sha256::new();
            for chunk in capture.bytes().chunks(64 * 1024) { check(canceled)?; hash.update(chunk); }
            Some(Arc::new(RetainedSource { path: candidate.path, capture, digest: hash.finalize() }))
        };
        self.validate_active()?; check(canceled)?;
        // Only this acceptance section changes counts/retained rows. No source
        // operation or cancellation poll can split a file's publication.
        self.examined = add(self.examined, 1)?;
        self.scanned = add(self.scanned, 1)?;
        self.matches_seen = add(self.matches_seen, extracted.matches)?;
        if extracted.matches != 0 { self.matching_files = add(self.matching_files, 1)?; }
        self.metadata_omitted = add(self.metadata_omitted, extracted.metadata_omitted)?;
        self.metadata_bytes += extracted.metadata_bytes;
        if !can_retain_source && extracted.window_matches != 0 && self.hits.len() < self.options.max_matches {
            self.source_retention_omitted = add(self.source_retention_omitted, 1)?;
        }
        if let Some(code) = extracted.limited { self.record_incomplete(candidate.file, Problem::AnalysisLimited, code)?; }
        if let Some(source) = source {
            self.retained_matching_files = add(self.retained_matching_files, 1)?;
            self.retained_source_bytes += source.capture.bytes().len();
            for row in extracted.rows { self.hits.push(Hit { source: Arc::clone(&source), row }); }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn stage_row(&self, result: &mut Extracted, remaining: usize, source_bytes: usize,
        candidate_id: u64, selected: ByteRange, evidence: ByteRange, name: &str, kind: &'static str,
        language: &'static str, heading_slug: Option<&str>) -> Result<(), CatalogStructureSearchError> {
        result.matches = add(result.matches, 1)?;
        let occurrence = add(self.matches_seen, result.matches)?;
        if occurrence <= self.options.start_after { return Ok(()); }
        result.window_matches = add(result.window_matches, 1)?;
        if result.rows.len() == remaining { return Ok(()); }
        let bytes = name.len().checked_add(heading_slug.map_or(0, str::len)).ok_or(AppError::Admission)?;
        if name.len() > MAX_NAME_BYTES || heading_slug.is_some_and(|slug| slug.len() > MAX_NAME_BYTES)
            || bytes > MAX_METADATA_BYTES - self.metadata_bytes - result.metadata_bytes {
            result.metadata_omitted = add(result.metadata_omitted, 1)?; return Ok(());
        }
        if selected.end().get() > source_bytes as u64 || evidence.end().get() > source_bytes as u64
            || selected.start().get() < evidence.start().get() || selected.end().get() > evidence.end().get() {
            return Err(CatalogStructureSearchError::InvalidLimits);
        }
        result.metadata_bytes += bytes;
        result.rows.push(Row { occurrence, candidate_id, selected, evidence, name: name.to_owned(),
            kind, language, heading_slug: heading_slug.map(str::to_owned) });
        Ok(())
    }
    fn problem(&mut self, file: FileId, problem: Problem, code: &'static str) -> Result<(), CatalogStructureSearchError> {
        self.examined = add(self.examined, 1)?; self.record_incomplete(file, problem, code)
    }
    fn record_incomplete(&mut self, file: FileId, problem: Problem, code: &'static str) -> Result<(), CatalogStructureSearchError> {
        self.incomplete = add(self.incomplete, 1)?;
        let counter = match problem { Problem::Unsupported => &mut self.unsupported, Problem::Oversized => &mut self.oversized,
            Problem::Unavailable => &mut self.unavailable, Problem::AnalysisLimited => &mut self.analysis_limited };
        *counter = add(*counter, 1)?;
        if self.diagnostics.len() < MAX_DIAGNOSTICS { self.diagnostics.push(Diagnostic { file, code }); }
        Ok(())
    }
}
fn document_problem(error: DocumentReadError) -> Problem {
    match error {
        DocumentReadError::EngineBudget | DocumentReadError::ResourceDenied | DocumentReadError::SourceLimit => Problem::AnalysisLimited,
        _ => Problem::Unsupported,
    }
}
fn lowercase_mode() -> SearchMode {
    SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Exact }
}
fn matches_name(name: &str, needle: &str, mode: SymbolNameMode, case_sensitive: bool,
    canceled: &mut impl FnMut() -> bool) -> Result<bool, CatalogStructureSearchError> {
    check(canceled)?;
    if case_sensitive {
        return Ok(match mode { SymbolNameMode::Exact => name == needle,
            SymbolNameMode::Prefix => name.starts_with(needle), SymbolNameMode::Contains => name.contains(needle) });
    }
    if mode != SymbolNameMode::Contains {
        let mut source = name.chars().flat_map(char::to_lowercase);
        for (index, unit) in needle.chars().flat_map(char::to_lowercase).enumerate() {
            if index % 256 == 0 { check(canceled)?; }
            if source.next() != Some(unit) { return Ok(false); }
        }
        return Ok(mode == SymbolNameMode::Prefix || source.next().is_none());
    }
    let mut cursor = NormalizedCursor::new(needle, lowercase_mode())
        .map_err(|_| AppError::Admission)?.ok_or(CatalogStructureSearchError::InvalidLimits)?;
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
fn is_markdown(path: &[u8]) -> bool {
    let basename = path.rsplit(|&byte| byte == b'/').next().unwrap_or(path);
    let Some(dot) = basename.iter().rposition(|&byte| byte == b'.') else { return false; };
    [b"md".as_slice(), b"markdown", b"mdown", b"mkd"].iter()
        .any(|extension| basename[dot + 1..].eq_ignore_ascii_case(extension))
}
fn add(left: u64, right: u64) -> Result<u64, CatalogStructureSearchError> {
    left.checked_add(right).ok_or(CatalogStructureSearchError::IdentityExhausted)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogStructureSearchError> {
    if canceled() { Err(CatalogStructureSearchError::Canceled) } else { Ok(()) }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, CatalogStructureSearchError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| AppError::Admission)?;
    if values.capacity() > count { return Err(AppError::Admission.into()); }
    Ok(values)
}
