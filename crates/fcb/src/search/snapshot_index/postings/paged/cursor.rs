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
    probes: GramProbes, lists: [Range<usize>; MAX_PROBE_KEYS],
    drivers: [Range<usize>; 8], heads: [Option<usize>; 8], driver_count: usize,
    prepared: usize, fallback_next: usize, policy: MatchPolicy, last_examined: Option<usize>,
    bound: Option<LowerBound>, checking: Option<usize>, check_group: usize, check_key: usize,
    check_position: Option<usize>, stats: PostingProbeStats,
    failure: Option<PagedPostingError>,
}
impl<'index> PagedPostingCandidates<'index> {
    pub(super) fn new(source: &'index mut dyn PairSource, needle: &[u8], policy: MatchPolicy) -> Self {
        let probes = GramProbes::new(needle, policy);
        let stats = PostingProbeStats { list_lookups: probes.count, ..Default::default() };
        Self { source, probes, lists: std::array::from_fn(|_| 0..0),
            drivers: std::array::from_fn(|_| 0..0), heads: [None; 8], driver_count: 0,
            prepared: 0, fallback_next: 0, policy, last_examined: None,
            bound: None, checking: None, check_group: 0, check_key: 0, check_position: None,
            stats, failure: None }
    }
    pub const fn stats(&self) -> PostingProbeStats { self.stats }
    pub fn is_finished(&self) -> bool {
        self.failure.is_none() && self.prepared == self.probes.count * 2 && self.checking.is_none()
            && self.drivers[..self.driver_count].iter().all(Range::is_empty)
            && self.fallback_next == self.source.fallback(self.policy, self.probes.fallback_all()).len()
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
        while self.prepared < self.probes.count * 2 {
            let key = self.probes.keys[self.prepared / 2] + (self.prepared % 2) as u32;
            let target = pack(key, 0);
            if self.bound.is_none() { self.bound = Some(LowerBound::new(self.source.bracket(target), target)); }
            let Some(position) = self.bound.as_mut().unwrap().poll(self.source, &mut loads, canceled)? else {
                return Ok(PostingStep::Pending);
            };
            self.bound = None;
            if self.prepared % 2 == 0 { self.lists[self.prepared / 2].start = position; }
            else { self.lists[self.prepared / 2].end = position; }
            self.prepared += 1;
            if self.prepared == self.probes.count * 2 {
                let rarest = self.probes.rarest(&self.lists);
                self.driver_count = rarest.len();
                for (driver, key) in rarest.enumerate() { self.drivers[driver] = self.lists[key].clone(); }
            }
        }
        if self.checking.is_none() {
            // Keep verified list heads across page misses. A one-page cache can
            // otherwise thrash forever while preparing a multi-list union.
            for driver in 0..self.driver_count {
                if self.heads[driver].is_none() && !self.drivers[driver].is_empty() {
                    let Some(pair) = self.source.pair(self.drivers[driver].start, &mut loads, canceled)? else {
                        return Ok(PostingStep::Pending);
                    };
                    self.heads[driver] = Some(ordinal(pair));
                }
            }
            let driver = (0..self.driver_count).filter(|&i| self.heads[i].is_some())
                .min_by_key(|&i| self.heads[i]);
            let posting = driver.and_then(|i| self.heads[i]);
            let fallback = self.source.fallback(self.policy, self.probes.fallback_all())
                .get(self.fallback_next).map(|&n| n as usize);
            if let Some(file) = fallback.filter(|file| posting.is_none_or(|p| *file <= p)) {
                self.fallback_next += 1;
                if posting == Some(file) { self.consume(driver.unwrap()); }
                self.last_examined = Some(file);
                self.stats.fallback_emitted += 1;
                return Ok(PostingStep::Candidate { ordinal: file, decision: IndexDecision::Fallback });
            }
            let Some(file) = posting else { return Ok(PostingStep::Finished); };
            self.consume(driver.unwrap());
            if self.last_examined == Some(file) { return Ok(PostingStep::Pending); }
            self.last_examined = Some(file);
            if !self.policy.compatible(self.source.coverage(file)) { return Ok(PostingStep::Pending); }
            self.checking = Some(file); self.check_group = 0; self.check_key = self.probes.groups[0].start;
        }
        let file = self.checking.unwrap();
        while self.check_group < self.probes.groups.len() {
            let target = pack(self.probes.keys[self.check_key], file);
            if self.check_position.is_none() {
                if self.bound.is_none() {
                    self.stats.membership_lookups += 1;
                    self.bound = Some(LowerBound::new(self.lists[self.check_key].clone(), target));
                }
                let Some(position) = self.bound.as_mut().unwrap().poll(self.source, &mut loads, canceled)? else {
                    return Ok(PostingStep::Pending);
                };
                self.bound = None; self.check_position = Some(position);
            }
            let position = self.check_position.unwrap();
            let matches = if position == self.lists[self.check_key].end { false } else {
                let Some(pair) = self.source.pair(position, &mut loads, canceled)? else { return Ok(PostingStep::Pending); };
                pair == target
            };
            self.check_position = None;
            if matches {
                self.check_group += 1;
                if self.check_group < self.probes.groups.len() { self.check_key = self.probes.groups[self.check_group].start; }
            } else {
                self.check_key += 1;
                if self.check_key == self.probes.groups[self.check_group].end {
                    self.checking = None; return Ok(PostingStep::Pending);
                }
            }
        }
        self.checking = None; self.stats.candidates_emitted += 1;
        Ok(PostingStep::Candidate { ordinal: file, decision: IndexDecision::Verify })
    }
    fn consume(&mut self, driver: usize) {
        self.drivers[driver].start += 1;
        self.heads[driver] = None;
        self.stats.posting_entries_visited += 1;
    }
}
