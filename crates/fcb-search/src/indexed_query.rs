#![forbid(unsafe_code)]

//! Progressive exact verification over an immutable index and source manifest.
//! No background threads, filesystem access, or candidate-universe allocation.
//! Steps admit a bounded number of documents. The existing scanner enforces the
//! remaining operation byte budget and checks cancellation within exact scans.
//! A step is a worker operation, NOT a fixed-duration UI callback: one admitted
//! document may require multiple scanner quanta.

use std::mem::size_of;

use fcb_core::{ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};

use crate::index::{EphemeralIndex, IndexError, MembershipState, SearchManifestId};
use crate::{ParsedQuery, QueryOptions, ReferenceScanOracle, SearchCoverage, SearchMatch, SearchMode, SearchResult, UnicodeNormalization};

pub const MAX_QUERY_STEP_DOCUMENTS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexedQueryState { Running, Finished, Canceled, Failed }

/// Query results retain their output reservation, exact source-universe and
/// query identities. Buffer fullness, examined membership, missing captures,
/// unavailable decoding, and terminal execution are independent facts.
#[derive(Debug)]
pub struct IndexedSearchReport<'source> {
    manifest: SearchManifestId,
    generation: QueryGeneration,
    membership: MembershipState,
    unavailable: &'source [FileId],
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

impl<'source> IndexedSearchReport<'source> {
    pub const fn manifest(&self) -> SearchManifestId { self.manifest }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn membership(&self) -> MembershipState { self.membership }
    pub const fn unavailable_files(&self) -> &'source [FileId] { self.unavailable }
    pub const fn known_files(&self) -> usize { self.known_files }
    pub const fn examined_files(&self) -> usize { self.examined_files }
    pub const fn skipped_by_index(&self) -> usize { self.skipped_by_index }
    pub const fn excluded_by_path(&self) -> usize { self.excluded_by_path }
    pub const fn scan_attempts(&self) -> usize { self.scan_attempts }
    pub const fn fallback_attempts(&self) -> usize { self.fallback_attempts }
    pub const fn state(&self) -> IndexedQueryState { self.state }
    pub const fn failure(&self) -> Option<IndexError> { self.failure }
    /// Results for examined captures. Use this report's is_complete(), not the
    /// capture result alone, to assess the entire requested source universe.
    pub const fn capture_results(&self) -> &SearchResult { &self.results }
    pub fn is_complete(&self) -> bool {
        self.state == IndexedQueryState::Finished
            && self.membership == MembershipState::Closed
            && self.unavailable.is_empty()
            && self.examined_files == self.known_files
            && self.results.is_complete()
    }
    pub fn is_terminal(&self) -> bool { self.state != IndexedQueryState::Running }
    /// Validate at delivery time, not merely when a worker started. A replaced
    /// query/manifest cannot use an old batch as current navigation state.
    pub fn validate_delivery(
        &self, manifest: SearchManifestId, generation: QueryGeneration,
    ) -> Result<(), IndexError> {
        if manifest != self.manifest || generation != self.generation {
            return Err(IndexError::StaleQuery);
        }
        Ok(())
    }
    /// Reservation includes retained output and bounded child-result overlap;
    /// the borrowed source captures and source-decoder scratch are separate.
    pub fn reserved_output_bytes(&self) -> u64 { self._lease.info().bytes().get() }
}

/// Owns query execution only. Borrowing the index and AST prevents either from
/// changing under this operation. New captures require a new manifest/index;
/// old readers continue to resolve exactly the bytes they started with.
pub struct IndexedQuery<'index, 'source, 'query> {
    index: &'index EphemeralIndex<'source>,
    query: &'query ParsedQuery,
    options: QueryOptions,
    next_document: usize,
    hit_capacity: usize,
    text_capacity_per_hit: usize,
    report: IndexedSearchReport<'source>,
}

