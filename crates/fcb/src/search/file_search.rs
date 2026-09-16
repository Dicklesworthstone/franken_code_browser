#![forbid(unsafe_code)]

//! Explicit whole-file search of a host-authorized OPEN regular file.
//!
//! No pathname is resolved, no shared file cursor is moved, and no temporary
//! source copy is written. The expected length is observed once at admission;
//! length/mtime are checked again on completion. Matching reads are a continuous
//! observed sequence, not an atomic snapshot or a promise of rereadable holes.

use std::{fs::{File, Metadata}, io::{self, Read}, time::SystemTime};
use fcb_core::{ByteLength, QueryGeneration, ResourceAllocationId, ResourceBudget};
use fcb_source::{CaptureRequest, SourceError};
use super::{ExtentConsistency, ExtentError, ObservedExtent};
use super::streaming::{ReaderSearch, StreamReadError, StreamReadOptions, StreamReadReport,
    StreamReadState, StreamReadStats, StreamReadStep, StreamingNeedle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSearchError { Stream(StreamReadError), Source(SourceError), Extent(ExtentError), UnsupportedPlatform }
impl std::fmt::Display for FileSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Stream(error) => write!(f, "{error}"), Self::Source(error) => write!(f, "{error}"),
            Self::Extent(error) => write!(f, "{error}"), Self::UnsupportedPlatform => f.write_str("FILE_SEARCH_PLATFORM_UNSUPPORTED") }
    }
}
impl std::error::Error for FileSearchError {}
impl From<StreamReadError> for FileSearchError { fn from(error: StreamReadError) -> Self { Self::Stream(error) } }
impl From<SourceError> for FileSearchError { fn from(error: SourceError) -> Self { Self::Source(error) } }
impl From<ExtentError> for FileSearchError { fn from(error: ExtentError) -> Self { Self::Extent(error) } }

struct PositionedInput { file: File, offset: u64 }
impl Read for PositionedInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let count = self.file.read_at(bytes, self.offset)?;
            self.offset = self.offset.checked_add(count as u64)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "source offset overflow"))?;
            Ok(count)
        }
        #[cfg(not(unix))]
        { let _ = bytes; Err(io::Error::new(io::ErrorKind::Unsupported, "positioned file reads unavailable")) }
    }
}
#[derive(Clone, Copy)]
struct Stamp { length: u64, modified: Option<SystemTime> }
impl Stamp {
    fn from_metadata(metadata: &Metadata) -> Self { Self { length: metadata.len(), modified: metadata.modified().ok() } }
}

pub struct FileSearch<'needle> {
    search: ReaderSearch<'needle, PositionedInput>,
    before: Stamp,
}
impl<'needle> FileSearch<'needle> {
    /// The host allocates a fresh source revision for this observation. Creating
    /// another job with an old revision cannot establish capture identity.
    pub fn new(file: File, request: CaptureRequest, needle: &'needle StreamingNeedle,
        options: StreamReadOptions, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<Self, FileSearchError> {
        if !cfg!(unix) { return Err(FileSearchError::UnsupportedPlatform); }
        let metadata = file.metadata().map_err(|_| SourceError::CaptureUnavailable)?;
        if !metadata.is_file() { return Err(SourceError::SpecialObject.into()); }
        let before = Stamp::from_metadata(&metadata);
        let search = ReaderSearch::new(PositionedInput { file, offset: 0 }, request,
            ByteLength::new(before.length), needle, options, budget, allocation)?;
        Ok(Self { search, before })
    }
    pub fn state(&self) -> StreamReadState { self.search.state() }
    pub fn stats(&self) -> StreamReadStats { self.search.stats() }
    pub fn generation(&self) -> QueryGeneration { self.search.generation() }
    pub fn cancel(&mut self) { self.search.cancel(); }
    pub fn step(&mut self, step: StreamReadStep, generation: QueryGeneration,
        canceled: impl FnMut() -> bool) -> Result<StreamReadState, FileSearchError> {
        Ok(self.search.step(step, generation, canceled)?)
    }
    pub fn finish(self) -> Result<FileSearchReport<'needle>, FileSearchError> {
        let (input, report) = self.search.finish()?;
        let after = input.file.metadata().ok().filter(|metadata| metadata.is_file())
            .map(|metadata| Stamp::from_metadata(&metadata));
        let consistency = if report.state() == StreamReadState::ShortRead { ExtentConsistency::ShortRead }
            else { match after {
                Some(after) if after.length != self.before.length || matches!((self.before.modified, after.modified),
                    (Some(before), Some(after)) if before != after) => ExtentConsistency::ChangedDuringRead,
                Some(after) if self.before.modified.is_some() && after.modified.is_some() => ExtentConsistency::UnchangedMetadata,
                _ => ExtentConsistency::MetadataUnavailable,
            }};
        Ok(FileSearchReport { report, consistency, final_length: after.map(|stamp| ByteLength::new(stamp.length)) })
    }
}

pub struct FileSearchReport<'needle> {
    report: StreamReadReport<'needle>,
    consistency: ExtentConsistency,
    final_length: Option<ByteLength>,
}
impl FileSearchReport<'_> {
    pub fn search(&self) -> &StreamReadReport<'_> { &self.report }
    pub const fn consistency(&self) -> ExtentConsistency { self.consistency }
    pub const fn final_length(&self) -> Option<ByteLength> { self.final_length }
    /// Completed declared byte sequence with matching before/after metadata.
    /// This is explicitly NOT an atomic filesystem or whole-workspace snapshot.
    pub fn is_complete(&self) -> bool {
        self.report.input_complete() && self.consistency == ExtentConsistency::UnchangedMetadata
    }
    /// Materialize ONLY a verified literal witness for the selected hit, without
    /// rereading the live file. KMP equality and the declared exact encoding prove
    /// the bytes; no text outside the hit is synthesized. The returned extent is
    /// host-supplied witness backing, with this report carrying native consistency.
    /// It can be opened through BrowserSession::open_extent after the report/file
    /// has closed. Surrounding source requires a new explicitly labelled read.
    pub fn retain_hit(&self, ordinal: usize, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<ObservedExtent, FileSearchError> {
        let hit = self.report.hits().get(ordinal).ok_or(StreamReadError::InvalidRange)?;
        let request = self.report.request().with_range(hit.original_range())?;
        Ok(ObservedExtent::from_bytes(request, self.report.observed_length(),
            self.report.witness_bytes(ordinal)?, budget, allocation)?)
    }
}
