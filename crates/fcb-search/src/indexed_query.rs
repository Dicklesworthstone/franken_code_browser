#![forbid(unsafe_code)]

//! Progressive exact verification over immutable prepared source universes.
//! Borrowed and independently movable queries share the same execution state
//! machine. A step admits bounded documents, not a fixed-duration UI callback.
//! No filesystem calls, runtime, or repository-sized candidate list is created.

use std::{borrow::Cow, mem::size_of, sync::Arc};
use fcb_core::{ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::index::{EphemeralIndex, IndexError, MembershipState, SearchManifestId};
use crate::index::export::OwnedEphemeralIndex;
use crate::{LangFilterKind, ParsedQuery, PathFilterKind, QueryOptions, ReferenceScanOracle,
    SearchCoverage, SearchDocument, SearchMatch, SearchMode, SearchResult, UnicodeNormalization};

pub const MAX_QUERY_STEP_DOCUMENTS: usize = 256;
/// Includes retained AST vector/string capacities, not just their lengths.
pub const MAX_OWNED_QUERY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexedQueryState { Running, Finished, Canceled, Failed }

/// Result storage owns its reservation. Borrowed queries borrow unavailable
/// membership; movable queries own a separately admitted copy of those IDs.
/// Source bytes are NOT owned by the report: exact activation still needs the
/// named retained capture. Moving a report never confers filesystem authority.
#[derive(Debug)]
pub struct IndexedSearchReport<'source> {
    manifest: SearchManifestId,
    generation: QueryGeneration,
    membership: MembershipState,
    unavailable: Cow<'source, [FileId]>,
    known_files: usize,
    examined_files: usize,
    skipped_by_index: usize,
    excluded_by_path: usize,
    scan_attempts: usize,
    fallback_attempts: usize,
    state: IndexedQueryState,
    failure: Option<IndexError>,
    results: SearchResult,
    _lease: ResourceLease,
}
impl IndexedSearchReport<'_> {
    pub const fn manifest(&self) -> SearchManifestId { self.manifest }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn membership(&self) -> MembershipState { self.membership }
    pub fn unavailable_files(&self) -> &[FileId] { &self.unavailable }
    pub const fn known_files(&self) -> usize { self.known_files }
    pub const fn examined_files(&self) -> usize { self.examined_files }
    pub const fn skipped_by_index(&self) -> usize { self.skipped_by_index }
    pub const fn excluded_by_path(&self) -> usize { self.excluded_by_path }
    pub const fn scan_attempts(&self) -> usize { self.scan_attempts }
    pub const fn fallback_attempts(&self) -> usize { self.fallback_attempts }
    pub const fn state(&self) -> IndexedQueryState { self.state }
    pub const fn failure(&self) -> Option<IndexError> { self.failure }
    pub const fn capture_results(&self) -> &SearchResult { &self.results }
    pub fn is_complete(&self) -> bool {
        self.state == IndexedQueryState::Finished && self.membership == MembershipState::Closed
            && self.unavailable.is_empty() && self.examined_files == self.known_files
            && self.results.is_complete()
    }
    pub fn is_terminal(&self) -> bool { self.state != IndexedQueryState::Running }
    pub fn validate_delivery(&self, manifest: SearchManifestId, generation: QueryGeneration)
        -> Result<(), IndexError> {
        if manifest != self.manifest || generation != self.generation { return Err(IndexError::StaleQuery); }
        Ok(())
    }
    pub fn reserved_output_bytes(&self) -> u64 { self._lease.info().bytes().get() }
}

