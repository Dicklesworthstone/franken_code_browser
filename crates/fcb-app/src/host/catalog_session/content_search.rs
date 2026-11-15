#![forbid(unsafe_code)]

//! Whole-file content search over an independently progressive catalog.
//!
//! The retained catalog pages, one active reader, result witnesses and diagnostic
//! prefix all have fixed bounds. Visiting more files grows only checked counters.
//! A file's finalized hits own their raw path and original bytes independently of
//! catalog eviction. Neither a map layout nor a full-workspace manifest is built.
//! Membership is an observed traversal, never an atomic filesystem snapshot.

mod wire;
pub(crate) mod cli;
#[cfg(all(test, unix))]
mod tests;

use std::{mem::size_of, path::Path, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, ByteRange, FileId, QueryGeneration, SourceRevision};
use fcb::search::{DetectedEncoding, ExtentConsistency, FileSearch, FileSearchError, FileSearchReport,
    ParsedQuery, ReferenceScanOracle, StreamReadError, StreamReadOptions, StreamReadState,
    StreamReadStats, StreamReadStep, StreamingNeedle};
use fcb::source::{CaptureRequest, NormalizedPath, RootGrant, SourceError};
use fcb::store::{Sha256, Sha256Digest};
use fcb_core::ResourceLease;
use crate::{AppError, input, workspace};
use crate::output::OutputError;
use super::{CatalogDiscoveryPolicy, CatalogSession, CatalogSessionError};
use super::super::{HostResponse, atlas_search::{AtlasSearchError, AtlasSearchScope},
    atlas_stream_search::{AtlasStreamSearchOptions, MAX_ATLAS_STREAM_SEARCH_HITS,
        MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES, MAX_ATLAS_STREAM_SEARCH_PAGE,
        MAX_ATLAS_STREAM_SEARCH_READ_CALLS, MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES}};

