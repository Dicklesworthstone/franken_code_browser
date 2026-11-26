#![forbid(unsafe_code)]

//! Movable, byte-bounded verification of one immutable whole-file capture.
//! A capture's Arc backing is retained without copying source bytes. Query and
//! decoder state survive scheduling pauses; a pause is never treated as EOF.
//! The existing byte/normalized matchers, decoder maps and expression predicates
//! own matching semantics. No runtime, filesystem access or thread is created.

use std::mem::size_of;

use fcb_core::{ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::CompleteCapture;

use crate::expression::Predicates;
use crate::normalized::SourceScalar;
use crate::stream::{ByteSearchCursor, MAX_STREAM_BATCH_HITS};
use crate::text::CapturedTextScan;
use crate::{
    CaptureQueryScanner, ParsedQuery, QueryError, QueryOptions, ReferenceScanOracle,
    SearchCoverage, SearchDocument, SearchMatch, SearchMode, SearchResult,
};

/// A hard scheduling quantum, independently of the query's total byte budget.
/// Metadata filtering and query-sized initialization are separate bounded work.
pub const MAX_CAPTURE_QUERY_STEP_BYTES: usize = 65_536;

/// Exact verification that can yield within a single large captured file.
/// Source ownership remains with the caller's admitted capture, shared here by
/// Arc. Managed callers reserve scratch_bytes() plus their bounded output before
/// construction and keep that reservation alive until this cursor is dropped.
///
/// Results become available only at a terminal state. In particular, tentative
/// primary hits cannot escape before a required term is observed or an exclusion
/// is disproved at real EOF. Result windows/counting have the same semantics as
/// CaptureQueryScanner::scan_document_window_with_cancel.
pub struct CaptureQueryScan {
    capture: CompleteCapture,
    state: State,
}
enum State {
    Raw(RawCaptureScan),
    Text(CapturedTextScan),
    Terminal(SearchResult),
    Failed,
}
impl CaptureQueryScan {
    pub fn new(
        document: &SearchDocument<'_>,
        query: &ParsedQuery,
        options: &QueryOptions,
        result_offset: u64,
        count_all_matches: bool,
    ) -> Result<Self, QueryError> {
        ReferenceScanOracle::scan_collection(&[], query, options)?;
        if document.file_id != document.capture.request().file()
            || document.file_id.owner() != options.generation.owner()
        {
            return Err(QueryError::InvalidRange);
        }
        crate::validate_whole_capture(document.capture.request())?;
        let state =
            if !ReferenceScanOracle::matches_path_filters(document.path, &query.path_filters)
                || !ReferenceScanOracle::matches_lang_filters(document.path, &query.lang_filters)
            {
                State::Terminal(SearchResult::empty(SearchCoverage::Exhaustive))
            } else {
                let predicates =
                    if query.conjunction_terms.is_empty() && query.exclusion_terms.is_empty() {
                        None
                    } else {
                        Some(Predicates::new_owned(query, options)?)
                    };
                let revision = document.capture.request().revision();
                match options.mode {
                    SearchMode::RawBytes => State::Raw(RawCaptureScan::new(
                        document.file_id,
                        revision,
                        &query.primary_needle,
                        options,
                        count_all_matches,
                        result_offset,
                        predicates,
                    )?),
                    SearchMode::DecodedText { .. } => State::Text(CapturedTextScan::new(
                        document.file_id,
                        revision,
                        &query.primary_needle,
                        options,
                        count_all_matches,
                        result_offset,
                        predicates,
                    )?),
                }
            };
        Ok(Self {
            capture: document.capture.clone(),
            state,
        })
    }

    /// Conservative temporary/retained matcher capacity, excluding output rows
    /// and the separately admitted shared capture. This covers decoder windows,
    /// map overlap and owned primary/predicate patterns before any allocation.
    pub fn scratch_bytes(query: &ParsedQuery, options: &QueryOptions) -> Result<usize, QueryError> {
        let predicates = CaptureQueryScanner::predicate_scratch_bytes(query, options)?;
        let owned_terms = query
            .conjunction_terms
            .iter()
            .chain(&query.exclusion_terms)
            .try_fold(0usize, |n, term| n.checked_add(term.len()))
            .ok_or(QueryError::LimitExceeded)?;
        let normalized =
            crate::normalized::matched_text_capacity(&query.primary_needle, options.mode)?;
        let primary = query
            .primary_needle
            .len()
            .checked_mul(4 + 2 * size_of::<usize>())
            .and_then(|n| {
                normalized
                    .checked_mul(
                        2 * (size_of::<SourceScalar>() + size_of::<usize>() + size_of::<char>()),
                    )
                    .and_then(|m| n.checked_add(m))
            })
            .ok_or(QueryError::LimitExceeded)?;
        (8usize * 1024 * 1024)
            .checked_add(primary)
            .and_then(|n| n.checked_add(predicates))
            .and_then(|n| n.checked_add(owned_terms))
            .ok_or(QueryError::LimitExceeded)
    }

    pub fn is_terminal(&self) -> bool {
        match &self.state {
            State::Raw(scan) => scan.terminal,
            State::Text(scan) => scan.is_terminal(),
            State::Terminal(_) | State::Failed => true,
        }
    }

    pub fn scanned_bytes(&self) -> u64 {
        match &self.state {
            State::Raw(scan) => scan.result.scanned_bytes,
            State::Text(scan) => scan.result().scanned_bytes,
            State::Terminal(result) => result.scanned_bytes,
            State::Failed => 0,
        }
    }

    /// Advance at most max_bytes original bytes, additionally capped by the
    /// hard per-call quantum. Zero leaves nonterminal work untouched. Returns
    /// actual admitted bytes; repeated terminal steps do not rescan.
    pub fn step(
        &mut self,
        max_bytes: usize,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<usize, QueryError> {
        if matches!(&self.state, State::Failed) {
            return Err(QueryError::InvalidRange);
        }
        if self.is_terminal() {
            return Ok(0);
        }
        // Move state out before invoking an arbitrary host callback: a panic
        // cannot leave a partially advanced cursor resumable.
        let mut state = std::mem::replace(&mut self.state, State::Failed);
        let bytes = max_bytes.min(MAX_CAPTURE_QUERY_STEP_BYTES);
        let consumed = match &mut state {
            State::Raw(scan) => scan.step(self.capture.bytes(), bytes, &mut canceled)?,
            State::Text(scan) => scan.step(self.capture.bytes(), bytes, &mut canceled)?,
            State::Terminal(_) => 0,
            State::Failed => return Err(QueryError::InvalidRange),
        };
        self.state = state;
        Ok(consumed)
    }

    pub fn result(&self) -> Option<&SearchResult> {
        match &self.state {
            State::Raw(scan) if scan.terminal => Some(&scan.result),
            State::Text(scan) if scan.is_terminal() => Some(scan.result()),
            State::Terminal(result) => Some(result),
            _ => None,
        }
    }

    pub fn into_result(self) -> Result<SearchResult, QueryError> {
        if !self.is_terminal() {
            return Err(QueryError::InvalidRange);
        }
        match self.state {
            State::Raw(scan) => Ok(scan.result),
            State::Text(scan) => Ok(scan.into_result()),
            State::Terminal(result) => Ok(result),
            State::Failed => Err(QueryError::InvalidRange),
        }
    }
}

struct RawCaptureScan {
    file: FileId,
    revision: SourceRevision,
    cursor: ByteSearchCursor<'static>,
    needle: String,
    options: QueryOptions,
    count_all: bool,
    skip: u64,
    predicates: Option<Predicates<'static>>,
    primary_limited: bool,
    terminal: bool,
    result: SearchResult,
}
impl RawCaptureScan {
    fn new(
        file: FileId,
        revision: SourceRevision,
        needle: &str,
        options: &QueryOptions,
        count_all: bool,
        skip: u64,
        predicates: Option<Predicates<'static>>,
    ) -> Result<Self, QueryError> {
        Ok(Self {
            file,
            revision,
            cursor: ByteSearchCursor::new_owned(needle.as_bytes())?,
            needle: needle.to_owned(),
            options: options.clone(),
            count_all,
            skip,
            predicates,
            primary_limited: false,
            terminal: false,
            result: SearchResult::empty(SearchCoverage::Exhaustive),
        })
    }
    fn finish(&mut self, complete: bool) {
        if let Some(predicates) = &self.predicates {
            predicates.finish(
                &mut self.result,
                complete,
                self.primary_limited,
                self.options.max_matches,
            );
        }
        self.terminal = true;
    }
    fn step(
        &mut self,
        bytes: &[u8],
        max_bytes: usize,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<usize, QueryError> {
        let before = self.result.scanned_bytes;
        if canceled() {
            self.result.coverage = SearchCoverage::CanceledEarly;
            self.finish(false);
            return Ok(0);
        }
        if max_bytes == 0 {
            return Ok(0);
        }
        if !self.count_all && self.options.max_matches == 0 && !bytes.is_empty() {
            self.result.coverage = SearchCoverage::TruncatedAtLimit { max_matches: 0 };
            self.finish(false);
            return Ok(0);
        }
        let total = bytes.len() as u64;
        let admitted = self.options.max_bytes_scanned.unwrap_or(total).min(total);
        let end = before.saturating_add(max_bytes as u64).min(admitted);
        while self.result.scanned_bytes < end {
            if canceled() {
                self.result.coverage = SearchCoverage::CanceledEarly;
                self.finish(false);
                break;
            }
            let offset = self.result.scanned_bytes as usize;
            let remaining = &bytes[offset..end as usize];
            if self.primary_limited {
                let predicates = self.predicates.as_mut().ok_or(QueryError::InvalidRange)?;
                self.result.scanned_bytes += remaining.len() as u64;
                if !predicates.feed_raw(remaining, canceled)? {
                    self.result.coverage = SearchCoverage::CanceledEarly;
                    self.finish(false);
                    break;
                }
                if predicates.rejected() || predicates.proven_without_eof() {
                    self.finish(false);
                    break;
                }
                continue;
            }
            let hit_budget = if self.count_all {
                MAX_STREAM_BATCH_HITS
            } else {
                self.options
                    .max_matches
                    .saturating_sub(self.result.matches.len())
                    .saturating_add(1)
                    .min(MAX_STREAM_BATCH_HITS)
            };
            let batch = self.cursor.step(remaining, remaining.len(), hit_budget)?;
            self.result.scanned_bytes = self.cursor.scanned_bytes();
            if let Some(predicates) = &mut self.predicates {
                if !predicates.feed_raw(&remaining[..batch.consumed], canceled)? {
                    self.result.coverage = SearchCoverage::CanceledEarly;
                    self.finish(false);
                    break;
                }
                if predicates.rejected() {
                    self.finish(false);
                    break;
                }
            }
            for hit in batch.hits {
                self.result.total_matches_counted = self
                    .result
                    .total_matches_counted
                    .checked_add(1)
                    .ok_or(QueryError::LimitExceeded)?;
                if self.result.total_matches_counted as u64 <= self.skip {
                    continue;
                }
                if self.result.matches.len() == self.options.max_matches {
                    if self.count_all {
                        continue;
                    }
                    if self.predicates.is_some() {
                        self.primary_limited = true;
                        break;
                    }
                    self.result.coverage = SearchCoverage::TruncatedAtLimit {
                        max_matches: self.options.max_matches,
                    };
                    self.finish(false);
                    break;
                }
                self.result
                    .matches
                    .try_reserve_exact(1)
                    .map_err(|_| QueryError::LimitExceeded)?;
                self.result.matches.push(SearchMatch {
                    occurrence_id: hit.occurrence_id,
                    file_id: self.file,
                    revision: self.revision,
                    decoded_range: None,
                    original_byte_range: ByteRange::new(
                        ByteOffset::new(hit.start),
                        ByteOffset::new(hit.end),
                    )
                    .map_err(|_| QueryError::InvalidRange)?,
                    matched_text: self.needle.clone(),
                    multiplicity: 1,
                });
            }
            if self.terminal {
                break;
            }
            if self.primary_limited
                && self
                    .predicates
                    .as_ref()
                    .is_some_and(Predicates::proven_without_eof)
            {
                self.finish(false);
                break;
            }
        }
        if !self.terminal {
            if self.result.scanned_bytes == total {
                self.finish(true);
            } else if self.result.scanned_bytes == admitted {
                self.result.coverage = SearchCoverage::BudgetExhausted {
                    bytes_scanned: admitted,
                };
                self.finish(false);
            }
        }
        Ok((self.result.scanned_bytes - before) as usize)
    }
}
