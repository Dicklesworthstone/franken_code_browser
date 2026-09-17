#![forbid(unsafe_code)]

//! Multi-term saved-source queries over the existing parser, metadata filters,
//! decoder and streaming matcher. Predicates are document-wide, not line-local.
//! Each step evaluates one metadata record or one bounded matcher quantum. A
//! member's verified buffer moves between predicate scans without rereading or
//! copying its source. No runtime, source grant, or filesystem path is inferred.

use std::{io::{Cursor, Read, Seek}, mem::size_of};
use fcb_core::{ArenaOwnerId, ByteLength, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use super::{CaptureRequest, ParsedQuery, QueryError, ReferenceScanOracle, RawPath,
    ReaderSearch, StreamingNeedle, StreamReadError, StreamReadOptions, StreamReadState, StreamReadStep};
use super::paged_snapshot::{PagedSnapshot, PagedMemberData, VerifiedMember, PagedCapture,
    PagedSearchError, Sha256Digest};
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
/// Hosts reserve a noncolliding allocation interval beginning at first_pattern;
/// exactly exclusion_count + conjunction_count + 1 pattern IDs are used.
pub struct ExpressionPlan {
    owner: ArenaOwnerId,
    syntax: ParsedQuery,
    patterns: Vec<StreamingNeedle>,
    _lease: ResourceLease,
}
impl ExpressionPlan {
    pub fn parse(owner: ArenaOwnerId, query: &str, budget: &ResourceBudget,
        allocation: ResourceAllocationId, first_pattern: ResourceAllocationId)
        -> Result<Self, ExpressionError> {
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
        let mut patterns = Vec::new();
        patterns.try_reserve_exact(count).map_err(|_| ExpressionError::ResourceDenied)?;
        if patterns.capacity() > count { return Err(ExpressionError::ResourceDenied); }
        for (i, term) in syntax.exclusion_terms.iter().chain(&syntax.conjunction_terms)
            .chain(std::iter::once(&syntax.primary_needle)).enumerate() {
            let id = ResourceAllocationId::new(first_pattern.get() + i as u64)
                .map_err(|_| ExpressionError::IdentityExhausted)?;
            patterns.push(StreamingNeedle::text(owner, term, budget, id)?);
        }
        Ok(Self { owner, syntax, patterns, _lease: lease })
    }
    pub fn syntax(&self) -> &ParsedQuery { &self.syntax }
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    fn primary(&self) -> usize { self.patterns.len() - 1 }
    fn required(&self, term: usize) -> bool { term >= self.syntax.exclusion_terms.len() }
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
    /// Aggregate raw bytes consumed by ALL predicate and primary scans. A
    /// reread of retained bytes is still matching work and spends this budget.
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
    pub members_loaded: usize,
    pub loaded_bytes: u64,
    pub peak_member_bytes: usize,
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
    archive: &'archive mut PagedSnapshot<R>,
    plan: &'plan ExpressionPlan,
    index: Option<&'plan SnapshotIndex>,
    options: ExpressionOptions,
    allocations: [ResourceAllocationId; 3],
    scope_at: usize,
    next: usize,
    term: usize,
    active: Option<ReaderSearch<'plan, Cursor<VerifiedMember>>>,
    hits: Vec<ExpressionHit>,
    seen: u64,
    state: ExpressionState,
    stats: ExpressionStats,
    _lease: ResourceLease,
}
impl<'archive, 'plan, R: Read + Seek> ExpressionQuery<'archive, 'plan, R> {
    /// allocations = [result/metadata scratch, one member, one active matcher].
    /// The query owns no backing handle beyond its borrowed archive.
    pub fn new(archive: &'archive mut PagedSnapshot<R>, plan: &'plan ExpressionPlan,
        options: ExpressionOptions, budget: &ResourceBudget, allocations: [ResourceAllocationId; 3])
        -> Result<Self, ExpressionError> {
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
        Ok(Self { archive, plan, index: None, options, allocations, scope_at: 0, next: 0, term: 0,
            active: None, hits, seen: 0, state: ExpressionState::Pending, stats: ExpressionStats::default(), _lease: lease })
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
    pub const fn state(&self) -> ExpressionState { self.state }
    pub const fn stats(&self) -> ExpressionStats { self.stats }
    pub fn cancel(&mut self) { self.active = None; self.state = ExpressionState::Canceled; }
    pub fn step(&mut self, step: StreamReadStep, generation: QueryGeneration, budget: &ResourceBudget,
        mut canceled: impl FnMut() -> bool) -> Result<ExpressionState, ExpressionError> {
        self.stats.last_step_scanned_bytes = 0;
        if generation != self.options.generation { self.cancel(); return Err(ExpressionError::StaleQuery); }
        if canceled() { self.cancel(); return Err(ExpressionError::Canceled); }
        if self.state != ExpressionState::Pending { return Ok(self.state); }
        if step.max_bytes == 0 || step.max_calls == 0 || step.max_hits == 0 { return Ok(self.state); }
        let result = self.advance(step, budget, &mut canceled);
        if canceled() { self.cancel(); return Err(ExpressionError::Canceled); }
        if let Err(error) = result { self.active = None; self.state = ExpressionState::Failed(error); return Err(error); }
        Ok(self.state)
    }
    fn advance(&mut self, step: StreamReadStep, budget: &ResourceBudget,
        canceled: &mut impl FnMut() -> bool) -> Result<(), ExpressionError> {
        if self.scope_at < self.archive.directory().len() {
            let member = self.archive.directory().member(self.scope_at).ok_or(PagedSearchError::InvalidHit)?;
            self.scope_at += 1; self.stats.metadata_examined += 1;
            if self.plan.admits(member.path) {
                self.stats.scope_files += 1;
                if matches!(member.data, PagedMemberData::Unavailable(_)) { self.stats.unavailable_files += 1; }
            } else { self.stats.metadata_excluded += 1; }
            return Ok(());
        }
        if self.active.is_none() {
            if self.next == self.archive.directory().len() { self.state = ExpressionState::Complete; return Ok(()); }
            let ordinal = self.next; self.next += 1;
            let member = self.archive.directory().member(ordinal).ok_or(PagedSearchError::InvalidHit)?;
            if !self.plan.admits(member.path) || matches!(member.data, PagedMemberData::Unavailable(_)) { return Ok(()); }
            if let Some(index) = self.index {
                match index.text_decision(ordinal, Some(&self.plan.syntax.primary_needle)) {
                    IndexDecision::Excluded => { self.stats.index_eliminated += 1; return Ok(()); }
                    IndexDecision::Verify => self.stats.index_candidates += 1,
                    IndexDecision::Fallback => self.stats.index_fallbacks += 1,
                }
            }
            if member.observed_bytes > 0 && self.stats.scanned_bytes == self.options.max_scan_bytes {
                self.state = ExpressionState::WorkLimit; return Ok(());
            }
            let verified = self.archive.load(ordinal, budget, self.allocations[1], &mut *canceled)
                .map_err(PagedSearchError::from)?;
            self.stats.members_loaded += 1;
            self.stats.loaded_bytes += verified.bytes().len() as u64;
            self.stats.peak_member_bytes = self.stats.peak_member_bytes.max(verified.bytes().len());
            self.term = 0;
            self.start(verified, budget)?;
            // Member validation is distinct worker work. Do not hide a matcher
            // quantum behind this load or a second term behind one step.
            return Ok(());
        }
        let scan = self.active.as_mut().ok_or(ExpressionError::Pending)?;
        let before = scan.stats().scanned_bytes;
        let state = scan.step(step, self.options.generation, &mut *canceled)?;
        let used = scan.stats().scanned_bytes - before;
        self.stats.scanned_bytes += used; self.stats.last_step_scanned_bytes = used;
        if state == StreamReadState::Pending { return Ok(()); }
        let (cursor, report) = self.active.take().ok_or(ExpressionError::Pending)?.finish()?;
        let verified = cursor.into_inner();
        let primary = self.term == self.plan.primary();
        match report.state() {
            StreamReadState::UnsupportedText => { self.stats.unsupported_files += 1; return Ok(()); }
            StreamReadState::Canceled => return Err(ExpressionError::Canceled),
            StreamReadState::Failed(error) => return Err(error.into()),
            StreamReadState::Complete | StreamReadState::Truncated | StreamReadState::ByteLimit => {},
            _ => return Err(PagedSearchError::IncompleteInput.into()),
        }
        if primary {
            self.seen += report.matches_seen();
            for hit in report.hits() {
                self.hits.push(ExpressionHit { ordinal: verified.ordinal(), file: report.request().file(),
                    revision: report.request().revision(), generation: self.options.generation,
                    occurrence: hit.occurrence_id(), range: hit.original_range(),
                    archive: verified.archive_digest(), source: verified.source_digest() });
            }
            if report.state() == StreamReadState::Truncated { self.state = ExpressionState::Truncated; }
            else if report.state() == StreamReadState::ByteLimit { self.state = ExpressionState::WorkLimit; }
        } else {
            let found = report.matches_seen() > 0;
            let complete = report.state() == StreamReadState::Complete;
            let required = self.plan.required(self.term);
            if (found && !required) || (!found && required && complete) {
                self.stats.predicate_rejections += 1;
            } else if !found && !complete {
                self.state = ExpressionState::WorkLimit;
            } else {
                self.term += 1;
                // Drop the prior matcher's result lease before reserving the
                // same allocation ID for its successor. Source remains owned.
                drop(report);
                self.start(verified, budget)?;
            }
        }
        Ok(())
    }
    fn start(&mut self, verified: VerifiedMember, budget: &ResourceBudget) -> Result<(), ExpressionError> {
        let ordinal = verified.ordinal() as u64;
        let owner = self.options.generation.owner();
        let file = FileId::new(owner, self.options.first_file.get() + ordinal).map_err(|_| ExpressionError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, self.options.first_revision.get() + ordinal).map_err(|_| ExpressionError::IdentityExhausted)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| PagedSearchError::InvalidHit)?;
        let length = verified.bytes().len() as u64;
        let mut options = StreamReadOptions::new(self.options.generation);
        options.max_matches = if self.term == self.plan.primary() { self.options.max_matches - self.hits.len() } else { 1 };
        options.max_bytes = length.min(self.options.max_scan_bytes.saturating_sub(self.stats.scanned_bytes));
        options.max_read_calls = fcb_store::paged_snapshot::MAX_SNAPSHOT_READ_CALLS;
        let scan = ReaderSearch::new(Cursor::new(verified), request, ByteLength::new(length),
            &self.plan.patterns[self.term], options, budget, self.allocations[2])?;
        if self.term == self.plan.primary() { self.stats.primary_scans += 1; } else { self.stats.predicate_scans += 1; }
        self.active = Some(scan); Ok(())
    }
    pub fn finish(self) -> Result<ExpressionReport, ExpressionError> {
        match self.state {
            ExpressionState::Pending => return Err(ExpressionError::Pending),
            ExpressionState::Canceled => return Err(ExpressionError::Canceled),
            ExpressionState::Failed(error) => return Err(error), _ => {},
        }
        Ok(ExpressionReport { archive: self.archive.directory().digest(), generation: self.options.generation,
            discovery_complete: self.archive.directory().discovery_complete(), state: self.state,
            hits: self.hits, seen: self.seen, stats: self.stats, _lease: self._lease })
    }
}

pub struct ExpressionReport {
    archive: Sha256Digest, generation: QueryGeneration, discovery_complete: bool,
    state: ExpressionState, hits: Vec<ExpressionHit>, seen: u64, stats: ExpressionStats, _lease: ResourceLease,
}
impl ExpressionReport {
    pub fn hits(&self) -> &[ExpressionHit] { &self.hits }
    pub const fn matches_seen(&self) -> u64 { self.seen }
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
        assert_eq!(plan.patterns.len(), 3);
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
