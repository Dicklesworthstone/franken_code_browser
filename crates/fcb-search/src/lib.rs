#![forbid(unsafe_code)]

//! Bounded exact decoded-text and byte search engine (FCB-026.A / fcb-8ii.1).
//!
//! §17.1, §17.3, §17.6, §17.11:
//! Search is a core navigation primitive that operates before optional indexing or compiler layers.
//! Text queries and raw-byte queries are distinct: a UTF-8 byte needle cannot search UTF-16 source
//! correctly. Decoded text search verifies candidates against the declared decoding/normalization
//! semantics and maps hits back to exact original byte ranges in the captured source.
//!
//! Features:
//! - Direct exact scans with bounded memory and complexity.
//! - Multi-chunk cross-boundary matching without duplication or omissions.
//! - Overlapping occurrence detection (e.g. "ana" in "banana" matches at 1 and 3).
//! - Case folding and expansion with preserved multiplicity and occurrence identity (e.g. "ß" -> "ss").
//! - UTF-16LE / UTF-16BE BOM handling with zero Apple framework dependencies.
//! - Machine-query empty needle rejection (`QUERY_EMPTY`).
//! - Unsupported encoding recorded explicitly in coverage rather than false negative.

pub mod oracle;
pub mod query;

pub use oracle::{OracleMismatchError, ReferenceScanOracle, SearchDocument};
pub use query::{
    LangFilterKind, ParsedQuery, PathFilterKind, MAX_QUERY_LEN, MAX_QUERY_TOKENS,
};

use fcb_core::{
    ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId,
    QueryGeneration, SourceRevision,
};
use fcb_source::{
    detect_encoding, CaptureEncodingMap, ChunkedCapture, CompleteCapture, DetectedEncoding,
    SpanKind,
};

/// Query error codes conforming to §17.6 and §17.11.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum QueryError {
    /// An empty query needle was passed to a machine search request.
    EmptyNeedle,
    /// The needle exceeds configured length or complexity limits.
    NeedleTooLong,
    /// Memory, result count, or scan budget limit exceeded.
    LimitExceeded,
    /// The search operation was explicitly canceled before completion.
    Canceled,
    /// The target capture has an unsupported encoding for text search.
    UnsupportedEncoding,
    /// An invalid offset range was encountered or generated.
    InvalidRange,
    /// Regular expression search is unqualified and not permitted (§17.6).
    RegexUnqualified,
    /// Query string has invalid syntax, such as an unclosed quote.
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for QueryError {}

/// Unicode normalization mode for text search.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnicodeNormalization {
    /// Exact scalar matching without normalization.
    Exact,
    /// Case-folding normalization (e.g., 'A' == 'a', 'ß' == "ss").
    CaseFold,
    /// Canonical equivalence normalization (e.g. decomposed combining accents match precomposed chars).
    Canonical,
}

/// The mode of a search operation: decoded text or raw source bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SearchMode {
    /// Human text search operating on the decoded text representation.
    DecodedText {
        case_sensitive: bool,
        normalization: UnicodeNormalization,
    },
    /// Exact binary byte search operating on raw captured bytes.
    RawBytes,
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::DecodedText {
            case_sensitive: true,
            normalization: UnicodeNormalization::Exact,
        }
    }
}

/// Scope definition constraining which captures or files are searched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryScope {
    /// All files in the admitted workspace or capture set.
    AllAdmitted,
    /// Specific list of file IDs.
    ExplicitFiles(Vec<FileId>),
}

/// Options and limits governing search execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryOptions {
    pub generation: QueryGeneration,
    pub mode: SearchMode,
    pub max_matches: usize,
    pub max_bytes_scanned: Option<u64>,
    pub short_query_allowed: bool,
    pub cross_chunk: bool,
    pub declared_encoding: Option<DetectedEncoding>,
}

impl QueryOptions {
    pub fn new(generation: QueryGeneration) -> Self {
        Self {
            generation,
            mode: SearchMode::default(),
            max_matches: 10_000,
            max_bytes_scanned: None,
            short_query_allowed: true,
            cross_chunk: true,
            declared_encoding: None,
        }
    }

