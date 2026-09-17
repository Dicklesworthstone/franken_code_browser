#![forbid(unsafe_code)]

//! Allocation-free intersection over the private validated page source. Each
//! call loads at most ONE posting page. Binary searches retain their exact low/
//! high bounds across misses; fallback ordering and limit lookahead match the
//! existing resident posting cursor. A page failure cannot become a negative.

use super::*;

#[derive(Clone, Copy)]
struct LowerBound { low: usize, high: usize, value: u64 }
impl LowerBound {
    fn new(range: Range<usize>, value: u64) -> Self { Self { low: range.start, high: range.end, value } }
    fn poll(&mut self, source: &mut dyn PairSource, loads: &mut usize, canceled: &mut dyn FnMut() -> bool)
        -> Result<Option<usize>, PagedPostingError> {
        while self.low < self.high {
            if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            let middle = self.low + (self.high - self.low) / 2;
            let Some(pair) = source.pair(middle, loads, canceled)? else { return Ok(None); };
            if pair < self.value { self.low = middle + 1; } else { self.high = middle; }
        }
        Ok(Some(self.low))
    }
}

pub struct PagedPostingCandidates<'index> {
    source: &'index mut dyn PairSource,
    keys: [u32; 3], lists: [Range<usize>; 3], driver: Range<usize>,
    prepared: usize, fallback_next: usize, text: bool, short: bool,
    bound: Option<LowerBound>, checking: Option<usize>, check_list: usize,
    check_position: Option<usize>, stats: PostingProbeStats,
    failure: Option<PagedPostingError>,
}
impl<'index> PagedPostingCandidates<'index> {
    pub(super) fn new(source: &'index mut dyn PairSource, needle: &[u8], text: bool) -> Self {
        let short = needle.len() < 3;
        let keys = if short { [0; 3] } else {
            let last = needle.len() - 3;
            [0, last / 2, last].map(|i| u32::from_be_bytes([0, needle[i], needle[i + 1], needle[i + 2]]))
        };
        Self { source, keys, lists: [0..0, 0..0, 0..0], driver: 0..0,
            prepared: if short { 6 } else { 0 }, fallback_next: 0, text, short,
            bound: None, checking: None, check_list: 0, check_position: None,
            stats: PostingProbeStats { list_lookups: if short { 0 } else { 3 }, ..Default::default() }, failure: None }
    }
    pub const fn stats(&self) -> PostingProbeStats { self.stats }
    pub fn is_finished(&self) -> bool {
        self.failure.is_none() && self.prepared == 6 && self.checking.is_none() && self.driver.is_empty()
            && self.fallback_next == self.source.fallback(self.text, self.short).len()
    }
    pub fn excluded_files(&self) -> Option<usize> {
        self.is_finished().then(|| self.source.captured_count() - self.stats.candidates_emitted - self.stats.fallback_emitted)
    }
    pub fn step(&mut self, mut canceled: impl FnMut() -> bool) -> Result<PostingStep, PagedPostingError> {
        if let Some(error) = self.failure { return Err(error); }
        let result = if canceled() { Err(SnapshotIndexError::Canceled.into()) } else { self.advance(&mut canceled) };
        let result = if result.is_ok() && canceled() { Err(SnapshotIndexError::Canceled.into()) } else { result };
        if let Err(error) = result { self.failure = Some(error); }
        result
    }
    fn advance(&mut self, canceled: &mut dyn FnMut() -> bool) -> Result<PostingStep, PagedPostingError> {
        let mut loads = 1;
        // Pinned page fences narrow each lower-bound search to at most one
        // page. A gram spanning many pages still needs only its two boundaries.
        while self.prepared < 6 {
            let key = self.keys[self.prepared / 2] + (self.prepared % 2) as u32;
            let target = pack(key, 0);
            if self.bound.is_none() { self.bound = Some(LowerBound::new(self.source.bracket(target), target)); }
            let Some(position) = self.bound.as_mut().unwrap().poll(self.source, &mut loads, canceled)? else {
                return Ok(PostingStep::Pending);
            };
            self.bound = None;
            if self.prepared % 2 == 0 { self.lists[self.prepared / 2].start = position; }
            else { self.lists[self.prepared / 2].end = position; }
            self.prepared += 1;
            if self.prepared == 6 {
                let rarest = (0..3).min_by_key(|&i| self.lists[i].len()).unwrap_or(0);
                self.driver = self.lists[rarest].clone();
            }
        }
        if self.checking.is_none() {
            let posting = if self.driver.is_empty() { None } else {
                let Some(pair) = self.source.pair(self.driver.start, &mut loads, canceled)? else { return Ok(PostingStep::Pending); };
                Some(ordinal(pair))
            };
            let fallback = self.source.fallback(self.text, self.short).get(self.fallback_next).map(|&n| n as usize);
            if let Some(file) = fallback.filter(|file| posting.is_none_or(|p| *file <= p)) {
                self.fallback_next += 1;
                if posting == Some(file) { self.driver.start += 1; self.stats.posting_entries_visited += 1; }
                self.stats.fallback_emitted += 1;
                return Ok(PostingStep::Candidate { ordinal: file, decision: IndexDecision::Fallback });
            }
            let Some(file) = posting else { return Ok(PostingStep::Finished); };
            self.driver.start += 1; self.stats.posting_entries_visited += 1;
            if self.text && self.source.coverage(file) != Coverage::Utf8 { return Ok(PostingStep::Pending); }
            self.checking = Some(file); self.check_list = 0;
        }
        let file = self.checking.unwrap();
        while self.check_list < 3 {
            let target = pack(self.keys[self.check_list], file);
            if self.check_position.is_none() {
                if self.bound.is_none() {
                    self.stats.membership_lookups += 1;
                    self.bound = Some(LowerBound::new(self.lists[self.check_list].clone(), target));
                }
                let Some(position) = self.bound.as_mut().unwrap().poll(self.source, &mut loads, canceled)? else {
                    return Ok(PostingStep::Pending);
                };
                self.bound = None; self.check_position = Some(position);
            }
            let position = self.check_position.unwrap();
            let matches = if position == self.lists[self.check_list].end { false } else {
                let Some(pair) = self.source.pair(position, &mut loads, canceled)? else { return Ok(PostingStep::Pending); };
                pair == target
            };
            self.check_position = None;
            if !matches { self.checking = None; return Ok(PostingStep::Pending); }
            self.check_list += 1;
        }
        self.checking = None; self.stats.candidates_emitted += 1;
        Ok(PostingStep::Candidate { ordinal: file, decision: IndexDecision::Verify })
    }
}
