#![forbid(unsafe_code)]

use std::cmp::Ordering;
use fcb_core::{ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, RootId};
use super::{IndexedPath, MembershipState, PathIndex, PathSearchError, SearchManifestId,
    MAX_PATH_QUERY_BYTES, PATH_KEY_VERSION, add, key_lengths, keys, mul, path_order, reserve};

pub const MAX_PATH_RESULTS: usize = 4096;
pub const MAX_PATH_STEP_CANDIDATES: usize = 4096;
pub const MAX_PATH_STEP_UNITS: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathCase { Sensitive, UnicodeLowercase }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathMatchMode { Exact, Prefix, Fuzzy }

/// Lower ranks sort first. Preference/recency never override lexical class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PathMatchKind {
    ExactFilename, ExactPath, ExactComponent, FilenamePrefix, ComponentPrefix,
    PathPrefix, FilenameSubsequence, PathSubsequence,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PathRank {
    pub kind: PathMatchKind,
    pub scope_penalty: u8,
    pub case_penalty: u8,
    pub gaps: usize,
    pub boundary_penalty: u8,
    pub recent_penalty: u16,
    pub start: usize,
    pub path_units: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct PathMatch<'index> {
    path: &'index IndexedPath,
    rank: PathRank,
}
impl<'index> PathMatch<'index> {
    pub const fn path(self) -> &'index IndexedPath { self.path }
    pub const fn file_id(self) -> FileId { self.path.file_id() }
    pub const fn root_id(self) -> RootId { self.path.root_id() }
    pub const fn rank(self) -> PathRank { self.rank }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathSearchOptions {
    pub generation: QueryGeneration,
    pub case: PathCase,
    pub mode: PathMatchMode,
    pub max_results: usize,
    /// Restricts membership to one already-authorized root.
    pub scope_root: Option<RootId>,
    /// A ranking preference, not an expansion of authorized membership.
    pub preferred_root: Option<RootId>,
    /// Stable selection may remain pinned outside the current top-k buffer.
    pub selected_file: Option<FileId>,
}
impl PathSearchOptions {
    pub fn new(generation: QueryGeneration) -> Self {
        Self { generation, case: PathCase::UnicodeLowercase, mode: PathMatchMode::Fuzzy,
            max_results: 100, scope_root: None, preferred_root: None, selected_file: None }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathStepBudget { pub max_candidates: usize, pub max_units: usize }
impl Default for PathStepBudget {
    fn default() -> Self { Self { max_candidates: 256, max_units: MAX_PATH_STEP_UNITS } }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathSearchState { Running, Finished, Canceled }
#[derive(Clone, Copy, Debug)]
pub enum PathSelection<'index> {
    None,
    Pending(FileId),
    Matched(PathMatch<'index>),
    NotMatched(FileId),
    /// No longer present in a closed index. Never silently select another row.
    Missing(FileId),
}

/// Synchronous-resumable path navigation. Exact/prefix queries traverse a
/// sorted component range; fuzzy queries visit the immutable file-key universe.
/// Top-k storage is bounded independently from the count of all matches seen.
/// All-match bitsets, rather than truncated top-k rows, make refinement sound.
/// Key processing and result shifts are admitted before visiting each file.
/// This is worker work, not a claim about fixed-duration interaction callbacks.
pub struct PathSearch<'index> {
    index: &'index PathIndex,
    options: PathSearchOptions,
    sensitive: Vec<u32>,
    folded: Vec<u32>,
    next: usize,
    end: usize,
    component_route: bool,
    eligible: Vec<u64>,
    seen: Vec<u64>,
    matched: Vec<u64>,
    ranked: Vec<PathMatch<'index>>,
    frozen: Vec<PathMatch<'index>>,
    protected: bool,
    selected: Option<FileId>,
    selected_hit: Option<PathMatch<'index>>,
    state: PathSearchState,
    matches_seen: usize,
    files_examined: usize,
    candidates_examined: usize,
    work_units: u64,
    last_step_units: usize,
    reused_candidates: bool,
    _lease: ResourceLease,
}
impl<'index> PathSearch<'index> {
    /// `needle` is native bytes or UTF-8 text. Invalid bytes never become U+FFFD.
    /// Empty queries do not enumerate a repository: return PATH_QUERY_EMPTY.
    pub fn new(
        index: &'index PathIndex, needle: &[u8], options: PathSearchOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
    ) -> Result<Self, PathSearchError> {
        if needle.is_empty() { return Err(PathSearchError::EmptyQuery); }
        if needle.len() > MAX_PATH_QUERY_BYTES { return Err(PathSearchError::QueryTooLong); }
        if options.max_results > MAX_PATH_RESULTS { return Err(PathSearchError::InvalidLimits); }
        let owner = index.id.owner();
        if options.generation.owner() != owner
            || options.scope_root.is_some_and(|root| root.owner() != owner)
            || options.preferred_root.is_some_and(|root| root.owner() != owner)
            || options.selected_file.is_some_and(|file| file.owner() != owner) {
            return Err(PathSearchError::OwnerMismatch);
        }
        let count = index.records.len();
        let words = count.div_ceil(64);
        let hit_capacity = options.max_results.min(count);
        let (sensitive_len, folded_len) = key_lengths(needle)?;
        let bit_bytes = mul(mul(words, 3)?, std::mem::size_of::<u64>())?;
        let hit_bytes = mul(mul(hit_capacity, 2)?, std::mem::size_of::<PathMatch<'index>>())?;
        let key_bytes = mul(add(sensitive_len, folded_len)?, std::mem::size_of::<u32>())?;
        let bytes = add(std::mem::size_of::<Self>(), add(bit_bytes, add(hit_bytes, key_bytes)?)?)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(bytes as u64))
            .map_err(|_| PathSearchError::ResourceDenied)?;
        let (sensitive, folded) = keys(needle)?;
        let mut eligible = Vec::new();
        let mut seen = Vec::new();
        let mut matched = Vec::new();
        let mut ranked = Vec::new();
        let mut frozen = Vec::new();
        reserve(&mut eligible, words)?;
        reserve(&mut seen, words)?;
        reserve(&mut matched, words)?;
        reserve(&mut ranked, hit_capacity)?;
        reserve(&mut frozen, hit_capacity)?;
        eligible.resize(words, u64::MAX);
        seen.resize(words, 0);
        matched.resize(words, 0);
        let component_route = options.mode != PathMatchMode::Fuzzy;
        let range = if component_route {
            index.component_range(&folded, options.mode == PathMatchMode::Prefix)
        } else { 0..count };
        Ok(Self { index, options, sensitive, folded, next: range.start, end: range.end,
            component_route, eligible, seen, matched, ranked, frozen, protected: false,
            selected: options.selected_file, selected_hit: None, state: PathSearchState::Running,
            matches_seen: 0, files_examined: 0, candidates_examined: 0, work_units: 0,
            last_step_units: 0, reused_candidates: false, _lease: lease })
    }

    /// Refine without losing matches outside a prior top-k buffer. Prefix and
    /// subsequence eligibility narrow only for a normalized-key extension with
    /// unchanged mode/case/scope and a FINISHED scan of this exact index. Exact
    /// mode, backspace, scope changes, and unfinished/canceled scans restart.
    /// A new query always has independent output capacity and generation.
    pub fn refine(
        &self, needle: &[u8], mut options: PathSearchOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
    ) -> Result<Self, PathSearchError> {
        if options.generation == self.options.generation { return Err(PathSearchError::StaleQuery); }
        if options.selected_file.is_none() { options.selected_file = self.selected; }
        let mut next = Self::new(self.index, needle, options, budget, allocation)?;
        if self.state == PathSearchState::Finished
            && options.mode == self.options.mode && options.mode != PathMatchMode::Exact
            && options.case == self.options.case && options.scope_root == self.options.scope_root
            && next.needle().starts_with(self.needle()) {
            next.eligible.copy_from_slice(&self.matched);
            next.reused_candidates = true;
        }
        Ok(next)
    }

    pub const fn index_id(&self) -> SearchManifestId { self.index.id }
    pub const fn generation(&self) -> QueryGeneration { self.options.generation }
    pub const fn key_version(&self) -> u32 { PATH_KEY_VERSION }
    pub const fn options(&self) -> PathSearchOptions { self.options }
    pub const fn state(&self) -> PathSearchState { self.state }
    pub const fn matches_seen(&self) -> usize { self.matches_seen }
    pub const fn files_examined(&self) -> usize { self.files_examined }
    pub const fn candidates_examined(&self) -> usize { self.candidates_examined }
    pub const fn work_units(&self) -> u64 { self.work_units }
    pub const fn last_step_units(&self) -> usize { self.last_step_units }
    pub const fn reused_candidates(&self) -> bool { self.reused_candidates }
    pub fn ranked_matches(&self) -> &[PathMatch<'index>] { &self.ranked }
    pub fn visible_matches(&self) -> &[PathMatch<'index>] {
        if self.protected { &self.frozen } else { &self.ranked }
    }
    pub fn truncated(&self) -> bool { self.matches_seen > self.ranked.len() }
    pub fn is_complete(&self) -> bool {
        self.state == PathSearchState::Finished && self.index.membership == MembershipState::Closed
    }
    pub fn validate_delivery(&self, index: SearchManifestId, generation: QueryGeneration) -> Result<(), PathSearchError> {
        if index != self.index.id { return Err(PathSearchError::StaleIndex); }
        if generation != self.options.generation { return Err(PathSearchError::StaleQuery); }
        Ok(())
    }
    pub fn selection(&self) -> PathSelection<'index> {
        let Some(file) = self.selected else { return PathSelection::None; };
        if let Some(hit) = self.selected_hit { return PathSelection::Matched(hit); }
        let Ok(ordinal) = self.index.records.binary_search_by_key(&file, |record| record.file) else {
            return if self.index.membership == MembershipState::Closed { PathSelection::Missing(file) }
                else { PathSelection::Pending(file) };
        };
        if has(&self.seen, ordinal) || !has(&self.eligible, ordinal) || self.state == PathSearchState::Finished {
            PathSelection::NotMatched(file)
        } else { PathSelection::Pending(file) }
    }

    /// Freeze the currently visible ordering while worker results continue to
    /// improve separately in ranked_matches(). Does not allocate or move focus.
    pub fn protect_ordering(&mut self) {
        if !self.protected {
            self.frozen.clear();
            self.frozen.extend_from_slice(&self.ranked);
            self.protected = true;
        }
    }
    pub fn release_ordering(&mut self) { self.protected = false; self.frozen.clear(); }
    pub fn select(&mut self, file: FileId) -> Result<(), PathSearchError> {
        let hit = self.visible_matches().iter().find(|hit| hit.file_id() == file).copied()
            .ok_or(PathSearchError::SelectionUnavailable)?;
        self.selected = Some(file);
        self.selected_hit = Some(hit);
        self.protect_ordering();
        Ok(())
    }
    pub fn clear_selection(&mut self) { self.selected = None; self.selected_hit = None; }
    pub fn cancel(&mut self) {
        if self.state == PathSearchState::Running { self.state = PathSearchState::Canceled; }
    }

    /// Minimum unit allowance for the next candidate; never consumes work.
    pub fn next_work_units(&self) -> usize {
        if self.next == self.end { return 0; }
        let ordinal = self.ordinal();
        if has(&self.seen, ordinal) || !has(&self.eligible, ordinal)
            || self.options.scope_root.is_some_and(|root| root != self.index.records[ordinal].root) {
            return 1;
        }
        let record = &self.index.records[ordinal];
        // Covers linear key passes plus bounded shifts in the top-k buffer.
        8 * (record.sensitive.len() + record.folded.len() + self.sensitive.len() + self.folded.len() + 1)
            + self.options.max_results
    }

    pub fn step(
        &mut self, budget: PathStepBudget, active_generation: QueryGeneration,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<(), PathSearchError> {
        self.last_step_units = 0;
        if active_generation != self.options.generation { self.cancel(); return Err(PathSearchError::StaleQuery); }
        if canceled() { self.cancel(); }
        if self.state != PathSearchState::Running || budget.max_candidates == 0 || budget.max_units == 0 { return Ok(()); }
        let limit = budget.max_candidates.min(MAX_PATH_STEP_CANDIDATES);
        let allowance = budget.max_units.min(MAX_PATH_STEP_UNITS);
        let mut candidates = 0;
        while self.next < self.end && candidates < limit {
            if canceled() { self.cancel(); break; }
            let cost = self.next_work_units();
            if cost > allowance - self.last_step_units {
                if candidates == 0 { return Err(PathSearchError::StepBudgetTooSmall); }
                break;
            }
            let ordinal = self.ordinal();
            self.next += 1;
            candidates += 1;
            self.candidates_examined += 1;
            self.last_step_units += cost;
            self.work_units += cost as u64;
            if has(&self.seen, ordinal) { continue; }
            set(&mut self.seen, ordinal);
            if !has(&self.eligible, ordinal) { continue; }
            let record = self.index.records[ordinal].as_ref();
            if self.options.scope_root.is_some_and(|root| root != record.root) { continue; }
            self.files_examined += 1;
            if let Some(rank) = rank(record, &self.sensitive, &self.folded, self.options) {
                let hit = PathMatch { path: record, rank };
                set(&mut self.matched, ordinal);
                self.matches_seen += 1;
                if self.selected == Some(record.file) { self.selected_hit = Some(hit); }
                self.insert(hit);
            }
        }
        if canceled() { self.cancel(); }
        if self.state == PathSearchState::Running && self.next == self.end { self.state = PathSearchState::Finished; }
        Ok(())
    }
    pub fn run_to_completion(&mut self, mut canceled: impl FnMut() -> bool) -> Result<(), PathSearchError> {
        while self.state == PathSearchState::Running {
            self.step(PathStepBudget::default(), self.options.generation, &mut canceled)?;
        }
        Ok(())
    }
    fn needle(&self) -> &[u32] {
        match self.options.case { PathCase::Sensitive => &self.sensitive, PathCase::UnicodeLowercase => &self.folded }
    }
    fn ordinal(&self) -> usize {
        if self.component_route { self.index.components[self.next].record } else { self.next }
    }
    fn insert(&mut self, hit: PathMatch<'index>) {
        let limit = self.options.max_results.min(self.index.records.len());
        if limit == 0 { return; }
        let position = self.ranked.binary_search_by(|existing| compare(existing, &hit)).unwrap_or_else(|position| position);
        if position >= limit { return; }
        if self.ranked.len() == limit { self.ranked.pop(); }
        self.ranked.insert(position, hit);
    }
}

fn has(bits: &[u64], ordinal: usize) -> bool { bits[ordinal / 64] & (1u64 << (ordinal % 64)) != 0 }
fn set(bits: &mut [u64], ordinal: usize) { bits[ordinal / 64] |= 1u64 << (ordinal % 64); }
fn compare(left: &PathMatch<'_>, right: &PathMatch<'_>) -> Ordering {
    left.rank.cmp(&right.rank).then_with(|| path_order(left.path, right.path))
}

fn rank(record: &IndexedPath, sensitive: &[u32], folded: &[u32], options: PathSearchOptions) -> Option<PathRank> {
    let (key, needle) = match options.case {
        PathCase::Sensitive => (&record.sensitive[..], sensitive),
        PathCase::UnicodeLowercase => (&record.folded[..], folded),
    };
    let (kind, gaps, start, boundary_penalty) = score(key, needle, options.mode)?;
    let case_penalty = if options.case == PathCase::Sensitive
        || score(&record.sensitive, sensitive, options.mode).is_some_and(|score| score.0 == kind) { 0 } else { 1 };
    Some(PathRank { kind, scope_penalty: u8::from(options.preferred_root.is_some_and(|root| root != record.root)),
        case_penalty, gaps, boundary_penalty, recent_penalty: u16::MAX - record.recent,
        start, path_units: key.len() })
}

fn score(key: &[u32], needle: &[u32], mode: PathMatchMode) -> Option<(PathMatchKind, usize, usize, u8)> {
    let basename = key.iter().rposition(|&unit| unit == u32::from(b'/')).map_or(0, |i| i + 1);
    let name = &key[basename..];
    if name == needle { return Some((PathMatchKind::ExactFilename, 0, 0, 0)); }
    if key == needle { return Some((PathMatchKind::ExactPath, 0, 0, 0)); }
    if key.split(|&unit| unit == u32::from(b'/')).any(|part| part == needle) {
        return Some((PathMatchKind::ExactComponent, 0, 0, 0));
    }
    if mode == PathMatchMode::Exact { return None; }
    if name.starts_with(needle) { return Some((PathMatchKind::FilenamePrefix, 0, 0, 0)); }
    if key.split(|&unit| unit == u32::from(b'/')).any(|part| part.starts_with(needle)) {
        return Some((PathMatchKind::ComponentPrefix, 0, 0, 0));
    }
    if key.starts_with(needle) { return Some((PathMatchKind::PathPrefix, 0, 0, 0)); }
    if mode == PathMatchMode::Prefix { return None; }
    if let Some((gaps, start, boundary)) = subsequence(name, needle) {
        return Some((PathMatchKind::FilenameSubsequence, gaps, start, boundary));
    }
    subsequence(key, needle).map(|(gaps, start, boundary)| (PathMatchKind::PathSubsequence, gaps, start, boundary))
}

/// Deterministic greedy subsequence score, not an optimal-alignment claim.
/// Eligibility is exact for subsequence semantics; matching never combines
/// bytes from unrelated UTF-8 scalars or aliases invalid native bytes.
fn subsequence(key: &[u32], needle: &[u32]) -> Option<(usize, usize, u8)> {
    let mut next = 0;
    let mut start = 0;
    for (position, &unit) in key.iter().enumerate() {
        if unit == needle[next] {
            if next == 0 { start = position; }
            next += 1;
            if next == needle.len() {
                let boundary = start == 0 || matches!(key[start - 1], 45 | 46 | 47 | 95);
                return Some((position - start + 1 - needle.len(), start, u8::from(!boundary)));
            }
        }
    }
    None
}