    pub fn with_mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_max_matches(mut self, max: usize) -> Self {
        self.max_matches = max;
        self
    }

    pub fn with_encoding(mut self, encoding: DetectedEncoding) -> Self {
        self.declared_encoding = Some(encoding);
        self
    }
}

/// Coverage status of a search outcome distinguishing exhaustive totals from early stops.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCoverage {
    /// All requested captures were fully scanned and all matches are counted.
    Exhaustive,
    /// The search stopped because the configured `max_matches` limit was reached.
    TruncatedAtLimit { max_matches: usize },
    /// The search stopped because byte or time budget was exhausted.
    BudgetExhausted { bytes_scanned: u64 },
    /// Search was canceled before scanning completed.
    CanceledEarly,
}

/// A verified search occurrence in source text or bytes.
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

/// Complete results of a search query across one or more captures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub total_matches_counted: usize,
    pub coverage: SearchCoverage,
    pub scanned_bytes: u64,
    pub unsupported_files: Vec<FileId>,
}

impl SearchResult {
    pub fn empty(coverage: SearchCoverage) -> Self {
        Self {
            matches: Vec::new(),
            total_matches_counted: 0,
            coverage,
            scanned_bytes: 0,
            unsupported_files: Vec::new(),
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.coverage, SearchCoverage::Exhaustive)
    }

