#![forbid(unsafe_code)]

//! Exact decoded-text and byte search over immutable source captures.
//!
//! Default exact searches use bounded decoding windows and an overlapping,
//! incremental matcher. They do not concatenate chunked captures or materialize
//! an unbounded intermediate hit list. Normalized search retains the existing
//! normalization repertoire under explicit admission limits; it is not a claim
//! of a newly qualified complete Unicode normalization implementation.

mod capture;
pub mod index;
pub mod indexed_query;
pub mod oracle;
pub mod paths;
pub mod query;
pub mod stream;
mod text;

pub use index::{
    EphemeralIndex, IndexError, IndexLimits, IndexStatistics, ManifestLimits,
    MembershipState, SearchManifest, SearchManifestId, SegmentCoverage, UncoveredReason,
};
pub use indexed_query::{IndexedQuery, IndexedQueryState, IndexedSearchReport};
pub use oracle::{OracleMismatchError, ReferenceScanOracle, SearchDocument};
pub use query::{LangFilterKind, ParsedQuery, PathFilterKind, MAX_QUERY_LEN, MAX_QUERY_TOKENS};

use fcb_core::{ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId, QueryGeneration, SourceRevision};
use fcb_source::{CaptureEncodingMap, ChunkedCapture, CompleteCapture, DetectedEncoding, SpanKind, detect_encoding};

/// Stable machine-query errors; unsupported query modes are never reinterpreted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum QueryError {
    EmptyNeedle,
    NeedleTooLong,
    LimitExceeded,
    Canceled,
    UnsupportedEncoding,
    InvalidRange,
    RegexUnqualified,
    SyntaxError,
}

