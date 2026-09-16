#![forbid(unsafe_code)]

//! Incremental literal byte matching with bounded working memory.
//!
//! The cursor borrows the needle, not the input. Feed successive immutable source
//! slices and retain the unconsumed suffix of each slice. Matches may span any
//! number of calls, including calls ending in the middle of a match. No source
//! stitching or repository-sized occurrence list is constructed.

/// Admission limits for one cursor and one cooperative execution quantum.
pub const MAX_STREAM_NEEDLE_BYTES: usize = 65_536;
pub const MAX_STREAM_STEP_BYTES: usize = 65_536;
pub const MAX_STREAM_BATCH_HITS: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamError {
    EmptyNeedle,
    NeedleTooLong,
    AllocationFailed,
    OffsetOverflow,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EmptyNeedle => "STREAM_EMPTY_NEEDLE",
            Self::NeedleTooLong => "STREAM_NEEDLE_TOO_LONG",
            Self::AllocationFailed => "STREAM_ALLOCATION_FAILED",
            Self::OffsetOverflow => "STREAM_OFFSET_OVERFLOW",
        })
    }
}

impl std::error::Error for StreamError {}

/// A half-open range in the complete input stream, not the current input slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteMatch {
    pub occurrence_id: u64,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamStop {
    /// The supplied slice was consumed; this does not assert end of the source.
    InputConsumed,
    /// More of the supplied slice remains after the work quantum.
    ByteBudget,
    /// More of the supplied slice remains after filling the result batch.
    HitBudget,
    /// Explicit cancellation is terminal and never consumes more input.
    Canceled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteSearchBatch {
    pub hits: Vec<ByteMatch>,
    /// Advance the caller's slice by exactly this many bytes before resuming.
    pub consumed: usize,
    pub stop: StreamStop,
}

/// A resumable, overlapping KMP matcher. Auxiliary state is O(needle length),
/// and aggregate matching work is O(input length + needle length).
///
/// This type owns no runtime, filesystem authority, threads or source buffers.
/// The caller pins the source revision and checks its query generation before
/// publishing a batch. Call [`Self::cancel`] when a query is superseded.
#[derive(Debug)]
pub struct ByteSearchCursor<'needle> {
    needle: &'needle [u8],
    failure: Vec<usize>,
    matched: usize,
    scanned: u64,
    occurrences: u64,
    canceled: bool,
}

impl<'needle> ByteSearchCursor<'needle> {
    pub fn new(needle: &'needle [u8]) -> Result<Self, StreamError> {
        if needle.is_empty() {
            return Err(StreamError::EmptyNeedle);
        }
        if needle.len() > MAX_STREAM_NEEDLE_BYTES {
            return Err(StreamError::NeedleTooLong);
        }
        let mut failure = Vec::new();
        failure
            .try_reserve_exact(needle.len())
            .map_err(|_| StreamError::AllocationFailed)?;
        failure.resize(needle.len(), 0);
        let mut prefix = 0;
        for index in 1..needle.len() {
            while prefix > 0 && needle[index] != needle[prefix] {
                prefix = failure[prefix - 1];
            }
            if needle[index] == needle[prefix] {
                prefix += 1;
            }
            failure[index] = prefix;
        }
        Ok(Self {
            needle,
            failure,
            matched: 0,
            scanned: 0,
            occurrences: 0,
            canceled: false,
        })
    }

    pub const fn scanned_bytes(&self) -> u64 {
        self.scanned
    }

    pub const fn matches_seen(&self) -> u64 {
        self.occurrences
    }

    pub const fn is_canceled(&self) -> bool {
        self.canceled
    }

    pub fn cancel(&mut self) {
        self.canceled = true;
        self.matched = 0;
    }

    /// Break match continuity without resetting global offsets or hit identity.
    /// Use at an actual source-chunk boundary only when cross-chunk matching is
    /// disabled, never at a work-quantum or result-batch boundary.
    pub fn break_match_continuity(&mut self) {
        self.matched = 0;
    }

