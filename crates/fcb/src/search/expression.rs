#![forbid(unsafe_code)]

//! Multi-term saved-source queries over the existing parser, metadata filters,
//! decoder and streaming matcher. Predicates are document-wide, not line-local.
//! Each step evaluates one metadata record or one bounded source/matcher quantum.
//! One observed member stream feeds every predicate and the primary matcher in
//! one decoding pass. Results remain private until that entire stream matches its
//! saved digest. No runtime, source grant, or filesystem path is inferred.

use std::{io::{Read, Seek}, mem::size_of};
use fcb_core::{ArenaOwnerId, ByteLength, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use super::{CaptureRequest, ParsedQuery, QueryError, ReferenceScanOracle, RawPath,
    StreamingNeedle, StreamReadError, StreamReadOptions, StreamReadState, StreamReadStep};
use super::paged_snapshot::{PagedSnapshot, PagedMemberData, PagedCapture,
    PagedSearchError, Sha256Digest, SnapshotDirectory, SnapshotReadSession, MemberSearch};
use super::snapshot_index::{SnapshotIndex, IndexDecision};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpressionError {
    Syntax(QueryError), Pattern(StreamReadError), Search(PagedSearchError),
    ResourceDenied, IdentityExhausted, InvalidLimits, Pending, Canceled, StaleQuery,
}
impl std::fmt::Display for ExpressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(e) => write!(f, "{e}"), Self::Pattern(e) => write!(f, "{e}"),
            Self::Search(e) => write!(f, "{e}"),
            Self::ResourceDenied => f.write_str("EXPRESSION_RESOURCE_DENIED"),
            Self::IdentityExhausted => f.write_str("EXPRESSION_IDENTITY_EXHAUSTED"),
            Self::InvalidLimits => f.write_str("EXPRESSION_INVALID_LIMITS"),
            Self::Pending => f.write_str("EXPRESSION_PENDING"),
            Self::Canceled => f.write_str("EXPRESSION_CANCELED"),
            Self::StaleQuery => f.write_str("EXPRESSION_STALE_QUERY"),
        }
    }
}
impl std::error::Error for ExpressionError {}
impl From<QueryError> for ExpressionError { fn from(e: QueryError) -> Self { Self::Syntax(e) } }
impl From<StreamReadError> for ExpressionError { fn from(e: StreamReadError) -> Self { Self::Pattern(e) } }
impl From<PagedSearchError> for ExpressionError { fn from(e: PagedSearchError) -> Self { Self::Search(e) } }

