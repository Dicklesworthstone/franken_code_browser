#![forbid(unsafe_code)]

//! File-backed access to the existing FCBS/1 format, without retaining archive
//! payloads. Ordinary opening validates the ENTIRE bounded envelope in one
//! sequential pass, retaining metadata and per-member SHA-256 digests. Loading
//! a member verifies its bytes against that digest before returning them.
//! `open_pinned` is an explicit trusted-catalog alternative, not a full body scan.
//! No mmap, extraction, pathname lookup or grant.
//!
//! Open is cancellable worker work, not a constant-time UI callback. Every read
//! is at most 64 KiB, short/interrupted calls spend a hard call allowance, and all
//! lengths/counts are admitted before allocation. This is paged source access,
//! NOT persisted search postings. Consult fully_verified_on_open for the route.

pub mod catalog;
pub use catalog::{CatalogArtifact, CatalogError, PinnedCatalog};

use std::{io::{self, Read, Seek, SeekFrom}, mem::size_of, ops::Range};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::{Sha256, Sha256Digest, HEADER_LEN, CHECKSUM_LEN, FRAME_LEN};
use crate::snapshot::{SnapshotLimits, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_FILES, SNAPSHOT_SCHEMA};

pub const SNAPSHOT_READ_BYTES: usize = 64 * 1024;
pub const MAX_SNAPSHOT_READ_CALLS: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagedSnapshotError {
    Limits, Format, Version, Checksum, Path, Order, Member, Missing,
    Changed, Io, ReadCallLimit, InvalidReadCount, ResourceDenied, Canceled,
}
impl PagedSnapshotError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limits => "SNAPSHOT_LIMIT", Self::Format => "SNAPSHOT_INVALID_ENVELOPE",
            Self::Version => "SNAPSHOT_UNSUPPORTED_VERSION", Self::Checksum => "SNAPSHOT_CHECKSUM_MISMATCH",
            Self::Path => "SNAPSHOT_INVALID_PATH", Self::Order => "SNAPSHOT_PATH_ORDER",
            Self::Member => "SNAPSHOT_INVALID_MEMBER", Self::Missing => "SNAPSHOT_MEMBER_UNAVAILABLE",
            Self::Changed => "SNAPSHOT_MEMBER_CHANGED", Self::Io => "SNAPSHOT_READ_FAILED",
            Self::ReadCallLimit => "SNAPSHOT_READ_CALL_LIMIT", Self::InvalidReadCount => "SNAPSHOT_INVALID_READ_COUNT",
            Self::ResourceDenied => "SNAPSHOT_RESOURCE_DENIED", Self::Canceled => "SNAPSHOT_CANCELED",
        }
    }
}
impl std::fmt::Display for PagedSnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for PagedSnapshotError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SnapshotIoStats {
    pub bytes_read: u64,
    pub read_calls: u64,
    pub loaded_members: u64,
}
#[derive(Clone, Debug)]
enum Location {
    Captured { offset: u64, length: usize, digest: Sha256Digest },
    Unavailable(Range<usize>),
}
#[derive(Clone, Debug)]
struct Entry { path: Range<usize>, observed: u64, location: Location }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagedMemberData<'a> {
    Captured { archive_offset: u64, byte_length: usize, digest: Sha256Digest },
    Unavailable(&'a str),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagedMember<'a> {
    pub ordinal: usize,
    pub path: &'a [u8],
    pub observed_bytes: u64,
    pub data: PagedMemberData<'a>,
}

/// Metadata belongs to a verified envelope, not to today's live root. Ordinals
/// and raw paths do not grant filesystem access or carry process-local IDs.
pub struct SnapshotDirectory {
    owner: ArenaOwnerId,
    entries: Vec<Entry>,
    paths: Vec<u8>,
    reasons: Vec<u8>,
    policy: String,
    complete: bool,
    captured: usize,
    source_bytes: u64,
    archive_bytes: u64,
    digest: Sha256Digest,
    validation: SnapshotIoStats,
    catalog_pin: Option<Sha256Digest>,
    retained_charge: usize,
    _lease: ResourceLease,
}
impl SnapshotDirectory {
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
    pub const fn discovery_complete(&self) -> bool { self.complete }
    pub const fn captured_files(&self) -> usize { self.captured }
    pub const fn source_bytes(&self) -> u64 { self.source_bytes }
    pub const fn archive_bytes(&self) -> u64 { self.archive_bytes }
    pub const fn digest(&self) -> Sha256Digest { self.digest }
    pub fn policy(&self) -> &str { &self.policy }
    pub const fn validation_stats(&self) -> SnapshotIoStats { self.validation }
    /// Reserved metadata plus bounded validation scratch, never source payload size.
    pub const fn retained_charge(&self) -> usize { self.retained_charge }
    pub fn member(&self, ordinal: usize) -> Option<PagedMember<'_>> {
        let entry = self.entries.get(ordinal)?;
        let data = match &entry.location {
            Location::Captured { offset, length, digest } => PagedMemberData::Captured {
                archive_offset: *offset, byte_length: *length, digest: *digest },
            Location::Unavailable(range) => PagedMemberData::Unavailable(
                std::str::from_utf8(&self.reasons[range.clone()]).expect("validated reason")),
        };
        Some(PagedMember { ordinal, path: &self.paths[entry.path.clone()], observed_bytes: entry.observed, data })
    }
    pub fn members(&self) -> impl ExactSizeIterator<Item = PagedMember<'_>> {
        (0..self.entries.len()).map(|ordinal| self.member(ordinal).expect("validated ordinal"))
    }
    pub fn find_path(&self, native: &[u8]) -> Option<usize> {
        self.entries.binary_search_by(|entry| self.paths[entry.path.clone()].cmp(native)).ok()
    }
}

