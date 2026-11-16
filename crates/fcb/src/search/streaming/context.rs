#![forbid(unsafe_code)]

//! Optional source context retained from the same forward observation as a hit.
//! The ring never seeks or rereads. Retained contexts contain original bytes,
//! including partial encoding units at their edges; the existing extent decoder
//! owns text-boundary preparation. A context is not a whole-file snapshot.

use super::*;

pub const MAX_STREAM_CONTEXT_SIDE_BYTES: usize = 4096;
pub const MAX_STREAM_CONTEXT_BYTES: usize = 16 * 1024 * 1024;

/// Original-byte allowances on either side of a stored match. These do not
/// enlarge the scan, read-call, hit, or cancellation budgets. A stopped scan can
/// therefore retain less following context than requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamContextOptions {
    pub before_bytes: usize,
    pub after_bytes: usize,
}
impl Default for StreamContextOptions {
    fn default() -> Self { Self { before_bytes: 160, after_bytes: 160 } }
}

/// Borrowed original bytes surrounding one retained occurrence. The range may
/// start/end inside a UTF-8 scalar or UTF-16 code unit: use the qualified extent
/// decoder before displaying text, preserving its trimmed range and source map.
#[derive(Clone, Copy, Debug)]
pub struct StreamingContext<'a> {
    entry: &'a Entry,
}
impl<'a> StreamingContext<'a> {
    pub fn bytes(self) -> &'a [u8] { &self.entry.bytes }
    pub fn original_range(self) -> ByteRange { self.entry.actual_range() }
    /// Requested before + match + after, clipped to the declared input length.
    /// This remains distinct from the extent actually retained after a stop.
    pub const fn requested_range(self) -> ByteRange { self.entry.requested }
    pub const fn match_range(self) -> ByteRange { self.entry.matched }
    /// Completeness of this bounded context, never of the file or query.
    pub fn is_complete(self) -> bool { self.entry.complete() }
}

#[derive(Debug)]
struct Entry {
    requested: ByteRange,
    matched: ByteRange,
    bytes: Vec<u8>,
}
impl Entry {
    fn end(&self) -> u64 { self.requested.start().get() + self.bytes.len() as u64 }
    fn complete(&self) -> bool { self.end() == self.requested.end().get() }
    fn actual_range(&self) -> ByteRange {
        ByteRange::new(self.requested.start(), ByteOffset::new(self.end()))
            .expect("retained context is a validated prefix of its requested range")
    }
}

pub(super) struct ContextCapture {
    options: StreamContextOptions,
    maximum_witness: usize,
    ring: Vec<u8>,
    observed_end: u64,
    entries: Vec<Entry>,
    maximum_entries: usize,
    pending: usize,
    lease: ResourceLease,
}
pub(super) struct RetainedContexts {
    options: StreamContextOptions,
    entries: Vec<Entry>,
    // Conservative admission includes the retired ring and row capacity until
    // the report drops, just as the enclosing streaming report retains scratch
    // admission. No ring or reader backing survives into the report.
    _lease: ResourceLease,
}
impl RetainedContexts {
    pub(super) fn get(&self, ordinal: usize) -> Result<StreamingContext<'_>, StreamReadError> {
        Ok(StreamingContext { entry: self.entries.get(ordinal).ok_or(StreamReadError::InvalidRange)? })
    }
    pub(super) fn options(&self) -> StreamContextOptions { self.options }
    pub(super) fn bytes(&self) -> usize { self.entries.iter().map(|entry| entry.bytes.len()).sum() }
}