/// Immutable query syntax plus separately admitted matcher patterns. Borrowing
/// this plan binds every prefilter, predicate and primary scan to the SAME AST.
/// Hosts reserve a noncolliding allocation interval beginning at first_pattern.
/// The legacy term-sized interval is still validated, but the shared pattern
/// now retains only its first allocation ID.
pub struct ExpressionPlan {
    owner: ArenaOwnerId,
    case_sensitive: bool,
    syntax: ParsedQuery,
    pattern: StreamingNeedle,
    _lease: ResourceLease,
}
impl ExpressionPlan {
    pub fn parse(owner: ArenaOwnerId, query: &str, budget: &ResourceBudget,
        allocation: ResourceAllocationId, first_pattern: ResourceAllocationId)
        -> Result<Self, ExpressionError> {
        Self::parse_with_matching(owner, query, true, budget, allocation, first_pattern)
    }
    /// Apply one immutable comparison policy to every required, excluded and
    /// primary term. False uses the shared Unicode scalar lowercase matcher;
    /// it does not enable full case folding or canonical normalization. Path
    /// and language filters retain their existing metadata matching semantics.
    pub fn parse_with_matching(owner: ArenaOwnerId, query: &str, case_sensitive: bool,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        first_pattern: ResourceAllocationId) -> Result<Self, ExpressionError> {
        if query.len() > super::MAX_QUERY_LEN { return Err(QueryError::NeedleTooLong.into()); }
        // Includes the parser's temporary scalar/token vectors, retained AST,
        // lowercased filters, pattern descriptors, and geometric overlap.
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(128 * 1024))
            .map_err(|_| ExpressionError::ResourceDenied)?;
        let syntax = ParsedQuery::parse(query)?;
        let count = syntax.exclusion_terms.len() + syntax.conjunction_terms.len() + 1;
        let last = first_pattern.get().checked_add(count as u64 - 1)
            .ok_or(ExpressionError::IdentityExhausted)?;
        if (first_pattern.get()..=last).contains(&allocation.get()) { return Err(ExpressionError::InvalidLimits); }
        let pattern = if count == 1 {
            // A literal-only expression keeps its bounded lookahead route;
            // there are no document-wide predicates requiring an EOF scan.
            StreamingNeedle::text_matching(owner, &syntax.primary_needle, case_sensitive, budget, first_pattern)?
        } else {
            StreamingNeedle::expression(owner, &syntax, case_sensitive, budget, first_pattern)?
        };
        Ok(Self { owner, case_sensitive, syntax, pattern, _lease: lease })
    }
    pub fn syntax(&self) -> &ParsedQuery { &self.syntax }
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    pub const fn case_sensitive(&self) -> bool { self.case_sensitive }
    pub const fn realized_mode(&self) -> &'static str { realized_mode(self.case_sensitive) }
    fn predicate_count(&self) -> usize { self.syntax.exclusion_terms.len() + self.syntax.conjunction_terms.len() }
    fn admits(&self, native: &[u8]) -> bool {
        // Match the established SearchDocument key convention. Escaped labels
        // are filter keys ONLY; output/activation always use native identities.
        if let Ok(path) = std::str::from_utf8(native) { return self.admits_key(path); }
        let raw = RawPath::from_bytes(native);
        self.admits_key(&raw.display_escaped().to_string())
    }
    fn admits_key(&self, key: &str) -> bool {
        ReferenceScanOracle::matches_path_filters(key, &self.syntax.path_filters)
            && ReferenceScanOracle::matches_lang_filters(key, &self.syntax.lang_filters)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpressionOptions {
    pub generation: QueryGeneration,
    pub first_file: FileId,
    pub first_revision: SourceRevision,
    pub max_matches: usize,
    /// Aggregate original bytes consumed by the shared decoding pass. Each
    /// member byte is charged once regardless of the number of predicates.
    /// Matcher CPU work also depends on the parser's bounded term count.
    pub max_scan_bytes: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpressionState { Pending, Complete, Truncated, WorkLimit, Canceled, Failed(ExpressionError) }
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExpressionStats {
    pub metadata_examined: usize,
    pub scope_files: usize,
    pub metadata_excluded: usize,
    pub unavailable_files: usize,
    /// Members whose complete observed stream passed saved-digest verification.
    pub members_loaded: usize,
    /// Actual source I/O, including bounded integrity drains after match/work limits.
    pub loaded_bytes: u64,
    pub read_calls: u64,
    /// Peak occupied source buffer length, not the full member or reserved capacity.
    pub peak_member_bytes: usize,
    pub last_step_loaded_bytes: usize,
    pub last_step_read_calls: usize,
    /// Predicate cursors evaluated inside the shared source passes, not
    /// additional reads or decoder passes.
    pub predicate_scans: usize,
    pub predicate_rejections: usize,
    pub primary_scans: usize,
    pub unsupported_files: usize,
    pub scanned_bytes: u64,
    pub last_step_scanned_bytes: u64,
    pub index_eliminated: usize,
    pub index_candidates: usize,
    pub index_fallbacks: usize,
}

/// An immutable exact occurrence. Only the query can create these values.
/// Path labels and later live bytes cannot be substituted during activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpressionHit {
    ordinal: usize, file: FileId, revision: SourceRevision, generation: QueryGeneration,
    occurrence: u64, range: ByteRange, archive: Sha256Digest, source: Sha256Digest,
}
impl ExpressionHit {
    pub const fn ordinal(self) -> usize { self.ordinal }
    pub const fn file(self) -> FileId { self.file }
    pub const fn revision(self) -> SourceRevision { self.revision }
    pub const fn occurrence_id(self) -> u64 { self.occurrence }
    pub const fn original_range(self) -> ByteRange { self.range }
    pub const fn source_digest(self) -> Sha256Digest { self.source }
    pub const fn archive_digest(self) -> Sha256Digest { self.archive }
    pub fn open<R: Read + Seek>(self, archive: &mut PagedSnapshot<R>, generation: QueryGeneration,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2], canceled: impl FnMut() -> bool)
        -> Result<PagedCapture, ExpressionError> {
        if generation != self.generation || archive.directory().digest() != self.archive {
            return Err(ExpressionError::StaleQuery);
        }
        let capture = PagedCapture::load(archive, self.ordinal, self.file, self.revision, budget, allocations, canceled)?;
        let (_, end) = self.range.as_usize_bounds().map_err(|_| PagedSearchError::InvalidHit)?;
        if capture.source_digest() != self.source || self.range.is_empty() || end > capture.bytes().len() {
            return Err(PagedSearchError::InvalidHit.into());
        }
        Ok(capture)
    }
}