/// Owns the host's explicitly supplied seekable reader. File callers must admit
/// a regular file before construction. The handle is never replaced using its
/// pathname; std File therefore keeps reading its inode after pathname replacement.
/// Generic readers must honor the Read/Seek contracts; no OS deadline is invented.
pub struct PagedSnapshot<R: Read + Seek> {
    input: R,
    directory: SnapshotDirectory,
    loads: SnapshotIoStats,
}
impl<R: Read + Seek> PagedSnapshot<R> {
    pub fn open(mut input: R, owner: ArenaOwnerId, limits: SnapshotLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, PagedSnapshotError> {
        validate_limits(limits)?;
        if canceled() { return Err(PagedSnapshotError::Canceled); }
        let position = input.seek(SeekFrom::Start(0)).map_err(|_| PagedSnapshotError::Io)?;
        if position != 0 { return Err(PagedSnapshotError::Io); }
        let mut scan = Scan { input: &mut input, hash: Sha256::new(), position: 0,
            end: limits.max_document_bytes as u64, stats: SnapshotIoStats::default() };
        let mut header = [0u8; HEADER_LEN];
        scan.exact(&mut header, true, &mut canceled)?;
        if header[..4] != SNAPSHOT_SCHEMA.magic || u32::from_le_bytes(header[4..8].try_into().unwrap()) != SNAPSHOT_SCHEMA.schema_id {
            return Err(PagedSnapshotError::Format);
        }
        if u16::from_le_bytes(header[8..10].try_into().unwrap()) != SNAPSHOT_SCHEMA.major || header[10..16] != [0; 6] {
            return Err(PagedSnapshotError::Version);
        }
        let payload = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let archive_bytes = payload.checked_add(FRAME_LEN as u64).ok_or(PagedSnapshotError::Limits)?;
        if archive_bytes > limits.max_document_bytes as u64 { return Err(PagedSnapshotError::Limits); }
        scan.end = payload + HEADER_LEN as u64;
        let complete = match scan.byte(&mut canceled)? { 0 => false, 1 => true, _ => return Err(PagedSnapshotError::Member) };
        if scan.byte(&mut canceled)? != 1 { return Err(PagedSnapshotError::Version); }
        let count = usize::try_from(scan.number(&mut canceled)?).map_err(|_| PagedSnapshotError::Limits)?;
        if count > limits.max_files { return Err(PagedSnapshotError::Limits); }
        let path_capacity = count.checked_mul(limits.max_path_bytes).ok_or(PagedSnapshotError::Limits)?
            .min(limits.max_total_path_bytes);
        let reason_capacity = count.checked_mul(96).ok_or(PagedSnapshotError::Limits)?;
        let charge = count.checked_mul(size_of::<Entry>()).and_then(|n| n.checked_add(path_capacity))
            .and_then(|n| n.checked_add(reason_capacity))
            .and_then(|n| n.checked_add(size_of::<Self>() + SNAPSHOT_READ_BYTES + 128))
            .ok_or(PagedSnapshotError::Limits)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| PagedSnapshotError::ResourceDenied)?;
        let mut entries: Vec<Entry> = reserve(count)?;
        let mut paths: Vec<u8> = reserve(path_capacity)?;
        let mut reasons: Vec<u8> = reserve(reason_capacity)?;
        let mut policy_bytes = [0u8; 128];
        let policy_len = scan.length(128, &mut canceled)?;
        scan.exact(&mut policy_bytes[..policy_len], true, &mut canceled)?;
        let policy = std::str::from_utf8(&policy_bytes[..policy_len]).map_err(|_| PagedSnapshotError::Member)?;
        if policy.is_empty() || !policy.bytes().all(|b| b.is_ascii_graphic()) { return Err(PagedSnapshotError::Member); }
        let policy = policy.to_owned();
        let mut scratch = [0u8; SNAPSHOT_READ_BYTES];
        let mut source_bytes = 0u64;
        let mut captured = 0usize;
        for _ in 0..count {
            if canceled() { return Err(PagedSnapshotError::Canceled); }
            let path_len = scan.length(limits.max_path_bytes, &mut canceled)?;
            let start = paths.len();
            let end = start.checked_add(path_len).ok_or(PagedSnapshotError::Limits)?;
            if end > path_capacity { return Err(PagedSnapshotError::Limits); }
            paths.resize(end, 0);
            scan.exact(&mut paths[start..end], true, &mut canceled)?;
            let path = &paths[start..end];
            if path.is_empty() || path.contains(&0)
                || path.split(|&b| b == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
                return Err(PagedSnapshotError::Path);
            }
            if entries.last().is_some_and(|previous| &paths[previous.path.clone()] >= path) {
                return Err(PagedSnapshotError::Order);
            }
            let observed = scan.number(&mut canceled)?;
            let tag = scan.byte(&mut canceled)?;
            let length = scan.length(if tag == 1 { limits.max_file_bytes } else { 96 }, &mut canceled)?;
            let location = match tag {
                1 => {
                    if length as u64 != observed { return Err(PagedSnapshotError::Member); }
                    source_bytes = source_bytes.checked_add(observed).ok_or(PagedSnapshotError::Limits)?;
                    if source_bytes > limits.max_source_bytes as u64 { return Err(PagedSnapshotError::Limits); }
                    let offset = scan.position;
                    let mut hash = Sha256::new();
                    let mut remaining = length;
                    while remaining > 0 {
                        let n = remaining.min(scratch.len());
                        scan.exact(&mut scratch[..n], true, &mut canceled)?;
                        hash.update(&scratch[..n]);
                        remaining -= n;
                    }
                    captured += 1;
                    Location::Captured { offset, length, digest: hash.finalize() }
                }
                0 => {
                    if length == 0 { return Err(PagedSnapshotError::Member); }
                    let start = reasons.len(); let end = start + length;
                    if end > reason_capacity { return Err(PagedSnapshotError::Limits); }
                    reasons.resize(end, 0);
                    scan.exact(&mut reasons[start..end], true, &mut canceled)?;
                    if !reasons[start..end].iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_') {
                        return Err(PagedSnapshotError::Member);
                    }
                    Location::Unavailable(start..end)
                }
                _ => return Err(PagedSnapshotError::Member),
            };
            entries.push(Entry { path: start..end, observed, location });
        }
        if scan.position != scan.end { return Err(PagedSnapshotError::Format); }
        scan.end = archive_bytes;
        let mut checksum = [0u8; CHECKSUM_LEN];
        scan.exact(&mut checksum, false, &mut canceled)?;
        scan.eof(&mut canceled)?;
        let stats = scan.stats;
        let digest = scan.hash.finalize();
        if digest.as_bytes() != &checksum { return Err(PagedSnapshotError::Checksum); }
        if canceled() { return Err(PagedSnapshotError::Canceled); }
        let directory = SnapshotDirectory { owner, entries, paths, reasons, policy, complete, captured,
            source_bytes, archive_bytes, digest, validation: stats, catalog_pin: None, retained_charge: charge, _lease: lease };
        Ok(Self { input, directory, loads: SnapshotIoStats::default() })
    }
    pub fn directory(&self) -> &SnapshotDirectory { &self.directory }
    pub const fn load_stats(&self) -> SnapshotIoStats { self.loads }

    /// Load ONE bounded member. SHA-256 is checked on the bytes being returned,
    /// not on a prior pathname/metadata observation. Failure releases candidate
    /// memory and returns no partial bytes. Canceled reads may be retried by an
    /// explicit new call; each starts at the verified saved offset.
    pub fn load(&mut self, ordinal: usize, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<VerifiedMember, PagedSnapshotError> {
        let member = self.directory.member(ordinal).ok_or(PagedSnapshotError::Missing)?;
        let PagedMemberData::Captured { archive_offset, byte_length, digest } = member.data else {
            return Err(PagedSnapshotError::Missing);
        };
        if canceled() { return Err(PagedSnapshotError::Canceled); }
        let charge = byte_length.checked_add(size_of::<VerifiedMember>()).ok_or(PagedSnapshotError::Limits)?;
        let lease = budget.try_reserve_managed(self.directory.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| PagedSnapshotError::ResourceDenied)?;
        let mut bytes = reserve(byte_length)?; bytes.resize(byte_length, 0);
        if self.input.seek(SeekFrom::Start(archive_offset)).map_err(|_| PagedSnapshotError::Io)? != archive_offset {
            return Err(PagedSnapshotError::Io);
        }
        let mut scan = Scan { input: &mut self.input, hash: Sha256::new(), position: archive_offset,
            end: archive_offset + byte_length as u64, stats: SnapshotIoStats::default() };
        let result = scan.exact(&mut bytes, true, &mut canceled);
        self.loads.bytes_read = self.loads.bytes_read.saturating_add(scan.stats.bytes_read);
        self.loads.read_calls = self.loads.read_calls.saturating_add(scan.stats.read_calls);
        result?;
        if scan.hash.finalize() != digest { return Err(PagedSnapshotError::Changed); }
        if canceled() { return Err(PagedSnapshotError::Canceled); }
        self.loads.loaded_members = self.loads.loaded_members.saturating_add(1);
        Ok(VerifiedMember { bytes, ordinal, archive: self.directory.digest, digest, _lease: lease })
    }
}