/// Internal access to two OWNERSHIP forms of the same prepared index. No
/// alternative matcher or segment format is selected by this trait.
pub(crate) trait QuerySource {
    fn id(&self) -> SearchManifestId;
    fn membership(&self) -> MembershipState;
    fn count(&self) -> usize;
    fn document(&self, ordinal: usize) -> SearchDocument<'_>;
    fn may_match(&self, ordinal: usize, query: &ParsedQuery, options: &QueryOptions) -> Result<bool, IndexError>;
    fn fallback(&self, ordinal: usize, options: &QueryOptions, needle: usize) -> bool;
}
impl QuerySource for EphemeralIndex<'_> {
    fn id(&self) -> SearchManifestId { self.manifest().id() }
    fn membership(&self) -> MembershipState { self.manifest().membership() }
    fn count(&self) -> usize { self.manifest().documents().len() }
    fn document(&self, ordinal: usize) -> SearchDocument<'_> { self.manifest().documents()[ordinal].clone() }
    fn may_match(&self, ordinal: usize, query: &ParsedQuery, options: &QueryOptions) -> Result<bool, IndexError> {
        EphemeralIndex::may_match(self, ordinal, query, options)
    }
    fn fallback(&self, ordinal: usize, options: &QueryOptions, needle: usize) -> bool {
        self.uses_fallback(ordinal, options, needle)
    }
}

