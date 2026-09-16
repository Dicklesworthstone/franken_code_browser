#![forbid(unsafe_code)]

//! Offline comparison of explicitly selected saved observations. Native path
//! equality aligns records, not logical file identities. Missing entries prove
//! saved-scope additions/removals only with matching policies and a closed
//! opposite membership. Unavailable capture is never treated as empty content.
//! No rename inference, live-root lookup, Git history, or automatic reattachment.

use std::{io::{Read, Seek}, mem::size_of, sync::Arc};
use fcb_core::{ByteLength, ByteRange, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::SourceCapture;
use super::{CaptureRequest, CompleteCapture, RawPath, ReaderError, ReaderLimits,
    ReadingAnchor, ReadingSeek, ReadingTarget, ReadingWindow, ReadingWindowOptions, SourceReader};
use super::paged_snapshot::{PagedSnapshot, PagedSnapshotError, PagedMemberData, SnapshotDirectory, Sha256Digest};
use fcb_analysis::CaptureComparison;
pub use fcb_analysis::{ComparisonError, ComparisonLimits, ComparisonQuality, ComparisonRelation,
    ComparisonStats, Correspondence, CorrespondenceKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotCompareError {
    Archive(PagedSnapshotError), Analysis(ComparisonError), OwnerMismatch,
    Stale, InvalidPair, ResourceDenied, Canceled,
}
impl std::fmt::Display for SnapshotCompareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(error) => write!(f, "{error}"), Self::Analysis(error) => write!(f, "{error}"),
            Self::OwnerMismatch => f.write_str("SNAPSHOT_DIFF_OWNER_MISMATCH"),
            Self::Stale => f.write_str("SNAPSHOT_DIFF_STALE"),
            Self::InvalidPair => f.write_str("SNAPSHOT_DIFF_UNAVAILABLE_PAIR"),
            Self::ResourceDenied => f.write_str("SNAPSHOT_DIFF_RESOURCE_DENIED"),
            Self::Canceled => f.write_str("SNAPSHOT_DIFF_CANCELED"),
        }
    }
}
impl std::error::Error for SnapshotCompareError {}
impl From<PagedSnapshotError> for SnapshotCompareError { fn from(e: PagedSnapshotError) -> Self { Self::Archive(e) } }
impl From<ComparisonError> for SnapshotCompareError { fn from(e: ComparisonError) -> Self { Self::Analysis(e) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotChangeKind { Unchanged, Changed, Added, Removed, OnlyBefore, OnlyAfter, Unavailable }
impl SnapshotChangeKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged", Self::Changed => "changed", Self::Added => "added",
            Self::Removed => "removed", Self::OnlyBefore => "only-before", Self::OnlyAfter => "only-after",
            Self::Unavailable => "unavailable",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SnapshotComparisonStats {
    pub compared_paths: usize, pub unchanged: usize, pub changed: usize,
    pub added: usize, pub removed: usize, pub only_before: usize, pub only_after: usize,
    pub unavailable: usize,
}
impl SnapshotComparisonStats {
    pub const fn known_differences(self) -> usize { self.changed + self.added + self.removed }
    pub const fn uncertain_paths(self) -> usize { self.only_before + self.only_after + self.unavailable }
}

/// Fixed-size archive-qualified record: it can be held while loading a member
/// mutably. It contains neither a borrowed directory path nor an owning payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotChange {
    before_archive: Sha256Digest, after_archive: Sha256Digest, generation: QueryGeneration,
    before: Option<usize>, after: Option<usize>, kind: SnapshotChangeKind,
}
impl SnapshotChange {
    pub const fn before_ordinal(self) -> Option<usize> { self.before }
    pub const fn after_ordinal(self) -> Option<usize> { self.after }
    pub const fn kind(self) -> SnapshotChangeKind { self.kind }
    pub const fn generation(self) -> QueryGeneration { self.generation }
    pub fn validate(self, before: &SnapshotDirectory, after: &SnapshotDirectory,
        generation: QueryGeneration) -> Result<(), SnapshotCompareError> {
        if before.owner() != generation.owner() || after.owner() != generation.owner() {
            return Err(SnapshotCompareError::OwnerMismatch);
        }
        if before.digest() != self.before_archive || after.digest() != self.after_archive || generation != self.generation {
            return Err(SnapshotCompareError::Stale);
        }
        Ok(())
    }
    pub fn path<'a>(self, before: &'a SnapshotDirectory, after: &'a SnapshotDirectory) -> Result<&'a [u8], SnapshotCompareError> {
        self.validate(before, after, self.generation)?;
        self.before.and_then(|i| before.member(i)).or_else(|| self.after.and_then(|i| after.member(i)))
            .map(|entry| entry.path).ok_or(SnapshotCompareError::InvalidPair)
    }
}

/// O(n+m) sorted membership merge with fixed-size cursor storage. Each next()
/// visits at most one pair of bounded native paths. No source bytes are loaded;
/// unchanged classification uses digests obtained by full archive validation.
/// Reopening a different archive cannot reuse a cursor's ordinals.
pub struct SnapshotComparison {
    before_archive: Sha256Digest, after_archive: Sha256Digest, generation: QueryGeneration,
    before: usize, after: usize, policies_match: bool, membership_comparable: bool,
    finished: bool, stats: SnapshotComparisonStats,
}
impl SnapshotComparison {
    pub fn new(before: &SnapshotDirectory, after: &SnapshotDirectory, generation: QueryGeneration) -> Result<Self, SnapshotCompareError> {
        if before.owner() != generation.owner() || after.owner() != generation.owner() {
            return Err(SnapshotCompareError::OwnerMismatch);
        }
        let policies_match = before.policy() == after.policy();
        Ok(Self { before_archive: before.digest(), after_archive: after.digest(), generation,
            before: 0, after: 0, policies_match,
            membership_comparable: policies_match && before.discovery_complete() && after.discovery_complete(),
            finished: false, stats: SnapshotComparisonStats::default() })
    }
    pub const fn stats(&self) -> SnapshotComparisonStats { self.stats }
    pub const fn policies_match(&self) -> bool { self.policies_match }
    pub const fn membership_comparable(&self) -> bool { self.membership_comparable }
    pub const fn finished(&self) -> bool { self.finished }
    pub fn next(&mut self, before: &SnapshotDirectory, after: &SnapshotDirectory,
        generation: QueryGeneration, canceled: impl FnOnce() -> bool) -> Result<Option<SnapshotChange>, SnapshotCompareError> {
        let mut change = SnapshotChange { before_archive: self.before_archive, after_archive: self.after_archive,
            generation: self.generation, before: None, after: None, kind: SnapshotChangeKind::Unavailable };
        change.validate(before, after, generation)?;
        if canceled() { return Err(SnapshotCompareError::Canceled); }
        if self.finished { return Ok(None); }
        let (a, b) = (before.member(self.before), after.member(self.after));
        match (a, b) {
            (None, None) => { self.finished = true; return Ok(None); }
            (Some(a), Some(b)) if a.path == b.path => {
                change.before = Some(self.before); change.after = Some(self.after);
                self.before += 1; self.after += 1;
                change.kind = match (a.data, b.data) {
                    (PagedMemberData::Captured { digest: x, byte_length: n, .. }, PagedMemberData::Captured { digest: y, byte_length: m, .. }) =>
                        if x == y && n == m { SnapshotChangeKind::Unchanged } else { SnapshotChangeKind::Changed },
                    _ => SnapshotChangeKind::Unavailable,
                };
            }
            (Some(a), b) if b.is_none_or(|b| a.path < b.path) => {
                change.before = Some(self.before); self.before += 1;
                change.kind = if matches!(a.data, PagedMemberData::Unavailable(_)) { SnapshotChangeKind::Unavailable }
                    else if self.policies_match && after.discovery_complete() { SnapshotChangeKind::Removed }
                    else { SnapshotChangeKind::OnlyBefore };
            }
            (_, Some(b)) => {
                change.after = Some(self.after); self.after += 1;
                change.kind = if matches!(b.data, PagedMemberData::Unavailable(_)) { SnapshotChangeKind::Unavailable }
                    else if self.policies_match && before.discovery_complete() { SnapshotChangeKind::Added }
                    else { SnapshotChangeKind::OnlyAfter };
            }
            _ => return Err(SnapshotCompareError::InvalidPair),
        }
        self.stats.compared_paths += 1;
        match change.kind {
            SnapshotChangeKind::Unchanged => self.stats.unchanged += 1,
            SnapshotChangeKind::Changed => self.stats.changed += 1,
            SnapshotChangeKind::Added => self.stats.added += 1,
            SnapshotChangeKind::Removed => self.stats.removed += 1,
            SnapshotChangeKind::OnlyBefore => self.stats.only_before += 1,
            SnapshotChangeKind::OnlyAfter => self.stats.only_after += 1,
            SnapshotChangeKind::Unavailable => self.stats.unavailable += 1,
        }
        Ok(Some(change))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonSide { Before, After }
struct Side { source: SourceCapture, capture: CompleteCapture }
/// Owns the two source versions and their shared-byte reservations. Reading
/// either side never reopens an archive or live path. There is no cloneable
/// SourceCapture/CompleteCapture getter that could outlive the pair's lease.
pub struct SnapshotPair {
    before: Side, after: Side, change: SnapshotChange, _lease: ResourceLease,
}
impl SnapshotPair {
    pub fn load<A: Read + Seek, B: Read + Seek>(before: &mut PagedSnapshot<A>, after: &mut PagedSnapshot<B>,
        change: SnapshotChange, requests: [CaptureRequest; 2], generation: QueryGeneration,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
        mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotCompareError> {
        change.validate(before.directory(), after.directory(), generation)?;
        if requests.iter().any(|r| r.file().owner() != generation.owner() || r.range().is_some())
            || requests[0] == requests[1] || allocations[0] == allocations[1] {
            return Err(SnapshotCompareError::InvalidPair);
        }
        let old = change.before.and_then(|i| before.directory().member(i)).ok_or(SnapshotCompareError::InvalidPair)?;
        let new = change.after.and_then(|i| after.directory().member(i)).ok_or(SnapshotCompareError::InvalidPair)?;
        let (PagedMemberData::Captured { byte_length: old_len, .. }, PagedMemberData::Captured { byte_length: new_len, .. }) = (old.data, new.data)
            else { return Err(SnapshotCompareError::InvalidPair); };
        if old.path != new.path { return Err(SnapshotCompareError::InvalidPair); }
        let charge = old_len.checked_add(new_len).and_then(|n| (old.path.len() + new.path.len()).checked_mul(16).and_then(|p| n.checked_add(p)))
            .and_then(|n| n.checked_add(size_of::<Self>() + 512)).ok_or(SnapshotCompareError::ResourceDenied)?;
        if canceled() { return Err(SnapshotCompareError::Canceled); }
        let lease = budget.try_reserve_managed(generation.owner(), allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| SnapshotCompareError::ResourceDenied)?;
        let old = load_side(before, change.before.ok_or(SnapshotCompareError::InvalidPair)?, requests[0], budget, allocations[1], &mut canceled)?;
        let new = load_side(after, change.after.ok_or(SnapshotCompareError::InvalidPair)?, requests[1], budget, allocations[1], &mut canceled)?;
        if canceled() { return Err(SnapshotCompareError::Canceled); }
        Ok(Self { before: old, after: new, change, _lease: lease })
    }
    pub const fn change(&self) -> SnapshotChange { self.change }
    fn side(&self, side: ComparisonSide) -> &Side { match side { ComparisonSide::Before => &self.before, ComparisonSide::After => &self.after } }
    pub fn request(&self, side: ComparisonSide) -> CaptureRequest { self.side(side).capture.request() }
    pub fn bytes(&self, side: ComparisonSide) -> &[u8] { self.side(side).source.bytes() }
    pub fn compare(&self, generation: QueryGeneration, limits: ComparisonLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: impl FnMut() -> bool)
        -> Result<SnapshotPairComparison<'_>, SnapshotCompareError> {
        if generation != self.change.generation { return Err(SnapshotCompareError::Stale); }
        Ok(SnapshotPairComparison { inner: CaptureComparison::build(&self.before.capture, &self.after.capture,
            generation, limits, budget, allocation, canceled)? })
    }
    pub fn reader(&self, side: ComparisonSide, limits: ReaderLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<ComparisonReader<'_>, ReaderError> {
        Ok(ComparisonReader { inner: SourceReader::new(&self.side(side).source, limits, budget, allocation)? })
    }
}
fn load_side<R: Read + Seek>(archive: &mut PagedSnapshot<R>, ordinal: usize, request: CaptureRequest,
    budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool) -> Result<Side, SnapshotCompareError> {
    let verified = archive.load(ordinal, budget, allocation, &mut *canceled)?;
    let path = archive.directory().member(ordinal).ok_or(SnapshotCompareError::InvalidPair)?.path;
    let bytes: Arc<[u8]> = Arc::from(verified.bytes());
    let capture = CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::clone(&bytes))
        .map_err(|_| SnapshotCompareError::InvalidPair)?;
    let source = SourceCapture { owner: request.file().owner(), file: request.file(), revision: request.revision(),
        logical_path: RawPath::from_bytes(path).display_escaped().to_string(), bytes };
    Ok(Side { source, capture })
}

/// Restricts engine access to borrowed bytes/ranges so pair-owned leases cannot
/// be bypassed by cloning a source handle out of the analysis engine.
pub struct SnapshotPairComparison<'a> { inner: CaptureComparison<'a> }
impl SnapshotPairComparison<'_> {
    pub fn spans(&self) -> &[Correspondence] { self.inner.spans() }
    pub fn quality(&self) -> ComparisonQuality { self.inner.quality() }
    pub fn relation(&self) -> ComparisonRelation { self.inner.relation() }
    pub fn stats(&self) -> ComparisonStats { self.inner.stats() }
    pub fn before_bytes(&self, ordinal: usize) -> Option<&[u8]> { self.inner.before_bytes(ordinal) }
    pub fn after_bytes(&self, ordinal: usize) -> Option<&[u8]> { self.inner.after_bytes(ordinal) }
    pub fn corresponding_after(&self, range: ByteRange) -> Option<ByteRange> { self.inner.corresponding_after(range) }
    pub fn validate_delivery(&self, before: CaptureRequest, after: CaptureRequest, generation: QueryGeneration) -> Result<(), ComparisonError> {
        self.inner.validate_delivery(before, after, generation)
    }
}
pub struct ComparisonReader<'a> { inner: SourceReader<'a> }
impl<'a> ComparisonReader<'a> {
    pub fn seek(&self, target: ReadingTarget, generation: QueryGeneration) -> Result<ReadingSeek<'a>, ReaderError> { self.inner.seek(target, generation) }
    pub fn window(&self, anchor: ReadingAnchor, generation: QueryGeneration, options: ReadingWindowOptions,
        budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: impl FnMut() -> bool) -> Result<ReadingWindow<'a>, ReaderError> {
        self.inner.window(anchor, generation, options, budget, allocation, canceled)
    }
}