/// Bytes and their lease travel together, including when moved into a Cursor
/// for the existing streaming search engine. No uncharged owned-byte escape.
pub struct VerifiedMember {
    bytes: Vec<u8>, ordinal: usize, archive: Sha256Digest, digest: Sha256Digest, _lease: ResourceLease,
}
impl VerifiedMember {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub const fn ordinal(&self) -> usize { self.ordinal }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn source_digest(&self) -> Sha256Digest { self.digest }
}
impl AsRef<[u8]> for VerifiedMember { fn as_ref(&self) -> &[u8] { self.bytes() } }

struct Scan<'a, R> { input: &'a mut R, hash: Sha256, position: u64, end: u64, stats: SnapshotIoStats }
impl<R: Read> Scan<'_, R> {
    fn read(&mut self, into: &mut [u8], canceled: &mut impl FnMut() -> bool) -> Result<usize, PagedSnapshotError> {
        loop {
            if canceled() { return Err(PagedSnapshotError::Canceled); }
            if self.stats.read_calls >= MAX_SNAPSHOT_READ_CALLS { return Err(PagedSnapshotError::ReadCallLimit); }
            self.stats.read_calls += 1;
            match self.input.read(into) {
                Ok(n) if n <= into.len() => { self.stats.bytes_read += n as u64; return Ok(n); }
                Ok(_) => return Err(PagedSnapshotError::InvalidReadCount),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
                Err(_) => return Err(PagedSnapshotError::Io),
            }
        }
    }
    fn exact(&mut self, mut into: &mut [u8], hash: bool, canceled: &mut impl FnMut() -> bool) -> Result<(), PagedSnapshotError> {
        if into.len() as u64 > self.end.saturating_sub(self.position) { return Err(PagedSnapshotError::Format); }
        while !into.is_empty() {
            let length = into.len().min(SNAPSHOT_READ_BYTES);
            let n = self.read(&mut into[..length], canceled)?;
            if n == 0 { return Err(PagedSnapshotError::Format); }
            if hash { self.hash.update(&into[..n]); }
            self.position += n as u64;
            into = &mut into[n..];
        }
        Ok(())
    }
    fn byte(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<u8, PagedSnapshotError> {
        let mut byte = [0]; self.exact(&mut byte, true, canceled)?; Ok(byte[0])
    }
    fn number(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<u64, PagedSnapshotError> {
        let mut bytes = [0; 8]; self.exact(&mut bytes, true, canceled)?; Ok(u64::from_le_bytes(bytes))
    }
    fn length(&mut self, maximum: usize, canceled: &mut impl FnMut() -> bool) -> Result<usize, PagedSnapshotError> {
        let n = self.number(canceled)?;
        if n > maximum as u64 || n > self.end.saturating_sub(self.position) { return Err(PagedSnapshotError::Limits); }
        usize::try_from(n).map_err(|_| PagedSnapshotError::Limits)
    }
    fn eof(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), PagedSnapshotError> {
        if self.read(&mut [0], canceled)? != 0 { return Err(PagedSnapshotError::Format); }
        Ok(())
    }
}
fn validate_limits(l: SnapshotLimits) -> Result<(), PagedSnapshotError> {
    if l.max_files > MAX_SNAPSHOT_FILES || l.max_path_bytes > 16_384
        || l.max_document_bytes > MAX_SNAPSHOT_BYTES || l.max_document_bytes < FRAME_LEN
        || l.max_source_bytes > MAX_SNAPSHOT_BYTES || l.max_total_path_bytes > MAX_SNAPSHOT_BYTES
        || l.max_file_bytes > MAX_SNAPSHOT_BYTES { return Err(PagedSnapshotError::Limits); }
    Ok(())
}
fn reserve<T>(count: usize) -> Result<Vec<T>, PagedSnapshotError> {
    let mut v = Vec::new(); v.try_reserve_exact(count).map_err(|_| PagedSnapshotError::ResourceDenied)?;
    if v.capacity() > count { return Err(PagedSnapshotError::ResourceDenied); } Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData, SnapshotView};
    use std::io::Cursor;
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1751).unwrap() }
    fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
    fn budget(n: u64) -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(n)).unwrap() }
    fn encode(entries: &[SnapshotEntry<'_>], complete: bool) -> Vec<u8> {
        let b = budget(128 * 1024 * 1024);
        SnapshotBytes::encode(owner(), complete, "test-v1", entries, SnapshotLimits::default(), &b, id(1), || false).unwrap().bytes().to_vec()
    }
    #[test]
    fn metadata_and_loaded_members_match_zero_copy_decoder_including_raw_names() {
        let entries = [SnapshotEntry { path: b"a\\\xff", observed_bytes: 3, data: SnapshotData::Captured(b"a\xffz") },
            SnapshotEntry { path: b"empty", observed_bytes: 0, data: SnapshotData::Captured(b"") },
            SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") }];
        for complete in [true, false] {
            let bytes = encode(&entries, complete); let b = budget(1024 * 1024);
            let view = SnapshotView::open(&bytes, SnapshotLimits::default(), || false).unwrap();
            let mut paged = PagedSnapshot::open(Cursor::new(bytes.as_slice()), owner(), SnapshotLimits::default(), &b, id(1), || false).unwrap();
            assert_eq!(paged.directory().digest(), view.digest());
            assert_eq!(paged.directory().discovery_complete(), complete);
            assert_eq!(paged.directory().source_bytes(), 3);
            for (i, entry) in entries.iter().enumerate() {
                assert_eq!(paged.directory().find_path(entry.path), Some(i));
                assert_eq!(paged.directory().member(i).unwrap().observed_bytes, entry.observed_bytes);
                match entry.data {
                    SnapshotData::Captured(bytes) => assert_eq!(paged.load(i, &b, id(2), || false).unwrap().bytes(), bytes),
                    SnapshotData::Unavailable(reason) => {
                        assert_eq!(paged.directory().member(i).unwrap().data, PagedMemberData::Unavailable(reason));
                        assert!(matches!(paged.load(i, &b, id(2), || false), Err(PagedSnapshotError::Missing)));
                    }
                }
            }
            drop(paged); assert_eq!(b.accounting().reserved().get(), 0);
        }
    }
    #[test]
    fn archive_payload_size_does_not_enter_the_directory_allocation_formula() {
        let mut charges = Vec::new();
        for size in [16, 1024 * 1024] {
            let source = vec![b'x'; size];
            let bytes = encode(&[SnapshotEntry { path: b"a", observed_bytes: size as u64, data: SnapshotData::Captured(&source) }], true);
            let b = budget(128 * 1024);
            let paged = PagedSnapshot::open(Cursor::new(bytes.as_slice()), owner(), SnapshotLimits::default(), &b, id(1), || false).unwrap();
            assert_eq!(paged.directory().validation_stats().bytes_read, bytes.len() as u64);
            assert!(paged.directory().retained_charge() < 128 * 1024);
            charges.push(b.accounting().reserved().get());
        }
        assert_eq!(charges[0], charges[1]);
    }
    #[test]
    fn digest_revalidation_refuses_modified_member_without_discarding_old_captures() {
        let bytes = encode(&[SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Captured(b"banana") }], true);
        let b = budget(1024 * 1024);
        let mut paged = PagedSnapshot::open(Cursor::new(bytes), owner(), SnapshotLimits::default(), &b, id(1), || false).unwrap();
        let old = paged.load(0, &b, id(2), || false).unwrap();
        let PagedMemberData::Captured { archive_offset, .. } = paged.directory().member(0).unwrap().data else { panic!() };
        paged.input.get_mut()[archive_offset as usize] ^= 1;
        let before = b.accounting().reserved();
        assert!(matches!(paged.load(0, &b, id(3), || false), Err(PagedSnapshotError::Changed)));
        assert_eq!(b.accounting().reserved(), before);
        assert_eq!(old.bytes(), b"banana");
    }
    #[test]
    fn corruption_and_truncation_are_refused_before_directory_publication() {
        let bytes = encode(&[SnapshotEntry { path: b"a", observed_bytes: 2, data: SnapshotData::Captured(b"hi") }], true);
        let b = budget(1024 * 1024);
        for i in 0..bytes.len() {
            let mut corrupt = bytes.clone(); corrupt[i] ^= 1;
            assert!(PagedSnapshot::open(Cursor::new(corrupt), owner(), SnapshotLimits::default(), &b, id(1), || false).is_err());
            assert!(PagedSnapshot::open(Cursor::new(&bytes[..i]), owner(), SnapshotLimits::default(), &b, id(1), || false).is_err());
            assert_eq!(b.accounting().reserved().get(), 0);
        }
    }
    #[test]
    fn cancellation_and_denial_return_no_partial_directory_or_member() {
        let bytes = encode(&[], true); let b = budget(1024 * 1024);
        assert!(matches!(PagedSnapshot::open(Cursor::new(&bytes), owner(), SnapshotLimits::default(), &b, id(1),
            || b.accounting().reserved().get() > 0), Err(PagedSnapshotError::Canceled)));
        assert_eq!(b.accounting().reserved().get(), 0);
        let tiny = budget(1);
        assert!(matches!(PagedSnapshot::open(Cursor::new(&bytes), owner(), SnapshotLimits::default(), &tiny, id(1), || false), Err(PagedSnapshotError::ResourceDenied)));
    }
    #[test]
    fn short_reads_are_bounded_and_do_not_change_the_digest() {
        struct Short(Cursor<Vec<u8>>);
        impl Read for Short {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> { let n = b.len().min(3); self.0.read(&mut b[..n]) }
        }
        impl Seek for Short { fn seek(&mut self, p: SeekFrom) -> io::Result<u64> { self.0.seek(p) } }
        let bytes = encode(&[SnapshotEntry { path: b"a", observed_bytes: 6, data: SnapshotData::Captured(b"banana") }], true);
        let b = budget(1024 * 1024);
        let mut paged = PagedSnapshot::open(Short(Cursor::new(bytes)), owner(), SnapshotLimits::default(), &b, id(1), || false).unwrap();
        assert_eq!(paged.load(0, &b, id(2), || false).unwrap().bytes(), b"banana");
        assert!(paged.directory().validation_stats().read_calls > 10);
    }
}
