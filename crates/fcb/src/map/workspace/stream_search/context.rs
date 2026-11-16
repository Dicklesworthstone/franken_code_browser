#![forbid(unsafe_code)]

//! Bounded same-observation context transferred before a file worker is dropped.
//! The atlas owns these exact bytes; selections never reopen a live pathname.

use std::mem::size_of;
use fcb_core::{ArenaOwnerId, ByteLength, ByteRange, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::search::{StreamContextOptions, StreamingContext, MAX_STREAM_CONTEXT_BYTES, MAX_STREAM_CONTEXT_SIDE_BYTES};
use super::{AtlasStreamError, reserve};

#[derive(Clone, Copy)]
struct Record {
    requested: ByteRange,
    original: ByteRange,
    matched: ByteRange,
    start: usize,
    len: usize,
}

pub(super) struct Contexts {
    options: StreamContextOptions,
    records: Vec<Record>,
    bytes: Vec<u8>,
    lease: ResourceLease,
}
impl Contexts {
    pub(super) fn new(owner: ArenaOwnerId, matches: usize, witness_bytes: usize,
        options: StreamContextOptions, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<Self, AtlasStreamError> {
        if options.before_bytes > MAX_STREAM_CONTEXT_SIDE_BYTES || options.after_bytes > MAX_STREAM_CONTEXT_SIDE_BYTES {
            return Err(AtlasStreamError::InvalidLimits);
        }
        let capacity = options.before_bytes.checked_add(witness_bytes)
            .and_then(|n| n.checked_add(options.after_bytes))
            .and_then(|n| n.checked_mul(matches)).ok_or(AtlasStreamError::InvalidLimits)?;
        if capacity > MAX_STREAM_CONTEXT_BYTES { return Err(AtlasStreamError::ResourceDenied); }
        let charge = matches.checked_mul(size_of::<Record>())
            .and_then(|n| n.checked_add(capacity))
            .and_then(|n| n.checked_add(size_of::<Self>())).ok_or(AtlasStreamError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasStreamError::ResourceDenied)?;
        Ok(Self { options, records: reserve(matches)?, bytes: reserve(capacity)?, lease })
    }
    pub(super) fn options(&self) -> StreamContextOptions { self.options }
    pub(super) fn bytes_len(&self) -> usize { self.bytes.len() }
    pub(super) fn reserved_bytes(&self) -> u64 { self.lease.info().bytes().get() }
    pub(super) fn push(&mut self, context: StreamingContext<'_>, matched: ByteRange)
        -> Result<(), AtlasStreamError> {
        let original = context.original_range();
        let requested = context.requested_range();
        let bytes = context.bytes();
        if context.match_range() != matched || original.len().get() != bytes.len() as u64
            || original.start() < requested.start() || original.end() > requested.end()
            || original.start() > matched.start() || original.end() < matched.end()
            || context.is_complete() != (original == requested) {
            return Err(AtlasStreamError::InvalidHit);
        }
        if self.records.len() == self.records.capacity() || bytes.len() > self.bytes.capacity() - self.bytes.len() {
            return Err(AtlasStreamError::ResourceDenied);
        }
        let start = self.bytes.len();
        self.bytes.extend_from_slice(bytes);
        self.records.push(Record { requested, original, matched, start, len: bytes.len() });
        Ok(())
    }
    pub(super) fn get(&self, position: usize, matched: ByteRange) -> Result<AtlasStreamContext<'_>, AtlasStreamError> {
        let record = self.records.get(position).ok_or(AtlasStreamError::InvalidHit)?;
        let bytes = self.bytes.get(record.start..record.start + record.len).ok_or(AtlasStreamError::InvalidHit)?;
        if matched != record.matched { return Err(AtlasStreamError::InvalidHit); }
        Ok(AtlasStreamContext { requested: record.requested, original: record.original, matched, bytes })
    }
}

/// Exact neighboring bytes read by the same file scan that produced this hit.
/// Completeness describes only this bounded requested interval, never the file.
#[derive(Clone, Copy, Debug)]
pub struct AtlasStreamContext<'a> {
    requested: ByteRange,
    original: ByteRange,
    matched: ByteRange,
    bytes: &'a [u8],
}
impl<'a> AtlasStreamContext<'a> {
    pub const fn requested_range(&self) -> ByteRange { self.requested }
    pub const fn original_range(&self) -> ByteRange { self.original }
    pub const fn match_range(&self) -> ByteRange { self.matched }
    pub const fn bytes(&self) -> &'a [u8] { self.bytes }
    pub fn is_complete(&self) -> bool { self.original == self.requested }
}