/// First counts explicitly filtered membership, one record per step, so missing
/// selected sources remain visible even when a subsequent result limit stops the
/// scan. No per-repository candidate vector or capture collection is allocated.
pub struct ExpressionQuery<'archive, 'plan, R: Read + Seek> {
    directory: &'archive SnapshotDirectory,
    source: Option<SnapshotReadSession<'archive, R>>,
    plan: &'plan ExpressionPlan,
    index: Option<&'plan SnapshotIndex>,
    options: ExpressionOptions,
    allocations: [ResourceAllocationId; 3],
    scope_at: usize,
    next: usize,
    active: Option<MemberSearch<'archive, 'plan, R>>,
    hits: Vec<ExpressionHit>,
    seen: u64,
    matching_files: usize,
    count_all_matches: bool,
    skip_matches: u64,
    state: ExpressionState,
    stats: ExpressionStats,
    _lease: ResourceLease,
}
impl<'archive, 'plan, R: Read + Seek> ExpressionQuery<'archive, 'plan, R> {
    /// allocations = [result/metadata scratch, bounded source scratch, one active matcher].
    /// The query owns no backing handle beyond its borrowed archive.
    pub fn new(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        options: ExpressionOptions, budget: &ResourceBudget, allocations: [ResourceAllocationId; 3])
        -> Result<Self, ExpressionError> {
        Self::new_page(archive, plan, options, 0, false, budget, allocations)
    }
    /// Reevaluate the same saved-member predicates and skip only their qualified
    /// primary occurrences. Predicate work and skipped matches spend the same
    /// aggregate scan budget; no source bytes or candidate table are retained.
    pub fn new_page(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        options: ExpressionOptions, skip_matches: u64, count_all_matches: bool,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 3]) -> Result<Self, ExpressionError> {
        let owner = archive.directory().owner();
        if plan.owner != owner || options.generation.owner() != owner || options.first_file.owner() != owner
            || options.first_revision.owner() != owner { return Err(PagedSearchError::OwnerMismatch.into()); }
        if options.max_matches > super::streaming::MAX_STREAM_RESULTS
            || allocations[0] == allocations[1] || allocations[0] == allocations[2] || allocations[1] == allocations[2] {
            return Err(ExpressionError::InvalidLimits);
        }
        let last = archive.directory().len().saturating_sub(1) as u64;
        options.first_file.get().checked_add(last).ok_or(ExpressionError::IdentityExhausted)?;
        options.first_revision.get().checked_add(last).ok_or(ExpressionError::IdentityExhausted)?;
        let charge = options.max_matches.checked_mul(size_of::<ExpressionHit>())
            .and_then(|n| n.checked_add(size_of::<Self>() + 512 * 1024))
            .ok_or(ExpressionError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(owner, allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| ExpressionError::ResourceDenied)?;
        let mut hits = Vec::new(); hits.try_reserve_exact(options.max_matches).map_err(|_| ExpressionError::ResourceDenied)?;
        if hits.capacity() > options.max_matches { return Err(ExpressionError::ResourceDenied); }
        let source = archive.read_session();
        let directory = source.directory();
        Ok(Self { directory, source: Some(source), plan, index: None, options, allocations, scope_at: 0, next: 0,
            active: None, hits, seen: 0, matching_files: 0, count_all_matches, skip_matches,
            state: ExpressionState::Pending, stats: ExpressionStats::default(), _lease: lease })
    }
    /// Evaluate the same whole-member predicates, then count every primary
    /// occurrence within max_scan_bytes. Only max_matches witnesses survive;
    /// zero retains no rows and does not turn an unknown predicate into absence.
    pub fn new_counting(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        options: ExpressionOptions, budget: &ResourceBudget, allocations: [ResourceAllocationId; 3])
        -> Result<Self, ExpressionError> {
        let mut query = Self::new(archive, plan, options, budget, allocations)?;
        query.count_all_matches = true; Ok(query)
    }
    /// Only the mandatory positive primary literal can prove exclusion. NOT
    /// terms are never used as a negative prefilter. Incompatible representations
    /// and uncovered segments take the complete decoder/predicate route.
    pub fn new_indexed(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        index: &'plan SnapshotIndex, options: ExpressionOptions, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 3]) -> Result<Self, ExpressionError> {
        index.validate_directory(archive.directory()).map_err(PagedSearchError::from)?;
        let mut query = Self::new(archive, plan, options, budget, allocations)?;
        query.index = Some(index); Ok(query)
    }
    pub fn new_indexed_counting(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        index: &'plan SnapshotIndex, options: ExpressionOptions, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 3]) -> Result<Self, ExpressionError> {
        let mut query = Self::new_indexed(archive, plan, index, options, budget, allocations)?;
        query.count_all_matches = true; Ok(query)
    }
    pub fn new_indexed_page(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        index: &'plan SnapshotIndex, options: ExpressionOptions, skip_matches: u64,
        count_all_matches: bool, budget: &ResourceBudget, allocations: [ResourceAllocationId; 3])
        -> Result<Self, ExpressionError> {
        let mut query = Self::new_indexed(archive, plan, index, options, budget, allocations)?;
        query.skip_matches = skip_matches; query.count_all_matches = count_all_matches; Ok(query)
    }
    pub const fn state(&self) -> ExpressionState { self.state }
    pub const fn stats(&self) -> ExpressionStats { self.stats }
    pub fn cancel(&mut self) { self.active = None; self.source = None; self.state = ExpressionState::Canceled; }
    pub fn step(&mut self, step: StreamReadStep, generation: QueryGeneration, budget: &ResourceBudget,
        mut canceled: impl FnMut() -> bool) -> Result<ExpressionState, ExpressionError> {
        self.stats.last_step_scanned_bytes = 0;
        self.stats.last_step_loaded_bytes = 0; self.stats.last_step_read_calls = 0;
        if generation != self.options.generation { self.cancel(); return Err(ExpressionError::StaleQuery); }
        if canceled() { self.cancel(); return Err(ExpressionError::Canceled); }
        if self.state != ExpressionState::Pending { return Ok(self.state); }
        if step.max_bytes == 0 || step.max_calls == 0 || step.max_hits == 0 { return Ok(self.state); }
        let result = self.advance(step, budget, &mut canceled);
        if canceled() { self.cancel(); return Err(ExpressionError::Canceled); }
        if let Err(error) = result {
            self.active = None; self.source = None;
            self.state = if error == ExpressionError::Canceled { ExpressionState::Canceled }
                else { ExpressionState::Failed(error) };
            return Err(error);
        }
        Ok(self.state)
    }
    fn advance(&mut self, step: StreamReadStep, budget: &ResourceBudget,
        canceled: &mut impl FnMut() -> bool) -> Result<(), ExpressionError> {
        if self.scope_at < self.directory.len() {
            let member = self.directory.member(self.scope_at).ok_or(PagedSearchError::InvalidHit)?;
            self.scope_at += 1; self.stats.metadata_examined += 1;
            if self.plan.admits(member.path) {
                self.stats.scope_files += 1;
                if matches!(member.data, PagedMemberData::Unavailable(_)) { self.stats.unavailable_files += 1; }
            } else { self.stats.metadata_excluded += 1; }
            return Ok(());
        }
        if self.active.is_none() {
            if self.next == self.directory.len() { self.state = ExpressionState::Complete; return Ok(()); }
            let ordinal = self.next; self.next += 1;
            let member = self.directory.member(ordinal).ok_or(PagedSearchError::InvalidHit)?;
            if !self.plan.admits(member.path) || matches!(member.data, PagedMemberData::Unavailable(_)) { return Ok(()); }
            if let Some(index) = self.index {
                // The saved index certifies absence of exact UTF-8 grams only.
                // Lowercase matching can find different bytes (even different
                // byte lengths), so exact negatives must never skip a member.
                let exact = self.plan.case_sensitive.then_some(self.plan.syntax.primary_needle.as_str());
                match index.text_decision(ordinal, exact) {
                    IndexDecision::Excluded => { self.stats.index_eliminated += 1; return Ok(()); }
                    IndexDecision::Verify => self.stats.index_candidates += 1,
                    IndexDecision::Fallback => self.stats.index_fallbacks += 1,
                }
            }
            if member.observed_bytes > 0 && self.stats.scanned_bytes == self.options.max_scan_bytes {
                self.state = ExpressionState::WorkLimit; return Ok(());
            }
            self.start(ordinal, budget)?;
            // Admission is distinct from source I/O. The next step starts one
            // bounded source/matcher quantum, with no whole-member allocation.
            return Ok(());
        }
        let scan = self.active.as_mut().ok_or(ExpressionError::Pending)?;
        let before = scan.stats();
        let result = scan.step(step, self.options.generation, &mut *canceled);
        let after = scan.stats();
        self.stats.scanned_bytes += after.scanned_bytes - before.scanned_bytes;
        self.stats.loaded_bytes += after.io.bytes_read - before.io.bytes_read;
        self.stats.read_calls += after.io.read_calls - before.io.read_calls;
        self.stats.peak_member_bytes = self.stats.peak_member_bytes.max(after.peak_source_bytes);
        self.stats.last_step_scanned_bytes = after.last_step_scanned_bytes;
        self.stats.last_step_loaded_bytes = after.last_step_loaded_bytes;
        self.stats.last_step_read_calls = after.last_step_read_calls;
        let complete = result.map_err(|error| match error {
            PagedSearchError::Canceled => ExpressionError::Canceled,
            _ => ExpressionError::Search(error),
        })?;
        if !complete { return Ok(()); }
        let (session, verified, report) = self.active.take().ok_or(ExpressionError::Pending)?.finish()?;
        self.source = Some(session);
        self.stats.members_loaded += 1;
        let report = report.ok_or(ExpressionError::Pending)?;
        match report.state() {
            StreamReadState::UnsupportedText => { self.stats.unsupported_files += 1; return Ok(()); }
            StreamReadState::Canceled => return Err(ExpressionError::Canceled),
            StreamReadState::Failed(error) => return Err(error.into()),
            StreamReadState::Complete | StreamReadState::Truncated | StreamReadState::ByteLimit => {},
            _ => return Err(PagedSearchError::IncompleteInput.into()),
        }
        if report.content_predicates_qualified() == Some(false) {
            self.stats.predicate_rejections += 1;
        }
        // The shared expression matcher finishes its predicates before exposing
        // any primary candidates. Preserve this query's public non-counting
        // contract: only one extra occurrence proves truncation, and later
        // members remain unexamined once that evidence exists.
        let remaining = self.options.max_matches - self.hits.len();
        let skip = self.skip_matches.saturating_sub(self.seen);
        let counted = if self.count_all_matches { report.matches_seen() } else {
            report.matches_seen().min(skip.saturating_add(remaining as u64).saturating_add(1))
        };
        self.seen = self.seen.checked_add(counted).ok_or(ExpressionError::InvalidLimits)?;
        if counted > 0 { self.matching_files += 1; }
        for hit in report.hits() {
            self.hits.push(ExpressionHit { ordinal: verified.ordinal(), file: report.request().file(),
                revision: report.request().revision(), generation: self.options.generation,
                occurrence: hit.occurrence_id(), range: hit.original_range(),
                archive: verified.archive_digest(), source: verified.source_digest() });
        }
        if report.state() == StreamReadState::ByteLimit { self.state = ExpressionState::WorkLimit; }
        else if !self.count_all_matches && report.has_more_matches() { self.state = ExpressionState::Truncated; }
        Ok(())
    }
    fn start(&mut self, ordinal: usize, budget: &ResourceBudget) -> Result<(), ExpressionError> {
        let owner = self.options.generation.owner();
        let file = FileId::new(owner, self.options.first_file.get() + ordinal as u64).map_err(|_| ExpressionError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, self.options.first_revision.get() + ordinal as u64).map_err(|_| ExpressionError::IdentityExhausted)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| PagedSearchError::InvalidHit)?;
        let member = self.directory.member(ordinal).ok_or(PagedSearchError::InvalidHit)?;
        let PagedMemberData::Captured { byte_length, .. } = member.data else { return Err(PagedSearchError::InvalidHit.into()); };
        let length = byte_length as u64;
        let mut options = StreamReadOptions::new(self.options.generation);
        options.max_matches = self.options.max_matches - self.hits.len();
        options.max_bytes = length.min(self.options.max_scan_bytes.saturating_sub(self.stats.scanned_bytes));
        options.max_read_calls = fcb_store::paged_snapshot::MAX_SNAPSHOT_READ_CALLS;
        let session = self.source.take().ok_or(ExpressionError::Pending)?;
        let scan = MemberSearch::new(session, ordinal, request, &self.plan.pattern, options,
            self.skip_matches.saturating_sub(self.seen), self.count_all_matches, false,
            budget, [self.allocations[1], self.allocations[2]])?;
        self.stats.primary_scans += 1;
        self.stats.predicate_scans += self.plan.predicate_count();
        self.active = Some(scan); Ok(())
    }
    pub fn finish(self) -> Result<ExpressionReport, ExpressionError> {
        match self.state {
            ExpressionState::Pending => return Err(ExpressionError::Pending),
            ExpressionState::Canceled => return Err(ExpressionError::Canceled),
            ExpressionState::Failed(error) => return Err(error), _ => {},
        }
        Ok(ExpressionReport { archive: self.directory.digest(), generation: self.options.generation,
            discovery_complete: self.directory.discovery_complete(), state: self.state,
            hits: self.hits, seen: self.seen, matching_files: self.matching_files,
            case_sensitive: self.plan.case_sensitive, count_all_matches: self.count_all_matches,
            skip_matches: self.skip_matches,
            stats: self.stats, _lease: self._lease })
    }
}