impl QueryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::EmptyNeedle => "QUERY_EMPTY",
            Self::NeedleTooLong => "QUERY_NEEDLE_TOO_LONG",
            Self::LimitExceeded => "QUERY_LIMIT_EXCEEDED",
            Self::Canceled => "QUERY_CANCELED",
            Self::UnsupportedEncoding => "QUERY_UNSUPPORTED_ENCODING",
            Self::InvalidRange => "QUERY_INVALID_RANGE",
            Self::RegexUnqualified => "QUERY_REGEX_UNQUALIFIED",
            Self::SyntaxError => "QUERY_SYNTAX_ERROR",
        }
    }
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for QueryError {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnicodeNormalization {
    /// Exact scalar matching without normalization.
    Exact,
    /// The existing lowercasing/eszett expansion repertoire, not full Unicode folding.
    CaseFold,
    /// The existing Latin canonical decomposition repertoire, not full Unicode NFC/NFD.
    Canonical,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SearchMode {
    DecodedText { case_sensitive: bool, normalization: UnicodeNormalization },
    RawBytes,
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Exact }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryScope {
    AllAdmitted,
    ExplicitFiles(Vec<FileId>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryOptions {
    pub generation: QueryGeneration,
    pub mode: SearchMode,
    /// Maximum retained hits. Zero performs no scan of a nonempty capture.
    pub max_matches: usize,
    /// Raw source bytes admitted for this operation, not decoded UTF-8 bytes.
    pub max_bytes_scanned: Option<u64>,
    pub short_query_allowed: bool,
    pub cross_chunk: bool,
    pub declared_encoding: Option<DetectedEncoding>,
}

impl QueryOptions {
    pub fn new(generation: QueryGeneration) -> Self {
        Self { generation, mode: SearchMode::default(), max_matches: 10_000,
            max_bytes_scanned: None, short_query_allowed: true, cross_chunk: true,
            declared_encoding: None }
    }
    pub fn with_mode(mut self, mode: SearchMode) -> Self { self.mode = mode; self }
    pub fn with_max_matches(mut self, max: usize) -> Self { self.max_matches = max; self }
    pub fn with_encoding(mut self, encoding: DetectedEncoding) -> Self {
        self.declared_encoding = Some(encoding); self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCoverage {
    /// The admitted scan finished. Consult unsupported_files or is_complete()
    /// before claiming a complete answer for the requested text semantics.
    Exhaustive,
    TruncatedAtLimit { max_matches: usize },
    BudgetExhausted { bytes_scanned: u64 },
    CanceledEarly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub occurrence_id: u64,
    pub file_id: FileId,
    pub revision: SourceRevision,
    pub decoded_range: Option<DecodedUtf8Range>,
    pub original_byte_range: ByteRange,
    pub matched_text: String,
    pub multiplicity: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    /// Matches actually seen, including at most one unstored lookahead match.
    /// This is not an exhaustive total when coverage is partial or truncated.
    pub total_matches_counted: usize,
    pub coverage: SearchCoverage,
    pub scanned_bytes: u64,
    pub unsupported_files: Vec<FileId>,
}

impl SearchResult {
    pub fn empty(coverage: SearchCoverage) -> Self {
        Self { matches: Vec::new(), total_matches_counted: 0, coverage,
            scanned_bytes: 0, unsupported_files: Vec::new() }
    }
    pub fn is_complete(&self) -> bool {
        matches!(self.coverage, SearchCoverage::Exhaustive) && self.unsupported_files.is_empty()
    }
    pub fn match_count(&self) -> usize { self.total_matches_counted }
    pub fn is_empty(&self) -> bool { self.matches.is_empty() }
}

/// The compatibility normalization algorithms are admitted only for small
/// scopes. Exact text and raw-byte scans have no whole-capture size ceiling.
pub const MAX_NORMALIZED_SCAN_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_NORMALIZED_SCAN_WORK: u64 = 16 * 1024 * 1024;

pub struct DirectSourceScanner;

impl DirectSourceScanner {
    pub fn scan_complete_capture(
        capture: &CompleteCapture, query: &str, options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        Self::scan_complete_capture_with_cancel(capture, query, options, || false)
    }

    /// Cancellation is checked between bounded exact-scan quanta. The callback
    /// must be cheap and must not acquire ambient resources. Returned partial
    /// hits retain their source revision; cancellation never claims completeness.
    pub fn scan_complete_capture_with_cancel(
        capture: &CompleteCapture, query: &str, options: &QueryOptions,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<SearchResult, QueryError> {
        validate_needle(query.as_bytes())?;
        let bytes = capture.bytes();
        let file = capture.request().file();
        let revision = capture.request().revision();
        match options.mode {
            SearchMode::RawBytes => capture::scan_raw_chunks(
                std::iter::once(Ok(bytes)), bytes.len() as u64, file, revision,
                query.as_bytes(), options, canceled,
            ),
            SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Exact } => {
                text::scan_exact_chunks(std::iter::once(Ok(bytes)), bytes.len() as u64,
                    file, revision, query, options, canceled)
            }
            SearchMode::DecodedText { .. } => {
                if canceled() { return Ok(SearchResult::empty(SearchCoverage::CanceledEarly)); }
                let mut result = scan_normalized(bytes, file, revision, query, options)?;
                if canceled() { result.coverage = SearchCoverage::CanceledEarly; }
                Ok(result)
            }
        }
    }

    pub fn scan_chunked_capture(
        capture: &ChunkedCapture, query: &str, options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        Self::scan_chunked_capture_with_cancel(capture, query, options, || false)
    }

    pub fn scan_chunked_capture_with_cancel(
        capture: &ChunkedCapture, query: &str, options: &QueryOptions,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<SearchResult, QueryError> {
        validate_needle(query.as_bytes())?;
        let file = capture.request().file();
        let revision = capture.request().revision();
        let total = capture.total_length().get();
        let chunks = (0..capture.chunk_count()).map(|index| {
            capture.chunk(index).map(|chunk| chunk.bytes()).ok_or(QueryError::InvalidRange)
        });
        match options.mode {
            SearchMode::RawBytes => capture::scan_raw_chunks(
                chunks, total, file, revision, query.as_bytes(), options, canceled,
            ),
            SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Exact } => {
                text::scan_exact_chunks(chunks, total, file, revision, query, options, canceled)
            }
            SearchMode::DecodedText { .. } => {
                if canceled() { return Ok(SearchResult::empty(SearchCoverage::CanceledEarly)); }
                if options.max_matches == 0 && total > 0 {
                    return Ok(SearchResult::empty(SearchCoverage::TruncatedAtLimit { max_matches: 0 }));
                }
                if options.max_bytes_scanned.is_some_and(|limit| limit < total) {
                    return Ok(SearchResult::empty(SearchCoverage::BudgetExhausted { bytes_scanned: 0 }));
                }
                admit_normalized(total, query.len())?;
                // Legacy normalized matching is whole-scope but strictly
                // admitted before allocating. The default exact route above
                // does not take this path.
                let size = usize::try_from(total).map_err(|_| QueryError::LimitExceeded)?;
                let mut stitched = Vec::new();
                stitched.try_reserve_exact(size).map_err(|_| QueryError::LimitExceeded)?;
                for chunk in chunks {
                    for part in chunk?.chunks(stream::MAX_STREAM_STEP_BYTES) {
                        if canceled() {
                            let mut result = SearchResult::empty(SearchCoverage::CanceledEarly);
                            result.scanned_bytes = stitched.len() as u64;
                            return Ok(result);
                        }
                        stitched.extend_from_slice(part);
                    }
                }
                let mut result = scan_normalized(&stitched, file, revision, query, options)?;
                if !options.cross_chunk {
                    // Preserve original source-chunk scope for normalized hits
                    // as well, without treating work quanta as chunk boundaries.
                    let chunk_bytes = capture.chunk_size().as_u64();
                    result.matches.retain(|hit| {
                        hit.original_byte_range.start().get() / chunk_bytes
                            == (hit.original_byte_range.end().get() - 1) / chunk_bytes
                    });
                    // A cap reached before filtering is still conservative
                    // partial coverage, not an exhaustive negative result.
                    result.total_matches_counted = result.matches.len();
                }
                if canceled() { result.coverage = SearchCoverage::CanceledEarly; }
                Ok(result)
            }
        }
    }

    /// Explicit raw-byte search; arbitrary byte needles are not decoded text.
    pub fn scan_raw_bytes(
        bytes: &[u8], file_id: FileId, revision: SourceRevision, needle: &[u8], options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        capture::scan_raw_chunks(std::iter::once(Ok(bytes)), bytes.len() as u64,
            file_id, revision, needle, options, || false)
    }
}

fn validate_needle(needle: &[u8]) -> Result<(), QueryError> {
    if needle.is_empty() { return Err(QueryError::EmptyNeedle); }
    if needle.len() > stream::MAX_STREAM_NEEDLE_BYTES { return Err(QueryError::NeedleTooLong); }
    Ok(())
}

fn admit_normalized(bytes: u64, needle: usize) -> Result<(), QueryError> {
    if bytes > MAX_NORMALIZED_SCAN_BYTES as u64
        || bytes.checked_mul(needle as u64).is_none_or(|work| work > MAX_NORMALIZED_SCAN_WORK) {
        return Err(QueryError::LimitExceeded);
    }
    Ok(())
}

fn scan_normalized(
    bytes: &[u8], file_id: FileId, revision: SourceRevision, query: &str, options: &QueryOptions,
) -> Result<SearchResult, QueryError> {
    if options.max_matches == 0 && !bytes.is_empty() {
        return Ok(SearchResult::empty(SearchCoverage::TruncatedAtLimit { max_matches: 0 }));
    }
    if options.max_bytes_scanned.is_some_and(|limit| limit < bytes.len() as u64) {
        // A partial normalization unit cannot establish a complete normalized
        // result. Refuse admission honestly instead of ignoring the budget.
        return Ok(SearchResult::empty(SearchCoverage::BudgetExhausted { bytes_scanned: 0 }));
    }
    admit_normalized(bytes.len() as u64, query.len())?;
    let encoding = options.declared_encoding.unwrap_or_else(|| detect_encoding(bytes));
    let map = CaptureEncodingMap::build_with_encoding(bytes, encoding)
        .map_err(|_| QueryError::UnsupportedEncoding)?;
    let mut result = SearchResult::empty(SearchCoverage::Exhaustive);
    result.scanned_bytes = bytes.len() as u64;
    if encoding == DetectedEncoding::Unsupported || map.spans().iter().any(|span|
        matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
        result.unsupported_files.push(file_id);
        return Ok(result);
    }
    let limit = options.max_matches.saturating_add(1);
    let text = map.decoded_text();
    let hits = match options.mode {
        SearchMode::DecodedText { normalization: UnicodeNormalization::CaseFold, .. } =>
            find_unicode_folded_substrings(text, query, limit)?,
        SearchMode::DecodedText { normalization: UnicodeNormalization::Canonical, case_sensitive } =>
            find_canonical_equivalent_substrings(text, query, case_sensitive, limit),
        _ => find_case_insensitive_substrings(text, query, limit)?,
    };
    result.total_matches_counted = hits.len();
    if hits.len() > options.max_matches {
        result.coverage = SearchCoverage::TruncatedAtLimit { max_matches: options.max_matches };
    }
    result.matches.try_reserve_exact(hits.len().min(options.max_matches))
        .map_err(|_| QueryError::LimitExceeded)?;
    for (index, hit) in hits.into_iter().take(options.max_matches).enumerate() {
        let decoded = DecodedUtf8Range::new(DecodedUtf8Offset::new(hit.start as u64),
            DecodedUtf8Offset::new(hit.end as u64)).map_err(|_| QueryError::InvalidRange)?;
        result.matches.push(SearchMatch {
            occurrence_id: index as u64 + 1, file_id, revision,
            decoded_range: Some(decoded),
            original_byte_range: map.decoded_utf8_range_to_byte_range(decoded)
                .map_err(|_| QueryError::InvalidRange)?,
            matched_text: text[hit.start..hit.end].to_owned(), multiplicity: hit.multiplicity,
        });
    }
    Ok(result)
}

struct NormalizedHit { start: usize, end: usize, multiplicity: usize }

fn find_case_insensitive_substrings(
    haystack: &str, needle: &str, limit: usize,
) -> Result<Vec<NormalizedHit>, QueryError> {
    find_lowered_substrings(haystack, needle, limit, false)
}

fn find_unicode_folded_substrings(
    haystack: &str, needle: &str, limit: usize,
) -> Result<Vec<NormalizedHit>, QueryError> {
    find_lowered_substrings(haystack, needle, limit, true)
}

/// Apply the same declared scalar transform to source and query. Comparing
/// source slices of needle.len() bytes loses matches when lowercasing changes
/// UTF-8 length. Expansions retain one provenance entry per transformed scalar;
/// two occurrences inside one source scalar must not collapse into one hit.
fn lowered_with_spans(s: &str, fold_eszett: bool) -> Result<Vec<DecomposedChar>, QueryError> {
    let mut out = Vec::new();
    for (orig_start, ch) in s.char_indices() {
        let orig_end = orig_start + ch.len_utf8();
        let mut push = |ch| -> Result<(), QueryError> {
            out.try_reserve(1).map_err(|_| QueryError::LimitExceeded)?;
            out.push(DecomposedChar { ch, orig_start, orig_end });
            Ok(())
        };
        if fold_eszett && matches!(ch, 'ß' | 'ẞ') {
            push('s')?;
            push('s')?;
        } else {
            for lowered in ch.to_lowercase() { push(lowered)?; }
        }
    }
    Ok(out)
}

fn find_lowered_substrings(
    haystack: &str, needle: &str, limit: usize, fold_eszett: bool,
) -> Result<Vec<NormalizedHit>, QueryError> {
    if limit == 0 { return Ok(Vec::new()); }
    let needle = lowered_with_spans(needle, fold_eszett)?;
    if needle.is_empty() { return Err(QueryError::EmptyNeedle); }
    let source = lowered_with_spans(haystack, fold_eszett)?;
    let mut failure = Vec::new();
    failure.try_reserve_exact(needle.len()).map_err(|_| QueryError::LimitExceeded)?;
    failure.resize(needle.len(), 0usize);
    let mut prefix = 0;
    for index in 1..needle.len() {
        while prefix > 0 && needle[index].ch != needle[prefix].ch {
            prefix = failure[prefix - 1];
        }
        if needle[index].ch == needle[prefix].ch { prefix += 1; }
        failure[index] = prefix;
    }
    let mut results = Vec::new();
    let mut matched = 0;
    for (index, unit) in source.iter().enumerate() {
        while matched > 0 && unit.ch != needle[matched].ch {
            matched = failure[matched - 1];
        }
        if unit.ch == needle[matched].ch { matched += 1; }
        if matched == needle.len() {
            results.try_reserve(1).map_err(|_| QueryError::LimitExceeded)?;
            results.push(NormalizedHit {
                start: source[index + 1 - needle.len()].orig_start,
                end: unit.orig_end,
                multiplicity: 1,
            });
            if results.len() == limit { break; }
            matched = failure[matched - 1];
        }
    }
    Ok(results)
}

fn canonical_decompose_char(c: char) -> Option<&'static [char]> {
    match c {
        '\u{00C0}' => Some(&['A', '\u{0300}']),
        '\u{00C1}' => Some(&['A', '\u{0301}']),
        '\u{00C2}' => Some(&['A', '\u{0302}']),
        '\u{00C3}' => Some(&['A', '\u{0303}']),
        '\u{00C4}' => Some(&['A', '\u{0308}']),
        '\u{00C5}' => Some(&['A', '\u{030A}']),
        '\u{00C7}' => Some(&['C', '\u{0327}']),
        '\u{00C8}' => Some(&['E', '\u{0300}']),
        '\u{00C9}' => Some(&['E', '\u{0301}']),
        '\u{00CA}' => Some(&['E', '\u{0302}']),
        '\u{00CB}' => Some(&['E', '\u{0308}']),
        '\u{00CC}' => Some(&['I', '\u{0300}']),
        '\u{00CD}' => Some(&['I', '\u{0301}']),
        '\u{00CE}' => Some(&['I', '\u{0302}']),
        '\u{00CF}' => Some(&['I', '\u{0308}']),
        '\u{00D1}' => Some(&['N', '\u{0303}']),
        '\u{00D2}' => Some(&['O', '\u{0300}']),
        '\u{00D3}' => Some(&['O', '\u{0301}']),
        '\u{00D4}' => Some(&['O', '\u{0302}']),
        '\u{00D5}' => Some(&['O', '\u{0303}']),
        '\u{00D6}' => Some(&['O', '\u{0308}']),
        '\u{00D9}' => Some(&['U', '\u{0300}']),
        '\u{00DA}' => Some(&['U', '\u{0301}']),
        '\u{00DB}' => Some(&['U', '\u{0302}']),
        '\u{00DC}' => Some(&['U', '\u{0308}']),
        '\u{00DD}' => Some(&['Y', '\u{0301}']),
        '\u{00E0}' => Some(&['a', '\u{0300}']),
        '\u{00E1}' => Some(&['a', '\u{0301}']),
        '\u{00E2}' => Some(&['a', '\u{0302}']),
        '\u{00E3}' => Some(&['a', '\u{0303}']),
        '\u{00E4}' => Some(&['a', '\u{0308}']),
        '\u{00E5}' => Some(&['a', '\u{030A}']),
        '\u{00E7}' => Some(&['c', '\u{0327}']),
        '\u{00E8}' => Some(&['e', '\u{0300}']),
        '\u{00E9}' => Some(&['e', '\u{0301}']),
        '\u{00EA}' => Some(&['e', '\u{0302}']),
        '\u{00EB}' => Some(&['e', '\u{0308}']),
        '\u{00EC}' => Some(&['i', '\u{0300}']),
        '\u{00ED}' => Some(&['i', '\u{0301}']),
        '\u{00EE}' => Some(&['i', '\u{0302}']),
        '\u{00EF}' => Some(&['i', '\u{0308}']),
        '\u{00F1}' => Some(&['n', '\u{0303}']),
        '\u{00F2}' => Some(&['o', '\u{0300}']),
        '\u{00F3}' => Some(&['o', '\u{0301}']),
        '\u{00F4}' => Some(&['o', '\u{0302}']),
        '\u{00F5}' => Some(&['o', '\u{0303}']),
        '\u{00F6}' => Some(&['o', '\u{0308}']),
        '\u{00F9}' => Some(&['u', '\u{0300}']),
        '\u{00FA}' => Some(&['u', '\u{0301}']),
        '\u{00FB}' => Some(&['u', '\u{0302}']),
        '\u{00FC}' => Some(&['u', '\u{0308}']),
        '\u{00FD}' => Some(&['y', '\u{0301}']),
        '\u{00FF}' => Some(&['y', '\u{0308}']),
        _ => None,
    }
}

struct DecomposedChar { ch: char, orig_start: usize, orig_end: usize }

fn decompose_with_spans(s: &str) -> Vec<DecomposedChar> {
    let mut out = Vec::new();
    for (byte_idx, ch) in s.char_indices() {
        let end = byte_idx + ch.len_utf8();
        if let Some(decomp) = canonical_decompose_char(ch) {
            for &dc in decomp { out.push(DecomposedChar { ch: dc, orig_start: byte_idx, orig_end: end }); }
        } else { out.push(DecomposedChar { ch, orig_start: byte_idx, orig_end: end }); }
    }
    out
}

fn decompose_chars(s: &str) -> Vec<char> {
    let mut out = Vec::new();
    for ch in s.chars() {
        if let Some(decomp) = canonical_decompose_char(ch) { out.extend_from_slice(decomp); }
        else { out.push(ch); }
    }
    out
}

fn find_canonical_equivalent_substrings(
    haystack: &str, needle: &str, case_sensitive: bool, limit: usize,
) -> Vec<NormalizedHit> {
    let needle_decomp = decompose_chars(needle);
    let haystack_decomp = decompose_with_spans(haystack);
    let mut results = Vec::new();
    if needle_decomp.is_empty() || needle_decomp.len() > haystack_decomp.len() { return results; }
    for i in 0..=haystack_decomp.len() - needle_decomp.len() {
        let matched = needle_decomp.iter().enumerate().all(|(j, &nc)| {
            let hc = haystack_decomp[i + j].ch;
            if case_sensitive { hc == nc } else { hc.to_lowercase().eq(nc.to_lowercase()) }
        });
        if matched {
            results.push(NormalizedHit { start: haystack_decomp[i].orig_start,
                end: haystack_decomp[i + needle_decomp.len() - 1].orig_end, multiplicity: 1 });
            if results.len() == limit { break; }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use super::*;
    use fcb_core::{ArenaOwnerId, ByteLength};
    use fcb_source::CaptureRequest;

    fn test_file_and_revision() -> (FileId, SourceRevision, QueryGeneration) {
        let owner = ArenaOwnerId::new(1).unwrap();
        let file = FileId::new(owner, 10).unwrap();
        let rev = SourceRevision::new(owner, 1).unwrap();
        let generation = QueryGeneration::new(owner, 1).unwrap();
        (file, rev, generation)
    }
    fn make_capture(file: FileId, rev: SourceRevision, bytes: &[u8]) -> CompleteCapture {
        let req = CaptureRequest::new(file, rev).unwrap();
        let len = ByteLength::new(bytes.len() as u64);
        CompleteCapture::new(req, len, Arc::from(bytes)).unwrap()
    }
    #[test]
    fn overlapping_occurrences_are_both_reported() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation);
        let capture = make_capture(file, rev, b"banana");
        let result = DirectSourceScanner::scan_complete_capture(&capture, "ana", &options).unwrap();
        assert_eq!(result.match_count(), 2);
        assert_eq!(result.matches[0].original_byte_range.start().get(), 1);
        assert_eq!(result.matches[0].original_byte_range.end().get(), 4);
        assert_eq!(result.matches[1].original_byte_range.start().get(), 3);
        assert_eq!(result.matches[1].original_byte_range.end().get(), 6);
    }
    #[test]
    fn empty_query_returns_machine_error() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation);
        let capture = make_capture(file, rev, b"hello");
        let err = DirectSourceScanner::scan_complete_capture(&capture, "", &options);
        assert_eq!(err, Err(QueryError::EmptyNeedle));
        assert_eq!(err.unwrap_err().code(), "QUERY_EMPTY");
    }
    #[test]
    fn utf16le_bom_text_search_maps_to_original_bytes() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation);
        let mut raw = vec![0xFF, 0xFE];
        for unit in "hello world".encode_utf16() { raw.extend_from_slice(&unit.to_le_bytes()); }
        let capture = make_capture(file, rev, &raw);
        let result = DirectSourceScanner::scan_complete_capture(&capture, "world", &options).unwrap();
        assert_eq!(result.match_count(), 1);
        let m = &result.matches[0];
        assert_eq!(m.matched_text, "world");
        assert_eq!(m.original_byte_range.start().get(), 14);
        assert_eq!(m.original_byte_range.end().get(), 24);
        let decoded_hit: Vec<u16> = raw[14..24].as_chunks::<2>().0.iter()
            .map(|c| u16::from_le_bytes(*c)).collect();
        assert_eq!(String::from_utf16(&decoded_hit).unwrap(), "world");
    }
    #[test]
    fn expansion_to_same_source_span_multiplicity() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
            case_sensitive: false, normalization: UnicodeNormalization::CaseFold,
        });
        let capture = make_capture(file, rev, "Straße".as_bytes());
        let res_ss = DirectSourceScanner::scan_complete_capture(&capture, "ss", &options).unwrap();
        assert_eq!(res_ss.match_count(), 1);
        assert_eq!(res_ss.matches[0].matched_text, "ß");
        assert_eq!(res_ss.matches[0].multiplicity, 1);
        let res_s = DirectSourceScanner::scan_complete_capture(&capture, "s", &options).unwrap();
        assert_eq!(res_s.match_count(), 3);
        assert_eq!(res_s.matches[0].original_byte_range.start().get(), 0);
        assert_eq!(res_s.matches[0].multiplicity, 1);
        assert_eq!(res_s.matches[1].original_byte_range.start().get(), 4);
        assert_eq!(res_s.matches[1].original_byte_range.end().get(), 6);
        assert_eq!(res_s.matches[1].multiplicity, 1);
        assert_eq!(res_s.matches[2].original_byte_range, res_s.matches[1].original_byte_range);
        assert_eq!(res_s.matches[2].multiplicity, 1);
        assert_ne!(res_s.matches[1].occurrence_id, res_s.matches[2].occurrence_id);
    }

    #[test]
    fn folded_words_and_needles_use_the_same_expansion() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
            case_sensitive: false, normalization: UnicodeNormalization::CaseFold,
        });
        let capture = make_capture(file, rev, "Straße STRASSE straẞe".as_bytes());
        for query in ["strasse", "Straße", "STRAẞE"] {
            let result = DirectSourceScanner::scan_complete_capture(&capture, query, &options).unwrap();
            assert!(result.is_complete());
            assert_eq!(result.match_count(), 3, "{query}");
            assert_eq!(result.matches.iter().map(|hit| hit.matched_text.as_str()).collect::<Vec<_>>(),
                ["Straße", "STRASSE", "straẞe"]);
        }
    }

    #[test]
    fn insensitive_matches_preserve_variable_length_source_units() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
            case_sensitive: false, normalization: UnicodeNormalization::Exact,
        });
        let capture = make_capture(file, rev, "K k İ i\u{0307}".as_bytes());
        for query in ["k", "K"] {
            let result = DirectSourceScanner::scan_complete_capture(&capture, query, &options).unwrap();
            assert_eq!(result.match_count(), 2);
            assert_eq!(result.matches[0].matched_text, "K");
            assert_eq!(result.matches[0].original_byte_range.start().get(), 0);
            assert_eq!(result.matches[0].original_byte_range.end().get(), 3);
        }
        for query in ["İ", "i\u{0307}"] {
            let result = DirectSourceScanner::scan_complete_capture(&capture, query, &options).unwrap();
            assert_eq!(result.match_count(), 2);
            assert_eq!(result.matches[0].matched_text, "İ");
            assert_eq!(result.matches[1].matched_text, "i\u{0307}");
        }
    }

    #[test]
    fn expansion_occurrences_obey_limits_and_utf16_source_mapping() {
        let (file, rev, generation) = test_file_and_revision();
        for little in [true, false] {
            let mut raw = if little { vec![0xFF, 0xFE] } else { vec![0xFE, 0xFF] };
            for unit in "ß".encode_utf16() {
                raw.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            let capture = make_capture(file, rev, &raw);
            for cap in [1, 2, 3] {
                let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
                    case_sensitive: false, normalization: UnicodeNormalization::CaseFold,
                }).with_max_matches(cap);
                let result = DirectSourceScanner::scan_complete_capture(&capture, "s", &options).unwrap();
                assert_eq!(result.match_count(), 2);
                assert_eq!(result.matches.len(), cap.min(2));
                assert_eq!(result.is_complete(), cap >= 2);
                if cap == 1 {
                    assert_eq!(result.coverage, SearchCoverage::TruncatedAtLimit { max_matches: 1 });
                }
                for hit in &result.matches {
                    assert_eq!(hit.original_byte_range.start().get(), 2);
                    assert_eq!(hit.original_byte_range.end().get(), 4);
                    assert_eq!(hit.matched_text, "ß");
                    assert_eq!(hit.multiplicity, 1);
                }
                if cap >= 2 {
                    assert_ne!(result.matches[0].occurrence_id, result.matches[1].occurrence_id);
                }
            }
        }
    }

    #[test]
    fn folded_overlaps_are_not_deduplicated_by_contributing_source_range() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
            case_sensitive: false, normalization: UnicodeNormalization::CaseFold,
        });
        let capture = make_capture(file, rev, "ßß".as_bytes());
        let result = DirectSourceScanner::scan_complete_capture(&capture, "sss", &options).unwrap();
        assert_eq!(result.match_count(), 2);
        assert_eq!(result.matches[0].original_byte_range, result.matches[1].original_byte_range);
        assert_ne!(result.matches[0].occurrence_id, result.matches[1].occurrence_id);
        assert!(result.matches.iter().all(|hit| hit.matched_text == "ßß" && hit.multiplicity == 1));
    }
}
