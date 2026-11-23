#![forbid(unsafe_code)]

//! A bounded writer for the existing FCBS/1 envelope. The caller supplies an
//! exclusively held, empty, readable and seekable destination and one immutable
//! member capture at a time. No capture or whole-archive buffer is retained.
//! Destination authorization, disk admission, publication and durability belong
//! to the host. Dropping or failing this writer does not remove partial output.

use std::{io::{self, Read, Seek, SeekFrom, Write}, mem::size_of};

use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::{Sha256, Sha256Digest, CHECKSUM_LEN, FRAME_LEN, HEADER_LEN};
use crate::envelope::canonical_header;
use super::{SnapshotData, SnapshotEntry, SnapshotError, SnapshotLimits, Validation,
    SNAPSHOT_SCHEMA, validate_policy};

pub const SNAPSHOT_STREAM_BYTES: usize = 64 * 1024;
/// All reads, writes, seeks and flushes share this finite allowance. Short and
/// interrupted I/O spends calls too; a broken sink cannot create an endless loop.
pub const MAX_SNAPSHOT_STREAM_CALLS: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotStreamError {
    Format(SnapshotError), OutputNotEmpty, MemberCount, Poisoned,
    Read, Write, Seek, Flush, IoCallLimit, InvalidReadCount, InvalidWriteCount,
    Changed, Canceled,
}
impl SnapshotStreamError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Format(error) => error.code(),
            Self::OutputNotEmpty => "SNAPSHOT_OUTPUT_NOT_EMPTY",
            Self::MemberCount => "SNAPSHOT_MEMBER_COUNT",
            Self::Poisoned => "SNAPSHOT_WRITER_POISONED",
            Self::Read => "SNAPSHOT_READ_FAILED", Self::Write => "SNAPSHOT_WRITE_FAILED",
            Self::Seek => "SNAPSHOT_SEEK_FAILED", Self::Flush => "SNAPSHOT_FLUSH_FAILED",
            Self::IoCallLimit => "SNAPSHOT_IO_CALL_LIMIT",
            Self::InvalidReadCount => "SNAPSHOT_INVALID_READ_COUNT",
            Self::InvalidWriteCount => "SNAPSHOT_INVALID_WRITE_COUNT",
            Self::Changed => "SNAPSHOT_OUTPUT_CHANGED", Self::Canceled => "SNAPSHOT_CANCELED",
        }
    }
    pub const fn is_canceled(self) -> bool {
        matches!(self, Self::Canceled | Self::Format(SnapshotError::Canceled))
    }
}
impl From<SnapshotError> for SnapshotStreamError {
    fn from(error: SnapshotError) -> Self { Self::Format(error) }
}
impl std::fmt::Display for SnapshotStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for SnapshotStreamError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SnapshotStreamStats {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub read_calls: u64,
    pub write_calls: u64,
    pub seek_calls: u64,
    pub flush_calls: u64,
}
impl SnapshotStreamStats {
    pub const fn total_calls(self) -> u64 {
        self.read_calls.saturating_add(self.write_calls).saturating_add(self.seek_calls)
            .saturating_add(self.flush_calls)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotWriteSummary {
    pub digest: Sha256Digest,
    pub discovery_complete: bool,
    pub files: usize,
    pub captured_files: usize,
    pub source_bytes: u64,
    pub archive_bytes: u64,
    pub stats: SnapshotStreamStats,
}

/// A writer retains a single previous path and 64 KiB checksum scratch, charged
/// before allocation. Source slices and destination storage are caller-owned.
/// Passing `&mut File` keeps the destination available to the host after an
/// error, so it can report the actual partial effect and choose its cleanup.
pub struct SnapshotStreamWriter<W: Read + Write + Seek> {
    output: W,
    limits: SnapshotLimits,
    expected_files: usize,
    complete: bool,
    files: usize,
    captured: usize,
    paths: usize,
    sources: usize,
    previous: Vec<u8>,
    scratch: Vec<u8>,
    payload_hash: Sha256,
    end: u64,
    stats: SnapshotStreamStats,
    poisoned: bool,
    retained_charge: usize,
    _lease: ResourceLease,
}

impl<W: Read + Write + Seek> SnapshotStreamWriter<W> {
    /// Begin a private candidate. This never truncates an existing destination.
    /// Invalid policy/count/limits and denied memory perform no destination I/O.
    pub fn new(output: W, owner: ArenaOwnerId, discovery_complete: bool, policy: &str,
        expected_files: usize, limits: SnapshotLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotStreamError> {
        limits.validate_paged()?;
        validate_policy(policy)?;
        if expected_files > limits.max_files { return Err(SnapshotError::Limit.into()); }
        let prefix = FRAME_LEN + 18 + policy.len();
        // Even an empty captured member needs a one-byte relative path and its
        // fixed-width record fields. Admit this lower bound before any writes.
        let minimum = expected_files.checked_mul(26).and_then(|n| n.checked_add(prefix))
            .ok_or(SnapshotError::Limit)?;
        if minimum > limits.max_document_bytes { return Err(SnapshotError::Limit.into()); }
        if canceled() { return Err(SnapshotStreamError::Canceled); }
        let path_capacity = if expected_files == 0 { 0 } else { limits.max_path_bytes };
        let charge = size_of::<Self>().checked_add(path_capacity)
            .and_then(|n| n.checked_add(SNAPSHOT_STREAM_BYTES))
            .and_then(|n| n.checked_add(3 * size_of::<Sha256>() + HEADER_LEN))
            .ok_or(SnapshotError::Limit)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| SnapshotError::ResourceDenied)?;
        let previous = reserve(path_capacity)?;
        let mut scratch = reserve(SNAPSHOT_STREAM_BYTES)?;
        scratch.resize(SNAPSHOT_STREAM_BYTES, 0);
        let mut writer = Self { output, limits, expected_files, complete: discovery_complete,
            files: 0, captured: 0, paths: 0, sources: 0, previous, scratch, payload_hash: Sha256::new(),
            end: (prefix - CHECKSUM_LEN) as u64, stats: SnapshotStreamStats::default(),
            poisoned: false, retained_charge: charge, _lease: lease };
        if writer.seek(SeekFrom::End(0), &mut canceled)? != 0 {
            return Err(SnapshotStreamError::OutputNotEmpty);
        }
        writer.seek_to(0, &mut canceled)?;
        writer.write_all(&canonical_header(SNAPSHOT_SCHEMA, 0), &mut canceled)?;
        writer.write_payload(&[u8::from(discovery_complete), 1], &mut canceled)?;
        writer.write_payload(&(expected_files as u64).to_le_bytes(), &mut canceled)?;
        writer.write_payload(&(policy.len() as u64).to_le_bytes(), &mut canceled)?;
        writer.write_payload(policy.as_bytes(), &mut canceled)?;
        Ok(writer)
    }

    pub const fn stats(&self) -> SnapshotStreamStats { self.stats }
    pub const fn retained_charge(&self) -> usize { self.retained_charge }
    pub const fn is_poisoned(&self) -> bool { self.poisoned }

    /// Append one exact capture or an explicit unavailable record. Every field,
    /// aggregate limit and raw-path ordering check completes before writing.
    /// A rejected record can be replaced and retried; I/O failure or cancellation
    /// poisons the writer, preventing publication of a partially appended record.
    pub fn append(&mut self, entry: SnapshotEntry<'_>, mut canceled: impl FnMut() -> bool)
        -> Result<(), SnapshotStreamError> {
        self.checkpoint(&mut canceled)?;
        if self.files >= self.expected_files { return Err(SnapshotStreamError::MemberCount); }
        let (paths, sources, captured) = {
            let mut validation = Validation { limits: self.limits,
                previous: if self.files == 0 { None } else { Some(self.previous.as_slice()) },
                paths: self.paths, sources: self.sources, captured: self.captured };
            validation.entry(entry)?;
            (validation.paths, validation.sources, validation.captured)
        };
        let (tag, data) = match entry.data {
            SnapshotData::Captured(bytes) => (1, bytes),
            SnapshotData::Unavailable(reason) => (0, reason.as_bytes()),
        };
        let next_end = self.end.checked_add(25 + entry.path.len() as u64)
            .and_then(|n| n.checked_add(data.len() as u64)).ok_or(SnapshotError::Limit)?;
        if next_end.checked_add(CHECKSUM_LEN as u64).ok_or(SnapshotError::Limit)?
            > self.limits.max_document_bytes as u64 { return Err(SnapshotError::Limit.into()); }
        self.write_payload(&(entry.path.len() as u64).to_le_bytes(), &mut canceled)?;
        self.write_payload(entry.path, &mut canceled)?;
        self.write_payload(&entry.observed_bytes.to_le_bytes(), &mut canceled)?;
        self.write_payload(&[tag], &mut canceled)?;
        self.write_payload(&(data.len() as u64).to_le_bytes(), &mut canceled)?;
        self.write_payload(data, &mut canceled)?;
        // The path buffer already has its maximum admitted capacity; no source
        // bytes or per-member metadata accumulate across calls.
        self.previous.clear();
        self.previous.extend_from_slice(entry.path);
        self.paths = paths;
        self.sources = sources;
        self.captured = captured;
        self.files += 1;
        self.end = next_end;
        Ok(())
    }

    /// Patch the now-known payload length, hash the destination in bounded
    /// reads, verify those bytes against the accepted payload, and append the
    /// canonical checksum. Success means the entire
    /// envelope was accepted by `Write`; the host still owns flush/sync and
    /// publication. Cancellation is never polled after the last checksum byte
    /// succeeds, so a completed effect cannot be relabeled as canceled.
    pub fn finish(mut self, mut canceled: impl FnMut() -> bool)
        -> Result<(W, SnapshotWriteSummary), SnapshotStreamError> {
        self.checkpoint(&mut canceled)?;
        if self.files != self.expected_files { return Err(SnapshotStreamError::MemberCount); }
        self.flush(&mut canceled)?;
        if self.seek(SeekFrom::End(0), &mut canceled)? != self.end {
            return Err(self.poison(SnapshotStreamError::Changed));
        }
        self.seek_to(16, &mut canceled)?;
        self.write_all(&(self.end - HEADER_LEN as u64).to_le_bytes(), &mut canceled)?;
        self.flush(&mut canceled)?;
        self.seek_to(0, &mut canceled)?;
        let mut hash = Sha256::new();
        let mut observed_payload = Sha256::new();
        let header = canonical_header(SNAPSHOT_SCHEMA, self.end - HEADER_LEN as u64);
        let mut remaining = self.end;
        while remaining > 0 {
            let length = remaining.min(SNAPSHOT_STREAM_BYTES as u64) as usize;
            let count = self.read(length, &mut canceled)?;
            if count == 0 { return Err(self.poison(SnapshotStreamError::Changed)); }
            let offset = self.end - remaining;
            let header_bytes = if offset < HEADER_LEN as u64 {
                let offset = offset as usize;
                let length = count.min(HEADER_LEN - offset);
                if self.scratch[..length] != header[offset..offset + length] {
                    return Err(self.poison(SnapshotStreamError::Changed));
                }
                length
            } else { 0 };
            hash.update(&self.scratch[..count]);
            observed_payload.update(&self.scratch[header_bytes..count]);
            remaining -= count as u64;
        }
        if self.read(1, &mut canceled)? != 0 { return Err(self.poison(SnapshotStreamError::Changed)); }
        let accepted_payload = std::mem::replace(&mut self.payload_hash, Sha256::new()).finalize();
        if observed_payload.finalize() != accepted_payload {
            return Err(self.poison(SnapshotStreamError::Changed));
        }
        let digest = hash.finalize();
        self.seek_to(self.end, &mut canceled)?;
        self.write_all(digest.as_bytes(), &mut canceled)?;
        let summary = SnapshotWriteSummary { digest, discovery_complete: self.complete,
            files: self.files, captured_files: self.captured, source_bytes: self.sources as u64,
            archive_bytes: self.end + CHECKSUM_LEN as u64, stats: self.stats };
        Ok((self.output, summary))
    }

    fn poison(&mut self, error: SnapshotStreamError) -> SnapshotStreamError {
        self.poisoned = true;
        error
    }
    fn checkpoint(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), SnapshotStreamError> {
        if self.poisoned { return Err(SnapshotStreamError::Poisoned); }
        if canceled() { return Err(self.poison(SnapshotStreamError::Canceled)); }
        Ok(())
    }
    fn admit_call(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), SnapshotStreamError> {
        self.checkpoint(canceled)?;
        if self.stats.total_calls() >= MAX_SNAPSHOT_STREAM_CALLS {
            return Err(self.poison(SnapshotStreamError::IoCallLimit));
        }
        Ok(())
    }
    fn write_all(&mut self, bytes: &[u8], canceled: &mut impl FnMut() -> bool)
        -> Result<(), SnapshotStreamError> {
        self.write(bytes, false, canceled)
    }
    fn write_payload(&mut self, bytes: &[u8], canceled: &mut impl FnMut() -> bool)
        -> Result<(), SnapshotStreamError> {
        self.write(bytes, true, canceled)
    }
    fn write(&mut self, mut bytes: &[u8], payload: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<(), SnapshotStreamError> {
        while !bytes.is_empty() {
            self.admit_call(canceled)?;
            let length = bytes.len().min(SNAPSHOT_STREAM_BYTES);
            self.stats.write_calls += 1;
            match self.output.write(&bytes[..length]) {
                Ok(0) => return Err(self.poison(SnapshotStreamError::Write)),
                Ok(count) if count > length => return Err(self.poison(SnapshotStreamError::InvalidWriteCount)),
                Ok(count) => {
                    self.stats.bytes_written += count as u64;
                    if payload { self.payload_hash.update(&bytes[..count]); }
                    bytes = &bytes[count..];
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(self.poison(SnapshotStreamError::Write)),
            }
        }
        Ok(())
    }
    fn read(&mut self, length: usize, canceled: &mut impl FnMut() -> bool) -> Result<usize, SnapshotStreamError> {
        loop {
            self.admit_call(canceled)?;
            self.stats.read_calls += 1;
            match self.output.read(&mut self.scratch[..length]) {
                Ok(count) if count > length => return Err(self.poison(SnapshotStreamError::InvalidReadCount)),
                Ok(count) => { self.stats.bytes_read += count as u64; return Ok(count); }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(self.poison(SnapshotStreamError::Read)),
            }
        }
    }
    fn seek(&mut self, position: SeekFrom, canceled: &mut impl FnMut() -> bool) -> Result<u64, SnapshotStreamError> {
        loop {
            self.admit_call(canceled)?;
            self.stats.seek_calls += 1;
            match self.output.seek(position) {
                Ok(offset) => return Ok(offset),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(self.poison(SnapshotStreamError::Seek)),
            }
        }
    }
    fn seek_to(&mut self, offset: u64, canceled: &mut impl FnMut() -> bool) -> Result<(), SnapshotStreamError> {
        if self.seek(SeekFrom::Start(offset), canceled)? != offset {
            return Err(self.poison(SnapshotStreamError::Seek));
        }
        Ok(())
    }
    fn flush(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), SnapshotStreamError> {
        loop {
            self.admit_call(canceled)?;
            self.stats.flush_calls += 1;
            match self.output.flush() {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(self.poison(SnapshotStreamError::Flush)),
            }
        }
    }
}

fn reserve(bytes: usize) -> Result<Vec<u8>, SnapshotStreamError> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(bytes).map_err(|_| SnapshotError::ResourceDenied)?;
    if buffer.capacity() != bytes { return Err(SnapshotError::ResourceDenied.into()); }
    Ok(buffer)
}

#[cfg(test)]
mod tests;