pub struct ExpressionReport {
    archive: Sha256Digest, generation: QueryGeneration, discovery_complete: bool,
    state: ExpressionState, hits: Vec<ExpressionHit>, seen: u64, matching_files: usize,
    case_sensitive: bool, count_all_matches: bool, skip_matches: u64, stats: ExpressionStats, _lease: ResourceLease,
}
impl ExpressionReport {
    pub fn hits(&self) -> &[ExpressionHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.seen }
    pub const fn skip_matches(&self) -> u64 { self.skip_matches }
    pub fn skipped_matches(&self) -> u64 { self.skip_matches.min(self.seen) }
    /// Qualified occurrences after this window were omitted. This is count
    /// evidence, independent of whether any rows can advance a continuation.
    /// False leaves later matches unknown when input coverage is incomplete.
    pub fn has_more_matches(&self) -> bool {
        self.seen.saturating_sub(self.skipped_matches()) > self.hits.len() as u64
    }
    /// A continuation must retain rows and move beyond the requested offset.
    /// Count-only and zero-row existence probes never advertise an unchanged
    /// offset, even when they establish that omitted matches exist.
    pub fn next_match_offset(&self) -> Option<u64> {
        if !self.has_more_matches() || self.hits.is_empty() { return None; }
        self.skipped_matches().checked_add(self.hits.len() as u64)
            .filter(|next| *next > self.skip_matches)
    }
    pub const fn case_sensitive(&self) -> bool { self.case_sensitive }
    pub const fn realized_mode(&self) -> &'static str { realized_mode(self.case_sensitive) }
    pub const fn count_all_matches(&self) -> bool { self.count_all_matches }
    pub fn hits_truncated(&self) -> bool { self.seen > self.hits.len() as u64 }
    pub const fn matching_files(&self) -> usize { self.matching_files }
    pub fn retained_matching_files(&self) -> usize {
        self.hits.iter().enumerate().filter(|(position, hit)|
            *position == 0 || self.hits[*position - 1].ordinal != hit.ordinal).count()
    }
    pub const fn stats(&self) -> ExpressionStats { self.stats }
    pub const fn state(&self) -> ExpressionState { self.state }
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, ExpressionState::Complete) && self.discovery_complete
            && self.stats.unavailable_files == 0 && self.stats.unsupported_files == 0
    }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub fn validate_delivery(&self, archive: Sha256Digest, generation: QueryGeneration) -> Result<(), ExpressionError> {
        if self.archive != archive || self.generation != generation { return Err(ExpressionError::StaleQuery); }
        Ok(())
    }
}

