#![forbid(unsafe_code)]

//! Owning transfer of prepared segments AND their complete captured universe.
//! Repeated queries borrow those same segments through a scoped view. The view
//! allocates only bounded document descriptors, never copies source or postings,
//! hashes source, reparses paths, or rebuilds an index. No self-reference or leak.

use std::mem::{size_of, take};
use fcb_core::{ByteLength, FileId, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::CompleteCapture;
use super::super::{EphemeralIndex, IndexError, IndexStatistics, MembershipState,
    SearchManifest, SearchManifestId, Segment};
use crate::SearchDocument;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRetentionLimits {
    pub max_files: usize,
    pub max_source_bytes: u64,
    pub max_path_bytes: usize,
}
impl Default for SourceRetentionLimits {
    fn default() -> Self {
        Self { max_files: 1_000_000, max_source_bytes: 256 * 1024 * 1024,
            max_path_bytes: 128 * 1024 * 1024 }
    }
}
struct OwnedDocument { file: FileId, path: String, capture: CompleteCapture }

/// Whole source captures remain pinned even for files rejected by the gram
/// quota or absent from the last query's top-k. Source retention has its own
/// admission, independent of the original caller's capture and index leases.
/// Methods are explicit worker operations. Final destruction is worker work.
pub struct OwnedEphemeralIndex {
    id: SearchManifestId,
    membership: MembershipState,
    documents: Vec<OwnedDocument>,
    unavailable: Vec<FileId>,
    segments: Vec<Segment>,
    grams: Vec<u32>,
    statistics: IndexStatistics,
    source_bytes: u64,
    _source_lease: ResourceLease,
    _index_lease: ResourceLease,
}
impl EphemeralIndex<'_> {
    /// Consume only a privately prepared candidate. On failure that candidate
    /// is discarded, not an independently accepted older index. Arc cloning
    /// shares the immutable source allocation; the new lease keeps it charged
    /// after the caller's original source owner goes away.
    pub fn into_owned(self, limits: SourceRetentionLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<OwnedEphemeralIndex, IndexError> {
        check(&mut canceled)?;
        let manifest = self.manifest;
        let files = manifest.documents.len().checked_add(manifest.unavailable.len())
            .ok_or(IndexError::LimitExceeded)?;
        if files > limits.max_files { return Err(IndexError::LimitExceeded); }
        let mut source_bytes = 0u64;
        let mut path_bytes = 0usize;
        for doc in manifest.documents {
            check(&mut canceled)?;
            if doc.capture.request().range().is_some() { return Err(IndexError::InvalidManifest); }
            source_bytes = source_bytes.checked_add(doc.capture.bytes().len() as u64)
                .ok_or(IndexError::LimitExceeded)?;
            path_bytes = path_bytes.checked_add(doc.path.len()).ok_or(IndexError::LimitExceeded)?;
            if source_bytes > limits.max_source_bytes || path_bytes > limits.max_path_bytes {
                return Err(IndexError::LimitExceeded);
            }
        }
        let metadata = manifest.documents.len().checked_mul(size_of::<OwnedDocument>() + 32)
            .and_then(|n| manifest.unavailable.len().checked_mul(size_of::<FileId>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(path_bytes))
            .and_then(|n| n.checked_add(size_of::<OwnedEphemeralIndex>()))
            .ok_or(IndexError::LimitExceeded)?;
        let charge = source_bytes.checked_add(metadata as u64).ok_or(IndexError::LimitExceeded)?;
        let source_lease = budget.try_reserve_managed(manifest.id.owner(), allocation, ByteLength::new(charge))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut documents = reserve(manifest.documents.len())?;
        let mut unavailable = reserve(manifest.unavailable.len())?;
        for doc in manifest.documents {
            check(&mut canceled)?;
            let mut path = String::new();
            path.try_reserve_exact(doc.path.len()).map_err(|_| IndexError::AllocationFailed)?;
            if path.capacity() > doc.path.len() { return Err(IndexError::ResourceDenied); }
            path.push_str(doc.path);
            documents.push(OwnedDocument { file: doc.file_id, path, capture: doc.capture.clone() });
        }
        for &file in manifest.unavailable { check(&mut canceled)?; unavailable.push(file); }
        check(&mut canceled)?;
        Ok(OwnedEphemeralIndex { id: manifest.id, membership: manifest.membership,
            documents, unavailable, segments: self.segments, grams: self.grams,
            statistics: self.statistics, source_bytes,
            _source_lease: source_lease, _index_lease: self._lease })
    }
}
impl OwnedEphemeralIndex {
    pub const fn id(&self) -> SearchManifestId { self.id }
    pub const fn membership(&self) -> MembershipState { self.membership }
    pub fn captured_files(&self) -> usize { self.documents.len() }
    pub fn unavailable_files(&self) -> &[FileId] { &self.unavailable }
    pub const fn source_bytes(&self) -> u64 { self.source_bytes }
    pub const fn statistics(&self) -> IndexStatistics { self.statistics }
    pub fn capture(&self, file: FileId) -> Option<&CompleteCapture> {
        self.documents.binary_search_by_key(&file, |doc| doc.file)
            .ok().map(|i| &self.documents[i].capture)
    }

    /// Use the existing indexed-query, segment-export and exact-verification
    /// APIs. The callback's result cannot borrow the temporary descriptor view.
    /// Prepare owned result/activation records inside it, then publish only on
    /// success. Descriptor admission precedes allocation. Cancellation and panic
    /// unwinding restore the original segment buffers without allocation.
    pub fn with_index<R>(&mut self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool, work: impl FnOnce(&EphemeralIndex<'_>) -> R)
        -> Result<R, IndexError> {
        check(&mut canceled)?;
        let bytes = self.documents.len().checked_mul(size_of::<SearchDocument<'_>>())
            .and_then(|n| n.checked_add(size_of::<Restore<'_, '_>>() + size_of::<Vec<SearchDocument<'_>>>()))
            .ok_or(IndexError::LimitExceeded)?;
        let _scratch = budget.try_reserve_managed(self.id.owner(), allocation, ByteLength::new(bytes as u64))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut documents = reserve(self.documents.len())?;
        for doc in &self.documents {
            check(&mut canceled)?;
            documents.push(SearchDocument::new(doc.file, &doc.path, &doc.capture));
        }
        // Exact copies of a previously validated manifest, not caller-supplied
        // metadata. No source identity or coverage state changes during transfer.
        let manifest = SearchManifest { id: self.id, documents: &documents,
            unavailable: &self.unavailable, membership: self.membership };
        let view = EphemeralIndex { manifest, segments: take(&mut self.segments),
            grams: take(&mut self.grams), statistics: self.statistics, _lease: self._index_lease.clone() };
        let restore = Restore { view, segments: &mut self.segments, grams: &mut self.grams };
        check(&mut canceled)?;
        let result = work(&restore.view);
        drop(restore);
        check(&mut canceled)?;
        Ok(result)
    }
}
struct Restore<'slot, 'source> {
    view: EphemeralIndex<'source>,
    segments: &'slot mut Vec<Segment>,
    grams: &'slot mut Vec<u32>,
}
impl Drop for Restore<'_, '_> {
    fn drop(&mut self) {
        *self.segments = take(&mut self.view.segments);
        *self.grams = take(&mut self.view.grams);
    }
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), IndexError> {
    if canceled() { Err(IndexError::Canceled) } else { Ok(()) }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, IndexError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| IndexError::AllocationFailed)?;
    if values.capacity() > count { return Err(IndexError::ResourceDenied); }
    Ok(values)
}