/// One step does one bounded metadata batch, one file admission or one shared
/// streaming quantum (at most one 16 KiB successful read and 64 primary hits).
pub const MAX_CATALOG_STREAM_STEPS: usize = 256;
const MAX_DIAGNOSTICS: usize = 32;
// At most 256 retained rows, each with <= 2 * (12 * 1024) witness/text bytes,
// a shared <= 2048-byte path and <= 1024 path components; include conservative
// vector/string growth overlap, the <= 1024-byte query AST, one candidate and
// Arc/row bookkeeping. This 32 MiB bound is independent of visited file count.
// Reader/finalized-report, compiled-pattern and output leases are additional
// reservations from this SAME catalog budget.
const STATE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogContentSearchMode { Text, Occurrences, Expression }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogContentSearchOptions {
    pub mode: CatalogContentSearchMode,
    pub count_all_matches: bool,
    pub start_after: u64,
    pub limits: AtlasStreamSearchOptions,
    pub scope: AtlasSearchScope,
}
impl Default for CatalogContentSearchOptions {
    fn default() -> Self {
        Self { mode: CatalogContentSearchMode::Text, count_all_matches: false, start_after: 0,
            limits: AtlasStreamSearchOptions::default(), scope: AtlasSearchScope::all() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogContentSearchError {
    Catalog(CatalogSessionError), App(AppError), Scope(AtlasSearchError), Stream(StreamReadError),
    File(FileSearchError), InvalidLimits, InvalidExpression, StaleQuery, StaleRevision,
    MissingHit, NoMoreResults, Canceled, IdentityExhausted,
}
impl CatalogContentSearchError {
    pub fn is_canceled(self) -> bool {
        match self {
            Self::Canceled | Self::Scope(AtlasSearchError::Canceled)
                | Self::File(FileSearchError::Source(SourceError::Canceled)) => true,
            Self::Catalog(error) => error.is_canceled(), Self::App(error) => error.is_canceled(),
            _ => false,
        }
    }
}
impl std::fmt::Display for CatalogContentSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog(e) => write!(f, "{e}"), Self::App(e) => write!(f, "{e}"),
            Self::Scope(e) => write!(f, "{e}"), Self::Stream(e) => write!(f, "{e}"), Self::File(e) => write!(f, "{e}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "CATALOG_STREAM_INVALID_LIMITS",
                Self::InvalidExpression => "CATALOG_STREAM_INVALID_EXPRESSION",
                Self::StaleQuery => "CATALOG_STREAM_STALE_QUERY",
                Self::StaleRevision => "CATALOG_STREAM_STALE_REVISION",
                Self::MissingHit => "CATALOG_STREAM_MISSING_HIT",
                Self::NoMoreResults => "CATALOG_STREAM_NO_MORE_RESULTS",
                Self::Canceled => "CATALOG_STREAM_CANCELED",
                Self::IdentityExhausted => "CATALOG_STREAM_IDENTITY_EXHAUSTED",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for CatalogContentSearchError {}
impl From<CatalogSessionError> for CatalogContentSearchError { fn from(e: CatalogSessionError) -> Self { Self::Catalog(e) } }
impl From<AppError> for CatalogContentSearchError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<AtlasSearchError> for CatalogContentSearchError { fn from(e: AtlasSearchError) -> Self { Self::Scope(e) } }
impl From<StreamReadError> for CatalogContentSearchError { fn from(e: StreamReadError) -> Self { Self::Stream(e) } }
impl From<FileSearchError> for CatalogContentSearchError { fn from(e: FileSearchError) -> Self { Self::File(e) } }
impl From<OutputError> for CatalogContentSearchError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

#[derive(Clone, Copy)]
struct Receipt {
    request: CaptureRequest,
    state: StreamReadState,
    unavailable: Option<SourceError>,
    encoding: Option<DetectedEncoding>,
    length: Option<ByteLength>,
    final_length: Option<ByteLength>,
    consistency: Option<ExtentConsistency>,
    unsupported_at: Option<fcb::ByteOffset>,
    complete: bool,
}
struct Hit {
    path: Arc<NormalizedPath>,
    file: Receipt,
    occurrence: u64,
    range: ByteRange,
    witness_start: usize,
    witness_len: usize,
    text_start: usize,
    text_len: usize,
    digest: Sha256Digest,
}
struct Candidate { file: FileId, path: Arc<NormalizedPath> }
struct Active { path: Arc<NormalizedPath>, search: FileSearch<'static> }
struct Finished { path: Arc<NormalizedPath>, report: FileSearchReport<'static> }

#[derive(Clone, Copy)]
enum Stop { MatchLimit, ByteLimit, CallLimit, UserStopped, DiscoveryUnavailable }
impl Stop {
    fn code(self) -> &'static str {
        match self { Self::MatchLimit => "match-limit", Self::ByteLimit => "byte-limit",
            Self::CallLimit => "read-call-limit", Self::UserStopped => "user-stopped",
            Self::DiscoveryUnavailable => "discovery-unavailable" }
    }
}

pub struct CatalogContentSearchSession {
    catalog: CatalogSession,
    generation: QueryGeneration,
    options: CatalogContentSearchOptions,
    needle: Arc<StreamingNeedle>,
    expression: Option<ParsedQuery>,
    revision: u64,
    replay_after_revision: Option<u64>,
    next_entry: usize,
    candidate: Option<Candidate>,
    active: Option<Active>,
    finished: Option<Finished>,
    hits: Vec<Hit>,
    witnesses: Vec<u8>,
    matched_text: String,
    diagnostics: Vec<Receipt>,
    eligible: u64,
    excluded: u64,
    expression_scope_files: u64,
    region_catalogued: u64,
    examined: u64,
    incomplete: u64,
    unavailable: u64,
    unsupported: u64,
    failed: u64,
    matching_files: u64,
    retained_matching_files: u64,
    matches_seen: u64,
    io: StreamReadStats,
    stop_reason: Option<Stop>,
    discovery_error: Option<CatalogSessionError>,
    canceled: bool,
    _lease: ResourceLease,
}

impl CatalogContentSearchSession {
    /// Freeze one query and metadata predicate, admitting only the first bounded
    /// discovery page. Content reads start with explicit advance calls. Changing
    /// the query or requesting a later occurrence window creates a new session
    /// and live observation; independently enumerated walks are not snapshots.
    pub fn open(owner: ArenaOwnerId, root: &Path, generation: u64, query: &str,
        options: CatalogContentSearchOptions, canceled: impl FnMut() -> bool)
        -> Result<Self, CatalogContentSearchError> {
        Self::open_with_policy(owner, root, generation, query, options,
            CatalogDiscoveryPolicy::RepositoryRules, canceled)
    }

    /// An explicit override can search ignored/generated source with the same
    /// streaming limits and exact witnesses. Policy is immutable for the query;
    /// it does not read ignore configuration or expand native root authority.
    pub fn open_with_policy(owner: ArenaOwnerId, root: &Path, generation: u64, query: &str,
        mut options: CatalogContentSearchOptions, policy: CatalogDiscoveryPolicy,
        mut canceled: impl FnMut() -> bool)
        -> Result<Self, CatalogContentSearchError> {
        let limits = options.limits;
        if query.is_empty() || query.len() > MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES
            || !(1..=MAX_ATLAS_STREAM_SEARCH_HITS).contains(&limits.max_matches)
            || !(1..=MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES).contains(&limits.max_scan_bytes)
            || !(1..=MAX_ATLAS_STREAM_SEARCH_READ_CALLS).contains(&limits.max_read_calls)
            || options.start_after.checked_add(limits.max_matches as u64).and_then(|n| n.checked_add(1)).is_none()
            || (options.mode == CatalogContentSearchMode::Occurrences && !options.scope.case_sensitive()) {
            return Err(CatalogContentSearchError::InvalidLimits);
        }
        let generation = QueryGeneration::new(owner, generation).map_err(|_| CatalogContentSearchError::StaleQuery)?;
        check(&mut canceled)?;
        let mut catalog = CatalogSession::open_with_policy(owner, root, policy, &mut canceled)?;
        let id = catalog.next_id()?;
        let lease = catalog.budget.try_reserve_managed(owner, id,
            ByteLength::new(STATE_BYTES + size_of::<Self>() as u64)).map_err(|_| AppError::Admission)?;
        // Parser allocation is covered before parsing. Metadata predicates use
        // the same parser/oracle as captured and atlas-streaming search.
        let expression = if options.mode == CatalogContentSearchMode::Expression {
            options.count_all_matches = true;
            Some(ParsedQuery::parse(query).map_err(|_| CatalogContentSearchError::InvalidExpression)?)
        } else { None };
        let id = catalog.next_id()?;
        let needle = Arc::new(if let Some(expression) = &expression {
            StreamingNeedle::expression(owner, expression, options.scope.case_sensitive(), &catalog.budget, id)?
        } else if options.mode == CatalogContentSearchMode::Occurrences {
            StreamingNeedle::identifier(owner, query, &catalog.budget, id)?
        } else {
            StreamingNeedle::text_matching(owner, query, options.scope.case_sensitive(), &catalog.budget, id)?
        });
        let bytes = limits.max_matches.checked_mul(needle.max_witness_bytes()).ok_or(AppError::Admission)?;
        let mut matched_text = String::new();
        matched_text.try_reserve_exact(bytes).map_err(|_| AppError::Admission)?;
        if matched_text.capacity() > bytes { return Err(AppError::Admission.into()); }
        let hits = reserve(limits.max_matches)?;
        let witnesses = reserve(bytes)?;
        let diagnostics = reserve(MAX_DIAGNOSTICS)?;
        check(&mut canceled)?; catalog.validate_active()?;
        Ok(Self { catalog, generation, options, needle, expression, revision: 1,
            replay_after_revision: None, next_entry: 0, candidate: None, active: None, finished: None,
            hits, witnesses, matched_text, diagnostics, eligible: 0, excluded: 0,
            expression_scope_files: 0, region_catalogued: 0, examined: 0, incomplete: 0,
            unavailable: 0, unsupported: 0, failed: 0, matching_files: 0, retained_matching_files: 0,
            matches_seen: 0, io: StreamReadStats::default(), stop_reason: None,
            discovery_error: None, canceled: false, _lease: lease })
    }
    pub fn owner(&self) -> ArenaOwnerId { self.catalog.owner() }
    pub fn grant(&self) -> &RootGrant { self.catalog.grant() }
    pub fn validate_active(&self) -> Result<(), CatalogContentSearchError> {
        if self.canceled { return Err(CatalogContentSearchError::Canceled); }
        Ok(self.catalog.validate_active()?)
    }
    pub fn info(&mut self, canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogContentSearchError> {
        self.snapshot("info", 0, MAX_ATLAS_STREAM_SEARCH_PAGE, canceled)
    }
    pub fn page(&mut self, generation: u64, revision: u64, start: usize, count: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogContentSearchError> {
        self.validate_revision(generation, revision)?;
        self.snapshot("page", start, count, canceled)
    }
    pub fn hit(&mut self, generation: u64, revision: u64, position: usize,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogContentSearchError> {
        self.validate_revision(generation, revision)?;
        self.witness(position, canceled)
    }
    /// A predecessor retry replays the current accepted result without another
    /// read. Revision advances BEFORE work/output, so consumed metadata and
    /// source bytes remain recoverable after cancellation or output denial.
    pub fn advance(&mut self, generation: u64, after_revision: u64, max_steps: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogContentSearchError> {
        self.advance_work(generation, after_revision, max_steps, &mut canceled)?;
        self.snapshot("advance", 0, MAX_ATLAS_STREAM_SEARCH_PAGE, canceled)
    }
    fn advance_work(&mut self, generation: u64, after_revision: u64, max_steps: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogContentSearchError> {
        self.validate_generation(generation)?;
        if !(1..=MAX_CATALOG_STREAM_STEPS).contains(&max_steps) { return Err(CatalogContentSearchError::InvalidLimits); }
        if self.replay_after_revision == Some(after_revision) && after_revision < self.revision {
            return Ok(());
        }
        if after_revision != self.revision { return Err(CatalogContentSearchError::StaleRevision); }
        if !self.more() { return Err(CatalogContentSearchError::NoMoreResults); }
        check(canceled)?; self.validate_active()?;
        self.revision = add(self.revision, 1)?;
        self.replay_after_revision = Some(after_revision);
        let result = (|| {
            for _ in 0..max_steps {
                check(canceled)?; self.validate_active()?;
                if !self.more() { break; }
                self.step(canceled)?;
            }
            Ok(())
        })();
        if result.as_ref().is_err_and(|error: &CatalogContentSearchError| error.is_canceled())
            && self.active.as_ref().is_some_and(|active| active.search.state() == StreamReadState::Canceled) {
            // Cancellation revokes this query's future publication. Soft stop
            // is a separate operation retaining only already finalized files.
            self.canceled = true;
            self.candidate = None;
            self.active = None;
            self.finished = None;
        }
        result
    }
    pub fn stop(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, CatalogContentSearchError> {
        self.stop_work(generation, &mut canceled)?;
        self.snapshot("stop", 0, MAX_ATLAS_STREAM_SEARCH_PAGE, canceled)
    }
    fn stop_work(&mut self, generation: u64, canceled: &mut impl FnMut() -> bool)
        -> Result<(), CatalogContentSearchError> {
        self.validate_generation(generation)?;
        check(canceled)?; self.validate_active()?;
        if self.more() {
            self.revision = add(self.revision, 1)?;
            self.replay_after_revision = None;
            self.candidate = None;
            self.active = None;
            self.finished = None;
            self.stop_reason = Some(Stop::UserStopped);
        }
        Ok(())
    }

    fn validate_generation(&self, generation: u64) -> Result<(), CatalogContentSearchError> {
        if self.generation.get() != generation { Err(CatalogContentSearchError::StaleQuery) } else { Ok(()) }
    }
    fn validate_revision(&self, generation: u64, revision: u64) -> Result<(), CatalogContentSearchError> {
        self.validate_generation(generation)?;
        if self.revision != revision { Err(CatalogContentSearchError::StaleRevision) } else { Ok(()) }
    }
    fn pending_entries(&self) -> bool {
        self.catalog.pages.back().is_some_and(|page| self.next_entry < page.files.len())
    }
    fn more(&self) -> bool {
        !self.canceled && self.stop_reason.is_none() && (self.candidate.is_some() || self.active.is_some() || self.finished.is_some()
            || self.pending_entries() || self.catalog.discovery.is_some())
    }
    fn search_files(&self) -> u64 {
        if self.expression.is_some() { self.expression_scope_files } else { self.eligible }
    }
    fn complete(&self) -> bool {
        !self.canceled && !self.more() && self.stop_reason.is_none() && self.catalog.complete && self.incomplete == 0
            && self.search_files() == self.examined
            && self.options.scope.region_files().is_none_or(|files| files as u64 == self.region_catalogued)
    }
    fn expression_admits(&self, path: &NormalizedPath) -> bool {
        self.expression.as_ref().is_none_or(|expression| {
            let matches = |key: &str| ReferenceScanOracle::matches_path_filters(key, &expression.path_filters)
                && ReferenceScanOracle::matches_lang_filters(key, &expression.lang_filters);
            match path.as_str() { Ok(key) => matches(key), Err(_) => matches(&path.display_escaped().to_string()) }
        })
    }

    fn step(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogContentSearchError> {
        if self.finished.is_some() { return self.accept_finished(); }
        if let Some(active) = &mut self.active {
            let before = active.search.stats();
            let outcome = active.search.step(StreamReadStep::default(), self.generation, &mut *canceled);
            let after = active.search.stats();
            self.io.bytes_read = add(self.io.bytes_read, after.bytes_read - before.bytes_read)?;
            self.io.scanned_bytes = add(self.io.scanned_bytes, after.scanned_bytes - before.scanned_bytes)?;
            self.io.read_calls = add(self.io.read_calls, after.read_calls - before.read_calls)?;
            self.io.interrupted_calls = add(self.io.interrupted_calls, after.interrupted_calls - before.interrupted_calls)?;
            self.io.peak_buffer_bytes = self.io.peak_buffer_bytes.max(after.peak_buffer_bytes);
            if active.search.state() == StreamReadState::Canceled || canceled() { return Err(CatalogContentSearchError::Canceled); }
            if active.search.state() != StreamReadState::Pending {
                let active = self.active.take().expect("active file checked");
                self.finished = Some(Finished { path: active.path, report: active.search.finish()? });
                return self.accept_finished();
            }
            outcome?;
            return Ok(());
        }
        if self.candidate.is_some() { return self.open_candidate(canceled); }
        if !self.pending_entries() {
            if self.catalog.discovery.is_some() {
                match self.catalog.consume_page(canceled) {
                    Ok(()) => self.next_entry = 0,
                    Err(error) if error.is_canceled() => return Err(error.into()),
                    Err(error) => { self.discovery_error = Some(error); self.stop_reason = Some(Stop::DiscoveryUnavailable); }
                }
            }
            return Ok(());
        }
        let page = self.catalog.pages.back().expect("pending catalog page");
        let file = &page.files[self.next_entry];
        let in_region = self.options.scope.region_includes_path(file.path.as_bytes());
        let admitted = self.options.scope.includes_path(file.path.as_bytes());
        let expression_admitted = admitted && self.expression_admits(&file.path);
        let path = Arc::new(file.path.clone());
        let file_id = file.id;
        // All path/filter work happens before committing this metadata row.
        if in_region { self.region_catalogued = add(self.region_catalogued, 1)?; }
        if admitted { self.eligible = add(self.eligible, 1)?; } else { self.excluded = add(self.excluded, 1)?; }
        self.next_entry += 1;
        if !expression_admitted { return Ok(()); }
        self.expression_scope_files = add(self.expression_scope_files, 1)?;
        self.candidate = Some(Candidate { file: file_id, path });
        Ok(())
    }

    fn open_candidate(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogContentSearchError> {
        // Excluded metadata never spends a source quota. Continue classifying
        // it even when the last eligible file consumed the exact byte budget.
        if self.io.bytes_read >= self.options.limits.max_scan_bytes { self.stop_reason = Some(Stop::ByteLimit); return Ok(()); }
        if self.io.read_calls >= self.options.limits.max_read_calls { self.stop_reason = Some(Stop::CallLimit); return Ok(()); }
        let candidate = self.candidate.as_ref().ok_or(CatalogContentSearchError::MissingHit)?;
        let file_id = candidate.file;
        let path = Arc::clone(&candidate.path);
        let revision = SourceRevision::new(self.owner(), file_id.get()).map_err(|_| CatalogContentSearchError::IdentityExhausted)?;
        let request = CaptureRequest::new(file_id, revision).map_err(|error| CatalogContentSearchError::File(error.into()))?;
        let root = self.grant().root_path().to_path_buf();
        let opened = workspace::checked_source_path(&root, &path).and_then(|native| {
            input::open_regular(&native).map(|(file, _)| file).map_err(|_| SourceError::CaptureUnavailable)
        });
        self.validate_active()?; check(canceled)?;
        let file = match opened {
            Ok(file) => file,
            Err(error) => {
                let receipt = Receipt { request, state: StreamReadState::Failed(StreamReadError::Io), unavailable: Some(error),
                    encoding: None, length: None, final_length: None, consistency: None, unsupported_at: None, complete: false };
                self.record_receipt(receipt)?;
                self.candidate = None;
                return Ok(());
            }
        };
        let options = StreamReadOptions { generation: self.generation, encoding: None,
            max_matches: self.options.limits.max_matches - self.hits.len(),
            max_bytes: self.options.limits.max_scan_bytes - self.io.bytes_read,
            max_read_calls: self.options.limits.max_read_calls - self.io.read_calls };
        let id = self.catalog.next_id()?;
        let search = FileSearch::new_owned_page(file, request, Arc::clone(&self.needle), options,
            self.options.start_after.saturating_sub(self.matches_seen), self.options.count_all_matches,
            &self.catalog.budget, id)?;
        self.active = Some(Active { path, search });
        self.candidate = None;
        Ok(())
    }

    fn accept_finished(&mut self) -> Result<(), CatalogContentSearchError> {
        let finished = self.finished.as_ref().ok_or(CatalogContentSearchError::MissingHit)?;
        let report = finished.report.search();
        let receipt = Receipt { request: report.request(), state: report.state(), unavailable: None,
            encoding: report.encoding(), length: Some(report.observed_length()), final_length: finished.report.final_length(),
            consistency: Some(finished.report.consistency()), unsupported_at: report.unsupported_at(), complete: finished.report.is_complete() };
        let matches = add(self.matches_seen, report.matches_seen())?;
        let matching_files = add(self.matching_files, u64::from(report.matches_seen() > 0))?;
        let retained_files = add(self.retained_matching_files, u64::from(!report.hits().is_empty()))?;
        let mut bytes = self.witnesses.len();
        let mut text_bytes = self.matched_text.len();
        let shared_exact = self.options.scope.case_sensitive();
        for (ordinal, _) in report.hits().iter().enumerate() {
            let original = report.witness_bytes(ordinal)?;
            let text = report.matched_text(ordinal)?;
            if !shared_exact || ordinal == 0 {
                bytes = bytes.checked_add(original.len()).ok_or(AppError::Admission)?;
                text_bytes = text_bytes.checked_add(text.len()).ok_or(AppError::Admission)?;
            }
        }
        if self.hits.len() + report.hits().len() > self.hits.capacity()
            || bytes > self.witnesses.capacity() || text_bytes > self.matched_text.capacity() {
            return Err(AppError::Admission.into());
        }
        // Validate receipt counter headroom before any hit becomes visible.
        self.examined.checked_add(1).ok_or(CatalogContentSearchError::IdentityExhausted)?;
        self.incomplete.checked_add(1).ok_or(CatalogContentSearchError::IdentityExhausted)?;
        self.unsupported.checked_add(1).ok_or(CatalogContentSearchError::IdentityExhausted)?;
        self.failed.checked_add(1).ok_or(CatalogContentSearchError::IdentityExhausted)?;
        let mut shared = None;
        for (ordinal, hit) in report.hits().iter().enumerate() {
            let (witness_start, witness_len, text_start, text_len, digest) = if let Some(shared) = shared {
                shared
            } else {
                let original = report.witness_bytes(ordinal).expect("validated original witness");
                let text = report.matched_text(ordinal).expect("validated matched text");
                let witness_start = self.witnesses.len();
                let text_start = self.matched_text.len();
                self.witnesses.extend_from_slice(original);
                self.matched_text.push_str(text);
                let mut digest = Sha256::new(); digest.update(original);
                let retained = (witness_start, original.len(), text_start, text.len(), digest.finalize());
                if shared_exact { shared = Some(retained); }
                retained
            };
            self.hits.push(Hit { path: Arc::clone(&finished.path), file: receipt, occurrence: hit.occurrence_id(),
                range: hit.original_range(), witness_start, witness_len, text_start, text_len, digest });
        }
        self.matches_seen = matches; self.matching_files = matching_files; self.retained_matching_files = retained_files;
        self.stop_reason = match report.state() {
            StreamReadState::Truncated => Some(Stop::MatchLimit), StreamReadState::ByteLimit => Some(Stop::ByteLimit),
            StreamReadState::CallLimit => Some(Stop::CallLimit), _ => self.stop_reason,
        };
        self.record_receipt(receipt)?;
        self.finished = None;
        Ok(())
    }
    fn record_receipt(&mut self, receipt: Receipt) -> Result<(), CatalogContentSearchError> {
        self.examined = add(self.examined, 1)?;
        if !receipt.complete {
            self.incomplete = add(self.incomplete, 1)?;
            if receipt.unavailable.is_some() { self.unavailable = add(self.unavailable, 1)?; }
            else if receipt.state == StreamReadState::UnsupportedText { self.unsupported = add(self.unsupported, 1)?; }
            else if matches!(receipt.state, StreamReadState::Failed(_)) { self.failed = add(self.failed, 1)?; }
            if self.diagnostics.len() < MAX_DIAGNOSTICS { self.diagnostics.push(receipt); }
        }
        Ok(())
    }
}

fn add(left: u64, right: u64) -> Result<u64, CatalogContentSearchError> {
    left.checked_add(right).ok_or(CatalogContentSearchError::IdentityExhausted)
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, CatalogContentSearchError> {
    let mut values = Vec::new(); values.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if values.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok(values)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), CatalogContentSearchError> {
    if canceled() { Err(CatalogContentSearchError::Canceled) } else { Ok(()) }
}
