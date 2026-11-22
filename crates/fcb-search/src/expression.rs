#![forbid(unsafe_code)]

//! Production capture queries with one source/decoder pass for all content
//! predicates. The independent reference oracle deliberately keeps its simple
//! separate scans. Metadata predicates and literal matching retain their shared
//! implementations; this module adds only document-wide eligibility state.

use std::mem::size_of;

use fcb_core::DecodedUtf8Offset;
use fcb_source::CaptureEncodingMap;

use crate::normalized::{NormalizedCursor, SourceScalar};
use crate::stream::ByteSearchCursor;
use crate::{DirectSourceScanner, ParsedQuery, QueryError, QueryOptions,
    ReferenceScanOracle, SearchCoverage, SearchDocument, SearchMode, SearchResult};

/// Exact production verification of one immutable captured document.
///
/// All terms consume the same raw-byte allowance once. Required terms must be
/// observed, and absence of exclusions requires the completed decoding pass.
/// Primary hits remain tentative until eligibility is proved. A row limit does
/// not prevent predicate evaluation; non-counting queries still report only
/// their bounded lookahead count. Complete decoded passes validate malformed
/// suffixes even after an exclusion retires matching; unavailable text does not
/// become a complete decoded negative. No runtime, filesystem or source clone
/// is used.
pub struct CaptureQueryScanner;

impl CaptureQueryScanner {
    pub fn scan_document(doc: &SearchDocument<'_>, query: &ParsedQuery,
        options: &QueryOptions) -> Result<SearchResult, QueryError> {
        Self::scan_document_with_cancel(doc, query, options, || false)
    }

    pub fn scan_document_with_cancel(doc: &SearchDocument<'_>, query: &ParsedQuery,
        options: &QueryOptions, canceled: impl FnMut() -> bool)
        -> Result<SearchResult, QueryError> {
        Self::scan(doc, query, options, false, canceled)
    }

    pub fn scan_document_counting_with_cancel(doc: &SearchDocument<'_>, query: &ParsedQuery,
        options: &QueryOptions, canceled: impl FnMut() -> bool)
        -> Result<SearchResult, QueryError> {
        Self::scan(doc, query, options, true, canceled)
    }

    /// Retain only the requested occurrence window while verifying the same
    /// complete capture. Skipped matches consume work, not output capacity.
    /// Counts include skipped occurrences; expression predicates qualify the
    /// whole document before any of its counts or rows become authoritative.
    pub fn scan_document_window_with_cancel(doc: &SearchDocument<'_>, query: &ParsedQuery,
        options: &QueryOptions, skip_matches: u64, count_all_matches: bool,
        mut canceled: impl FnMut() -> bool) -> Result<SearchResult, QueryError> {
        ReferenceScanOracle::scan_collection(&[], query, options)?;
        if doc.file_id != doc.capture.request().file() { return Err(QueryError::InvalidRange); }
        if canceled() { return Ok(SearchResult::empty(SearchCoverage::CanceledEarly)); }
        if !ReferenceScanOracle::matches_path_filters(doc.path, &query.path_filters)
            || !ReferenceScanOracle::matches_lang_filters(doc.path, &query.lang_filters) {
            return Ok(SearchResult::empty(SearchCoverage::Exhaustive));
        }
        let predicates = if query.conjunction_terms.is_empty() && query.exclusion_terms.is_empty() {
            None
        } else { Some(Predicates::new(query, options)?) };
        let bytes = doc.capture.bytes();
        let revision = doc.capture.request().revision();
        match options.mode {
            SearchMode::RawBytes => crate::capture::scan_raw_window(bytes, doc.file_id, revision,
                query.primary_needle.as_bytes(), options, count_all_matches, skip_matches, predicates, canceled),
            SearchMode::DecodedText { .. } => crate::text::scan_text_window(bytes, doc.file_id, revision,
                &query.primary_needle, options, count_all_matches, skip_matches, predicates, canceled),
        }
    }

