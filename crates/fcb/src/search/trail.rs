#![forbid(unsafe_code)]

//! Exact user-selected reading trails over saved sources. No candidate generator,
//! Markdown parser, native UI, database or live-root resolver is implemented here.
//! A note remains user rationale, not a compiler/relationship evidence claim.

use std::{io::{Read, Seek}, mem::size_of};
use fcb_core::{ArenaOwnerId, ByteLength, ByteRange, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use super::paged_snapshot::{PagedCapture, PagedReader, PagedSearchError, PagedSnapshot, PagedSnapshotError, PagedMemberData, SnapshotDirectory};
use super::{ReaderError, ReaderLimits};
pub use fcb_store::{Sha256Digest, trail::{TrailBytes, TrailEntry, TrailError, TrailView, MAX_TRAIL_BYTES, MAX_TRAIL_ITEMS}};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailReadError {
    Format(TrailError), Archive(PagedSnapshotError), Capture(PagedSearchError),
    OtherArchive, MissingMember, UnavailableMember, ChangedSource, InvalidRange,
    StaleDelivery, OwnerMismatch, GenerationExhausted,
}
impl std::fmt::Display for TrailReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(e) => write!(f, "{e}"), Self::Archive(e) => write!(f, "{e}"), Self::Capture(e) => write!(f, "{e}"),
            Self::OtherArchive => f.write_str("TRAIL_OTHER_ARCHIVE"), Self::MissingMember => f.write_str("TRAIL_MEMBER_MISSING"),
            Self::UnavailableMember => f.write_str("TRAIL_MEMBER_UNAVAILABLE"), Self::ChangedSource => f.write_str("TRAIL_SOURCE_CHANGED"),
            Self::InvalidRange => f.write_str("TRAIL_INVALID_RANGE"), Self::StaleDelivery => f.write_str("TRAIL_STALE_DELIVERY"),
            Self::OwnerMismatch => f.write_str("TRAIL_OWNER_MISMATCH"), Self::GenerationExhausted => f.write_str("TRAIL_GENERATION_EXHAUSTED"),
        }
    }
}
impl std::error::Error for TrailReadError {}
impl From<TrailError> for TrailReadError { fn from(e: TrailError) -> Self { Self::Format(e) } }
impl From<PagedSnapshotError> for TrailReadError { fn from(e: PagedSnapshotError) -> Self { Self::Archive(e) } }
impl From<PagedSearchError> for TrailReadError { fn from(e: PagedSearchError) -> Self { Self::Capture(e) } }

/// Pin only after loading and verifying the exact member. Never read a source
/// pathname or manufacture an anchor for an unavailable member. The returned
/// reference borrows the archive directory and caller's note; it owns no bytes.
/// A later open always verifies the member again, not merely these old digests.
pub fn pin_selection<'a, R: Read + Seek>(archive: &'a mut PagedSnapshot<R>, native_path: &[u8],
    range: ByteRange, rationale: &'a str, budget: &ResourceBudget, allocation: ResourceAllocationId,
    mut canceled: impl FnMut() -> bool) -> Result<TrailEntry<'a>, TrailReadError> {
    if rationale.len() > fcb_store::trail::MAX_RATIONALE_BYTES { return Err(TrailError::Text.into()); }
    let ordinal = archive.directory().find_path(native_path).ok_or(TrailReadError::MissingMember)?;
    let member = archive.directory().member(ordinal).ok_or(TrailReadError::MissingMember)?;
    let (length, source) = match member.data {
        PagedMemberData::Captured { byte_length, digest, .. } => (byte_length as u64, digest),
        PagedMemberData::Unavailable(_) => return Err(TrailReadError::UnavailableMember),
    };
    if range.end().get() > length { return Err(TrailReadError::InvalidRange); }
    let verified = archive.load(ordinal, budget, allocation, &mut canceled)?;
    if verified.source_digest() != source || verified.bytes().len() as u64 != length { return Err(TrailReadError::ChangedSource); }
    if canceled() { return Err(PagedSnapshotError::Canceled.into()); }
    let member = archive.directory().member(ordinal).ok_or(TrailReadError::MissingMember)?;
    Ok(TrailEntry { archive: archive.directory().digest(), source, source_length: length,
        path: member.path, range, rationale })
}

/// A new immutable generation, preserving ALL old visits and rationale. No
/// in-place mutation, cache cleanup, hidden deduplication or user-note eviction.
/// Repeated selections remain distinct sequence positions. Exceeding a bound
/// refuses the append instead of silently dropping the oldest user selections.
pub fn append_selection(owner: ArenaOwnerId, previous: Option<TrailView<'_>>, title: &str,
    selection: TrailEntry<'_>, budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
    mut canceled: impl FnMut() -> bool) -> Result<TrailBytes, TrailError> {
    let count = previous.map_or(0, |view| view.len()).checked_add(1).ok_or(TrailError::Limit)?;
    if count > MAX_TRAIL_ITEMS || allocations[0] == allocations[1] { return Err(TrailError::Limit); }
    let charge = count.checked_mul(size_of::<TrailEntry<'_>>()).and_then(|n| n.checked_add(size_of::<Vec<TrailEntry<'_>>>()))
        .ok_or(TrailError::Limit)?;
    let _scratch = budget.try_reserve_managed(owner, allocations[0], ByteLength::new(charge as u64))
        .map_err(|_| TrailError::ResourceDenied)?;
    let mut entries = Vec::new(); entries.try_reserve_exact(count).map_err(|_| TrailError::ResourceDenied)?;
    if entries.capacity() > count { return Err(TrailError::ResourceDenied); }
    if let Some(previous) = previous {
        for entry in previous.entries() {
            if canceled() { return Err(TrailError::Canceled); }
            entries.push(entry?);
        }
    }
    entries.push(selection);
    TrailBytes::encode(owner, previous.map_or(title, |view| view.title()), previous.map(|view| view.digest()),
        &entries, budget, allocations[1], canceled)
}

