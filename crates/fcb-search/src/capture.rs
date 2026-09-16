#![forbid(unsafe_code)]

//! Capture adapters for the bounded streaming matcher.

use fcb_core::{ByteOffset, ByteRange, FileId, SourceRevision};

use crate::stream::{ByteSearchCursor, StreamError, MAX_STREAM_BATCH_HITS, MAX_STREAM_STEP_BYTES};
use crate::{QueryError, QueryOptions, SearchCoverage, SearchMatch, SearchResult};

impl From<StreamError> for QueryError {
    fn from(error: StreamError) -> Self {
        match error {
            StreamError::EmptyNeedle => Self::EmptyNeedle,
            StreamError::NeedleTooLong => Self::NeedleTooLong,
            StreamError::AllocationFailed => Self::LimitExceeded,
            StreamError::OffsetOverflow => Self::InvalidRange,
        }
    }
}

/// Scan a closed, contiguous sequence of capture chunks without concatenating
/// them. The caller supplies the exact total and retains the captured revision.
/// `total_matches_counted` is matches actually seen, not an invented total for
/// an unscanned suffix. At most one extra hit is inspected to prove truncation.
pub(crate) fn scan_raw_chunks<'a>(
    chunks: impl IntoIterator<Item = Result<&'a [u8], QueryError>>,
    total_bytes: u64,
    file_id: FileId,
    revision: SourceRevision,
    needle: &[u8],
    options: &QueryOptions,
    mut canceled: impl FnMut() -> bool,
) -> Result<SearchResult, QueryError> {
    let mut cursor = ByteSearchCursor::new(needle)?;
    let mut result = SearchResult::empty(SearchCoverage::Exhaustive);
    if canceled() {
        result.coverage = SearchCoverage::CanceledEarly;
        return Ok(result);
    }
    if options.max_matches == 0 && total_bytes > 0 {
        result.coverage = SearchCoverage::TruncatedAtLimit { max_matches: 0 };
        return Ok(result);
    }
    let byte_limit = options.max_bytes_scanned.unwrap_or(u64::MAX);
    let mut declared_bytes = 0u64;
    for chunk in chunks {
        let chunk = chunk?;
        declared_bytes = declared_bytes
            .checked_add(chunk.len() as u64)
            .ok_or(QueryError::InvalidRange)?;
        if declared_bytes > total_bytes {
            return Err(QueryError::InvalidRange);
        }
        if !options.cross_chunk {
            cursor.break_match_continuity();
        }
        let mut remaining = chunk;
        while !remaining.is_empty() {
            if canceled() {
                result.coverage = SearchCoverage::CanceledEarly;
                return Ok(result);
            }
            let remaining_bytes = byte_limit.saturating_sub(cursor.scanned_bytes());
            if remaining_bytes == 0 {
                result.coverage = SearchCoverage::BudgetExhausted {
                    bytes_scanned: cursor.scanned_bytes(),
                };
                return Ok(result);
            }
            let work = remaining_bytes.min(MAX_STREAM_STEP_BYTES as u64) as usize;
            let hit_budget = options.max_matches.saturating_sub(result.matches.len())
                .saturating_add(1).min(MAX_STREAM_BATCH_HITS);
            let batch = cursor.step(remaining, work, hit_budget)?;
            result.scanned_bytes = cursor.scanned_bytes();
            remaining = &remaining[batch.consumed..];
            let stored = batch.hits.len().min(options.max_matches - result.matches.len());
            result.matches.try_reserve_exact(stored).map_err(|_| QueryError::LimitExceeded)?;
            for hit in batch.hits {
                result.total_matches_counted = result.total_matches_counted
                    .checked_add(1).ok_or(QueryError::LimitExceeded)?;
                if result.matches.len() == options.max_matches {
                    result.coverage = SearchCoverage::TruncatedAtLimit {
                        max_matches: options.max_matches,
                    };
                    return Ok(result);
                }
                result.matches.push(SearchMatch {
                    occurrence_id: hit.occurrence_id,
                    file_id,
                    revision,
                    decoded_range: None,
                    original_byte_range: ByteRange::new(
                        ByteOffset::new(hit.start), ByteOffset::new(hit.end),
                    ).map_err(|_| QueryError::InvalidRange)?,
                    matched_text: String::from_utf8_lossy(needle).into_owned(),
                    multiplicity: 1,
                });
            }
        }
    }
    if declared_bytes != total_bytes {
        return Err(QueryError::InvalidRange);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, QueryGeneration};
    use crate::SearchMode;

    fn options() -> (FileId, SourceRevision, QueryOptions) {
        let owner = ArenaOwnerId::new(1).unwrap();
        (FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap(),
         QueryOptions::new(QueryGeneration::new(owner, 1).unwrap()).with_mode(SearchMode::RawBytes))
    }

    fn scan(chunks: &[&[u8]], needle: &[u8], options: &QueryOptions) -> SearchResult {
        let (file, revision, _) = self::options();
        scan_raw_chunks(chunks.iter().copied().map(Ok), chunks.iter().map(|c| c.len() as u64).sum(),
            file, revision, needle, options, || false).unwrap()
    }

    #[test]
    fn capture_adapter_matches_across_three_or_more_chunks_in_source_order() {
        let (_, _, options) = options();
        let result = scan(&[b"a", b"b", b"a", b"b", b"a", b"b", b"a"], b"ababa", &options);
        assert_eq!(result.matches.iter().map(|hit| hit.original_byte_range.start().get()).collect::<Vec<_>>(), [0, 2]);
        assert!(result.is_complete());
        assert_eq!(result.scanned_bytes, 7);
    }

    #[test]
    fn byte_budget_never_reads_the_unsearched_suffix() {
        let (_, _, mut options) = options();
        options.max_bytes_scanned = Some(4);
        let result = scan(&[b"ab", b"aba"], b"aba", &options);
        assert_eq!(result.match_count(), 1);
        assert_eq!(result.scanned_bytes, 4);
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 4 });
        options.max_bytes_scanned = Some(0);
        assert_eq!(scan(&[b"abc"], b"a", &options).scanned_bytes, 0);
    }

    #[test]
    fn exact_limit_is_complete_but_an_additional_match_proves_truncation() {
        let (_, _, options) = options();
        let options = options.with_max_matches(2);
        assert!(scan(&[b"a-a"], b"a", &options).is_complete());
        let result = scan(&[b"aaaaa"], b"a", &options);
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.match_count(), 3);
        assert_eq!(result.scanned_bytes, 3);
        assert_eq!(result.coverage, SearchCoverage::TruncatedAtLimit { max_matches: 2 });
        let zero = scan(&[b"a"], b"a", &options.with_max_matches(0));
        assert!(zero.matches.is_empty());
        assert_eq!(zero.scanned_bytes, 0);
    }

    #[test]
    fn disabled_cross_chunk_does_not_disable_overlap_within_each_chunk() {
        let (_, _, mut options) = options();
        options.cross_chunk = false;
        let result = scan(&[b"aaa", b"aaa"], b"aa", &options);
        assert_eq!(result.matches.iter().map(|hit| hit.original_byte_range.start().get()).collect::<Vec<_>>(), [0, 1, 3, 4]);
    }

    #[test]
    fn cancellation_returns_partial_coverage_at_a_bounded_checkpoint() {
        let (file, revision, options) = options();
        let bytes = vec![b'x'; MAX_STREAM_STEP_BYTES * 3];
        let mut polls = 0;
        let result = scan_raw_chunks(std::iter::once(Ok(bytes.as_slice())), bytes.len() as u64,
            file, revision, b"z", &options, || { polls += 1; polls > 2 }).unwrap();
        assert_eq!(result.coverage, SearchCoverage::CanceledEarly);
        assert_eq!(result.scanned_bytes, MAX_STREAM_STEP_BYTES as u64);
    }
}
