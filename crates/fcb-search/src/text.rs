#![forbid(unsafe_code)]

//! Bounded exact decoded-text scans. Decoding and provenance are supplied by
//! fcb-source; this adapter retains only a decoding window and the mapping spans
//! needed by a needle that crosses windows. It never stitches a whole capture.

use std::{borrow::Cow, collections::VecDeque};

use fcb_core::{ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId, SourceRevision};
use fcb_source::{CaptureEncodingMap, DetectedEncoding, MappingSpan, SpanKind, detect_encoding};

use crate::stream::{ByteSearchCursor, MAX_STREAM_BATCH_HITS, MAX_STREAM_STEP_BYTES};
use crate::{QueryError, QueryOptions, SearchCoverage, SearchMatch, SearchResult};

const WINDOW_BYTES: usize = 16_384;

pub(crate) fn scan_exact_chunks<'a>(
    chunks: impl IntoIterator<Item = Result<&'a [u8], QueryError>>,
    total_bytes: u64,
    file_id: FileId,
    revision: SourceRevision,
    query: &str,
    options: &QueryOptions,
    mut canceled: impl FnMut() -> bool,
) -> Result<SearchResult, QueryError> {
    let mut scan = ExactScan {
        cursor: ByteSearchCursor::new(query.as_bytes())?,
        query,
        options,
        file_id,
        revision,
        encoding: options.declared_encoding,
        raw_offset: 0,
        decoded_offset: 0,
        utf16_offset: 0,
        scalar_offset: 0,
        spans: VecDeque::new(),
        chunk_ends: VecDeque::new(),
        result: SearchResult::empty(SearchCoverage::Exhaustive),
    };
    if canceled() {
        scan.result.coverage = SearchCoverage::CanceledEarly;
        return Ok(scan.result);
    }
    if options.max_matches == 0 && total_bytes > 0 {
        scan.result.coverage = SearchCoverage::TruncatedAtLimit { max_matches: 0 };
        return Ok(scan.result);
    }
    let byte_limit = options.max_bytes_scanned.unwrap_or(u64::MAX).min(total_bytes);
    let mut staging = Vec::new();
    staging.try_reserve_exact(WINDOW_BYTES).map_err(|_| QueryError::LimitExceeded)?;
    let mut declared_bytes = 0u64;
    for chunk in chunks {
        let chunk = chunk?;
        declared_bytes = declared_bytes.checked_add(chunk.len() as u64)
            .ok_or(QueryError::InvalidRange)?;
        if declared_bytes > total_bytes {
            return Err(QueryError::InvalidRange);
        }
        if !options.cross_chunk && !chunk.is_empty() {
            scan.chunk_ends.try_reserve(1).map_err(|_| QueryError::LimitExceeded)?;
            scan.chunk_ends.push_back(declared_bytes);
        }
        let mut remaining = chunk;
        while !remaining.is_empty() {
            if canceled() {
                scan.result.coverage = SearchCoverage::CanceledEarly;
                return Ok(scan.result);
            }
            let available = byte_limit.saturating_sub(scan.result.scanned_bytes);
            if available == 0 {
                if scan.process(&mut staging, false)? {
                    return Ok(scan.result);
                }
                scan.result.coverage = SearchCoverage::BudgetExhausted {
                    bytes_scanned: scan.result.scanned_bytes,
                };
                return Ok(scan.result);
            }
            let take = remaining.len().min(WINDOW_BYTES - staging.len())
                .min(available.min(WINDOW_BYTES as u64) as usize);
            staging.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            scan.result.scanned_bytes += take as u64;
            if staging.len() == WINDOW_BYTES && scan.process(&mut staging, false)? {
                return Ok(scan.result);
            }
        }
    }
    if declared_bytes != total_bytes {
        return Err(QueryError::InvalidRange);
    }
    scan.process(&mut staging, true)?;
    Ok(scan.result)
}

struct ExactScan<'a> {
    cursor: ByteSearchCursor<'a>,
    query: &'a str,
    options: &'a QueryOptions,
    file_id: FileId,
    revision: SourceRevision,
    encoding: Option<DetectedEncoding>,
    raw_offset: u64,
    decoded_offset: u64,
    utf16_offset: u64,
    scalar_offset: u64,
    spans: VecDeque<MappingSpan>,
    chunk_ends: VecDeque<u64>,
    result: SearchResult,
}