    /// Consume at most `byte_budget` bytes and emit at most `hit_budget` hits.
    /// Both budgets are additionally capped by the public per-step limits.
    /// Zero budgets consume no input. End-of-source is the caller's decision;
    /// exhausting an input slice is not a completeness claim.
    pub fn step(
        &mut self,
        input: &[u8],
        byte_budget: usize,
        hit_budget: usize,
    ) -> Result<ByteSearchBatch, StreamError> {
        let mut batch = ByteSearchBatch {
            hits: Vec::new(),
            consumed: 0,
            stop: StreamStop::InputConsumed,
        };
        if self.canceled {
            batch.stop = StreamStop::Canceled;
            return Ok(batch);
        }
        if input.is_empty() {
            return Ok(batch);
        }
        let work = input.len().min(byte_budget).min(MAX_STREAM_STEP_BYTES);
        let hit_limit = hit_budget.min(MAX_STREAM_BATCH_HITS);
        if hit_limit == 0 {
            batch.stop = StreamStop::HitBudget;
            return Ok(batch);
        }
        if work == 0 {
            batch.stop = StreamStop::ByteBudget;
            return Ok(batch);
        }
        // Validate the entire quantum before mutating any cursor state.
        self.scanned
            .checked_add(work as u64)
            .ok_or(StreamError::OffsetOverflow)?;
        batch
            .hits
            .try_reserve_exact(hit_limit.min(work))
            .map_err(|_| StreamError::AllocationFailed)?;

        for &byte in &input[..work] {
            while self.matched > 0 && byte != self.needle[self.matched] {
                self.matched = self.failure[self.matched - 1];
            }
            if byte == self.needle[self.matched] {
                self.matched += 1;
            }
            self.scanned += 1;
            batch.consumed += 1;
            if self.matched == self.needle.len() {
                // At most one occurrence ends at each byte, so this counter
                // cannot overflow before the prevalidated source offset does.
                self.occurrences += 1;
                batch.hits.push(ByteMatch {
                    occurrence_id: self.occurrences,
                    start: self.scanned - self.needle.len() as u64,
                    end: self.scanned,
                });
                self.matched = self.failure[self.matched - 1];
                if batch.hits.len() == hit_limit {
                    break;
                }
            }
        }
        batch.stop = if batch.consumed == input.len() {
            StreamStop::InputConsumed
        } else if batch.hits.len() == hit_limit {
            StreamStop::HitBudget
        } else {
            StreamStop::ByteBudget
        };
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &[u8], needle: &[u8], chunk: usize, work: usize, hits: usize) -> Vec<ByteMatch> {
        let mut cursor = ByteSearchCursor::new(needle).unwrap();
        let mut output = Vec::new();
        for block in input.chunks(chunk) {
            let mut remaining = block;
            while !remaining.is_empty() {
                let batch = cursor.step(remaining, work, hits).unwrap();
                assert!(batch.consumed > 0);
                assert!(batch.consumed <= work);
                assert!(batch.hits.len() <= hits);
                remaining = &remaining[batch.consumed..];
                output.extend(batch.hits);
            }
        }
        assert_eq!(cursor.scanned_bytes(), input.len() as u64);
        assert_eq!(cursor.matches_seen(), output.len() as u64);
        output
    }

    #[test]
    fn match_crosses_arbitrarily_many_chunks_and_resumes_after_overlap() {
        let hits = collect(b"abababababa", b"ababa", 1, 1, 1);
        assert_eq!(hits.iter().map(|hit| hit.start).collect::<Vec<_>>(), [0, 2, 4, 6]);
        assert_eq!(hits.iter().map(|hit| hit.occurrence_id).collect::<Vec<_>>(), [1, 2, 3, 4]);
    }

    #[test]
    fn zero_budgets_do_not_advance_or_lose_a_partial_match() {
        let mut cursor = ByteSearchCursor::new(b"abcd").unwrap();
        assert_eq!(cursor.step(b"ab", 2, 1).unwrap().consumed, 2);
        assert_eq!(cursor.step(b"cd", 0, 1).unwrap().stop, StreamStop::ByteBudget);
        assert_eq!(cursor.step(b"cd", 2, 0).unwrap().stop, StreamStop::HitBudget);
        assert_eq!(cursor.scanned_bytes(), 2);
        assert_eq!(cursor.step(b"cd", 2, 1).unwrap().hits[0].start, 0);
    }