/// Source-independent execution state. It is never rebound to a different
/// source universe by public code; the wrappers enforce borrow or instance pins.
struct Execution<'source> {
    options: QueryOptions,
    next_document: usize,
    hit_capacity: usize,
    text_capacity_per_hit: usize,
    report: IndexedSearchReport<'source>,
}
impl<'source> Execution<'source> {
    fn new(index: &impl QuerySource, query: &ParsedQuery, options: QueryOptions,
        unavailable: Cow<'source, [FileId]>, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<Self, IndexError> {
        if options.generation.owner() != index.id().owner() { return Err(IndexError::OwnerMismatch); }
        ReferenceScanOracle::scan_collection(&[], query, &options)?;
        let mut hit_capacity = 0usize;
        let mut largest_capture = 0usize;
        for ordinal in 0..index.count() {
            if canceled() { return Err(IndexError::Canceled); }
            let doc = index.document(ordinal);
            hit_capacity = hit_capacity.saturating_add(doc.capture.bytes().len()).min(options.max_matches);
            largest_capture = largest_capture.max(doc.capture.bytes().len());
        }
        let text_capacity_per_hit = match options.mode {
            SearchMode::RawBytes | SearchMode::DecodedText {
                case_sensitive: true, normalization: UnicodeNormalization::Exact,
            } => query.primary_needle.len(),
            _ => largest_capture.checked_mul(3).ok_or(IndexError::LimitExceeded)?,
        };
        let retained = output_bytes(hit_capacity, index.count(), text_capacity_per_hit)?;
        let unavailable_bytes = match &unavailable {
            Cow::Owned(ids) => ids.capacity().checked_mul(size_of::<FileId>()).ok_or(IndexError::LimitExceeded)?,
            Cow::Borrowed(_) => 0,
        };
        let reserved = retained.checked_mul(3).and_then(|n| n.checked_add(unavailable_bytes as u64))
            .ok_or(IndexError::LimitExceeded)?;
        let lease = budget.try_reserve_managed(index.id().owner(), allocation, ByteLength::new(reserved))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut results = SearchResult::empty(SearchCoverage::Exhaustive);
        results.matches.try_reserve_exact(hit_capacity).map_err(|_| IndexError::AllocationFailed)?;
        results.unsupported_files.try_reserve_exact(index.count()).map_err(|_| IndexError::AllocationFailed)?;
        if results.matches.capacity() > hit_capacity || results.unsupported_files.capacity() > index.count() {
            return Err(IndexError::ResourceDenied);
        }
        if canceled() { return Err(IndexError::Canceled); }
        let known_files = index.count().checked_add(unavailable.len()).ok_or(IndexError::LimitExceeded)?;
        Ok(Self { options: options.clone(), next_document: 0, hit_capacity, text_capacity_per_hit,
            report: IndexedSearchReport { manifest: index.id(), generation: options.generation,
                membership: index.membership(), unavailable, known_files, examined_files: 0,
                skipped_by_index: 0, excluded_by_path: 0, scan_attempts: 0, fallback_attempts: 0,
                state: IndexedQueryState::Running, failure: None, results, _lease: lease } })
    }
    fn cancel(&mut self) {
        if self.report.state == IndexedQueryState::Running {
            self.report.state = IndexedQueryState::Canceled;
            self.report.results.coverage = SearchCoverage::CanceledEarly;
        }
    }
    fn step(&mut self, index: &impl QuerySource, query: &ParsedQuery, document_budget: usize,
        active_generation: QueryGeneration, mut canceled: impl FnMut() -> bool) -> Result<(), IndexError> {
        if active_generation != self.options.generation { self.cancel(); return Err(IndexError::StaleQuery); }
        if canceled() { self.cancel(); }
        if self.report.is_terminal() || document_budget == 0 { return Ok(()); }
        let end = self.next_document.saturating_add(document_budget.min(MAX_QUERY_STEP_DOCUMENTS)).min(index.count());
        while self.next_document < end {
            if canceled() { self.cancel(); break; }
            let ordinal = self.next_document;
            // A panic during a child verifier must not leave a partially consumed
            // candidate resumable. Ordinary success explicitly restores Running.
            self.report.state = IndexedQueryState::Failed;
            let outcome = self.visit(index, query, ordinal, &mut canceled);
            if let Err(error) = outcome {
                self.report.failure = Some(error);
                return Err(error);
            }
            if self.report.state == IndexedQueryState::Failed { self.report.state = IndexedQueryState::Running; }
            self.next_document += 1;
            self.report.examined_files += 1;
            if self.report.is_terminal() { break; }
        }
        if canceled() { self.cancel(); }
        if self.report.state == IndexedQueryState::Running && self.next_document == index.count() {
            self.report.state = IndexedQueryState::Finished;
        }
        Ok(())
    }
    fn visit(&mut self, index: &impl QuerySource, query: &ParsedQuery, ordinal: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), IndexError> {
        let doc = index.document(ordinal);
        if !ReferenceScanOracle::matches_path_filters(doc.path, &query.path_filters)
            || !ReferenceScanOracle::matches_lang_filters(doc.path, &query.lang_filters) {
            self.report.excluded_by_path += 1; return Ok(());
        }
        if self.options.max_matches > 0 && !index.may_match(ordinal, query, &self.options)? {
            self.report.skipped_by_index += 1; return Ok(());
        }
        if index.fallback(ordinal, &self.options, query.primary_needle.len()) { self.report.fallback_attempts += 1; }
        self.report.scan_attempts += 1;
        let retained = self.report.results.matches.len();
        let remaining = self.options.max_matches.saturating_sub(retained);
        let mut options = self.options.clone();
        options.max_matches = if self.options.max_matches == 0 { 0 } else { remaining.max(1) };
        options.max_bytes_scanned = self.options.max_bytes_scanned
            .map(|limit| limit.saturating_sub(self.report.results.scanned_bytes));
        let child = ReferenceScanOracle::scan_document_with_cancel(&doc, query, &options, canceled)?;
        let scanned = self.report.results.scanned_bytes.checked_add(child.scanned_bytes).ok_or(IndexError::LimitExceeded)?;
        if self.options.max_bytes_scanned.is_some_and(|limit| scanned > limit) { return Err(IndexError::LimitExceeded); }
        let counted = child.total_matches_counted.min(remaining.saturating_add(1));
        let total = self.report.results.total_matches_counted.checked_add(counted).ok_or(IndexError::LimitExceeded)?;
        let take = remaining.min(child.matches.len());
        if take > self.hit_capacity.saturating_sub(retained)
            || child.matches.iter().take(take).any(|hit| hit.matched_text.capacity() > self.text_capacity_per_hit)
            || child.unsupported_files.len() > index.count().saturating_sub(self.report.results.unsupported_files.len()) {
            return Err(IndexError::ResourceDenied);
        }
        self.report.results.scanned_bytes = scanned;
        self.report.results.total_matches_counted = total;
        self.report.results.matches.extend(child.matches.into_iter().take(take));
        self.report.results.unsupported_files.extend(child.unsupported_files);
        if child.coverage == SearchCoverage::CanceledEarly {
            self.report.state = IndexedQueryState::Canceled;
            self.report.results.coverage = SearchCoverage::CanceledEarly;
        } else if total > self.options.max_matches || matches!(child.coverage, SearchCoverage::TruncatedAtLimit { .. }) {
            self.report.results.coverage = SearchCoverage::TruncatedAtLimit { max_matches: self.options.max_matches };
            self.report.state = IndexedQueryState::Finished;
        } else if matches!(child.coverage, SearchCoverage::BudgetExhausted { .. }) {
            self.report.results.coverage = SearchCoverage::BudgetExhausted { bytes_scanned: scanned };
            self.report.state = IndexedQueryState::Finished;
        }
        Ok(())
    }
}

