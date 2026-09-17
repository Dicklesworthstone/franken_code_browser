#![forbid(unsafe_code)]

//! Compact, capture-honest spatial results. Only literal witness bytes survive;
//! there is no rereadable whole-file backing and no decoder-scratch lease per file.

use std::cmp::Ordering;
use fcb_core::{ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use crate::map::{AtlasNodeId, NodeKind};
use crate::search::{CaptureRequest, DetectedEncoding, ExtentConsistency, FileSearchError,
    ObservedExtent, StreamReadState, StreamReadStats};
use super::{AtlasStreamError, AtlasStreamLimits, AtlasStreamState, AtlasStreamStop, WorkspaceAtlas};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasStreamFileState {
    Unavailable(FileSearchError),
    Scanned(StreamReadState),
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AtlasStreamStats {
    pub bytes_read: u64,
    pub scanned_bytes: u64,
    pub read_calls: u64,
    pub interrupted_calls: u64,
    pub peak_input_buffer_bytes: usize,
    /// Counts only finalized file reports. At most one unretained lookahead
    /// occurrence is counted; a partial result is not an exhaustive total.
    pub matches_seen: u64,
    pub incomplete_files: usize,
}
impl AtlasStreamStats {
    pub(super) fn add_step(&mut self, before: StreamReadStats, after: StreamReadStats) -> Result<(), AtlasStreamError> {
        fn add(total: u64, before: u64, after: u64) -> Result<u64, AtlasStreamError> {
            after.checked_sub(before).and_then(|delta| total.checked_add(delta)).ok_or(AtlasStreamError::InvalidLimits)
        }
        self.bytes_read = add(self.bytes_read, before.bytes_read, after.bytes_read)?;
        self.scanned_bytes = add(self.scanned_bytes, before.scanned_bytes, after.scanned_bytes)?;
        self.read_calls = add(self.read_calls, before.read_calls, after.read_calls)?;
        self.interrupted_calls = add(self.interrupted_calls, before.interrupted_calls, after.interrupted_calls)?;
        self.peak_input_buffer_bytes = self.peak_input_buffer_bytes.max(after.peak_buffer_bytes);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasStreamFile {
    pub(super) request: CaptureRequest,
    pub(super) node: AtlasNodeId,
    pub(super) state: AtlasStreamFileState,
    pub(super) encoding: Option<DetectedEncoding>,
    pub(super) observed_length: Option<ByteLength>,
    pub(super) final_length: Option<ByteLength>,
    pub(super) consistency: Option<ExtentConsistency>,
    pub(super) unsupported_at: Option<ByteOffset>,
    pub(super) stats: StreamReadStats,
    pub(super) matches_seen: u64,
    pub(super) first_hit: usize,
    pub(super) hit_count: usize,
    pub(super) witness_start: usize,
    pub(super) witness_len: usize,
}
impl AtlasStreamFile {
    pub const fn file(&self) -> FileId { self.request.file() }
    pub const fn revision(&self) -> SourceRevision { self.request.revision() }
    pub const fn node(&self) -> AtlasNodeId { self.node }
    pub const fn state(&self) -> AtlasStreamFileState { self.state }
    pub const fn encoding(&self) -> Option<DetectedEncoding> { self.encoding }
    pub const fn observed_length(&self) -> Option<ByteLength> { self.observed_length }
    pub const fn final_length(&self) -> Option<ByteLength> { self.final_length }
    pub const fn consistency(&self) -> Option<ExtentConsistency> { self.consistency }
    pub const fn unsupported_at(&self) -> Option<ByteOffset> { self.unsupported_at }
    pub const fn stats(&self) -> StreamReadStats { self.stats }
    pub const fn matches_seen(&self) -> u64 { self.matches_seen }
    pub const fn first_hit(&self) -> usize { self.first_hit }
    pub const fn hit_count(&self) -> usize { self.hit_count }
    /// Matching metadata is not atomic filesystem snapshot evidence.
    pub fn is_complete(&self) -> bool {
        self.state == AtlasStreamFileState::Scanned(StreamReadState::Complete)
            && self.consistency == Some(ExtentConsistency::UnchangedMetadata)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasStreamHit {
    pub(super) node: AtlasNodeId,
    pub(super) request: CaptureRequest,
    pub(super) file_record: usize,
    pub(super) occurrence_id: u64,
    pub(super) original_range: ByteRange,
}
impl AtlasStreamHit {
    pub const fn node(&self) -> AtlasNodeId { self.node }
    pub const fn file(&self) -> FileId { self.request.file() }
    pub const fn revision(&self) -> SourceRevision { self.request.revision() }
    pub const fn file_record(&self) -> usize { self.file_record }
    pub const fn occurrence_id(&self) -> u64 { self.occurrence_id }
    pub const fn original_range(&self) -> ByteRange { self.original_range }
}

pub struct AtlasStreamReport<'atlas, 'catalog> {
    pub(super) atlas: &'atlas WorkspaceAtlas<'catalog>,
    pub(super) generation: QueryGeneration,
    pub(super) limits: AtlasStreamLimits,
    pub(super) files: Vec<AtlasStreamFile>,
    pub(super) hits: Vec<AtlasStreamHit>,
    pub(super) matched_files: Vec<usize>,
    pub(super) witnesses: Vec<u8>,
    pub(super) literal: String,
    pub(super) stats: AtlasStreamStats,
    pub(super) state: AtlasStreamState,
    pub(super) stop: Option<AtlasStreamStop>,
    pub(super) _lease: ResourceLease,
}
impl<'atlas, 'catalog> AtlasStreamReport<'atlas, 'catalog> {
    pub fn atlas(&self) -> &'atlas WorkspaceAtlas<'catalog> { self.atlas }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn limits(&self) -> AtlasStreamLimits { self.limits }
    pub const fn state(&self) -> AtlasStreamState { self.state }
    pub const fn stop_reason(&self) -> Option<AtlasStreamStop> { self.stop }
    pub const fn stats(&self) -> AtlasStreamStats { self.stats }
    pub fn files(&self) -> &[AtlasStreamFile] { &self.files }
    pub fn hits(&self) -> &[AtlasStreamHit] { &self.hits }
    pub fn literal(&self) -> &str { &self.literal }
    pub fn files_examined(&self) -> usize { self.files.len() }
    /// Includes an active file whose final result has not yet been accepted.
    pub fn unexamined_files(&self) -> usize { self.atlas.file_count() - self.files.len() }
    pub fn retained_witness_bytes(&self) -> usize { self.witnesses.len() }
    pub fn reserved_bytes(&self) -> u64 { self._lease.info().bytes().get() }
    pub fn is_complete(&self) -> bool {
        self.state == AtlasStreamState::Finished && self.stop.is_none()
            && self.atlas.discovery_complete() && self.unexamined_files() == 0 && self.stats.incomplete_files == 0
    }
    pub fn validate_delivery(&self, atlas: &WorkspaceAtlas<'_>, generation: QueryGeneration) -> Result<(), AtlasStreamError> {
        self.atlas.validate_active()?;
        if !std::ptr::eq(self.atlas, atlas) { return Err(AtlasStreamError::WrongAtlas); }
        if self.generation != generation { return Err(AtlasStreamError::StaleQuery); }
        match self.state {
            AtlasStreamState::Canceled => Err(AtlasStreamError::Canceled),
            AtlasStreamState::Failed => Err(AtlasStreamError::Pending),
            _ => Ok(()),
        }
    }
    pub fn select_hit(&self, position: usize, atlas: &WorkspaceAtlas<'_>, generation: QueryGeneration)
        -> Result<AtlasStreamSelection<'_>, AtlasStreamError> {
        self.validate_delivery(atlas, generation)?;
        let hit = self.hits.get(position).ok_or(AtlasStreamError::InvalidHit)?;
        let file = self.files.get(hit.file_record).ok_or(AtlasStreamError::InvalidHit)?;
        let bytes = self.witnesses.get(file.witness_start..file.witness_start + file.witness_len)
            .ok_or(AtlasStreamError::InvalidHit)?;
        if hit.request != file.request || hit.original_range.len().get() != bytes.len() as u64 {
            return Err(AtlasStreamError::InvalidHit);
        }
        Ok(AtlasStreamSelection { hit, file, original_bytes: bytes, text: &self.literal })
    }
    /// Preserve ONLY the selected literal in the existing exact extent reader.
    /// The native consistency remains on this report; extent backing is the
    /// verified witness. Decoding surrounding context requires a different,
    /// explicit observation and is not silently performed by this method.
    pub fn retain_hit(&self, position: usize, atlas: &WorkspaceAtlas<'_>, generation: QueryGeneration,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<ObservedExtent, AtlasStreamError> {
        let selection = self.select_hit(position, atlas, generation)?;
        let request = selection.hit.request.with_range(selection.hit.original_range)
            .map_err(FileSearchError::from)?;
        let length = selection.file.observed_length.ok_or(AtlasStreamError::InvalidHit)?;
        ObservedExtent::from_bytes(request, length, selection.original_bytes, budget, allocation)
            .map_err(|e| AtlasStreamError::File(e.into()))
    }
    /// (retained occurrences, distinct retained matching files). No inference
    /// about unexamined/unreadable files or occurrences omitted by a result cap.
    /// Sibling-group boxes represent only part of a directory, so consumers
    /// must not attach a parent directory's count to such boxes.
    pub fn retained_matches_in(&self, node: AtlasNodeId) -> Result<(usize, usize), AtlasStreamError> {
        self.validate_delivery(self.atlas, self.generation)?;
        let layout = self.atlas.layout();
        if node.root() != layout.root() || node.layout() != layout.revision() { return Err(AtlasStreamError::WrongAtlas); }
        let parcel = layout.nodes().get(node.ordinal() as usize).ok_or(AtlasStreamError::InvalidHit)?;
        if parcel.path().is_empty() { return Ok((self.hits.len(), self.matched_files.len())); }
        let compare = |candidate: AtlasNodeId| {
            let path = layout.nodes()[candidate.ordinal() as usize].path();
            if parcel.kind() == NodeKind::Directory { subtree_compare(path, parcel.path()) }
            else { path.cmp(parcel.path()) }
        };
        // Catalog and completed-file order are raw-path order. Hits are appended
        // by file and source position; no sorting/allocation occurs on camera use.
        let start = self.hits.partition_point(|hit| compare(hit.node) == Ordering::Less);
        let end = self.hits.partition_point(|hit| compare(hit.node) != Ordering::Greater);
        let file_start = self.matched_files.partition_point(|&i| compare(self.files[i].node) == Ordering::Less);
        let file_end = self.matched_files.partition_point(|&i| compare(self.files[i].node) != Ordering::Greater);
        Ok((end - start, file_end - file_start))
    }
}

pub struct AtlasStreamSelection<'a> {
    hit: &'a AtlasStreamHit,
    file: &'a AtlasStreamFile,
    original_bytes: &'a [u8],
    text: &'a str,
}
impl<'a> AtlasStreamSelection<'a> {
    pub fn hit(&self) -> &'a AtlasStreamHit { self.hit }
    pub fn file_report(&self) -> &'a AtlasStreamFile { self.file }
    pub fn original_bytes(&self) -> &'a [u8] { self.original_bytes }
    pub fn matched_text(&self) -> &'a str { self.text }
}
fn subtree_compare(path: &[u8], dir: &[u8]) -> Ordering {
    let common = path.len().min(dir.len());
    let order = path[..common].cmp(&dir[..common]);
    if order != Ordering::Equal { return order; }
    if path.len() <= dir.len() { return Ordering::Less; }
    path[dir.len()].cmp(&b'/')
}
