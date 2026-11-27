#![forbid(unsafe_code)]

//! Spatial counts for a frozen search result. Every visible parcel, including
//! a partial group of siblings, represents one exact interval of hierarchy leaves.

use super::{AtlasError, AtlasIndex, AtlasNodeId, MAX_ATLAS_NODES, reserved_vec};
use crate::{NodeKind, VisibleParcel};
use fcb_core::{ByteLength, QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease};
use std::mem::size_of;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MatchCounts {
    pub occurrences: u64,
    pub files: usize,
}

/// Only counts supplied by the search engine are represented. The caller
/// retains its search coverage (retained-only, partial, or exhaustive).
pub struct AtlasMatchDensity {
    root: AtlasNodeId,
    query: QueryGeneration,
    // One entry per contributing file after coalescing. The second field
    // is the inclusive cumulative occurrence count in spatial order.
    totals: Vec<(usize, u64)>,
    _lease: ResourceLease,
}

impl AtlasMatchDensity {
    /// Accept (file, count) pairs in any order. A file may contribute several
    /// times. Pre-aggregated per-file counts need no allocation per occurrence.
    pub fn build(index: &AtlasIndex<'_>, query: QueryGeneration,
        contributions: impl ExactSizeIterator<Item = (AtlasNodeId, u64)>,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, AtlasError> {
        if query.owner() != index.owner() { return Err(AtlasError::OwnerMismatch); }
        let count = contributions.len();
        if count > MAX_ATLAS_NODES { return Err(AtlasError::InvalidLimits); }
        if canceled() { return Err(AtlasError::Canceled); }
        let bytes = size_of::<Self>() + count * size_of::<(usize, u64)>();
        let lease = budget.try_reserve_managed(index.owner(), allocation, ByteLength::new(bytes as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut totals = reserved_vec(count)?;
        for (position, (file, occurrences)) in contributions.enumerate() {
            if canceled() { return Err(AtlasError::Canceled); }
            if position >= count { return Err(AtlasError::InvalidLimits); }
            let ordinal = index.resolve(file)?;
            if index.layout.nodes()[ordinal].kind() != NodeKind::File { return Err(AtlasError::NotFile); }
            if occurrences > 0 { totals.push((index.nodes[ordinal].first_leaf, occurrences)); }
        }
        totals.sort_unstable_by_key(|&(rank, _)| rank);
        let mut used = 0;
        let mut through = 0u64;
        for i in 0..totals.len() {
            if canceled() { return Err(AtlasError::Canceled); }
            let (rank, count) = totals[i];
            through = through.checked_add(count).ok_or(AtlasError::InvalidLimits)?;
            if used == 0 || totals[used - 1].0 != rank { used += 1; }
            totals[used - 1] = (rank, through);
        }
        totals.truncate(used);
        if canceled() { return Err(AtlasError::Canceled); }
        Ok(Self { root: index.root_node(), query, totals, _lease: lease })
    }

    /// Two binary searches independent of hidden subtree size. Camera changes
    /// do not invalidate a search. Layout and search-generation changes do.
    pub fn counts(&self, parcel: VisibleParcel, query: QueryGeneration) -> Result<MatchCounts, AtlasError> {
        if query != self.query { return Err(AtlasError::StaleQuery); }
        if parcel.node().layout() != self.root.layout() || parcel.node().root() != self.root.root() {
            return Err(AtlasError::StaleLayout);
        }
        let start = parcel.first_leaf;
        let end = start + parcel.represented_leaves();
        let lo = self.totals.partition_point(|&(rank, _)| rank < start);
        let hi = self.totals.partition_point(|&(rank, _)| rank < end);
        let through = |n: usize| if n == 0 { 0 } else { self.totals[n - 1].1 };
        Ok(MatchCounts { occurrences: through(hi) - through(lo), files: hi - lo })
    }
}

impl VisibleParcel {
    /// Membership in this particular displayed region, even when several
    /// sibling groups report the same parent node as their navigation target.
    pub fn contains_file(self, index: &AtlasIndex<'_>, file: AtlasNodeId) -> Result<bool, AtlasError> {
        index.resolve(self.node())?;
        let ordinal = index.resolve(file)?;
        if index.layout.nodes()[ordinal].kind() != NodeKind::File { return Err(AtlasError::NotFile); }
        let rank = index.nodes[ordinal].first_leaf;
        Ok((self.first_leaf..self.first_leaf + self.represented_leaves()).contains(&rank))
    }
}
