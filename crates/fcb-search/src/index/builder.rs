#![forbid(unsafe_code)]

//! Incremental ownership around the existing single-file index builder. Each
//! append uses EphemeralIndex::build, then appends its completed segment into an
//! admitted aggregate. There is no second gram algorithm, source read, or hidden
//! final whole-universe rebuild. Finalization moves already prepared storage.

use super::*;
use crate::index::{IndexLimits, ManifestLimits, allocation_bytes};

/// A private candidate under construction. Members must arrive in increasing
/// FileId order, including unavailable members. A failed/panicking append makes
/// this candidate unpublishable; it never mutates an independently accepted index.
/// No source operation or parser runs in new()/finish(). Appending one capture
/// can sort its bounded grams synchronously, so use a host worker, not redraw.
pub struct OwnedIndexBuilder {
    index: OwnedEphemeralIndex,
    retention: SourceRetentionLimits,
    limits: IndexLimits,
    last_file: Option<FileId>,
    path_bytes: usize,
    failed: bool,
}
impl OwnedIndexBuilder {
    /// Admit retained source, metadata, and final segment capacity before any
    /// capture is supplied. Per-file engine scratch has a separate lease during
    /// append. Caller-owned source allocations keep their own admission until
    /// handed off; this reservation covers this builder's independent pins.
    pub fn new(id: SearchManifestId, retention: SourceRetentionLimits, limits: IndexLimits,
        budget: &ResourceBudget, source_allocation: ResourceAllocationId,
        index_allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<Self, IndexError> {
        check(&mut canceled)?;
        let metadata = retention.max_files.checked_mul(size_of::<OwnedDocument>() + 32 + size_of::<FileId>())
            .and_then(|n| n.checked_add(retention.max_path_bytes))
            .and_then(|n| n.checked_add(size_of::<Self>() + 64)).ok_or(IndexError::LimitExceeded)?;
        let charge = retention.max_source_bytes.checked_add(metadata as u64).ok_or(IndexError::LimitExceeded)?;
        let source_lease = budget.try_reserve_managed(id.owner(), source_allocation, ByteLength::new(charge))
            .map_err(|_| IndexError::ResourceDenied)?;
        let byte_bound = usize::try_from(retention.max_source_bytes.min(limits.max_source_bytes_total))
            .unwrap_or(usize::MAX);
        let gram_capacity = limits.max_total_grams.min(byte_bound)
            .min(retention.max_files.saturating_mul(limits.max_grams_per_file));
        let index_bytes = allocation_bytes(retention.max_files, gram_capacity, 0)?;
        let index_lease = budget.try_reserve_managed(id.owner(), index_allocation, ByteLength::new(index_bytes))
            .map_err(|_| IndexError::ResourceDenied)?;
        let index = OwnedEphemeralIndex { id, membership: MembershipState::Discovering,
            documents: reserve(retention.max_files)?, unavailable: reserve(retention.max_files)?,
            segments: reserve(retention.max_files)?, grams: reserve(gram_capacity)?,
            statistics: IndexStatistics { retained_bytes: index_bytes, peak_reserved_bytes: index_bytes,
                ..Default::default() }, source_bytes: 0, query_identity: Arc::new(()),
            _source_lease: source_lease, _index_lease: index_lease };
        check(&mut canceled)?;
        Ok(Self { index, retention, limits, last_file: None, path_bytes: 0, failed: false })
    }
    pub fn captured_files(&self) -> usize { self.index.documents.len() }
    pub fn unavailable_files(&self) -> usize { self.index.unavailable.len() }
    pub fn source_bytes(&self) -> u64 { self.index.source_bytes }
    pub fn statistics(&self) -> IndexStatistics { self.index.statistics }
    pub fn is_failed(&self) -> bool { self.failed }

    /// Retain one whole immutable observation and construct only its segment.
    /// Quota-uncovered members remain captured for exact query fallback. Source
    /// bytes are Arc-shared, not recopied or rehashed. The temporary gram segment
    /// is copied once into the reserved aggregate; existing segments never move.
    pub fn push_capture(&mut self, capture: &CompleteCapture, path: &str,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<(), IndexError> {
        self.start_append(capture.request().file())?;
        check(&mut canceled)?;
        if capture.request().range().is_some() { return Err(IndexError::InvalidManifest); }
        let source_bytes = self.index.source_bytes.checked_add(capture.bytes().len() as u64)
            .ok_or(IndexError::LimitExceeded)?;
        let path_bytes = self.path_bytes.checked_add(path.len()).ok_or(IndexError::LimitExceeded)?;
        if source_bytes > self.retention.max_source_bytes || path_bytes > self.retention.max_path_bytes {
            return Err(IndexError::LimitExceeded);
        }
        let mut key = String::new();
        key.try_reserve_exact(path.len()).map_err(|_| IndexError::AllocationFailed)?;
        if key.capacity() > path.len() { return Err(IndexError::ResourceDenied); }
        key.push_str(path);
        let documents = [SearchDocument::new(capture.request().file(), path, capture)];
        let manifest = SearchManifest::new(self.index.id, &documents, &[], MembershipState::Closed,
            ManifestLimits { max_files: 1, max_path_bytes: path.len() })?;
        let limits = IndexLimits {
            max_source_bytes_total: self.limits.max_source_bytes_total
                .saturating_sub(self.index.statistics.source_bytes_examined),
            max_total_grams: self.index.grams.capacity().saturating_sub(self.index.grams.len()),
            ..self.limits
        };
        let child = EphemeralIndex::build(manifest, limits, budget, allocation, &mut canceled)?;
        check(&mut canceled)?;
        let mut segment = *child.segments.first().ok_or(IndexError::InvalidManifest)?;
        let examined = self.index.statistics.source_bytes_examined.checked_add(child.statistics.source_bytes_examined)
            .ok_or(IndexError::LimitExceeded)?;
        let peak = self.index.statistics.retained_bytes.checked_add(child.statistics.peak_reserved_bytes)
            .ok_or(IndexError::LimitExceeded)?;
        if child.segments.len() != 1 || child.grams.len() > self.index.grams.capacity() - self.index.grams.len() {
            return Err(IndexError::ResourceDenied);
        }
        segment.start = self.index.grams.len();
        self.index.grams.extend_from_slice(&child.grams);
        self.index.segments.push(segment);
        self.index.documents.push(OwnedDocument { file: capture.request().file(), path: key, capture: capture.clone() });
        self.index.statistics.indexed_files += child.statistics.indexed_files;
        self.index.statistics.uncovered_files += child.statistics.uncovered_files;
        self.index.statistics.source_bytes_examined = examined;
        self.index.statistics.unique_grams = self.index.grams.len();
        self.index.statistics.peak_reserved_bytes = self.index.statistics.peak_reserved_bytes.max(peak);
        self.index.source_bytes = source_bytes;
        self.path_bytes = path_bytes;
        check(&mut canceled)?;
        self.failed = false;
        Ok(())
    }

    /// Record unavailable membership, never a successful empty file or an empty
    /// negative segment. Availability and index coverage remain independent.
    pub fn push_unavailable(&mut self, file: FileId, mut canceled: impl FnMut() -> bool)
        -> Result<(), IndexError> {
        self.start_append(file)?;
        check(&mut canceled)?;
        self.index.unavailable.push(file);
        check(&mut canceled)?;
        self.failed = false;
        Ok(())
    }

    /// Declare whether the host finished eligible membership. Finalization does
    /// not infer closure from a quota or from the last known successful capture.
    /// No source scan, descriptor array, gram rebuild, or buffer shrinking here.
    pub fn finish(mut self, membership: MembershipState, mut canceled: impl FnMut() -> bool)
        -> Result<OwnedEphemeralIndex, IndexError> {
        if self.failed { return Err(IndexError::InvalidManifest); }
        check(&mut canceled)?;
        let metadata = self.index.documents.capacity().checked_mul(size_of::<OwnedDocument>() + 32)
            .and_then(|n| self.index.unavailable.capacity().checked_mul(size_of::<FileId>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(self.path_bytes))
            .and_then(|n| n.checked_add(size_of::<Self>() + 64)).ok_or(IndexError::LimitExceeded)?;
        let charge = self.index.source_bytes.checked_add(metadata as u64).ok_or(IndexError::LimitExceeded)?;
        self.index._source_lease.reconcile(ByteLength::new(charge)).map_err(|_| IndexError::ResourceDenied)?;
        self.index.membership = membership;
        check(&mut canceled)?;
        Ok(self.index)
    }
    fn start_append(&mut self, file: FileId) -> Result<(), IndexError> {
        if self.failed { return Err(IndexError::InvalidManifest); }
        // Set before all fallible/user callbacks. Unwinding cannot leave a
        // half-appended candidate apparently ready to continue or publish.
        self.failed = true;
        if file.owner() != self.index.id.owner() { return Err(IndexError::OwnerMismatch); }
        if self.last_file.is_some_and(|last| last >= file) { return Err(IndexError::InvalidManifest); }
        if self.index.documents.len() + self.index.unavailable.len() >= self.retention.max_files {
            return Err(IndexError::LimitExceeded);
        }
        self.last_file = Some(file);
        Ok(())
    }
}