    #[test]
    fn cancellation_is_sticky_and_does_not_consume_input() {
        let mut cursor = ByteSearchCursor::new(b"abc").unwrap();
        cursor.step(b"ab", 2, 1).unwrap();
        cursor.cancel();
        for _ in 0..3 {
            let batch = cursor.step(b"cabc", usize::MAX, usize::MAX).unwrap();
            assert_eq!(batch.stop, StreamStop::Canceled);
            assert_eq!(batch.consumed, 0);
            assert!(batch.hits.is_empty());
        }
        assert_eq!(cursor.scanned_bytes(), 2);
    }

    #[test]
    fn continuity_break_preserves_offsets_and_occurrence_identity() {
        let mut cursor = ByteSearchCursor::new(b"aa").unwrap();
        assert_eq!(cursor.step(b"aa", 2, 10).unwrap().hits[0].start, 0);
        cursor.break_match_continuity();
        let batch = cursor.step(b"aaa", 3, 10).unwrap();
        assert_eq!(batch.hits.iter().map(|hit| hit.start).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(batch.hits[0].occurrence_id, 2);
    }

    #[test]
    fn admission_and_per_step_limits_are_enforced() {
        assert_eq!(ByteSearchCursor::new(b"").unwrap_err(), StreamError::EmptyNeedle);
        let needle = vec![b'a'; MAX_STREAM_NEEDLE_BYTES + 1];
        assert_eq!(ByteSearchCursor::new(&needle).unwrap_err(), StreamError::NeedleTooLong);
        let input = vec![b'a'; MAX_STREAM_STEP_BYTES + 10];
        let mut cursor = ByteSearchCursor::new(b"z").unwrap();
        let batch = cursor.step(&input, usize::MAX, usize::MAX).unwrap();
        assert_eq!(batch.consumed, MAX_STREAM_STEP_BYTES);
        assert_eq!(batch.stop, StreamStop::ByteBudget);
        let mut cursor = ByteSearchCursor::new(b"a").unwrap();
        let batch = cursor.step(&input, usize::MAX, usize::MAX).unwrap();
        assert_eq!(batch.hits.len(), MAX_STREAM_BATCH_HITS);
        assert_eq!(batch.stop, StreamStop::HitBudget);
    }

    #[test]
    fn overflow_is_rejected_before_mutation() {
        let mut cursor = ByteSearchCursor::new(b"a").unwrap();
        cursor.scanned = u64::MAX;
        assert_eq!(cursor.step(b"a", 1, 1), Err(StreamError::OffsetOverflow));
        assert_eq!(cursor.scanned_bytes(), u64::MAX);
        assert_eq!(cursor.matches_seen(), 0);
    }

    #[test]
    fn differential_all_small_binary_inputs_and_every_chunk_size() {
        // Independent windows oracle: the negative control below deliberately
        // removes a hit to prove that the equality assertion is meaningful.
        for size in 0..=8 {
            for bits in 0..(1usize << size) {
                let input: Vec<u8> = (0..size).map(|i| b'a' + ((bits >> i) & 1) as u8).collect();
                for needle_size in 1..=4 {
                    for needle_bits in 0..(1usize << needle_size) {
                        let needle: Vec<u8> = (0..needle_size)
                            .map(|i| b'a' + ((needle_bits >> i) & 1) as u8).collect();
                        let expected: Vec<u64> = input.windows(needle_size).enumerate()
                            .filter(|(_, window)| *window == needle.as_slice())
                            .map(|(offset, _)| offset as u64).collect();
                        for chunk in 1..=size.max(1) {
                            let actual = collect(&input, &needle, chunk, 2, 1);
                            assert_eq!(actual.iter().map(|hit| hit.start).collect::<Vec<_>>(), expected);
                        }
                    }
                }
            }
        }
        let actual = collect(b"aaaa", b"aa", 1, 1, 1);
        assert_ne!(&actual[1..], actual.as_slice());
    }
}
