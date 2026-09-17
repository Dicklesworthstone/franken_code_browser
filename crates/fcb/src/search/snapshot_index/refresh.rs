#![forbid(unsafe_code)]

//! Bounded replacement generations over a NEW snapshot's exact membership.
//! Reuse is by source digest + original length + this module's fixed semantics,
//! never ordinal/path/mtime. It conveys no rename or file-identity claim.
//! Unchanged/duplicated/moved content needs no source load or trigram rebuild.
//! The old index remains untouched; publication and retirement belong to hosts.

use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RefreshStats {
    /// Target members that copied a complete old segment, including duplicates.
    pub reused_files: usize,
    pub reused_source_bytes: u64,
    pub reused_grams: usize,
    /// Target members whose source was loaded to attempt fresh indexing.
    pub attempted_files: usize,
    pub loaded_source_bytes: u64,
    pub rebuilt_files: usize,
    /// Reusable segments refused by the NEW per-file or total gram quota.
    pub reuse_quota_refusals: usize,
}

/// A fully assembled candidate, not an on-disk publication receipt. The index
/// owns all copied/rebuilt keys; it does not borrow the prior generation.
pub struct SnapshotRefresh { index: SnapshotIndex, stats: RefreshStats }
impl SnapshotRefresh {
    pub fn index(&self) -> &SnapshotIndex { &self.index }
    pub const fn stats(&self) -> RefreshStats { self.stats }
    pub fn into_index(self) -> SnapshotIndex { self.index }
}

impl SnapshotIndex {
    /// Reuse complete segments from this trusted in-memory index in a new saved
    /// scope. A disk prior must first pass decode_pinned with its original pin.
    /// Target metadata must come from full validation or a separately pinned
    /// catalog. Neither input is widened or modified. Missing target members
    /// stay missing even when a same-path old capture was indexed.
    ///
    /// Limits apply to the NEW generation. Source-read/scratch budgets apply to
    /// newly built segments; reuse still consumes retained-gram/per-file limits.
    /// Members are admitted in target raw-path order. A segment is copied whole
    /// or left uncovered: a partial gram set cannot certify a negative.
    /// allocations = [new index, source load, capture copy, engine, reuse lookup].
    pub fn refresh<R: Read + Seek>(&self, archive: &mut PagedSnapshot<R>, limits: IndexLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 5],
        canceled: impl FnMut() -> bool) -> Result<SnapshotRefresh, SnapshotIndexError> {
        if allocations[..4].contains(&allocations[4]) { return Err(SnapshotIndexError::Limits); }
        construct(archive, limits, budget,
            [allocations[0], allocations[1], allocations[2], allocations[3]],
            Some((self, allocations[4])), canceled)
    }
}

