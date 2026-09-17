#![forbid(unsafe_code)]

//! Exact captured-text results projected onto an unchanged workspace atlas.
//! The caller explicitly captures sources first. Preparation and queries are
//! worker operations; neither performs I/O, reparses paths, or repacks geometry.
//! Source descriptors, index, and overlays borrow separately, avoiding a
//! self-referential owner and retaining the exact searched bytes for activation.

use std::{cmp::Ordering, mem::size_of};
use fcb_core::{ByteLength, ByteRange, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, ResourceLease, SourceRevision};
use crate::search::{CompleteCapture, EphemeralIndex, IndexError, IndexLimits,
    IndexedQueryState, IndexedSearchReport, ParsedQuery, QueryError, QueryOptions,
    SearchMatch};
use crate::search::workspace::{WorkspaceCaptures, WorkspaceError, WorkspaceSearchInputs};
use super::{AtlasNodeId, NodeKind, WorkspaceAtlas, WorkspaceAtlasError};

pub const MAX_ATLAS_TEXT_MATCHES: usize = 4096;
pub const MAX_ATLAS_TEXT_NEEDLE: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WorkspaceTextError {
    Atlas(WorkspaceAtlasError), Workspace(WorkspaceError), Index(IndexError),
    InvalidLimits, WrongSource, MissingHit,
}
impl std::fmt::Display for WorkspaceTextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(e) => write!(f, "{e}"), Self::Workspace(e) => write!(f, "{e}"),
            Self::Index(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("ATLAS_TEXT_INVALID_LIMITS"),
            Self::WrongSource => f.write_str("ATLAS_TEXT_WRONG_SOURCE"),
            Self::MissingHit => f.write_str("ATLAS_TEXT_MISSING_HIT"),
        }
    }
}
impl std::error::Error for WorkspaceTextError {}
impl From<WorkspaceAtlasError> for WorkspaceTextError {
    fn from(e: WorkspaceAtlasError) -> Self { Self::Atlas(e) }
}
impl From<WorkspaceError> for WorkspaceTextError {
    fn from(e: WorkspaceError) -> Self { Self::Workspace(e) }
}
impl From<IndexError> for WorkspaceTextError {
    fn from(e: IndexError) -> Self { Self::Index(e) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasTextLimits {
    pub max_matches: usize,
    /// Global exact-verification allowance, separate from capture/index I/O.
    pub max_bytes_scanned: u64,
}
impl Default for AtlasTextLimits {
    fn default() -> Self { Self { max_matches: 100, max_bytes_scanned: 32 * 1024 * 1024 } }
}

/// One frozen atlas and its completed capture attempts. Failed attempts remain
/// unavailable members; they do not become empty, searchable source files.
pub struct WorkspaceTextSource<'source, 'catalog> {
    atlas: &'source WorkspaceAtlas<'catalog>,
    captures: &'source WorkspaceCaptures<'catalog>,
    inputs: WorkspaceSearchInputs<'source>,
}
impl<'source, 'catalog> WorkspaceTextSource<'source, 'catalog> {
    pub fn new(atlas: &'source WorkspaceAtlas<'catalog>, captures: &'source WorkspaceCaptures<'catalog>,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, WorkspaceTextError> {
        atlas.validate_active()?;
        // Even equal IDs cannot substitute another independently frozen catalog.
        if !std::ptr::eq(atlas.catalog(), captures.catalog()) { return Err(WorkspaceTextError::WrongSource); }
        let inputs = captures.search_inputs(budget, allocation)?;
        if !captures.finished() { return Err(WorkspaceError::Pending.into()); }
        Ok(Self { atlas, captures, inputs })
    }
    pub fn atlas(&self) -> &'source WorkspaceAtlas<'catalog> { self.atlas }
    pub fn captures(&self) -> &'source WorkspaceCaptures<'catalog> { self.captures }
    pub fn validate_active(&self) -> Result<(), WorkspaceTextError> {
        Ok(self.atlas.validate_active()?)
    }
    /// Retain this prepared index for repeated queries. Quota-refused segments
    /// use the existing direct scanner; a UTF-8 prefilter cannot exclude UTF-16.
    pub fn index(&self, limits: IndexLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<WorkspaceTextIndex<'_, 'source, 'catalog>, WorkspaceTextError> {
        self.validate_active()?;
        let mut stop = || canceled() || self.validate_active().is_err();
        let index = self.inputs.index(limits, budget, allocation, &mut stop)?;
        self.validate_active()?;
        if canceled() { return Err(IndexError::Canceled.into()); }
        Ok(WorkspaceTextIndex { source: self, index })
    }
}

pub struct WorkspaceTextIndex<'index, 'source, 'catalog> {
    source: &'index WorkspaceTextSource<'source, 'catalog>,
    index: EphemeralIndex<'index>,
}
impl<'index, 'source, 'catalog> WorkspaceTextIndex<'index, 'source, 'catalog> {
    pub fn index(&self) -> &EphemeralIndex<'index> { &self.index }
    /// Literal decoded text only: punctuation, '-' and field-looking strings
    /// remain literal data. The shared exact search engine owns decoding and
    /// matching, including overlapping occurrences and original-byte maps.
    /// Allocations: [literal scratch, result report, atlas overlay].
    pub fn search(&self, needle: &str, generation: QueryGeneration, limits: AtlasTextLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 3],
        mut canceled: impl FnMut() -> bool)
        -> Result<AtlasTextOverlay<'index, 'source, 'catalog>, WorkspaceTextError> {
        self.source.validate_active()?;
        if needle.is_empty() { return Err(IndexError::Query(QueryError::EmptyNeedle).into()); }
        if needle.len() > MAX_ATLAS_TEXT_NEEDLE { return Err(IndexError::Query(QueryError::NeedleTooLong).into()); }
        if limits.max_matches > MAX_ATLAS_TEXT_MATCHES
            || allocations[0] == allocations[1] || allocations[0] == allocations[2]
            || allocations[1] == allocations[2] { return Err(WorkspaceTextError::InvalidLimits); }
        if canceled() { return Err(IndexError::Canceled.into()); }
        let literal_bytes = 2 * needle.len() + size_of::<ParsedQuery>();
        let _literal_lease = budget.try_reserve_managed(self.source.atlas.layout().owner(), allocations[0],
            ByteLength::new(literal_bytes as u64)).map_err(|_| IndexError::ResourceDenied)?;
        let query = ParsedQuery { primary_needle: copy_text(needle)?, is_phrase: true,
            conjunction_terms: Vec::new(), exclusion_terms: Vec::new(), path_filters: Vec::new(),
            lang_filters: Vec::new(), raw_query: copy_text(needle)? };
        let mut options = QueryOptions::new(generation).with_max_matches(limits.max_matches);
        options.max_bytes_scanned = Some(limits.max_bytes_scanned);
        let mut stop = || canceled() || self.source.validate_active().is_err();
        let report = self.index.search(&query, options, budget, allocations[1], &mut stop)?;
        self.source.validate_active()?;
        if canceled() || report.state() == IndexedQueryState::Canceled { return Err(IndexError::Canceled.into()); }
        if let Some(error) = report.failure() { return Err(error.into()); }
        if report.state() != IndexedQueryState::Finished { return Err(IndexError::StaleQuery.into()); }
        let count = report.capture_results().matches.len();
        let charge = count.checked_mul(size_of::<AtlasTextHit>() + 2 * size_of::<usize>())
            .and_then(|n| n.checked_add(size_of::<AtlasTextOverlay<'_, '_, '_>>()))
            .ok_or(WorkspaceTextError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(generation.owner(), allocations[2], ByteLength::new(charge as u64))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut hits = reserve(count)?;
        for hit in &report.capture_results().matches {
            if canceled() { return Err(IndexError::Canceled.into()); }
            let capture = self.source.captures.capture(hit.file_id).ok_or(WorkspaceTextError::WrongSource)?;
            exact_bytes(capture, hit)?;
            hits.push(AtlasTextHit { node: self.source.atlas.node_for_file(hit.file_id)?,
                file: hit.file_id, revision: hit.revision, original_range: hit.original_byte_range });
        }
        let mut by_path = reserve(count)?;
        let mut unique_files = reserve(count)?;
        by_path.extend(0..count);
        by_path.sort_unstable_by(|&a, &b| {
            self.source.atlas.layout().nodes()[hits[a].node.ordinal() as usize].path()
                .cmp(self.source.atlas.layout().nodes()[hits[b].node.ordinal() as usize].path())
                .then(a.cmp(&b))
        });
        let mut previous = None;
        for &i in &by_path {
            if previous != Some(hits[i].file) { unique_files.push(i); previous = Some(hits[i].file); }
        }
        self.source.validate_active()?;
        if canceled() { return Err(IndexError::Canceled.into()); }
        Ok(AtlasTextOverlay { source: self.source, report, hits, by_path, unique_files, _lease: lease })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasTextHit {
    node: AtlasNodeId, file: FileId, revision: SourceRevision, original_range: ByteRange,
}
impl AtlasTextHit {
    pub const fn node(self) -> AtlasNodeId { self.node }
    pub const fn file(self) -> FileId { self.file }
    pub const fn revision(self) -> SourceRevision { self.revision }
    pub const fn original_range(self) -> ByteRange { self.original_range }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AtlasTextCounts { pub occurrences: usize, pub files: usize }

/// Owns report/output leases but borrows exact captured source. Moving the
/// camera or dropping the search index never changes these occurrences.
pub struct AtlasTextOverlay<'index, 'source, 'catalog> {
    source: &'index WorkspaceTextSource<'source, 'catalog>,
    report: IndexedSearchReport<'index>,
    hits: Vec<AtlasTextHit>,
    by_path: Vec<usize>,
    unique_files: Vec<usize>,
    _lease: ResourceLease,
}
impl<'index, 'source, 'catalog> AtlasTextOverlay<'index, 'source, 'catalog> {
    pub fn source(&self) -> &'index WorkspaceTextSource<'source, 'catalog> { self.source }
    pub fn hits(&self) -> &[AtlasTextHit] { &self.hits }
    pub fn report(&self) -> &IndexedSearchReport<'index> { &self.report }
    pub fn generation(&self) -> QueryGeneration { self.report.generation() }
    pub fn is_complete(&self) -> bool { self.report.is_complete() }
    pub fn validate_delivery(&self, source: &WorkspaceTextSource<'_, '_>, generation: QueryGeneration)
        -> Result<(), WorkspaceTextError> {
        self.source.validate_active()?;
        if !std::ptr::eq(self.source, source) { return Err(WorkspaceTextError::WrongSource); }
        Ok(self.report.validate_delivery(source.atlas.catalog().id(), generation)?)
    }
    /// Occurrences and distinct files among RETAINED hits only. Counts are not
    /// repository totals when discovery, capture, decoding, or query is partial.
    /// Sibling-group boxes must not borrow their parent directory's counts.
    pub fn retained_matches_in(&self, node: AtlasNodeId) -> Result<AtlasTextCounts, WorkspaceTextError> {
        self.validate_delivery(self.source, self.generation())?;
        let layout = self.source.atlas.layout();
        if node.root() != layout.root() || node.layout() != layout.revision() {
            return Err(IndexError::StaleQuery.into());
        }
        let node = layout.nodes().get(node.ordinal() as usize).ok_or(WorkspaceTextError::MissingHit)?;
        if node.path().is_empty() { return Ok(AtlasTextCounts { occurrences: self.hits.len(), files: self.unique_files.len() }); }
        let compare = |&i: &usize| {
            let path = layout.nodes()[self.hits[i].node.ordinal() as usize].path();
            if node.kind() == NodeKind::Directory { subtree_compare(path, node.path()) }
            else { path.cmp(node.path()) }
        };
        let count = |order: &[usize]| {
            let start = order.partition_point(|i| compare(i) == Ordering::Less);
            let end = order.partition_point(|i| compare(i) != Ordering::Greater);
            end - start
        };
        Ok(AtlasTextCounts { occurrences: count(&self.by_path), files: count(&self.unique_files) })
    }
    /// Activate only this report's occurrence under the current source/query.
    /// The selection borrows immutable captured bytes, never a live path read.
    pub fn select_hit(&self, source: &WorkspaceTextSource<'_, '_>, position: usize,
        generation: QueryGeneration) -> Result<AtlasTextSelection<'_>, WorkspaceTextError> {
        self.validate_delivery(source, generation)?;
        let hit = self.hits.get(position).ok_or(WorkspaceTextError::MissingHit)?;
        let matched = self.report.capture_results().matches.get(position).ok_or(WorkspaceTextError::MissingHit)?;
        let capture = self.source.captures.capture(hit.file).ok_or(WorkspaceTextError::WrongSource)?;
        let bytes = exact_bytes(capture, matched)?;
        Ok(AtlasTextSelection { hit, matched, capture, bytes })
    }
}

/// An exact logical selection, not a presented pixel or a filesystem grant.
/// Borrowing retains source and its managed reservation. Hosts validate a new
/// action before delivery; revocation cannot retract bytes already delivered.
pub struct AtlasTextSelection<'a> {
    hit: &'a AtlasTextHit,
    matched: &'a SearchMatch,
    capture: &'a CompleteCapture,
    bytes: &'a [u8],
}
impl<'a> AtlasTextSelection<'a> {
    pub fn hit(&self) -> AtlasTextHit { *self.hit }
    pub fn matched_text(&self) -> &'a str { &self.matched.matched_text }
    pub fn original_bytes(&self) -> &'a [u8] { self.bytes }
    pub fn capture(&self) -> &'a CompleteCapture { self.capture }
}

fn exact_bytes<'a>(capture: &'a CompleteCapture, hit: &SearchMatch) -> Result<&'a [u8], WorkspaceTextError> {
    if capture.request().file() != hit.file_id || capture.request().revision() != hit.revision
        || hit.original_byte_range.is_empty() { return Err(WorkspaceTextError::WrongSource); }
    let (start, end) = hit.original_byte_range.as_usize_bounds().map_err(|_| WorkspaceTextError::WrongSource)?;
    capture.bytes().get(start..end).ok_or(WorkspaceTextError::WrongSource)
}
fn subtree_compare(path: &[u8], dir: &[u8]) -> Ordering {
    let common = path.len().min(dir.len());
    let prefix = path[..common].cmp(&dir[..common]);
    if prefix != Ordering::Equal { return prefix; }
    if path.len() <= dir.len() { return Ordering::Less; }
    path[dir.len()].cmp(&b'/')
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, WorkspaceTextError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| IndexError::AllocationFailed)?;
    if values.capacity() > capacity { return Err(IndexError::ResourceDenied.into()); }
    Ok(values)
}
fn copy_text(text: &str) -> Result<String, WorkspaceTextError> {
    let mut value = String::new();
    value.try_reserve_exact(text.len()).map_err(|_| IndexError::AllocationFailed)?;
    if value.capacity() > text.len() { return Err(IndexError::ResourceDenied.into()); }
    value.push_str(text); Ok(value)
}