impl ExactScan<'_> {
    /// Return true for a terminal result (proven truncation or unavailable text).
    fn process(&mut self, staging: &mut Vec<u8>, eof: bool) -> Result<bool, QueryError> {
        // A budget ending inside a BOM is incomplete, not an unsupported file.
        if staging.is_empty() {
            return Ok(false);
        }
        if !eof && self.raw_offset == 0 && self.encoding.is_none()
            && matches!(staging.as_slice(), [0xEF] | [0xEF, 0xBB] | [0xFF] | [0xFE]) {
            return Ok(false);
        }
        let encoding = *self.encoding.get_or_insert_with(|| detect_encoding(staging));
        if encoding == DetectedEncoding::Unsupported {
            self.unavailable();
            return Ok(true);
        }
        let process_len = if eof { staging.len() } else { complete_prefix(staging, encoding) };
        if process_len == 0 {
            return Ok(false);
        }
        // fcb-source recognizes a BOM at the start of each supplied slice.
        // At nonzero capture offsets those bytes are an actual U+FEFF scalar,
        // not a new file header. Restore that scalar while using the same
        // source decoder and provenance for the rest of the bounded window.
        let interior_bom = self.raw_offset != 0 && match encoding {
            DetectedEncoding::Utf8 { has_bom: true } => staging.starts_with(&[0xEF, 0xBB, 0xBF]),
            DetectedEncoding::Utf16Le => staging.starts_with(&[0xFF, 0xFE]),
            DetectedEncoding::Utf16Be => staging.starts_with(&[0xFE, 0xFF]),
            _ => false,
        };
        let extra = if interior_bom { 1u64 } else { 0 };
        let map = CaptureEncodingMap::build_with_base_offset(
            &staging[..process_len], encoding, self.raw_offset,
            self.decoded_offset.checked_add(3 * extra).ok_or(QueryError::InvalidRange)?,
            self.utf16_offset.checked_add(extra).ok_or(QueryError::InvalidRange)?,
            self.scalar_offset.checked_add(extra).ok_or(QueryError::InvalidRange)?,
        ).map_err(|_| QueryError::UnsupportedEncoding)?;
        if map.spans().iter().any(|span| matches!(span.kind,
            SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
            self.unavailable();
            return Ok(true);
        }
        self.spans.try_reserve(map.spans().len() + interior_bom as usize)
            .map_err(|_| QueryError::LimitExceeded)?;
        let decoded = if interior_bom {
            self.spans.push_back(MappingSpan {
                raw_start: self.raw_offset, raw_len: encoding.bom_bytes_len(),
                decoded_start: self.decoded_offset, decoded_len: 3,
                utf16_start: self.utf16_offset, utf16_len: 1,
                scalar_start: self.scalar_offset, scalar_len: 1,
                kind: SpanKind::BmpMultiByte,
            });
            let mut decoded = String::new();
            decoded.try_reserve_exact(3 + map.decoded_text().len())
                .map_err(|_| QueryError::LimitExceeded)?;
            decoded.push('\u{FEFF}');
            decoded.push_str(map.decoded_text());
            Cow::Owned(decoded)
        } else {
            Cow::Borrowed(map.decoded_text())
        };
        for &span in map.spans() {
            if span.decoded_len > 0 {
                self.spans.push_back(span);
            }
        }
        if let Some(last) = map.spans().last() {
            self.utf16_offset = last.utf16_start + last.utf16_len;
            self.scalar_offset = last.scalar_start + last.scalar_len;
        }
        self.raw_offset = self.raw_offset.checked_add(process_len as u64)
            .ok_or(QueryError::InvalidRange)?;
        self.decoded_offset = self.decoded_offset.checked_add(decoded.len() as u64)
            .ok_or(QueryError::InvalidRange)?;
        let mut remaining = decoded.as_bytes();
        while !remaining.is_empty() {
            let hit_budget = self.options.max_matches.saturating_sub(self.result.matches.len())
                .saturating_add(1).min(MAX_STREAM_BATCH_HITS);
            let batch = self.cursor.step(remaining, MAX_STREAM_STEP_BYTES, hit_budget)?;
            remaining = &remaining[batch.consumed..];
            for hit in batch.hits {
                let start = self.original_offset(hit.start)?;
                let end = self.original_offset(hit.end)?;
                if !self.options.cross_chunk && self.crosses_source_chunk(start, end) {
                    continue;
                }
                self.result.total_matches_counted = self.result.total_matches_counted
                    .checked_add(1).ok_or(QueryError::LimitExceeded)?;
                if self.result.matches.len() == self.options.max_matches {
                    self.result.coverage = SearchCoverage::TruncatedAtLimit {
                        max_matches: self.options.max_matches,
                    };
                    return Ok(true);
                }
                self.result.matches.try_reserve(1).map_err(|_| QueryError::LimitExceeded)?;
                self.result.matches.push(SearchMatch {
                    occurrence_id: self.result.total_matches_counted as u64,
                    file_id: self.file_id,
                    revision: self.revision,
                    decoded_range: Some(DecodedUtf8Range::new(
                        DecodedUtf8Offset::new(hit.start), DecodedUtf8Offset::new(hit.end),
                    ).map_err(|_| QueryError::InvalidRange)?),
                    original_byte_range: ByteRange::new(ByteOffset::new(start), ByteOffset::new(end))
                        .map_err(|_| QueryError::InvalidRange)?,
                    matched_text: self.query.to_owned(),
                    multiplicity: 1,
                });
            }
            self.prune();
        }
        // Empty decoded windows (for example a BOM) still allow retirement.
        self.prune();
        let retained = staging.len() - process_len;
        staging.copy_within(process_len.., 0);
        staging.truncate(retained);
        Ok(false)
    }

    fn unavailable(&mut self) {
        self.result.matches.clear();
        self.result.total_matches_counted = 0;
        self.result.unsupported_files.push(self.file_id);
    }

    fn prune(&mut self) {
        let keep_from = self.cursor.scanned_bytes().saturating_sub(self.query.len() as u64);
        while self.spans.front().is_some_and(|span| span.decoded_end() <= keep_from) {
            self.spans.pop_front();
        }
        let raw_from = self.spans.front().map_or(self.raw_offset, |span| span.raw_start);
        while self.chunk_ends.front().is_some_and(|end| *end <= raw_from) {
            self.chunk_ends.pop_front();
        }
    }

    fn original_offset(&self, decoded: u64) -> Result<u64, QueryError> {
        let mut low = 0;
        let mut high = self.spans.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if self.spans[mid].decoded_start <= decoded { low = mid + 1; } else { high = mid; }
        }
        let span = low.checked_sub(1).and_then(|index| self.spans.get(index))
            .ok_or(QueryError::InvalidRange)?;
        if decoded == span.decoded_start { return Ok(span.raw_start); }
        if decoded == span.decoded_end() { return Ok(span.raw_end()); }
        if decoded < span.decoded_end() && span.kind == SpanKind::AsciiRun {
            let scale = span.raw_len / span.decoded_len;
            return Ok(span.raw_start + (decoded - span.decoded_start) * scale);
        }
        Err(QueryError::InvalidRange)
    }

    fn crosses_source_chunk(&self, start: u64, end: u64) -> bool {
        let mut low = 0;
        let mut high = self.chunk_ends.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if self.chunk_ends[mid] <= start { low = mid + 1; } else { high = mid; }
        }
        self.chunk_ends.get(low).is_some_and(|boundary| *boundary < end)
    }
}