impl<'index, 'source, 'query> IndexedQuery<'index, 'source, 'query> {
    pub fn new(
        index: &'index EphemeralIndex<'source>,
        query: &'query ParsedQuery,
        options: QueryOptions,
        budget: &ResourceBudget,
        allocation: ResourceAllocationId,
    ) -> Result<Self, IndexError> {
        index.validate_options(&options)?;
        // Reuse the authoritative AST validation even for an empty manifest
        // or a query whose primary term will be rejected by every segment.
        ReferenceScanOracle::scan_collection(&[], query, &options)?;
        let manifest = index.manifest();
        let docs = manifest.documents();
        let mut hit_capacity = 0usize;
        let mut largest_capture = 0usize;
        for doc in docs {
            hit_capacity = hit_capacity.saturating_add(doc.capture.bytes().len()).min(options.max_matches);
            largest_capture = largest_capture.max(doc.capture.bytes().len());
        }
        let text_capacity_per_hit = match options.mode {
            SearchMode::RawBytes | SearchMode::DecodedText {
                case_sensitive: true, normalization: UnicodeNormalization::Exact,
            } => query.primary_needle.len(),
            // UTF-16 -> UTF-8 and the existing normalized source-span copy are
            // bounded by three output bytes per source byte, conservatively.
            _ => largest_capture.checked_mul(3).ok_or(IndexError::LimitExceeded)?,
        };
        let retained = output_bytes(hit_capacity, docs.len(), text_capacity_per_hit)?;
        // Child verification and accumulated output overlap. This does not
        // claim to account for existing source-decoder/normalizer scratch.
        let reserved = retained.checked_mul(3).ok_or(IndexError::LimitExceeded)?;
        let lease = budget.try_reserve_managed(manifest.id().owner(), allocation, ByteLength::new(reserved))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut results = SearchResult::empty(SearchCoverage::Exhaustive);
        results.matches.try_reserve_exact(hit_capacity).map_err(|_| IndexError::AllocationFailed)?;
        results.unsupported_files.try_reserve_exact(docs.len()).map_err(|_| IndexError::AllocationFailed)?;
        if results.matches.capacity() > hit_capacity || results.unsupported_files.capacity() > docs.len() {
            return Err(IndexError::ResourceDenied);
        }
        let report = IndexedSearchReport {
            manifest: manifest.id(), generation: options.generation, membership: manifest.membership(),
            unavailable: manifest.unavailable(), known_files: docs.len() + manifest.unavailable().len(),
            examined_files: 0, skipped_by_index: 0, excluded_by_path: 0,
            scan_attempts: 0, fallback_attempts: 0, state: IndexedQueryState::Running,
            failure: None, results, _lease: lease,
        };
        Ok(Self { index, query, options, next_document: 0, hit_capacity, text_capacity_per_hit, report })
    }

    pub fn report(&self) -> &IndexedSearchReport<'source> { &self.report }

    pub fn cancel(&mut self) {
        if self.report.state == IndexedQueryState::Running {
            self.report.state = IndexedQueryState::Canceled;
            self.report.results.coverage = SearchCoverage::CanceledEarly;
        }
    }

    /// Admit up to min(document_budget, MAX_QUERY_STEP_DOCUMENTS) source members.
    /// Call again to continue. Zero admits nothing. Hits append in stable
    /// FileId/source-occurrence order; existing rows are never reranked.
    pub fn step(
        &mut self,
        document_budget: usize,
        active_generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<&IndexedSearchReport<'source>, IndexError> {
        if active_generation != self.options.generation {
            self.cancel();
            return Err(IndexError::StaleQuery);
        }
        if canceled() { self.cancel(); }
        if self.report.is_terminal() || document_budget == 0 { return Ok(&self.report); }
        let end = self.next_document.saturating_add(document_budget.min(MAX_QUERY_STEP_DOCUMENTS))
            .min(self.index.manifest().documents().len());
        while self.next_document < end {
            if canceled() { self.cancel(); break; }
            let ordinal = self.next_document;
            let outcome = self.visit(ordinal, &mut canceled);
            if let Err(error) = outcome {
                self.report.state = IndexedQueryState::Failed;
                self.report.failure = Some(error);
                return Err(error);
            }
            self.next_document += 1;
            self.report.examined_files += 1;
            if self.report.is_terminal() { break; }
        }
        if canceled() { self.cancel(); }
        if self.report.state == IndexedQueryState::Running
            && self.next_document == self.index.manifest().documents().len() {
            self.report.state = IndexedQueryState::Finished;
        }
        Ok(&self.report)
    }