impl ContextCapture {
    fn new(owner: ArenaOwnerId, maximum_witness: usize, maximum_entries: usize,
        options: StreamContextOptions, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<Self, StreamReadError> {
        if options.before_bytes > MAX_STREAM_CONTEXT_SIDE_BYTES || options.after_bytes > MAX_STREAM_CONTEXT_SIDE_BYTES {
            return Err(StreamReadError::InvalidLimits);
        }
        let per_hit = maximum_witness.checked_add(options.before_bytes)
            .and_then(|bytes| bytes.checked_add(options.after_bytes)).ok_or(StreamReadError::InvalidLimits)?;
        let output_bytes = per_hit.checked_mul(maximum_entries).ok_or(StreamReadError::InvalidLimits)?;
        if output_bytes > MAX_STREAM_CONTEXT_BYTES { return Err(StreamReadError::InvalidLimits); }
        // A match can begin in a previous input buffer. Identifier matches are
        // established by the following scalar (up to four original bytes).
        // Keep that lookahead plus a whole read block until matching consumes it.
        let ring_bytes = if maximum_entries == 0 { 0 } else {
            maximum_witness.checked_add(options.before_bytes)
                .and_then(|bytes| bytes.checked_add(STREAM_BUFFER_BYTES + 4))
                .ok_or(StreamReadError::InvalidLimits)?
        };
        let charge = maximum_entries.checked_mul(size_of::<Entry>())
            .and_then(|bytes| bytes.checked_add(output_bytes))
            .and_then(|bytes| bytes.checked_add(ring_bytes))
            .and_then(|bytes| bytes.checked_add(size_of::<Self>() + size_of::<RetainedContexts>()))
            .ok_or(StreamReadError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| StreamReadError::ResourceDenied)?;
        let mut ring = reserve(ring_bytes)?;
        ring.resize(ring_bytes, 0);
        Ok(Self { options, maximum_witness, ring, observed_end: 0,
            entries: reserve(maximum_entries)?, maximum_entries, pending: 0, lease })
    }

    /// Called once for successful new input, before any matching consumes it.
    pub(super) fn input(&mut self, start: u64, bytes: &[u8]) -> Result<(), StreamReadError> {
        if start != self.observed_end || bytes.len() > STREAM_BUFFER_BYTES {
            return Err(StreamReadError::InvalidRange);
        }
        let end = start.checked_add(bytes.len() as u64).ok_or(StreamReadError::InvalidRange)?;
        // Only unfinished suffixes receive work. Completed early hits do not
        // impose an O(all_hits) cost on every later block of a giant source.
        for entry in &mut self.entries[self.pending..] {
            if entry.complete() { continue; }
            if entry.end() != start { return Err(StreamReadError::InvalidRange); }
            let count = (entry.requested.end().get() - start).min(bytes.len() as u64) as usize;
            if entry.bytes.len() + count > entry.bytes.capacity() { return Err(StreamReadError::ResourceDenied); }
            entry.bytes.extend_from_slice(&bytes[..count]);
        }
        while self.pending < self.entries.len() && self.entries[self.pending].complete() { self.pending += 1; }
        if self.entries.len() < self.maximum_entries {
            let offset = (start % self.ring.len() as u64) as usize;
            let first = bytes.len().min(self.ring.len() - offset);
            self.ring[offset..offset + first].copy_from_slice(&bytes[..first]);
            self.ring[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        }
        self.observed_end = end;
        Ok(())
    }

    pub(super) fn retain(&mut self, matched: ByteRange, length: ByteLength) -> Result<(), StreamReadError> {
        if self.entries.len() == self.maximum_entries || matched.is_empty()
            || matched.len().get() > self.maximum_witness as u64 || matched.end().get() > self.observed_end
            || matched.end().get() > length.get() {
            return Err(StreamReadError::InvalidRange);
        }
        let start = matched.start().get().saturating_sub(self.options.before_bytes as u64);
        let end = matched.end().get().saturating_add(self.options.after_bytes as u64).min(length.get());
        let requested = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end))
            .map_err(|_| StreamReadError::InvalidRange)?;
        let retained_end = end.min(self.observed_end);
        if start < self.observed_end.saturating_sub(self.ring.len() as u64) {
            return Err(StreamReadError::InvalidRange);
        }
        let mut bytes = reserve(usize::try_from(requested.len().get()).map_err(|_| StreamReadError::InvalidRange)?)?;
        let count = usize::try_from(retained_end - start).map_err(|_| StreamReadError::InvalidRange)?;
        let offset = (start % self.ring.len() as u64) as usize;
        let first = count.min(self.ring.len() - offset);
        bytes.extend_from_slice(&self.ring[offset..offset + first]);
        bytes.extend_from_slice(&self.ring[..count - first]);
        self.entries.push(Entry { requested, matched, bytes });
        Ok(())
    }

    pub(super) fn clear(&mut self) { self.entries.clear(); self.pending = 0; }
    pub(super) fn finish(self) -> RetainedContexts {
        RetainedContexts { options: self.options, entries: self.entries, _lease: self.lease }
    }
}

impl<'needle, R: Read> ReaderSearch<'needle, R> {
    /// Retain bounded surrounding original bytes for each stored hit from this
    /// SAME forward scan. Call before any I/O. This reserves independent storage
    /// and never requests extra reads to fill a suffix after a scan has stopped.
    /// Invalid text and unqualified expressions discard their contexts together
    /// with their candidate hits; literal cancellation preserves accepted bytes.
    pub fn with_context(mut self, options: StreamContextOptions, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        if self.context.is_some() || self.stats.read_calls != 0 || self.stats.bytes_read != 0
            || !self.hits.is_empty() || !matches!(self.state, StreamReadState::Pending | StreamReadState::Complete) {
            return Err(StreamReadError::InvalidLimits);
        }
        self.context = Some(ContextCapture::new(self.request.file().owner(), self.needle.max_witness_bytes,
            self.options.max_matches, options, budget, allocation)?);
        Ok(self)
    }
}
