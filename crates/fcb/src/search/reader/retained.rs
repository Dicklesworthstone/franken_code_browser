#![forbid(unsafe_code)]

//! Owned continuation of the ordinary source reader (FCB-019.A / FCB-050).
//! No second newline scanner, decoder, index policy or window implementation.
//! Source backing and its reservation survive reader/query destruction in either
//! order. Moving the sparse index into a scoped borrow allocates/copies nothing;
//! a drop guard returns it even during unwinding. All work/drop is worker-side.

use super::*;
use super::super::{ReadingWindow, ReadingWindowOptions};

pub struct RetainedSourceReader {
    source: SourceCapture,
    index: Option<IndexParts>,
    source_lease: ResourceLease,
}
impl RetainedSourceReader {
    /// Retain immutable backing without copying bytes. The first allocation
    /// explicitly charges source retention; the second owns the sparse index.
    pub fn new(source: &SourceCapture, limits: ReaderLimits, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2]) -> Result<Self, ReaderError> {
        if allocations[0] == allocations[1] { return Err(ReaderError::InvalidLimits); }
        let charge = source.bytes().len().checked_add(source.logical_path().len())
            .and_then(|n| n.checked_add(size_of::<Self>() + 256)).ok_or(ReaderError::ResourceDenied)?;
        let source_lease = budget.try_reserve_managed(source.owner(), allocations[0], ByteLength::new(charge as u64))
            .map_err(|_| ReaderError::ResourceDenied)?;
        let reader = SourceReader::new(source, limits, budget, allocations[1])?;
        Ok(Self { source: source.clone(), index: Some(IndexParts::take(reader)), source_lease })
    }
    pub fn source(&self) -> &SourceCapture { &self.source }
    pub fn validate_source(&self, source: &SourceCapture) -> Result<(), ReaderError> {
        validate_source(&self.source, source)
    }
    pub fn progress(&mut self) -> ReaderIndexProgress { self.borrowed().get().progress() }
    pub fn checkpoint_capacity(&self) -> usize { self.parts().capacity }
    pub fn checkpoint_stride_bytes(&self) -> usize { self.parts().stride }
    pub fn index_step(&mut self, max_bytes: usize, canceled: impl FnMut() -> bool)
        -> Result<ReaderIndexProgress, ReaderError> {
        self.borrowed().get_mut().index_step(max_bytes, canceled)
    }
    /// A request owns only its small cursor/descriptor and a shared source pin.
    /// It can be stepped while this reader indexes, or after this reader drops.
    pub fn seek(&mut self, target: ReadingTarget, generation: QueryGeneration,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<RetainedReadingSeek, ReaderError> {
        if generation.owner() != self.source.owner() { return Err(ReaderError::OwnerMismatch); }
        let charge = size_of::<RetainedReadingSeek>() + self.source.logical_path().len() + 128;
        let lease = budget.try_reserve_managed(self.source.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ReaderError::ResourceDenied)?;
        let state = SeekParts::take(self.borrowed().get().seek(target, generation)?);
        Ok(RetainedReadingSeek { source: self.source.clone(), state,
            _source_lease: self.source_lease.clone(), _lease: lease })
    }
    /// Reuse the exact prefix walked by a far jump as an index checkpoint.
    /// Canceled work may have a valid scanned prefix; it is never a navigation
    /// acceptance. Checkpoint gaps remain sparse, not an exact per-line table.
    pub fn learn(&mut self, seek: &RetainedReadingSeek) -> Result<(), ReaderError> {
        validate_source(&self.source, &seek.source)?;
        let state = self.index.as_mut().expect("index restored by scoped borrow");
        if seek.state.cursor.offset > state.cursor.offset {
            state.cursor = seek.state.cursor;
            if state.cursor.offset >= state.next_checkpoint && state.checkpoints.len() < state.capacity {
                state.checkpoints.push(state.cursor);
                state.next_checkpoint = state.cursor.offset.saturating_add(state.stride);
            }
        }
        Ok(())
    }
    /// A verified continuation anchor renders directly without another seek.
    /// The output borrows the retained source, not the temporary index wrapper.
    pub fn window(&mut self, anchor: ReadingAnchor, generation: QueryGeneration,
        options: ReadingWindowOptions, budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<ReadingWindow<'_>, ReaderError> {
        self.borrowed().get().window(anchor, generation, options, budget, allocation, canceled)
    }
    fn parts(&self) -> &IndexParts { self.index.as_ref().expect("index restored by scoped borrow") }
    fn borrowed(&mut self) -> ReaderBorrow<'_> {
        let parts = self.index.take().expect("exclusive index borrow");
        ReaderBorrow { reader: Some(parts.attach(&self.source)), destination: &mut self.index }
    }
}