    /// Additional simultaneous predicate capacity beyond the ordinary single
    /// needle scanner. Managed callers reserve this before entering a scan.
    /// The bound depends on the query, never on captured source size. It covers
    /// cursor/vector capacity and conservative growth overlap for transformed
    /// needles and their bounded source rings.
    pub fn predicate_scratch_bytes(query: &ParsedQuery, options: &QueryOptions)
        -> Result<usize, QueryError> {
        ReferenceScanOracle::scan_collection(&[], query, options)?;
        let mut bytes = 0usize;
        for term in query.conjunction_terms.iter().chain(&query.exclusion_terms) {
            let normalized = !matches!(options.mode, SearchMode::RawBytes | SearchMode::DecodedText {
                case_sensitive: true, normalization: crate::UnicodeNormalization::Exact,
            });
            let per_term = if normalized {
                let units = crate::normalized::matched_text_capacity(term, options.mode)? / 4;
                units.checked_mul(4 * size_of::<char>() + 2 * size_of::<usize>()
                    + 4 * size_of::<SourceScalar>())
            } else {
                term.len().checked_mul(2 * size_of::<usize>())
            }.and_then(|n| n.checked_add(2 * size_of::<Predicate<'_>>()))
                .ok_or(QueryError::LimitExceeded)?;
            bytes = bytes.checked_add(per_term).ok_or(QueryError::LimitExceeded)?;
        }
        Ok(bytes)
    }

    fn scan(doc: &SearchDocument<'_>, query: &ParsedQuery, options: &QueryOptions,
        count_all_matches: bool, mut canceled: impl FnMut() -> bool)
        -> Result<SearchResult, QueryError> {
        // Public ASTs can be built directly. Keep the oracle's validation and
        // metadata contracts without executing its per-term content scans.
        ReferenceScanOracle::scan_collection(&[], query, options)?;
        if doc.file_id != doc.capture.request().file() { return Err(QueryError::InvalidRange); }
        if canceled() { return Ok(SearchResult::empty(SearchCoverage::CanceledEarly)); }
        if !ReferenceScanOracle::matches_path_filters(doc.path, &query.path_filters)
            || !ReferenceScanOracle::matches_lang_filters(doc.path, &query.lang_filters) {
            return Ok(SearchResult::empty(SearchCoverage::Exhaustive));
        }
        if query.conjunction_terms.is_empty() && query.exclusion_terms.is_empty() {
            return if count_all_matches {
                DirectSourceScanner::scan_complete_capture_counting_with_cancel(
                    doc.capture, &query.primary_needle, options, canceled)
            } else {
                DirectSourceScanner::scan_complete_capture_with_cancel(
                    doc.capture, &query.primary_needle, options, canceled)
            };
        }
        if !count_all_matches && options.max_matches == 0 && !doc.capture.bytes().is_empty() {
            return Ok(SearchResult::empty(SearchCoverage::TruncatedAtLimit { max_matches: 0 }));
        }
        let predicates = Predicates::new(query, options)?;
        let bytes = doc.capture.bytes();
        let revision = doc.capture.request().revision();
        match options.mode {
            SearchMode::RawBytes => crate::capture::scan_raw_expression(
                bytes, doc.file_id, revision, query.primary_needle.as_bytes(), options,
                count_all_matches, predicates, canceled),
            SearchMode::DecodedText { .. } => crate::text::scan_text_expression(
                bytes, doc.file_id, revision, &query.primary_needle, options,
                count_all_matches, predicates, canceled),
        }
    }
}

enum Cursor<'query> {
    Exact(ByteSearchCursor<'query>),
    Normalized(NormalizedCursor),
}

struct Predicate<'query> { excluded: bool, seen: bool, cursor: Cursor<'query> }

pub(crate) struct Predicates<'query> { terms: Vec<Predicate<'query>> }

impl<'query> Predicates<'query> {
    fn new(query: &'query ParsedQuery, options: &QueryOptions) -> Result<Self, QueryError> {
        let count = query.conjunction_terms.len() + query.exclusion_terms.len();
        let mut terms = Vec::new();
        terms.try_reserve_exact(count).map_err(|_| QueryError::LimitExceeded)?;
        if terms.capacity() > count { return Err(QueryError::LimitExceeded); }
        for (text, excluded) in query.conjunction_terms.iter().map(|term| (term, false))
            .chain(query.exclusion_terms.iter().map(|term| (term, true))) {
            let cursor = match NormalizedCursor::new(text, options.mode)? {
                Some(cursor) => Cursor::Normalized(cursor),
                None => Cursor::Exact(ByteSearchCursor::new(text.as_bytes())?),
            };
            terms.push(Predicate { excluded, seen: false, cursor });
        }
        Ok(Self { terms })
    }