const fn realized_mode(case_sensitive: bool) -> &'static str {
    if case_sensitive { "decoded-text-expression" } else { "unicode-lowercase-expression" }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2670).unwrap() }
    fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
    #[test]
    fn parse_and_pattern_admission_are_atomic() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(4 * 1024 * 1024)).unwrap();
        assert!(ExpressionPlan::parse(owner(), "\"unterminated", &budget, allocation(1), allocation(10)).is_err());
        assert_eq!(budget.accounting().reserved().get(), 0);
        assert!(ExpressionPlan::parse(owner(), "needle required -excluded", &budget, allocation(11), allocation(10)).is_err());
        assert_eq!(budget.accounting().reserved().get(), 0);
        let plan = ExpressionPlan::parse(owner(), "\"pub fn\" Result -unsafe path:src/ lang:rust", &budget, allocation(1), allocation(10)).unwrap();
        assert_eq!(plan.syntax().primary_needle, "pub fn");
        assert_eq!(plan.predicate_count(), 2);
        assert!(plan.pattern.is_expression());
        assert!(plan.admits(b"src/lib.rs")); assert!(!plan.admits(b"src/lib.py"));
        drop(plan); assert_eq!(budget.accounting().reserved().get(), 0);
    }
    #[test]
    fn metadata_keys_follow_existing_capture_filter_semantics() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(4 * 1024 * 1024)).unwrap();
        let plan = ExpressionPlan::parse(owner(), "needle path:src/ -path:tests lang:rs", &budget, allocation(1), allocation(10)).unwrap();
        assert!(plan.admits(b"src/\xff.rs"));
        assert!(!plan.admits(b"src/tests/\xff.rs"));
        assert!(!plan.admits(b"src/\xff.py"));
    }
}