    pub fn run_to_completion(
        &mut self, mut canceled: impl FnMut() -> bool,
    ) -> Result<&IndexedSearchReport<'source>, IndexError> {
        while !self.report.is_terminal() {
            self.step(MAX_QUERY_STEP_DOCUMENTS, self.options.generation, &mut canceled)?;
        }
        Ok(&self.report)
    }

    /// Taking an unfinished report does not promote it to complete.
    pub fn into_report(self) -> IndexedSearchReport<'source> { self.report }

    fn visit(&mut self, ordinal: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), IndexError> {
        let doc = &self.index.manifest().documents()[ordinal];
        if !ReferenceScanOracle::matches_path_filters(doc.path, &self.query.path_filters)
            || !ReferenceScanOracle::matches_lang_filters(doc.path, &self.query.lang_filters) {
            self.report.excluded_by_path += 1;
            return Ok(());
        }
        // A zero result limit intentionally performs no nonempty source scan;
        // preserve that existing API behavior instead of changing it by indexing.
        if self.options.max_matches > 0 && !self.index.may_match(ordinal, self.query, &self.options)? {
            self.report.skipped_by_index += 1;
            return Ok(());
        }
        if self.index.uses_fallback(ordinal, &self.options, self.query.primary_needle.len()) {
            self.report.fallback_attempts += 1;
        }
        self.report.scan_attempts += 1;
        let retained = self.report.results.matches.len();
        let remaining = self.options.max_matches.saturating_sub(retained);
        let mut options = self.options.clone();
        options.max_matches = if self.options.max_matches == 0 { 0 } else { remaining.max(1) };
        options.max_bytes_scanned = self.options.max_bytes_scanned
            .map(|limit| limit.saturating_sub(self.report.results.scanned_bytes));
        let child = ReferenceScanOracle::scan_document_with_cancel(doc, self.query, &options, canceled)?;
        let scanned = self.report.results.scanned_bytes.checked_add(child.scanned_bytes)
            .ok_or(IndexError::LimitExceeded)?;
        if self.options.max_bytes_scanned.is_some_and(|limit| scanned > limit) {
            return Err(IndexError::LimitExceeded);
        }
        let counted = child.total_matches_counted.min(remaining.saturating_add(1));
        let total = self.report.results.total_matches_counted.checked_add(counted)
            .ok_or(IndexError::LimitExceeded)?;
        let take = remaining.min(child.matches.len());
        if take > self.hit_capacity.saturating_sub(retained)
            || child.matches.iter().take(take).any(|hit| hit.matched_text.capacity() > self.text_capacity_per_hit)
            || child.unsupported_files.len() > self.index.manifest().documents().len()
                .saturating_sub(self.report.results.unsupported_files.len()) {
            return Err(IndexError::ResourceDenied);
        }
        self.report.results.scanned_bytes = scanned;
        self.report.results.total_matches_counted = total;
        self.report.results.matches.extend(child.matches.into_iter().take(take));
        self.report.results.unsupported_files.extend(child.unsupported_files);
        if child.coverage == SearchCoverage::CanceledEarly {
            self.cancel();
        } else if total > self.options.max_matches
            || matches!(child.coverage, SearchCoverage::TruncatedAtLimit { .. }) {
            self.report.results.coverage = SearchCoverage::TruncatedAtLimit { max_matches: self.options.max_matches };
            self.report.state = IndexedQueryState::Finished;
        } else if matches!(child.coverage, SearchCoverage::BudgetExhausted { .. }) {
            self.report.results.coverage = SearchCoverage::BudgetExhausted { bytes_scanned: scanned };
            self.report.state = IndexedQueryState::Finished;
        }
        Ok(())
    }
}

impl<'source> EphemeralIndex<'source> {
    pub fn search(
        &self, query: &ParsedQuery, options: QueryOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool,
    ) -> Result<IndexedSearchReport<'source>, IndexError> {
        let mut session = IndexedQuery::new(self, query, options, budget, allocation)?;
        session.run_to_completion(canceled)?;
        Ok(session.into_report())
    }
}

fn output_bytes(hits: usize, files: usize, text: usize) -> Result<u64, IndexError> {
    let bytes = size_of::<SearchMatch>().checked_add(text)
        .and_then(|per_hit| per_hit.checked_mul(hits))
        .and_then(|n| files.checked_mul(size_of::<FileId>()).and_then(|f| n.checked_add(f)))
        .and_then(|n| n.checked_add(size_of::<IndexedSearchReport<'_>>()))
        .ok_or(IndexError::LimitExceeded)?;
    u64::try_from(bytes).map_err(|_| IndexError::LimitExceeded)
}