/// Do not manufacture malformed input by cutting a scalar or surrogate pair at
/// a work-quantum boundary. Actual malformed sequences are left for fcb-source
/// to identify; only a possibly valid incomplete suffix is retained.
fn complete_prefix(bytes: &[u8], encoding: DetectedEncoding) -> usize {
    match encoding {
        DetectedEncoding::Utf16Le | DetectedEncoding::Utf16Be => {
            let mut len = bytes.len() - bytes.len() % 2;
            if len >= 2 {
                let pair = [bytes[len - 2], bytes[len - 1]];
                let unit = if encoding == DetectedEncoding::Utf16Le {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                };
                if (0xD800..=0xDBFF).contains(&unit) { len -= 2; }
            }
            len
        }
        DetectedEncoding::Utf8 { .. } => match std::str::from_utf8(bytes) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => bytes.len(),
        },
        DetectedEncoding::Unsupported => bytes.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, QueryGeneration};

    fn scan(bytes: &[u8], needle: &str, chunk: usize, budget: Option<u64>) -> SearchResult {
        let owner = ArenaOwnerId::new(1).unwrap();
        let mut options = QueryOptions::new(QueryGeneration::new(owner, 1).unwrap());
        options.max_bytes_scanned = budget;
        scan_exact_chunks(bytes.chunks(chunk).map(Ok), bytes.len() as u64,
            FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap(),
            needle, &options, || false).unwrap()
    }

    #[test]
    fn utf8_matches_cross_decoding_windows_with_exact_ranges() {
        let prefix = "x".repeat(WINDOW_BYTES - 2);
        let text = format!("{prefix}é🙂é🙂");
        for chunk in [1, 3, 4096, WINDOW_BYTES, text.len()] {
            let result = scan(text.as_bytes(), "é🙂", chunk, None);
            assert_eq!(result.match_count(), 2);
            assert_eq!(result.matches[0].original_byte_range.start().get(), prefix.len() as u64);
            assert_eq!(result.matches[1].original_byte_range.end().get(), text.len() as u64);
            assert!(result.is_complete());
        }
    }

    #[test]
    fn utf16_surrogate_split_at_window_and_input_boundaries_preserves_provenance() {
        let text = format!("{}🙂z", "x".repeat(WINDOW_BYTES / 2 - 2));
        for little in [true, false] {
            let mut bytes = if little { vec![0xFF, 0xFE] } else { vec![0xFE, 0xFF] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            for chunk in [1, 2, 3, WINDOW_BYTES, bytes.len()] {
                let result = scan(&bytes, "🙂z", chunk, None);
                assert_eq!(result.match_count(), 1);
                assert_eq!(result.matches[0].original_byte_range.start().get(), (WINDOW_BYTES - 2) as u64);
                assert_eq!(result.matches[0].original_byte_range.end().get(), bytes.len() as u64);
                assert_eq!(result.matches[0].decoded_range.unwrap().start().get(), (WINDOW_BYTES / 2 - 2) as u64);
                assert!(result.is_complete());
            }
        }
    }

    #[test]
    fn budget_inside_a_scalar_or_bom_is_incomplete_not_unsupported() {
        let bytes = "ab🙂cd".as_bytes();
        let result = scan(bytes, "ab", 1, Some(4));
        assert_eq!(result.match_count(), 1);
        assert_eq!(result.scanned_bytes, 4);
        assert!(result.unsupported_files.is_empty());
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 4 });
        let result = scan(&[0xFF, 0xFE, b'a', 0], "a", 1, Some(1));
        assert!(result.unsupported_files.is_empty());
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 1 });
    }

    #[test]
    fn malformed_input_cannot_be_a_complete_negative_result() {
        let result = scan(b"hello\xFF", "absent", 1, None);
        assert_eq!(result.unsupported_files.len(), 1);
        assert!(!result.is_complete());
    }

    #[test]
    fn overlapping_matches_can_span_more_than_two_tiny_chunks() {
        let result = scan(b"ababababa", "ababa", 1, None);
        assert_eq!(result.matches.iter().map(|hit| hit.original_byte_range.start().get()).collect::<Vec<_>>(), [0, 2, 4]);
    }

    #[test]
    fn interior_bom_at_window_start_is_source_text_not_another_header() {
        let text = format!("{}\u{FEFF}z", "x".repeat(WINDOW_BYTES - 3));
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(text.as_bytes());
        let result = scan(&utf8, "\u{FEFF}z", 3, None);
        assert_eq!(result.match_count(), 1);
        assert_eq!(result.matches[0].original_byte_range.start().get(), WINDOW_BYTES as u64);
        assert_eq!(result.matches[0].decoded_range.unwrap().start().get(), (WINDOW_BYTES - 3) as u64);
        for little in [true, false] {
            let text = format!("{}\u{FEFF}z", "x".repeat(WINDOW_BYTES / 2 - 1));
            let mut bytes = if little { vec![0xFF, 0xFE] } else { vec![0xFE, 0xFF] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            let result = scan(&bytes, "\u{FEFF}z", 3, None);
            assert_eq!(result.match_count(), 1);
            assert_eq!(result.matches[0].original_byte_range.start().get(), WINDOW_BYTES as u64);
            assert_eq!(result.matches[0].original_byte_range.end().get(), bytes.len() as u64);
        }
    }

    #[test]
    fn tiny_ascii_budget_returns_available_hits_without_waiting_for_a_bom() {
        let result = scan(b"abc", "a", 1, Some(1));
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].original_byte_range.start().get(), 0);
        assert_eq!(result.scanned_bytes, 1);
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 1 });
    }

    #[test]
    fn disabled_cross_chunk_respects_source_boundaries_not_decoding_windows() {
        let owner = ArenaOwnerId::new(1).unwrap();
        let mut options = QueryOptions::new(QueryGeneration::new(owner, 1).unwrap());
        options.cross_chunk = false;
        let result = scan_exact_chunks(b"abababa".chunks(2).map(Ok), 7,
            FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap(),
            "aba", &options, || false).unwrap();
        assert!(result.matches.is_empty());
        assert!(result.is_complete());
        assert_eq!(result.scanned_bytes, 7);
    }

}