    pub(crate) fn rejected(&self) -> bool {
        self.terms.iter().any(|term| term.excluded && term.seen)
    }

    pub(crate) fn proven_without_eof(&self) -> bool {
        self.terms.iter().all(|term| !term.excluded && term.seen)
    }

    /// Returns false only for cancellation. Seen predicates retire their work;
    /// finding an exclusion can reject the document immediately.
    pub(crate) fn feed_raw(&mut self, bytes: &[u8], canceled: &mut impl FnMut() -> bool)
        -> Result<bool, QueryError> {
        for term in &mut self.terms {
            if canceled() { return Ok(false); }
            if term.seen { continue; }
            let Cursor::Exact(cursor) = &mut term.cursor else { return Err(QueryError::InvalidRange); };
            let mut remaining = bytes;
            while !remaining.is_empty() && !term.seen {
                if canceled() { return Ok(false); }
                let batch = cursor.step(remaining, remaining.len(), 1)?;
                if batch.consumed == 0 { return Err(QueryError::InvalidRange); }
                remaining = &remaining[batch.consumed..];
                term.seen = !batch.hits.is_empty();
            }
            if term.excluded && term.seen { break; }
        }
        Ok(true)
    }

    pub(crate) fn feed_text(&mut self, map: &CaptureEncodingMap, decoded_base: u64,
        canceled: &mut impl FnMut() -> bool) -> Result<bool, QueryError> {
        if self.rejected() { return Ok(true); }
        if self.terms.first().is_none_or(|term| matches!(&term.cursor, Cursor::Exact(_))) {
            return self.feed_raw(map.decoded_text().as_bytes(), canceled);
        }
        // Decode/map each original scalar once even when several normalized
        // predicates remain. Their existing KMP cursors own only needle-sized
        // rings; no source-sized normalized string or repeated decoder exists.
        for (ordinal, (offset, ch)) in map.decoded_text().char_indices().enumerate() {
            if ordinal % 64 == 0 && canceled() { return Ok(false); }
            let start = decoded_base.checked_add(offset as u64).ok_or(QueryError::InvalidRange)?;
            let end = start.checked_add(ch.len_utf8() as u64).ok_or(QueryError::InvalidRange)?;
            let scalar = SourceScalar { ch,
                raw_start: map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(start))
                    .map_err(|_| QueryError::InvalidRange)?.get(),
                raw_end: map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(end))
                    .map_err(|_| QueryError::InvalidRange)?.get(),
                decoded_start: start, decoded_end: end };
            for term in &mut self.terms {
                if term.seen { continue; }
                let Cursor::Normalized(cursor) = &mut term.cursor else { return Err(QueryError::InvalidRange); };
                for unit in cursor.units(ch) {
                    if cursor.push(unit, scalar).is_some() { term.seen = true; break; }
                }
                if term.excluded && term.seen { return Ok(true); }
            }
        }
        Ok(true)
    }

    pub(crate) fn finish(&self, result: &mut SearchResult, complete_source: bool,
        primary_limited: bool, max_matches: usize) {
        let rejected = self.rejected()
            || (complete_source && self.terms.iter().any(|term| !term.excluded && !term.seen));
        let qualified = !rejected && self.terms.iter()
            .all(|term| if term.excluded { complete_source } else { term.seen });
        if !qualified {
            result.matches.clear();
            result.total_matches_counted = 0;
        }
        if rejected && result.unsupported_files.is_empty()
            && result.coverage != SearchCoverage::CanceledEarly {
            result.coverage = SearchCoverage::Exhaustive;
        } else if qualified && primary_limited && result.coverage == SearchCoverage::Exhaustive {
            result.coverage = SearchCoverage::TruncatedAtLimit { max_matches };
        }
    }
}
