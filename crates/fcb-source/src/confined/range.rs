#![forbid(unsafe_code)]

//! Demand-read source extents from an already authorized, OPEN regular file.
//!
//! Unlike a chunked complete capture, this never reads or retains the whole
//! file. The host opens the handle under its own confinement policy; this module
//! does NOT resolve paths, acquire grants, or strengthen that policy. In
//! particular, it does not use the legacy path-check-then-open reader.
//!
//! Each read gets a fresh source observation revision. Adjacent reads of a live
//! file cannot be joined under one supposedly immutable revision. Old results
//! retain their actual owned bytes after the file changes or closes. Metadata
//! comparisons detect some changes; they NEVER establish an atomic snapshot.

use std::{fs::{File, Metadata}, io, mem::size_of, sync::Arc, time::SystemTime};
use fcb_core::{ByteLength, ByteOffset, ByteRange, FileId, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::{CaptureRequest, ObservationDigest, SourceError};

pub const MAX_OBSERVED_EXTENT_BYTES: usize = 1024 * 1024;
pub const MAX_EXTENT_STEP_BYTES: usize = 64 * 1024;
pub const MAX_EXTENT_STEP_CALLS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExtentError {
    Source(SourceError), InvalidLimits, AllocationFailed, ResourceDenied,
    StaleObservation, Pending, Canceled, UnsupportedPlatform, InvalidIoCount,
}
impl ExtentError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Source(error) => error.code(),
            Self::InvalidLimits => "EXTENT_INVALID_LIMITS",
            Self::AllocationFailed => "EXTENT_ALLOCATION_FAILED",
            Self::ResourceDenied => "EXTENT_RESOURCE_DENIED",
            Self::StaleObservation => "EXTENT_STALE_OBSERVATION",
            Self::Pending => "EXTENT_READ_PENDING",
            Self::Canceled => "EXTENT_CANCELED",
            Self::UnsupportedPlatform => "EXTENT_UNSUPPORTED_PLATFORM",
            Self::InvalidIoCount => "EXTENT_INVALID_IO_COUNT",
        }
    }
}
impl std::fmt::Display for ExtentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for ExtentError {}
impl From<SourceError> for ExtentError {
    fn from(error: SourceError) -> Self { Self::Source(error) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentConsistency {
    /// The host supplied these bytes; no native metadata comparison was made.
    HostSupplied,
    /// Available before/after length and modification time matched. Not atomic.
    UnchangedMetadata,
    ChangedDuringRead,
    /// Fewer requested bytes were readable. Only the delivered prefix exists.
    ShortRead,
    MetadataUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentReadState { Pending, Ready, Failed(ExtentError), Canceled }
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExtentReadStats {
    pub bytes_read: u64,
    pub read_calls: u64,
    pub interrupted_calls: u64,
    pub last_step_bytes: usize,
    pub last_step_calls: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentStepBudget { pub max_bytes: usize, pub max_calls: usize }
impl Default for ExtentStepBudget {
    fn default() -> Self { Self { max_bytes: MAX_EXTENT_STEP_BYTES, max_calls: MAX_EXTENT_STEP_CALLS } }
}

#[derive(Debug)]
struct ExtentData {
    request: CaptureRequest,
    range: ByteRange,
    bytes: Vec<u8>,
    observed_length: ByteLength,
    final_length: Option<ByteLength>,
    consistency: ExtentConsistency,
    digest: ObservationDigest,
    _lease: ResourceLease,
}

/// An immutable, byte-serving extent, distinct from metadata-only ExtentCapture.
/// Cloning shares the payload AND its lease. No method fills holes from live data.
#[derive(Clone, Debug)]
pub struct ObservedExtent(Arc<ExtentData>);
impl ObservedExtent {
    /// Admit and copy exactly the requested host-supplied range. This bounded
    /// worker operation does not hash an unseen whole file or infer its encoding.
    pub fn from_bytes(request: CaptureRequest, observed_length: ByteLength, bytes: &[u8],
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, ExtentError> {
        let range = checked_request(request, observed_length)?;
        if range.len().get() != bytes.len() as u64 { return Err(SourceError::MetadataMismatch.into()); }
        let (mut owned, lease) = buffer(request, bytes.len(), budget, allocation)?;
        owned.copy_from_slice(bytes);
        Ok(Self::publish(request, range, owned, observed_length, Some(observed_length),
            ExtentConsistency::HostSupplied, lease))
    }
    pub fn request(&self) -> CaptureRequest { self.0.request }
    pub fn range(&self) -> ByteRange { self.0.range }
    pub fn bytes(&self) -> &[u8] { &self.0.bytes }
    pub fn observed_length(&self) -> ByteLength { self.0.observed_length }
    pub fn final_length(&self) -> Option<ByteLength> { self.0.final_length }
    pub fn consistency(&self) -> ExtentConsistency { self.0.consistency }
    pub fn digest(&self) -> ObservationDigest { self.0.digest }
    /// Completeness of the requested range, NOT completeness of the file.
    pub fn request_filled(&self) -> bool {
        self.0.request.range().map_or(self.0.observed_length.get() == 0,
            |requested| requested == self.0.range)
    }
    pub fn covers_whole_observation(&self) -> bool {
        self.0.range.start().get() == 0 && self.0.range.end().get() == self.0.observed_length.get()
    }
    /// Checked LOCAL subtraction before usize conversion supports offsets >4 GiB.
    pub fn range_bytes(&self, range: ByteRange) -> Result<&[u8], ExtentError> {
        if range.start() < self.0.range.start() || range.end() > self.0.range.end() {
            return Err(SourceError::CaptureUnavailable.into());
        }
        let start = usize::try_from(range.start().get() - self.0.range.start().get())
            .map_err(|_| SourceError::RangeOutOfBounds)?;
        let end = usize::try_from(range.end().get() - self.0.range.start().get())
            .map_err(|_| SourceError::RangeOutOfBounds)?;
        self.0.bytes.get(start..end).ok_or(SourceError::RangeOutOfBounds.into())
    }
    fn publish(request: CaptureRequest, range: ByteRange, bytes: Vec<u8>, observed_length: ByteLength,
        final_length: Option<ByteLength>, consistency: ExtentConsistency, lease: ResourceLease) -> Self {
        let digest = ObservationDigest::observe(&bytes);
        Self(Arc::new(ExtentData { request, range, bytes, observed_length, final_length,
            consistency, digest, _lease: lease }))
    }
}

/// Plans a visible byte window with small decoder context on BOTH sides. The
/// full range, including context, is charged and captured in ONE observation.
/// This is scalar/CRLF context, NOT paragraph, grapheme or shaping context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentWindowRequest { pub visible: ByteRange, pub capture: ByteRange }
impl ExtentWindowRequest {
    pub fn new(offset: ByteOffset, visible_bytes: usize, observed_length: ByteLength) -> Result<Self, ExtentError> {
        if visible_bytes < 4 || visible_bytes > MAX_OBSERVED_EXTENT_BYTES - 16
            || offset.get() > observed_length.get() { return Err(ExtentError::InvalidLimits); }
        let start = offset.get();
        let end = start.saturating_add(visible_bytes as u64).min(observed_length.get());
        Ok(Self {
            visible: byte_range(start, end)?,
            capture: byte_range(start.saturating_sub(8), end.saturating_add(8).min(observed_length.get()))?,
        })
    }
}

/// Owns one explicitly supplied open file. Not Clone: revision consumption must
/// remain monotone for this reader. Hosts must allocate globally unique source
/// revisions when multiple readers share an owner, as for other FCB captures.
pub struct FileRangeReader {
    file: File,
    file_id: FileId,
    last_revision: u64,
}
impl FileRangeReader {
    pub fn new(file_id: FileId, file: File) -> Result<Self, ExtentError> {
        #[cfg(not(unix))]
        { let _ = (file_id, file); return Err(ExtentError::UnsupportedPlatform); }
        #[cfg(unix)]
        {
            regular_metadata(&file)?;
            Ok(Self { file, file_id, last_revision: 0 })
        }
    }
    pub fn file_id(&self) -> FileId { self.file_id }
    /// An observation, not a promise about a later range request.
    pub fn observed_length(&self) -> Result<ByteLength, ExtentError> {
        Ok(ByteLength::new(regular_metadata(&self.file)?.len()))
    }
    /// Reserves the entire bounded candidate before reading payload. Revisions
    /// are spent even when metadata/admission fails: failed requests never wrap
    /// around and become new requests with old identities.
    pub fn begin(&mut self, request: CaptureRequest, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<FileExtentRead<'_>, ExtentError> {
        if request.file() != self.file_id { return Err(SourceError::ForeignOwner.into()); }
        if request.revision().get() <= self.last_revision { return Err(ExtentError::StaleObservation); }
        self.last_revision = request.revision().get();
        let before = FileStamp::from_metadata(&regular_metadata(&self.file)?);
        let range = checked_request(request, ByteLength::new(before.length))?;
        let len = usize::try_from(range.len().get()).map_err(|_| ExtentError::InvalidLimits)?;
        let (bytes, lease) = buffer(request, len, budget, allocation)?;
        Ok(FileExtentRead { file: &self.file, request, range, before, bytes,
            filled: 0, state: ExtentReadState::Pending, short: false,
            stats: ExtentReadStats::default(), lease })
    }
}

pub struct FileExtentRead<'file> {
    file: &'file File,
    request: CaptureRequest,
    range: ByteRange,
    before: FileStamp,
    bytes: Vec<u8>,
    filled: usize,
    state: ExtentReadState,
    short: bool,
    stats: ExtentReadStats,
    lease: ResourceLease,
}
impl FileExtentRead<'_> {
    pub fn state(&self) -> ExtentReadState { self.state }
    pub fn stats(&self) -> ExtentReadStats { self.stats }
    pub fn cancel(&mut self) { self.state = ExtentReadState::Canceled; }
    /// Byte and syscall-count bounds are independent. Interrupted calls consume
    /// call budget, avoiding an unbounded retry loop. Zero budget does no I/O.
    /// The host's callback should also check revocation of its native grant.
    /// Blocking OS calls themselves have no invented wall-clock deadline.
    pub fn step(&mut self, step: ExtentStepBudget, canceled: impl FnMut() -> bool) -> Result<ExtentReadState, ExtentError> {
        let file = self.file;
        self.step_using(step, canceled, |bytes, offset| positioned_read(file, bytes, offset))
    }
    fn step_using(&mut self, step: ExtentStepBudget, mut canceled: impl FnMut() -> bool,
        mut read: impl FnMut(&mut [u8], u64) -> io::Result<usize>) -> Result<ExtentReadState, ExtentError> {
        self.stats.last_step_bytes = 0;
        self.stats.last_step_calls = 0;
        if canceled() { self.cancel(); }
        match self.state {
            ExtentReadState::Canceled => return Err(ExtentError::Canceled),
            ExtentReadState::Failed(error) => return Err(error),
            ExtentReadState::Ready => return Ok(self.state),
            ExtentReadState::Pending => {}
        }
        let bytes_limit = step.max_bytes.min(MAX_EXTENT_STEP_BYTES);
        let calls_limit = step.max_calls.min(MAX_EXTENT_STEP_CALLS);
        if bytes_limit == 0 || calls_limit == 0 { return Ok(self.state); }
        while self.filled < self.bytes.len() && self.stats.last_step_bytes < bytes_limit
            && self.stats.last_step_calls < calls_limit {
            if canceled() { self.cancel(); return Err(ExtentError::Canceled); }
            let count = (self.bytes.len() - self.filled).min(bytes_limit - self.stats.last_step_bytes);
            let offset = self.range.start().get() + self.filled as u64; // Range validated before allocation.
            self.stats.last_step_calls += 1;
            self.stats.read_calls = self.stats.read_calls.saturating_add(1);
            match read(&mut self.bytes[self.filled..self.filled + count], offset) {
                Ok(0) => { self.short = true; self.state = ExtentReadState::Ready; break; }
                Ok(read) if read <= count => {
                    self.filled += read;
                    self.stats.bytes_read += read as u64;
                    self.stats.last_step_bytes += read;
                }
                Ok(_) => { self.state = ExtentReadState::Failed(ExtentError::InvalidIoCount); return Err(ExtentError::InvalidIoCount); }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    self.stats.interrupted_calls = self.stats.interrupted_calls.saturating_add(1);
                }
                Err(_) => {
                    let error = ExtentError::Source(SourceError::CaptureUnavailable);
                    self.state = ExtentReadState::Failed(error); return Err(error);
                }
            }
        }
        if self.filled == self.bytes.len() { self.state = ExtentReadState::Ready; }
        if canceled() { self.cancel(); return Err(ExtentError::Canceled); }
        Ok(self.state)
    }
    /// One final stat and one bounded digest pass; no reread or Vec-to-Arc payload
    /// copy. Failure/cancel/pending candidates cannot publish zero-filled bytes.
    pub fn finish(mut self, mut canceled: impl FnMut() -> bool) -> Result<ObservedExtent, ExtentError> {
        if canceled() { return Err(ExtentError::Canceled); }
        match self.state {
            ExtentReadState::Canceled => return Err(ExtentError::Canceled),
            ExtentReadState::Failed(error) => return Err(error),
            ExtentReadState::Pending => return Err(ExtentError::Pending),
            ExtentReadState::Ready => {}
        }
        let after = regular_metadata(self.file).ok().map(|meta| FileStamp::from_metadata(&meta));
        let consistency = if self.short { ExtentConsistency::ShortRead }
            else { match after {
                Some(after) if before_after_differ(self.before, after) => ExtentConsistency::ChangedDuringRead,
                Some(after) if self.before.modified.is_some() && after.modified.is_some() => ExtentConsistency::UnchangedMetadata,
                _ => ExtentConsistency::MetadataUnavailable,
            }};
        self.bytes.truncate(self.filled);
        let range = byte_range(self.range.start().get(), self.range.start().get() + self.filled as u64)?;
        if canceled() { return Err(ExtentError::Canceled); }
        let output = ObservedExtent::publish(self.request, range, self.bytes, ByteLength::new(self.before.length),
            after.map(|stamp| ByteLength::new(stamp.length)), consistency, self.lease);
        if canceled() { return Err(ExtentError::Canceled); }
        Ok(output)
    }
}

#[derive(Clone, Copy)]
struct FileStamp { length: u64, modified: Option<SystemTime> }
impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Self { Self { length: metadata.len(), modified: metadata.modified().ok() } }
}
fn before_after_differ(before: FileStamp, after: FileStamp) -> bool {
    before.length != after.length || matches!((before.modified, after.modified), (Some(a), Some(b)) if a != b)
}
fn regular_metadata(file: &File) -> Result<Metadata, ExtentError> {
    let metadata = file.metadata().map_err(|_| SourceError::CaptureUnavailable)?;
    if !metadata.is_file() { return Err(SourceError::SpecialObject.into()); }
    Ok(metadata)
}
fn positioned_read(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    { use std::os::unix::fs::FileExt; file.read_at(bytes, offset) }
    #[cfg(not(unix))]
    { let _ = (file, bytes, offset); Err(io::Error::new(io::ErrorKind::Unsupported, "positioned source read unsupported")) }
}
fn checked_request(request: CaptureRequest, total: ByteLength) -> Result<ByteRange, ExtentError> {
    let range = match request.range() {
        Some(range) => range,
        None if total.get() == 0 => byte_range(0, 0)?,
        None => return Err(SourceError::InvalidRange.into()),
    };
    if range.end().get() > total.get() { return Err(SourceError::RangeOutOfBounds.into()); }
    if range.len().get() > MAX_OBSERVED_EXTENT_BYTES as u64 { return Err(ExtentError::InvalidLimits); }
    Ok(range)
}
fn buffer(request: CaptureRequest, len: usize, budget: &ResourceBudget,
    allocation: ResourceAllocationId) -> Result<(Vec<u8>, ResourceLease), ExtentError> {
    let charge = len.checked_add(size_of::<ExtentData>() + size_of::<FileExtentRead<'_>>() + 4 * size_of::<usize>())
        .ok_or(ExtentError::InvalidLimits)?;
    let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
        .map_err(|_| ExtentError::ResourceDenied)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| ExtentError::AllocationFailed)?;
    if bytes.capacity() > len { return Err(ExtentError::ResourceDenied); }
    bytes.resize(len, 0);
    Ok((bytes, lease))
}
fn byte_range(start: u64, end: u64) -> Result<ByteRange, ExtentError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| SourceError::InvalidRange.into())
}