pub struct IndexedQuery<'index, 'source, 'query> {
    index: &'index EphemeralIndex<'source>,
    query: &'query ParsedQuery,
    execution: Execution<'source>,
}
impl<'index, 'source, 'query> IndexedQuery<'index, 'source, 'query> {
    pub fn new(index: &'index EphemeralIndex<'source>, query: &'query ParsedQuery,
        options: QueryOptions, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<Self, IndexError> {
        let execution = Execution::new(index, query, options, Cow::Borrowed(index.manifest().unavailable()),
            budget, allocation, || false)?;
        Ok(Self { index, query, execution })
    }
    pub fn report(&self) -> &IndexedSearchReport<'source> { &self.execution.report }
    pub fn cancel(&mut self) { self.execution.cancel(); }
    pub fn step(&mut self, document_budget: usize, active_generation: QueryGeneration,
        canceled: impl FnMut() -> bool) -> Result<&IndexedSearchReport<'source>, IndexError> {
        self.execution.step(self.index, self.query, document_budget, active_generation, canceled)?;
        Ok(self.report())
    }
    pub fn run_to_completion(&mut self, mut canceled: impl FnMut() -> bool)
        -> Result<&IndexedSearchReport<'source>, IndexError> {
        while !self.report().is_terminal() {
            self.step(MAX_QUERY_STEP_DOCUMENTS, self.execution.options.generation, &mut canceled)?;
        }
        Ok(self.report())
    }
    pub fn into_report(self) -> IndexedSearchReport<'source> { self.execution.report }
}

