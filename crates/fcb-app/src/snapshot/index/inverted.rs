#![forbid(unsafe_code)]

//! Composition for the two saved-index layouts. Both use the same separately
//! pinned trust boundary, archive/member verification and exact source scanner.
//! Dispatch by magic is not a validation shortcut: the selected decoder checks
//! the pin, complete structure and binding before exposing any search decision.

use super::*;
use fcb::search::snapshot_index::{SnapshotPostings, IndexArtifact};
use fcb::search::paged_snapshot::PagedSnapshot;

pub(super) enum QueryIndex { Segments(SnapshotIndex), Postings(SnapshotPostings) }
impl QueryIndex {
    pub(super) fn decode(bytes: &[u8], pin: Sha256Digest, directory: &SnapshotDirectory,
        budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<Self, Failure> {
        if SnapshotPostings::is_encoded(bytes) {
            Ok(Self::Postings(SnapshotPostings::decode_pinned(bytes, pin, directory, budget, allocation(152), canceled)?))
        } else {
            Ok(Self::Segments(SnapshotIndex::decode_pinned(bytes, pin, directory, budget, allocation(152), canceled)?))
        }
    }
    pub(super) fn stats(&self) -> SavedIndexStats {
        match self { Self::Segments(index) => index.stats(), Self::Postings(index) => index.stats() }
    }
    pub(super) fn inverted(&self) -> bool { matches!(self, Self::Postings(_)) }
    pub(super) fn query<'archive, 'query, R: Read + Seek>(&'query self,
        archive: &'archive mut PagedSnapshot<R>, needle: &'query IndexedNeedle<'_>,
        options: PagedQueryOptions, count_all_matches: bool, budget: &ResourceBudget)
        -> Result<PagedQuery<'archive, 'query, R>, Failure> {
        let allocations = [allocation(158), allocation(159), allocation(160)];
        Ok(match (self, count_all_matches) {
            (Self::Segments(index), false) => PagedQuery::new_indexed(archive, needle, index, options, budget, allocations)?,
            (Self::Segments(index), true) => PagedQuery::new_indexed_counting(archive, needle, index, options, budget, allocations)?,
            (Self::Postings(index), false) => PagedQuery::new_postings(archive, needle, index, options, budget, allocations)?,
            (Self::Postings(index), true) => PagedQuery::new_postings_counting(archive, needle, index, options, budget, allocations)?,
        })
    }
}

pub(super) fn encode(index: &SnapshotIndex, inverted: bool, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<IndexArtifact, Failure> {
    if inverted {
        let postings = index.invert(budget, allocation(162), &mut *canceled)?;
        Ok(postings.encode(budget, allocation(156), canceled)?)
    } else { Ok(index.encode(budget, allocation(156), canceled)?) }
}

pub(super) fn fields(out: &mut Output, inverted: bool) -> Result<(), Failure> {
    search_fields(out, inverted, false)
}
pub(super) fn search_fields(out: &mut Output, inverted: bool, ignore_case: bool) -> Result<(), Failure> {
    out.literal(",\"index_layout\":")?;
    out.quoted(if inverted { "global-postings-v1" } else { "per-member-grams-v1" })?;
    out.literal(",\"candidate_strategy\":")?;
    out.quoted(if ignore_case { "direct-verified-member-scan" }
        else if inverted { "rarest-posting-intersection" } else { "per-member-probes" })?;
    Ok(())
}