pub(super) fn construct<R: Read + Seek>(archive: &mut PagedSnapshot<R>, limits: IndexLimits,
    budget: &ResourceBudget, allocations: [ResourceAllocationId; 4],
    prior: Option<(&SnapshotIndex, ResourceAllocationId)>, mut canceled: impl FnMut() -> bool)
    -> Result<SnapshotRefresh, SnapshotIndexError> {
    if allocations.iter().enumerate().any(|(i, id)| allocations[..i].contains(id)) {
        return Err(SnapshotIndexError::Limits);
    }
    let owner = archive.directory().owner();
    if prior.is_some_and(|(old, _)| old.owner != owner) { return Err(SnapshotIndexError::OwnerMismatch); }
    let count = archive.directory().len();
    if count > MAX_INDEX_MEMBERS || limits.max_total_grams > MAX_INDEX_GRAMS
        || limits.max_source_bytes_per_file > 1024 * 1024 || limits.max_scratch_bytes > 4 * 1024 * 1024 {
        return Err(SnapshotIndexError::Limits);
    }
    if canceled() { return Err(SnapshotIndexError::Canceled); }
    // Declare the lease before its vector: every exit destroys the vector first,
    // including cancellation while populating/sorting the lookup.
    let _lookup_lease = if let Some((old, allocation)) = prior {
        let bytes = old.rows.len().checked_mul(size_of::<usize>())
            .and_then(|n| n.checked_add(size_of::<Vec<usize>>())).ok_or(SnapshotIndexError::Limits)?;
        Some(budget.try_reserve_managed(owner, allocation, ByteLength::new(bytes as u64))
            .map_err(|_| SnapshotIndexError::ResourceDenied)?)
    } else { None };
    // Fixed-width sort keys only. No copies of paths, source or gram payloads.
    // Construction sorting is bounded worker work, not an interaction callback.
    let mut lookup = reserve(prior.map_or(0, |(old, _)| old.rows.len()))?;
    if let Some((old, _)) = prior {
        for (ordinal, row) in old.rows.iter().enumerate() {
            if canceled() { return Err(SnapshotIndexError::Canceled); }
            if row.coverage.indexed() { lookup.push(ordinal); }
        }
        lookup.sort_unstable_by(|&a, &b| {
            let (a_row, b_row) = (&old.rows[a], &old.rows[b]);
            a_row.digest.as_bytes().cmp(b_row.digest.as_bytes())
                .then_with(|| a_row.length.cmp(&b_row.length)).then_with(|| a.cmp(&b))
        });
    }
    let mut capacity = 0usize;
    for member in archive.directory().members() {
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        let n = usize::try_from(member.observed_bytes).unwrap_or(usize::MAX);
        if matches!(member.data, PagedMemberData::Captured { .. }) && n <= limits.max_source_bytes_per_file {
            capacity = capacity.saturating_add(n.saturating_sub(2).min(limits.max_grams_per_file)).min(limits.max_total_grams);
        }
    }
    let lease = reserve_charge(owner, count, capacity, budget, allocations[0])?;
    let mut rows = reserve(count)?;
    let mut grams = reserve(capacity)?;
    let mut stats = SavedIndexStats { members: count, ..SavedIndexStats::default() };
    let mut refreshed = RefreshStats::default();
    for ordinal in 0..count {
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        let member = archive.directory().member(ordinal).ok_or(SnapshotIndexError::SourceMismatch)?;
        let (length, digest) = match member.data {
            PagedMemberData::Unavailable(_) => {
                rows.push(Row { coverage: Coverage::Unavailable, length: member.observed_bytes,
                    digest: Sha256Digest::new([0; 32]), start: grams.len(), count: 0 });
                stats.unavailable_files += 1; continue;
            }
            PagedMemberData::Captured { byte_length, digest, .. } => (byte_length, digest),
        };
        let mut row = Row { coverage: Coverage::Uncovered, length: length as u64,
            digest, start: grams.len(), count: 0 };
        let reusable = prior.and_then(|(old, _)| {
            let compare = |&i: &usize| old.rows[i].digest.as_bytes().cmp(digest.as_bytes())
                .then_with(|| old.rows[i].length.cmp(&(length as u64)));
            let position = lookup.partition_point(|i| compare(i).is_lt());
            lookup.get(position).filter(|i| compare(i).is_eq()).map(|&i| (&old.rows[i], old))
        });
        if let Some((previous, old)) = reusable {
            if length <= limits.max_source_bytes_per_file && previous.count <= limits.max_grams_per_file
                && previous.count <= capacity - grams.len() {
                for chunk in old.grams[previous.start..previous.start + previous.count].chunks(4096) {
                    if canceled() { return Err(SnapshotIndexError::Canceled); }
                    grams.extend_from_slice(chunk);
                }
                row.coverage = previous.coverage;
                row.count = previous.count;
                refreshed.reused_files += 1;
                refreshed.reused_source_bytes += length as u64;
                refreshed.reused_grams += row.count;
            } else {
                // The known complete segment cannot fit. Rereading identical
                // bytes cannot create a smaller complete set under these quotas.
                refreshed.reuse_quota_refusals += 1;
            }
        } else if length <= limits.max_source_bytes_per_file
            && length.saturating_sub(2) <= limits.max_scratch_bytes / 4
            && length as u64 <= limits.max_source_bytes_total.saturating_sub(refreshed.loaded_source_bytes)
            && (length < 3 || grams.len() < capacity) {
            let verified = archive.load(ordinal, budget, allocations[1], &mut canceled)?;
            refreshed.attempted_files += 1;
            refreshed.loaded_source_bytes += length as u64;
            let copy_charge = length.checked_add(size_of::<CompleteCapture>() + 64).ok_or(SnapshotIndexError::Limits)?;
            let _copy = budget.try_reserve_managed(owner, allocations[2], ByteLength::new(copy_charge as u64))
                .map_err(|_| SnapshotIndexError::ResourceDenied)?;
            let file = FileId::new(owner, 1).map_err(|_| SnapshotIndexError::Limits)?;
            let revision = SourceRevision::new(owner, 1).map_err(|_| SnapshotIndexError::Limits)?;
            let capture = CompleteCapture::new(CaptureRequest::new(file, revision).map_err(|_| SnapshotIndexError::Format)?,
                ByteLength::new(length as u64), Arc::from(verified.bytes())).map_err(|_| SnapshotIndexError::Format)?;
            let documents = [SearchDocument::new(file, "saved-member", &capture)];
            let manifest = SearchManifest::new(SearchManifestId::new(owner, 1)?, &documents, &[],
                MembershipState::Closed, ManifestLimits::default())?;
            let local_limits = IndexLimits { max_total_grams: capacity - grams.len(),
                max_source_bytes_total: length as u64, ..limits };
            let index = EphemeralIndex::build(manifest, local_limits, budget, allocations[3], &mut canceled)?;
            stats.build_source_bytes += index.statistics().source_bytes_examined;
            let image = index.segment_image(0).ok_or(SnapshotIndexError::Format)?;
            if image.coverage() == SegmentCoverage::Indexed {
                row.coverage = if image.source_is_utf8() { Coverage::Utf8 } else { Coverage::Bytes };
                row.count = image.grams().len();
                grams.extend_from_slice(image.grams());
                refreshed.rebuilt_files += 1;
            }
        }
        if row.coverage.indexed() { stats.indexed_files += 1; } else { stats.uncovered_files += 1; }
        rows.push(row);
    }
    if canceled() { return Err(SnapshotIndexError::Canceled); }
    stats.unique_grams = grams.len();
    let index = SnapshotIndex { owner, archive: archive.directory().digest(), rows, grams, stats, _lease: lease };
    Ok(SnapshotRefresh { index, stats: refreshed })
}