pub struct RetainedReadingSeek {
    source: SourceCapture,
    state: SeekParts,
    _source_lease: ResourceLease,
    _lease: ResourceLease,
}
impl RetainedReadingSeek {
    pub fn source(&self) -> &SourceCapture { &self.source }
    pub fn state(&self) -> ReadingSeekState { self.state.state }
    pub fn generation(&self) -> QueryGeneration { self.state.generation }
    pub fn target(&self) -> ReadingTarget { self.state.target }
    pub fn scanned_bytes(&self) -> u64 { self.state.scanned_bytes }
    pub fn last_step_bytes(&self) -> usize { self.state.last_step_bytes }
    pub fn scanned_through(&self) -> ByteOffset { ByteOffset::new(self.state.cursor.offset as u64) }
    pub fn cancel(&mut self) {
        let mut borrowed = self.state.attach(&self.source);
        borrowed.cancel(); self.state = SeekParts::take(borrowed);
    }
    pub fn step(&mut self, max_bytes: usize, generation: QueryGeneration, canceled: impl FnMut() -> bool)
        -> Result<ReadingSeekState, ReaderError> {
        // A delayed request cannot cancel or advance newer work.
        if generation != self.generation() { return Err(ReaderError::StaleQuery); }
        let mut borrowed = self.state.attach(&self.source);
        let result = borrowed.step(max_bytes, generation, canceled);
        self.state = SeekParts::take(borrowed);
        result
    }
}

fn validate_source(left: &SourceCapture, right: &SourceCapture) -> Result<(), ReaderError> {
    if left.owner() != right.owner() { return Err(ReaderError::OwnerMismatch); }
    if left.file() != right.file() || left.revision() != right.revision()
        || !std::ptr::eq(left.bytes(), right.bytes()) { return Err(ReaderError::StaleSource); }
    Ok(())
}

// These are movable engine fields, not a serialized/untrusted checkpoint format.
// Neither representation exposes mutable coordinates to the caller.
struct IndexParts {
    encoding: DetectedEncoding, content_start: usize, checkpoints: Vec<Cursor>,
    cursor: Cursor, stride: usize, next_checkpoint: usize, capacity: usize, lease: ResourceLease,
}
impl IndexParts {
    fn take(reader: SourceReader<'_>) -> Self {
        Self { encoding: reader.encoding, content_start: reader.content_start, checkpoints: reader.checkpoints,
            cursor: reader.cursor, stride: reader.stride, next_checkpoint: reader.next_checkpoint,
            capacity: reader.capacity, lease: reader._lease }
    }
    fn attach(self, source: &SourceCapture) -> SourceReader<'_> {
        SourceReader { source, encoding: self.encoding, content_start: self.content_start, checkpoints: self.checkpoints,
            cursor: self.cursor, stride: self.stride, next_checkpoint: self.next_checkpoint,
            capacity: self.capacity, _lease: self.lease }
    }
}
struct ReaderBorrow<'a> { reader: Option<SourceReader<'a>>, destination: &'a mut Option<IndexParts> }
impl<'a> ReaderBorrow<'a> {
    fn get(&self) -> &SourceReader<'a> { self.reader.as_ref().expect("live scoped reader") }
    fn get_mut(&mut self) -> &mut SourceReader<'a> { self.reader.as_mut().expect("live scoped reader") }
}
impl Drop for ReaderBorrow<'_> {
    fn drop(&mut self) {
        *self.destination = self.reader.take().map(IndexParts::take);
    }
}
#[derive(Clone, Copy)]
struct SeekParts {
    encoding: DetectedEncoding, target: ReadingTarget, target_byte: Option<usize>,
    generation: QueryGeneration, cursor: Cursor, state: ReadingSeekState,
    scanned_bytes: u64, last_step_bytes: usize,
}
impl SeekParts {
    fn take(seek: ReadingSeek<'_>) -> Self {
        Self { encoding: seek.encoding, target: seek.target, target_byte: seek.target_byte,
            generation: seek.generation, cursor: seek.cursor, state: seek.state,
            scanned_bytes: seek.scanned_bytes, last_step_bytes: seek.last_step_bytes }
    }
    fn attach(self, source: &SourceCapture) -> ReadingSeek<'_> {
        ReadingSeek { source, encoding: self.encoding, target: self.target, target_byte: self.target_byte,
            generation: self.generation, cursor: self.cursor, state: self.state,
            scanned_bytes: self.scanned_bytes, last_step_bytes: self.last_step_bytes }
    }
}

#[cfg(test)]
mod tests;