/// Check reference readiness using already validated archive metadata. This
/// does not claim that a later payload read succeeded; open() does that check.
pub fn resolve_selection(entry: TrailEntry<'_>, directory: &SnapshotDirectory) -> Result<usize, TrailReadError> {
    if entry.archive != directory.digest() { return Err(TrailReadError::OtherArchive); }
    let ordinal = directory.find_path(entry.path).ok_or(TrailReadError::MissingMember)?;
    let member = directory.member(ordinal).ok_or(TrailReadError::MissingMember)?;
    match member.data {
        PagedMemberData::Unavailable(_) => Err(TrailReadError::UnavailableMember),
        PagedMemberData::Captured { byte_length, digest, .. } => {
            if digest != entry.source || byte_length as u64 != entry.source_length { return Err(TrailReadError::ChangedSource); }
            if entry.range.end().get() > byte_length as u64 { return Err(TrailReadError::InvalidRange); }
            Ok(ordinal)
        }
    }
}

pub struct TrailNavigator<'a> { view: TrailView<'a>, current: Option<usize>, generation: QueryGeneration }
impl<'a> TrailNavigator<'a> {
    pub fn new(view: TrailView<'a>, generation: QueryGeneration) -> Self {
        Self { view, current: (!view.is_empty()).then_some(0), generation }
    }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn current_ordinal(&self) -> Option<usize> { self.current }
    pub fn select(&mut self, ordinal: usize) -> Result<(), TrailReadError> {
        if ordinal >= self.view.len() { return Err(TrailError::MissingItem.into()); }
        if self.current == Some(ordinal) { return Ok(()); }
        let value = self.generation.get().checked_add(1).ok_or(TrailReadError::GenerationExhausted)?;
        let next = QueryGeneration::new(self.generation.owner(), value).map_err(|_| TrailReadError::GenerationExhausted)?;
        self.current = Some(ordinal); self.generation = next;
        Ok(())
    }
    pub fn next(&mut self) -> Result<bool, TrailReadError> {
        let Some(ordinal) = self.current.filter(|&i| i + 1 < self.view.len()) else { return Ok(false); };
        self.select(ordinal + 1)?; Ok(true)
    }
    pub fn previous(&mut self) -> Result<bool, TrailReadError> {
        let Some(ordinal) = self.current.filter(|&i| i > 0) else { return Ok(false); };
        self.select(ordinal - 1)?; Ok(true)
    }
    pub fn target(&self) -> Result<Option<TrailTarget<'a>>, TrailReadError> {
        self.current.map(|ordinal| Ok(TrailTarget { entry: self.view.entry(ordinal)?, ordinal,
            trail: self.view.digest(), generation: self.generation })).transpose()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TrailTarget<'a> { entry: TrailEntry<'a>, ordinal: usize, trail: Sha256Digest, generation: QueryGeneration }
impl<'a> TrailTarget<'a> {
    pub const fn entry(self) -> TrailEntry<'a> { self.entry }
    pub const fn ordinal(self) -> usize { self.ordinal }
    pub const fn trail_digest(self) -> Sha256Digest { self.trail }
    pub const fn generation(self) -> QueryGeneration { self.generation }
    pub fn validate_delivery(self, trail: Sha256Digest, generation: QueryGeneration) -> Result<(), TrailReadError> {
        if self.trail != trail || self.generation != generation { return Err(TrailReadError::StaleDelivery); }
        Ok(())
    }
    /// The caller supplies a currently authorized archive handle and fresh local
    /// identities. Paths never grant access and a changed archive never receives
    /// an old note merely because it contains an equal pathname or nearby text.
    pub fn open<R: Read + Seek>(self, archive: &mut PagedSnapshot<R>, active_trail: Sha256Digest,
        generation: QueryGeneration, file: FileId, revision: SourceRevision, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2], mut canceled: impl FnMut() -> bool) -> Result<TrailCapture<'a>, TrailReadError> {
        self.validate_delivery(active_trail, generation)?;
        if file.owner() != self.generation.owner() || revision.owner() != file.owner()
            || archive.directory().owner() != file.owner() { return Err(TrailReadError::OwnerMismatch); }
        let ordinal = resolve_selection(self.entry, archive.directory())?;
        let capture = PagedCapture::load(archive, ordinal, file, revision, budget, allocations, &mut canceled)?;
        if capture.source_digest() != self.entry.source || capture.bytes().len() as u64 != self.entry.source_length {
            return Err(TrailReadError::ChangedSource);
        }
        let (start, end) = self.entry.range.as_usize_bounds().map_err(|_| TrailReadError::InvalidRange)?;
        capture.bytes().get(start..end).ok_or(TrailReadError::InvalidRange)?;
        if canceled() { return Err(PagedSnapshotError::Canceled.into()); }
        Ok(TrailCapture { target: self, capture })
    }
}

/// Retains the exact saved source independently of the archive handle, while
/// borrowing the trail's reference/note bytes. No uncharged SourceCapture escapes.
pub struct TrailCapture<'a> { target: TrailTarget<'a>, capture: PagedCapture }
impl<'a> TrailCapture<'a> {
    pub const fn target(&self) -> TrailTarget<'a> { self.target }
    pub fn selected_bytes(&self) -> &[u8] {
        let (start, end) = self.target.entry.range.as_usize_bounds().expect("validated selection");
        &self.capture.bytes()[start..end]
    }
    pub fn reader(&self, limits: ReaderLimits, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<PagedReader<'_>, ReaderError> { self.capture.reader(limits, budget, allocation) }
}