/// A movable query cursor bound to one actual owned-index instance, not merely
/// a reusable manifest number or source digest. It owns query/result state, not
/// source bytes. Supply the same index to each step; moving that index is fine.
/// Multiple cursors can independently search it. Dropping a cursor releases only
/// its own state. Step performs no descriptor-universe allocation or gram rebuild.
pub struct OwnedIndexedQuery {
    identity: Arc<()>,
    query: ParsedQuery,
    execution: Execution<'static>,
    _query_lease: ResourceLease,
}
impl OwnedIndexedQuery {
    /// Allocations are [retained AST/unavailable-ID preparation, result state].
    /// The caller transfers an already constructed AST; capacity, not length,
    /// determines retention admission. No source scan occurs during admission.
    pub fn new(index: &OwnedEphemeralIndex, query: ParsedQuery, options: QueryOptions,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, IndexError> {
        if allocations[0] == allocations[1] { return Err(IndexError::InvalidManifest); }
        if canceled() { return Err(IndexError::Canceled); }
        if options.generation.owner() != index.id().owner() { return Err(IndexError::OwnerMismatch); }
        ReferenceScanOracle::scan_collection(&[], &query, &options)?;
        let ast = query_bytes(&query)?;
        if ast > MAX_OWNED_QUERY_BYTES { return Err(IndexError::LimitExceeded); }
        let bytes = index.unavailable_files().len().checked_mul(size_of::<FileId>())
            .and_then(|n| n.checked_add(ast + size_of::<Self>() + 64)).ok_or(IndexError::LimitExceeded)?;
        let lease = budget.try_reserve_managed(index.id().owner(), allocations[0], ByteLength::new(bytes as u64))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut unavailable = Vec::new();
        unavailable.try_reserve_exact(index.unavailable_files().len()).map_err(|_| IndexError::AllocationFailed)?;
        if unavailable.capacity() > index.unavailable_files().len() { return Err(IndexError::ResourceDenied); }
        for &file in index.unavailable_files() {
            if canceled() { return Err(IndexError::Canceled); }
            unavailable.push(file);
        }
        let execution = Execution::new(index, &query, options, Cow::Owned(unavailable), budget,
            allocations[1], &mut canceled)?;
        Ok(Self { identity: Arc::clone(index.query_identity()), query, execution, _query_lease: lease })
    }
    pub fn report(&self) -> &IndexedSearchReport<'static> { &self.execution.report }
    pub fn cancel(&mut self) { self.execution.cancel(); }
    pub fn step(&mut self, index: &OwnedEphemeralIndex, document_budget: usize,
        generation: QueryGeneration, canceled: impl FnMut() -> bool)
        -> Result<&IndexedSearchReport<'static>, IndexError> {
        // Wrong instances cannot poison or resume a correctly bound cursor.
        if !Arc::ptr_eq(&self.identity, index.query_identity()) { return Err(IndexError::InvalidManifest); }
        self.execution.step(index, &self.query, document_budget, generation, canceled)?;
        Ok(self.report())
    }
    pub fn into_report(self) -> IndexedSearchReport<'static> { self.execution.report }
}
impl<'source> EphemeralIndex<'source> {
    pub fn search(&self, query: &ParsedQuery, options: QueryOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool)
        -> Result<IndexedSearchReport<'source>, IndexError> {
        let mut session = IndexedQuery::new(self, query, options, budget, allocation)?;
        session.run_to_completion(canceled)?;
        Ok(session.into_report())
    }
}
fn query_bytes(query: &ParsedQuery) -> Result<usize, IndexError> {
    let mut bytes = query.raw_query.capacity().checked_add(query.primary_needle.capacity()).ok_or(IndexError::LimitExceeded)?;
    for (capacity, size) in [(query.conjunction_terms.capacity(), size_of::<String>()),
        (query.exclusion_terms.capacity(), size_of::<String>()),
        (query.path_filters.capacity(), size_of::<PathFilterKind>()),
        (query.lang_filters.capacity(), size_of::<LangFilterKind>())] {
        bytes = capacity.checked_mul(size).and_then(|n| bytes.checked_add(n)).ok_or(IndexError::LimitExceeded)?;
    }
    for text in query.conjunction_terms.iter().chain(&query.exclusion_terms)
        .chain(query.path_filters.iter().map(|filter| match filter { PathFilterKind::Include(s) | PathFilterKind::Exclude(s) => s }))
        .chain(query.lang_filters.iter().map(|filter| match filter { LangFilterKind::Include(s) | LangFilterKind::Exclude(s) => s })) {
        bytes = bytes.checked_add(text.capacity()).ok_or(IndexError::LimitExceeded)?;
    }
    Ok(bytes)
}
fn output_bytes(hits: usize, files: usize, text: usize) -> Result<u64, IndexError> {
    let bytes = size_of::<SearchMatch>().checked_add(text).and_then(|per_hit| per_hit.checked_mul(hits))
        .and_then(|n| files.checked_mul(size_of::<FileId>()).and_then(|f| n.checked_add(f)))
        .and_then(|n| n.checked_add(size_of::<IndexedSearchReport<'_>>() + size_of::<Execution<'_>>()))
        .ok_or(IndexError::LimitExceeded)?;
    u64::try_from(bytes).map_err(|_| IndexError::LimitExceeded)
}