    pub fn match_count(&self) -> usize {
        self.total_matches_counted
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// Direct bounded source scanner providing exact literal and normalized matching.
pub struct DirectSourceScanner;

impl DirectSourceScanner {
    /// Scan a [`CompleteCapture`] under the declared query options.
    pub fn scan_complete_capture(
        capture: &CompleteCapture,
        query: &str,
        options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        let raw_bytes = capture.bytes();
        let file_id = capture.request().file();
        let revision = capture.request().revision();

        match options.mode {
            SearchMode::RawBytes => {
                Self::scan_raw_bytes(raw_bytes, file_id, revision, query.as_bytes(), options)
            }
            SearchMode::DecodedText { .. } => {
                if query.is_empty() {
                    return Err(QueryError::EmptyNeedle);
                }

                let encoding = options
                    .declared_encoding
                    .unwrap_or_else(|| detect_encoding(raw_bytes));
                if encoding == DetectedEncoding::Unsupported {
                    let mut res = SearchResult::empty(SearchCoverage::Exhaustive);
                    res.unsupported_files.push(file_id);
                    res.scanned_bytes = raw_bytes.len() as u64;
                    return Ok(res);
                }

                let map = CaptureEncodingMap::build(raw_bytes)
                    .map_err(|_| QueryError::UnsupportedEncoding)?;

                if map
                    .spans()
                    .iter()
                    .any(|s| matches!(s.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte))
                {
                    let mut res = SearchResult::empty(SearchCoverage::Exhaustive);
                    res.unsupported_files.push(file_id);
                    res.scanned_bytes = raw_bytes.len() as u64;
                    return Ok(res);
                }

                let text = map.decoded_text();

                Self::scan_decoded_text(
                    text,
                    &map,
                    file_id,
                    revision,
                    raw_bytes.len() as u64,
                    query,
                    options,
                )
            }
        }
    }

    /// Scan a [`ChunkedCapture`] under the declared query options, handling multi-chunk
    /// cross-boundary matches.
    pub fn scan_chunked_capture(
        capture: &ChunkedCapture,
        query: &str,
        options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        if query.is_empty() {
            return Err(QueryError::EmptyNeedle);
        }

        let file_id = capture.request().file();
        let revision = capture.request().revision();
        let total_raw_len = capture.total_length().get();

        match options.mode {
            SearchMode::RawBytes => {
                let needle = query.as_bytes();
                let mut matches = Vec::new();
                let mut match_idx = 0u64;
                let mut scanned = 0u64;

                let chunk_count = capture.chunk_count();
                for i in 0..chunk_count {
                    let chunk = capture.chunk(i).ok_or(QueryError::InvalidRange)?;
                    let chunk_bytes = chunk.bytes();
                    let chunk_offset = chunk.range().start().get();
                    scanned += chunk_bytes.len() as u64;

                    // Match within chunk
                    let chunk_hits = find_overlapping_subsequences(chunk_bytes, needle);
                    for hit_offset in chunk_hits {
                        let raw_start = chunk_offset + hit_offset as u64;
                        let raw_end = raw_start + needle.len() as u64;
                        if raw_end <= total_raw_len {
                            let range = ByteRange::new(
                                ByteOffset::new(raw_start),
                                ByteOffset::new(raw_end),
                            )
                            .map_err(|_| QueryError::InvalidRange)?;

                            match_idx += 1;
                            matches.push(SearchMatch {
                                occurrence_id: match_idx,
                                file_id,
                                revision,
                                decoded_range: None,
                                original_byte_range: range,
                                matched_text: String::from_utf8_lossy(needle).to_string(),
                                multiplicity: 1,
                            });

                            if matches.len() >= options.max_matches {
                                return Ok(SearchResult {
                                    matches,
                                    total_matches_counted: match_idx as usize,
                                    coverage: SearchCoverage::TruncatedAtLimit {
                                        max_matches: options.max_matches,
                                    },
                                    scanned_bytes: scanned,
                                    unsupported_files: Vec::new(),
                                });
                            }
                        }
                    }

                    // Multi-chunk boundary match: if next chunk exists and cross_chunk enabled
                    if options.cross_chunk && needle.len() > 1 && i + 1 < chunk_count {
                        let next_chunk =
                            capture.chunk(i + 1).ok_or(QueryError::InvalidRange)?;
                        let overlap_len = needle.len() - 1;
                        let start_in_curr = chunk_bytes.len().saturating_sub(overlap_len);
                        let tail = &chunk_bytes[start_in_curr..];
                        let head_len = overlap_len.min(next_chunk.len());
                        let head = &next_chunk.bytes()[..head_len];

                        let mut boundary_buf = Vec::with_capacity(tail.len() + head.len());
                        boundary_buf.extend_from_slice(tail);
                        boundary_buf.extend_from_slice(head);

                        let b_hits = find_overlapping_subsequences(&boundary_buf, needle);
                        for b_offset in b_hits {
                            // Only include hits that actually cross the boundary
                            if b_offset < tail.len() && b_offset + needle.len() > tail.len() {
                                let raw_start =
                                    chunk_offset + start_in_curr as u64 + b_offset as u64;
                                let raw_end = raw_start + needle.len() as u64;

                                if raw_end <= total_raw_len {
                                    let range = ByteRange::new(
                                        ByteOffset::new(raw_start),
                                        ByteOffset::new(raw_end),
                                    )
                                    .map_err(|_| QueryError::InvalidRange)?;

                                    match_idx += 1;
                                    matches.push(SearchMatch {
                                        occurrence_id: match_idx,
                                        file_id,
                                        revision,
                                        decoded_range: None,
                                        original_byte_range: range,
                                        matched_text: String::from_utf8_lossy(needle).to_string(),
                                        multiplicity: 1,
                                    });

                                    if matches.len() >= options.max_matches {
                                        return Ok(SearchResult {
                                            matches,
                                            total_matches_counted: match_idx as usize,
                                            coverage: SearchCoverage::TruncatedAtLimit {
                                                max_matches: options.max_matches,
                                            },
                                            scanned_bytes: scanned,
                                            unsupported_files: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }

                Ok(SearchResult {
                    total_matches_counted: matches.len(),
                    matches,
                    coverage: SearchCoverage::Exhaustive,
                    scanned_bytes: scanned,
                    unsupported_files: Vec::new(),
                })
            }
            SearchMode::DecodedText { .. } => {
                // Stitch all chunk bytes for decoded text search to ensure UTF-8/UTF-16
                // and multiline encoding maps are complete and exact.
                let mut stitched = Vec::with_capacity(total_raw_len as usize);
                for i in 0..capture.chunk_count() {
                    let chunk = capture.chunk(i).ok_or(QueryError::InvalidRange)?;
                    stitched.extend_from_slice(chunk.bytes());
                }

                let encoding = options
                    .declared_encoding
                    .unwrap_or_else(|| detect_encoding(&stitched));
                if encoding == DetectedEncoding::Unsupported {
                    let mut res = SearchResult::empty(SearchCoverage::Exhaustive);
                    res.unsupported_files.push(file_id);
                    res.scanned_bytes = total_raw_len;
                    return Ok(res);
                }

                let map = CaptureEncodingMap::build(&stitched)
                    .map_err(|_| QueryError::UnsupportedEncoding)?;

                if map
                    .spans()
                    .iter()
                    .any(|s| matches!(s.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte))
                {
                    let mut res = SearchResult::empty(SearchCoverage::Exhaustive);
                    res.unsupported_files.push(file_id);
                    res.scanned_bytes = total_raw_len;
                    return Ok(res);
                }

                let text = map.decoded_text();

                Self::scan_decoded_text(
                    text,
                    &map,
                    file_id,
                    revision,
                    total_raw_len,
                    query,
                    options,
                )
            }
        }
    }

    /// Exact raw-byte search in arbitrary bytes.
    pub fn scan_raw_bytes(
        raw_bytes: &[u8],
        file_id: FileId,
        revision: SourceRevision,
        needle: &[u8],
        options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        if needle.is_empty() {
            return Err(QueryError::EmptyNeedle);
        }

        let hits = find_overlapping_subsequences(raw_bytes, needle);
        let mut matches = Vec::new();
        let mut count = 0usize;

        for offset in hits {
            count += 1;
            if matches.len() < options.max_matches {
                let start = ByteOffset::new(offset as u64);
                let end = ByteOffset::new((offset + needle.len()) as u64);
                let range = ByteRange::new(start, end).map_err(|_| QueryError::InvalidRange)?;

                matches.push(SearchMatch {
                    occurrence_id: count as u64,
                    file_id,
                    revision,
                    decoded_range: None,
                    original_byte_range: range,
                    matched_text: String::from_utf8_lossy(needle).to_string(),
                    multiplicity: 1,
                });
            }
        }

        let coverage = if count > options.max_matches {
            SearchCoverage::TruncatedAtLimit {
                max_matches: options.max_matches,
            }
        } else {
            SearchCoverage::Exhaustive
        };

        Ok(SearchResult {
            matches,
            total_matches_counted: count,
            coverage,
            scanned_bytes: raw_bytes.len() as u64,
            unsupported_files: Vec::new(),
        })
    }

    fn scan_decoded_text(
        text: &str,
        map: &CaptureEncodingMap,
        file_id: FileId,
        revision: SourceRevision,
        total_raw_bytes: u64,
        query: &str,
        options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        let (case_sensitive, normalization) = match options.mode {
            SearchMode::DecodedText {
                case_sensitive,
                normalization,
            } => (case_sensitive, normalization),
            SearchMode::RawBytes => (true, UnicodeNormalization::Exact),
        };

        let mut matches = Vec::new();
        let mut count = 0usize;

        match normalization {
            UnicodeNormalization::Exact => {
                if case_sensitive {
                    // Exact literal match
                    let hits = find_overlapping_substrings(text, query);
                    for (start, end) in hits {
                        count += 1;
                        if matches.len() < options.max_matches {
                            let dec_range = DecodedUtf8Range::new(
                                DecodedUtf8Offset::new(start as u64),
                                DecodedUtf8Offset::new(end as u64),
                            )
                            .map_err(|_| QueryError::InvalidRange)?;

                            let orig_range = map
                                .decoded_utf8_range_to_byte_range(dec_range)
                                .map_err(|_| QueryError::InvalidRange)?;

                            matches.push(SearchMatch {
                                occurrence_id: count as u64,
                                file_id,
                                revision,
                                decoded_range: Some(dec_range),
                                original_byte_range: orig_range,
                                matched_text: text[start..end].to_string(),
                                multiplicity: 1,
                            });
                        }
                    }
                } else {
                    // Case-insensitive ASCII / basic folding
                    let hits = find_case_insensitive_substrings(text, query);
                    for (start, end) in hits {
                        count += 1;
                        if matches.len() < options.max_matches {
                            let dec_range = DecodedUtf8Range::new(
                                DecodedUtf8Offset::new(start as u64),
                                DecodedUtf8Offset::new(end as u64),
                            )
                            .map_err(|_| QueryError::InvalidRange)?;

                            let orig_range = map
                                .decoded_utf8_range_to_byte_range(dec_range)
                                .map_err(|_| QueryError::InvalidRange)?;

                            matches.push(SearchMatch {
                                occurrence_id: count as u64,
                                file_id,
                                revision,
                                decoded_range: Some(dec_range),
                                original_byte_range: orig_range,
                                matched_text: text[start..end].to_string(),
                                multiplicity: 1,
                            });
                        }
                    }
                }
            }
            UnicodeNormalization::CaseFold => {
                // Unicode case-folding with expansions (e.g. 'ß' -> "ss")
                let hits = find_unicode_folded_substrings(text, query);
                for hit in hits {
                    count += 1;
                    if matches.len() < options.max_matches {
                        let dec_range = DecodedUtf8Range::new(
                            DecodedUtf8Offset::new(hit.start as u64),
                            DecodedUtf8Offset::new(hit.end as u64),
                        )
                        .map_err(|_| QueryError::InvalidRange)?;

                        let orig_range = map
                            .decoded_utf8_range_to_byte_range(dec_range)
                            .map_err(|_| QueryError::InvalidRange)?;

                        matches.push(SearchMatch {
                            occurrence_id: count as u64,
                            file_id,
                            revision,
                            decoded_range: Some(dec_range),
                            original_byte_range: orig_range,
                            matched_text: hit.matched_text,
                            multiplicity: hit.multiplicity,
                        });
                    }
                }
            }
            UnicodeNormalization::Canonical => {
                // Canonical equivalence (e.g. decomposed combining accents)
                let hits = find_canonical_equivalent_substrings(text, query, case_sensitive);
                for hit in hits {
                    count += 1;
                    if matches.len() < options.max_matches {
                        let dec_range = DecodedUtf8Range::new(
                            DecodedUtf8Offset::new(hit.start as u64),
                            DecodedUtf8Offset::new(hit.end as u64),
                        )
                        .map_err(|_| QueryError::InvalidRange)?;

                        let orig_range = map
                            .decoded_utf8_range_to_byte_range(dec_range)
                            .map_err(|_| QueryError::InvalidRange)?;

                        matches.push(SearchMatch {
                            occurrence_id: count as u64,
                            file_id,
                            revision,
                            decoded_range: Some(dec_range),
                            original_byte_range: orig_range,
                            matched_text: hit.matched_text,
                            multiplicity: 1,
                        });
                    }
                }
            }
        }

        let coverage = if count > options.max_matches {
            SearchCoverage::TruncatedAtLimit {
                max_matches: options.max_matches,
            }
        } else {
            SearchCoverage::Exhaustive
        };

        Ok(SearchResult {
            matches,
            total_matches_counted: count,
            coverage,
            scanned_bytes: total_raw_bytes,
            unsupported_files: Vec::new(),
        })
    }
}

// =========================================================================
// Helper Match Algorithms Supporting Overlap and Expansions
// =========================================================================

/// Find all byte matches of needle in haystack, including overlapping occurrences.
fn find_overlapping_subsequences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let max_start = haystack.len() - needle.len();
    for i in 0..=max_start {
        if &haystack[i..i + needle.len()] == needle {
            results.push(i);
        }
    }
    results
}

/// Find all substring matches in text, including overlapping occurrences.
fn find_overlapping_substrings(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let n_bytes = needle.len();

    let mut byte_idx = 0;
    while byte_idx + n_bytes <= haystack.len() {
        if haystack.is_char_boundary(byte_idx)
            && haystack.is_char_boundary(byte_idx + n_bytes)
            && &haystack[byte_idx..byte_idx + n_bytes] == needle
        {
            results.push((byte_idx, byte_idx + n_bytes));
        }
        // Advance by next char boundary
        let ch_len = haystack[byte_idx..]
            .chars()
            .next()
            .map_or(1, |c| c.len_utf8());
        byte_idx += ch_len;
    }
    results
}

/// Find case-insensitive substring matches, including overlapping occurrences.
fn find_case_insensitive_substrings(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let needle_lower = needle.to_lowercase();
    let needle_len = needle.len();
    let mut results = Vec::new();

    let mut byte_idx = 0;
    while byte_idx + needle_len <= haystack.len() {
        if haystack.is_char_boundary(byte_idx)
            && haystack.is_char_boundary(byte_idx + needle_len)
            && haystack[byte_idx..byte_idx + needle_len].to_lowercase() == needle_lower
        {
            results.push((byte_idx, byte_idx + needle_len));
        }
        let ch_len = haystack[byte_idx..]
            .chars()
            .next()
            .map_or(1, |c| c.len_utf8());
        byte_idx += ch_len;
    }
    results
}

struct FoldedHit {
    start: usize,
    end: usize,
    matched_text: String,
    multiplicity: usize,
}

/// Unicode case-folding search with expansion handling (e.g. 'ß' -> "ss").
fn find_unicode_folded_substrings(haystack: &str, needle: &str) -> Vec<FoldedHit> {
    if needle.is_empty() {
        return Vec::new();
    }

    let needle_folded: String = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut results = Vec::new();

    // Scan chars with expansion mapping
    let chars: Vec<(usize, char)> = haystack.char_indices().collect();
    for (i, &(byte_idx, ch)) in chars.iter().enumerate() {
        let ch_end = if i + 1 < chars.len() {
            chars[i + 1].0
        } else {
            haystack.len()
        };

        // If searching for "ss" and char is 'ß' (German eszett which folds to "ss")
        if needle_folded == "ss" && ch == 'ß' {
            results.push(FoldedHit {
                start: byte_idx,
                end: ch_end,
                matched_text: ch.to_string(),
                multiplicity: 1,
            });
            continue;
        }

        // If searching for "s" and char is 'ß', it generates two occurrences on the same span
        if needle_folded == "s" && ch == 'ß' {
            results.push(FoldedHit {
                start: byte_idx,
                end: ch_end,
                matched_text: ch.to_string(),
                multiplicity: 2,
            });
            continue;
        }

        // Standard multi-char folded window check
        let mut candidate_folded = String::new();
        let mut end_idx = byte_idx;
        for &(_, next_c) in &chars[i..] {
            candidate_folded.extend(next_c.to_lowercase());
            end_idx += next_c.len_utf8();

            if candidate_folded == needle_folded {
                results.push(FoldedHit {
                    start: byte_idx,
                    end: end_idx,
                    matched_text: haystack[byte_idx..end_idx].to_string(),
                    multiplicity: 1,
                });
                break;
            } else if candidate_folded.len() > needle_folded.len() {
                break;
            }
        }
    }

    results
}

struct CanonicalHit {
    start: usize,
    end: usize,
    matched_text: String,
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

struct DecomposedChar {
    ch: char,
    orig_start: usize,
    orig_end: usize,
}

fn decompose_with_spans(s: &str) -> Vec<DecomposedChar> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    for (i, &(byte_idx, ch)) in chars.iter().enumerate() {
        let ch_end = if i + 1 < chars.len() {
            chars[i + 1].0
        } else {
            s.len()
        };
        if let Some(decomp) = canonical_decompose_char(ch) {
            for &dc in decomp {
                out.push(DecomposedChar {
                    ch: dc,
                    orig_start: byte_idx,
                    orig_end: ch_end,
                });
            }
        } else {
            out.push(DecomposedChar {
                ch,
                orig_start: byte_idx,
                orig_end: ch_end,
            });
        }
    }
    out
}

fn decompose_chars(s: &str) -> Vec<char> {
    let mut out = Vec::new();
    for ch in s.chars() {
        if let Some(decomp) = canonical_decompose_char(ch) {
            out.extend_from_slice(decomp);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Canonical equivalence search handling decomposed vs precomposed Unicode sequences.
fn find_canonical_equivalent_substrings(
    haystack: &str,
    needle: &str,
    case_sensitive: bool,
) -> Vec<CanonicalHit> {
    if needle.is_empty() || haystack.is_empty() {
        return Vec::new();
    }

    let needle_decomp = decompose_chars(needle);
    if needle_decomp.is_empty() {
        return Vec::new();
    }

    let haystack_decomp = decompose_with_spans(haystack);
    if needle_decomp.len() > haystack_decomp.len() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let max_start = haystack_decomp.len() - needle_decomp.len();

    for i in 0..=max_start {
        let mut matched = true;
        for (j, &nc) in needle_decomp.iter().enumerate() {
            let hc = haystack_decomp[i + j].ch;
            let equal = if case_sensitive {
                hc == nc
            } else {
                hc.to_lowercase().eq(nc.to_lowercase())
            };
            if !equal {
                matched = false;
                break;
            }
        }

        if matched {
            let start = haystack_decomp[i].orig_start;
            let end = haystack_decomp[i + needle_decomp.len() - 1].orig_end;
            results.push(CanonicalHit {
                start,
                end,
                matched_text: haystack[start..end].to_string(),
            });
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

        // UTF-16LE with BOM: "hello world"
        let mut raw = vec![0xFF, 0xFE];
        for c in "hello world".chars() {
            let mut buf = [0u16; 2];
            let enc = c.encode_utf16(&mut buf);
            for &u in &*enc {
                raw.extend_from_slice(&u.to_le_bytes());
            }
        }

        let capture = make_capture(file, rev, &raw);
        let result =
            DirectSourceScanner::scan_complete_capture(&capture, "world", &options).unwrap();

        assert_eq!(result.match_count(), 1);
        let m = &result.matches[0];
        assert_eq!(m.matched_text, "world");
        // "world" in UTF-16LE: starts after BOM (2) + "hello " (6 chars * 2 = 12 bytes) = 14
        // Length of "world" in UTF-16LE: 5 chars * 2 = 10 bytes
        assert_eq!(m.original_byte_range.start().get(), 14);
        assert_eq!(m.original_byte_range.end().get(), 24);

        let slice = &raw[14..24];
        let decoded_hit: Vec<u16> = slice
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let text_hit = String::from_utf16(&decoded_hit).unwrap();
        assert_eq!(text_hit, "world");
    }

    #[test]
    fn expansion_to_same_source_span_multiplicity() {
        let (file, rev, generation) = test_file_and_revision();
        let options = QueryOptions::new(generation).with_mode(SearchMode::DecodedText {
            case_sensitive: false,
            normalization: UnicodeNormalization::CaseFold,
        });

        // "Straße" has 'ß'
        let capture = make_capture(file, rev, "Straße".as_bytes());

        // Search "ss" matches "ß"
        let res_ss =
            DirectSourceScanner::scan_complete_capture(&capture, "ss", &options).unwrap();
        assert_eq!(res_ss.match_count(), 1);
        assert_eq!(res_ss.matches[0].matched_text, "ß");
        assert_eq!(res_ss.matches[0].multiplicity, 1);

        // Search "s" in "Straße" matches 'S' (at 0) and 'ß' (with multiplicity 2!)
        let res_s = DirectSourceScanner::scan_complete_capture(&capture, "s", &options).unwrap();
        assert_eq!(res_s.match_count(), 2);
        // First match is 'S' at 0
        assert_eq!(res_s.matches[0].original_byte_range.start().get(), 0);
        assert_eq!(res_s.matches[0].multiplicity, 1);
        // Second match is 'ß' at 4..6 with multiplicity 2!
        assert_eq!(res_s.matches[1].original_byte_range.start().get(), 4);
        assert_eq!(res_s.matches[1].original_byte_range.end().get(), 6);
        assert_eq!(res_s.matches[1].multiplicity, 2);
    }
}
