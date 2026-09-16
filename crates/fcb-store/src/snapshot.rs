#![forbid(unsafe_code)]

//! Portable, explicit exports of an observed source universe. This is not the
//! mutable metadata database, a cache manifest, an authorization token, or an
//! atomic-filesystem claim. Reuses the existing canonical envelope and SHA-256.
//! All paths are root-relative native bytes; no host root pathname is persisted.
//! Unavailable members remain members, distinct from captured empty files.

use std::mem::size_of;
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::{EnvelopeLimits, EnvelopeReader, EnvelopeSchema, EnvelopeWriter, Sha256Digest,
    UnknownPolicy, CHECKSUM_LEN, FRAME_LEN};

pub const SNAPSHOT_SCHEMA: EnvelopeSchema = EnvelopeSchema::with_magic(*b"FCBS", 0x53524331, 1, 0);
pub const MAX_SNAPSHOT_BYTES: usize = 80 * 1024 * 1024;
pub const MAX_SNAPSHOT_FILES: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotLimits {
    pub max_files: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
    pub max_file_bytes: usize,
    pub max_source_bytes: usize,
    pub max_document_bytes: usize,
}
impl Default for SnapshotLimits {
    fn default() -> Self {
        Self { max_files: MAX_SNAPSHOT_FILES, max_path_bytes: 2048,
            max_total_path_bytes: 8 * 1024 * 1024, max_file_bytes: 1024 * 1024,
            max_source_bytes: 64 * 1024 * 1024, max_document_bytes: MAX_SNAPSHOT_BYTES }
    }
}
impl SnapshotLimits {
    fn validate(self) -> Result<(), SnapshotError> {
        if self.max_files > MAX_SNAPSHOT_FILES || self.max_path_bytes > 16_384
            || self.max_document_bytes > MAX_SNAPSHOT_BYTES || self.max_document_bytes < FRAME_LEN
            || self.max_source_bytes > MAX_SNAPSHOT_BYTES || self.max_total_path_bytes > MAX_SNAPSHOT_BYTES
            || self.max_file_bytes > MAX_SNAPSHOT_BYTES { return Err(SnapshotError::Limit); }
        Ok(())
    }
    fn envelope(self) -> EnvelopeLimits {
        EnvelopeLimits { max_document_bytes: self.max_document_bytes,
            max_payload_bytes: self.max_document_bytes - FRAME_LEN,
            max_field_bytes: self.max_file_bytes.max(self.max_path_bytes).max(128) }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError { Limit, InvalidPath, InvalidPolicy, InvalidMember, Order,
    Envelope, UnsupportedVersion, ResourceDenied, Canceled }
impl SnapshotError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limit => "SNAPSHOT_LIMIT", Self::InvalidPath => "SNAPSHOT_INVALID_PATH",
            Self::InvalidPolicy => "SNAPSHOT_INVALID_POLICY", Self::InvalidMember => "SNAPSHOT_INVALID_MEMBER",
            Self::Order => "SNAPSHOT_PATH_ORDER", Self::Envelope => "SNAPSHOT_INVALID_ENVELOPE",
            Self::UnsupportedVersion => "SNAPSHOT_UNSUPPORTED_VERSION",
            Self::ResourceDenied => "SNAPSHOT_RESOURCE_DENIED", Self::Canceled => "SNAPSHOT_CANCELED",
        }
    }
}
impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for SnapshotError {}
impl From<crate::EnvelopeError> for SnapshotError {
    fn from(_: crate::EnvelopeError) -> Self { Self::Envelope }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotData<'a> { Captured(&'a [u8]), Unavailable(&'a str) }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotEntry<'a> {
    pub path: &'a [u8],
    pub observed_bytes: u64,
    pub data: SnapshotData<'a>,
}

/// An encoded export holds its own reservation until the bytes are released.
/// No operation writes to the filesystem or publishes to an external receiver.
pub struct SnapshotBytes {
    bytes: Vec<u8>,
    _lease: ResourceLease,
}
impl SnapshotBytes {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub fn digest(&self) -> Sha256Digest {
        Sha256Digest::new(self.bytes[self.bytes.len() - CHECKSUM_LEN..].try_into().expect("validated envelope"))
    }

    /// Two bounded passes, first validating all lengths/order before building.
    /// The reservation conservatively covers the existing envelope writer's
    /// growing payload and final-document overlap. This is explicit worker work.
    pub fn encode(owner: ArenaOwnerId, discovery_complete: bool, policy: &str,
        entries: &[SnapshotEntry<'_>], limits: SnapshotLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotError> {
        limits.validate()?;
        validate_policy(policy)?;
        if entries.len() > limits.max_files { return Err(SnapshotError::Limit); }
        let mut check = Validation::new(limits);
        let mut length = FRAME_LEN + 2 + 8 + 8 + policy.len();
        for entry in entries {
            if canceled() { return Err(SnapshotError::Canceled); }
            check.entry(*entry)?;
            let data_len = match entry.data { SnapshotData::Captured(bytes) => bytes.len(), SnapshotData::Unavailable(reason) => reason.len() };
            length = length.checked_add(8 + entry.path.len() + 8 + 1 + 8)
                .and_then(|n| n.checked_add(data_len)).ok_or(SnapshotError::Limit)?;
        }
        if length > limits.max_document_bytes { return Err(SnapshotError::Limit); }
        if canceled() { return Err(SnapshotError::Canceled); }
        let charge = length.checked_mul(4).and_then(|n| n.checked_add(size_of::<Self>()))
            .ok_or(SnapshotError::Limit)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| SnapshotError::ResourceDenied)?;
        let mut writer = EnvelopeWriter::new(SNAPSHOT_SCHEMA);
        writer.put_u8(u8::from(discovery_complete));
        writer.put_u8(1); // Native Unix relative path bytes, not Unicode or a URI.
        writer.put_u64(entries.len() as u64);
        writer.put_str(policy);
        for entry in entries {
            if canceled() { return Err(SnapshotError::Canceled); }
            writer.put_bytes(entry.path);
            writer.put_u64(entry.observed_bytes);
            match entry.data {
                SnapshotData::Captured(bytes) => { writer.put_u8(1); writer.put_bytes(bytes); }
                SnapshotData::Unavailable(reason) => { writer.put_u8(0); writer.put_str(reason); }
            }
        }
        let bytes = writer.finish();
        if bytes.len() != length || bytes.capacity() > charge { return Err(SnapshotError::ResourceDenied); }
        if canceled() { return Err(SnapshotError::Canceled); }
        Ok(Self { bytes, _lease: lease })
    }
}

/// Checked, zero-copy archive view. Input ownership (and its resource charge)
/// belongs to the caller. Validation scans bounded input before exposing any
/// members. A valid checksum grants no access to a live source root.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotView<'a> {
    members: EnvelopeReader<'a>,
    count: usize,
    discovery_complete: bool,
    policy: &'a str,
    source_bytes: usize,
    path_bytes: usize,
    captured: usize,
    digest: Sha256Digest,
}
impl<'a> SnapshotView<'a> {
    pub fn open(bytes: &'a [u8], limits: SnapshotLimits, mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotError> {
        limits.validate()?;
        if canceled() { return Err(SnapshotError::Canceled); }
        if bytes.len() < FRAME_LEN || bytes.len() > limits.max_document_bytes { return Err(SnapshotError::Limit); }
        // Validate full-width length before the compatibility envelope reader's
        // usize conversion, including on a 32-bit host. This schema has no flags.
        let payload_len = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| SnapshotError::Envelope)?))
            .map_err(|_| SnapshotError::Limit)?;
        if payload_len != bytes.len() - FRAME_LEN { return Err(SnapshotError::Envelope); }
        if bytes[10..16] != [0; 6] { return Err(SnapshotError::UnsupportedVersion); }
        let mut reader = EnvelopeReader::open(bytes, SNAPSHOT_SCHEMA, limits.envelope(), UnknownPolicy::Strict)?;
        let discovery_complete = match reader.get_u8()? { 0 => false, 1 => true, _ => return Err(SnapshotError::InvalidMember) };
        if reader.get_u8()? != 1 { return Err(SnapshotError::UnsupportedVersion); }
        let count = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotError::Limit)?;
        if count > limits.max_files { return Err(SnapshotError::Limit); }
        let policy = std::str::from_utf8(field(&mut reader)?).map_err(|_| SnapshotError::InvalidPolicy)?;
        validate_policy(policy)?;
        let members = reader;
        let mut check = Validation::new(limits);
        for _ in 0..count {
            if canceled() { return Err(SnapshotError::Canceled); }
            check.entry(read_entry(&mut reader)?)?;
        }
        reader.finish()?;
        if canceled() { return Err(SnapshotError::Canceled); }
        Ok(Self { members, count, discovery_complete, policy, source_bytes: check.sources,
            path_bytes: check.paths, captured: check.captured,
            digest: Sha256Digest::new(bytes[bytes.len() - CHECKSUM_LEN..].try_into().map_err(|_| SnapshotError::Envelope)?) })
    }
    pub const fn len(self) -> usize { self.count }
    pub const fn is_empty(self) -> bool { self.count == 0 }
    pub const fn discovery_complete(self) -> bool { self.discovery_complete }
    pub const fn policy(self) -> &'a str { self.policy }
    pub const fn source_bytes(self) -> usize { self.source_bytes }
    pub const fn path_bytes(self) -> usize { self.path_bytes }
    pub const fn captured_files(self) -> usize { self.captured }
    pub const fn digest(self) -> Sha256Digest { self.digest }
    pub fn entries(self) -> impl ExactSizeIterator<Item = Result<SnapshotEntry<'a>, SnapshotError>> {
        let mut reader = self.members;
        (0..self.count).map(move |_| read_entry(&mut reader))
    }
}

fn field<'a>(reader: &mut EnvelopeReader<'a>) -> Result<&'a [u8], SnapshotError> {
    let mut peek = *reader;
    let length = usize::try_from(peek.get_u64()?).map_err(|_| SnapshotError::Limit)?;
    if length > peek.remaining() { return Err(SnapshotError::Envelope); }
    Ok(reader.get_bytes()?)
}
fn read_entry<'a>(reader: &mut EnvelopeReader<'a>) -> Result<SnapshotEntry<'a>, SnapshotError> {
    let path = field(reader)?;
    let observed_bytes = reader.get_u64()?;
    let tag = reader.get_u8()?;
    let payload = field(reader)?;
    let data = match tag {
        1 => SnapshotData::Captured(payload),
        0 => SnapshotData::Unavailable(std::str::from_utf8(payload).map_err(|_| SnapshotError::InvalidMember)?),
        _ => return Err(SnapshotError::InvalidMember),
    };
    Ok(SnapshotEntry { path, observed_bytes, data })
}
fn validate_policy(policy: &str) -> Result<(), SnapshotError> {
    if policy.is_empty() || policy.len() > 128 || !policy.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(SnapshotError::InvalidPolicy);
    }
    Ok(())
}
struct Validation<'a> {
    limits: SnapshotLimits,
    previous: Option<&'a [u8]>,
    paths: usize,
    sources: usize,
    captured: usize,
}
impl<'a> Validation<'a> {
    fn new(limits: SnapshotLimits) -> Self { Self { limits, previous: None, paths: 0, sources: 0, captured: 0 } }
    fn entry(&mut self, entry: SnapshotEntry<'a>) -> Result<(), SnapshotError> {
        if entry.path.is_empty() || entry.path.contains(&0) || entry.path.len() > self.limits.max_path_bytes
            || entry.path.split(|&b| b == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
            return Err(SnapshotError::InvalidPath);
        }
        if self.previous.is_some_and(|path| path >= entry.path) { return Err(SnapshotError::Order); }
        self.previous = Some(entry.path);
        self.paths = self.paths.checked_add(entry.path.len()).ok_or(SnapshotError::Limit)?;
        if self.paths > self.limits.max_total_path_bytes { return Err(SnapshotError::Limit); }
        match entry.data {
            SnapshotData::Captured(bytes) => {
                if bytes.len() as u64 != entry.observed_bytes { return Err(SnapshotError::InvalidMember); }
                if bytes.len() > self.limits.max_file_bytes { return Err(SnapshotError::Limit); }
                self.sources = self.sources.checked_add(bytes.len()).ok_or(SnapshotError::Limit)?;
                if self.sources > self.limits.max_source_bytes { return Err(SnapshotError::Limit); }
                self.captured += 1;
            }
            SnapshotData::Unavailable(reason) => {
                if reason.is_empty() || reason.len() > 96
                    || !reason.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
                    return Err(SnapshotError::InvalidMember);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sha256;
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(721).unwrap() }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
    fn encode<'a>(entries: &[SnapshotEntry<'a>], complete: bool, budget: &ResourceBudget) -> Result<SnapshotBytes, SnapshotError> {
        SnapshotBytes::encode(owner(), complete, "test-policy-v1", entries, SnapshotLimits::default(), budget,
            ResourceAllocationId::new(1).unwrap(), || false)
    }
    #[test]
    fn roundtrip_keeps_bytes_missing_members_empty_files_and_raw_paths() {
        let entries = [SnapshotEntry { path: b"a\\b.rs", observed_bytes: 3, data: SnapshotData::Captured(b"x\xffz") },
            SnapshotEntry { path: b"empty", observed_bytes: 0, data: SnapshotData::Captured(b"") },
            SnapshotEntry { path: b"missing", observed_bytes: u64::MAX, data: SnapshotData::Unavailable("SOURCE_UNAVAILABLE") },
            SnapshotEntry { path: b"\xff", observed_bytes: 2, data: SnapshotData::Captured(&[0xff, 0xfe]) }];
        for complete in [false, true] {
            let budget = budget(); let encoded = encode(&entries, complete, &budget).unwrap();
            let view = SnapshotView::open(encoded.bytes(), SnapshotLimits::default(), || false).unwrap();
            assert_eq!(view.entries().collect::<Result<Vec<_>, _>>().unwrap(), entries);
            assert_eq!(view.discovery_complete(), complete); assert_eq!(view.captured_files(), 3);
            assert_eq!(view.source_bytes(), 5); assert_eq!(view.digest(), encoded.digest());
            drop(encoded); assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }
    #[test]
    fn encoding_is_deterministic_and_checksum_catches_all_single_byte_mutations() {
        let budget = budget();
        let entries = [SnapshotEntry { path: b"a", observed_bytes: 3, data: SnapshotData::Captured(b"abc") }];
        let first = encode(&entries, true, &budget).unwrap();
        let expected = first.bytes().to_vec(); drop(first);
        let second = encode(&entries, true, &budget).unwrap(); assert_eq!(expected, second.bytes());
        for index in 0..expected.len() {
            let mut broken = expected.clone(); broken[index] ^= 1;
            assert!(SnapshotView::open(&broken, SnapshotLimits::default(), || false).is_err(), "byte {index}");
        }
        for end in 0..expected.len() { assert!(SnapshotView::open(&expected[..end], SnapshotLimits::default(), || false).is_err()); }
    }
    #[test]
    fn valid_checksum_does_not_approve_unknown_schema_flags_counts_or_trailing_data() {
        let budget = budget(); let encoded = encode(&[], true, &budget).unwrap();
        for (offset, value) in [(4, 0xff), (8, 2), (10, 1), (12, 1), (24, 2), (25, 2), (26, 255)] {
            let mut bytes = encoded.bytes().to_vec(); bytes[offset] = value;
            let end = bytes.len() - CHECKSUM_LEN;
            let sum = Sha256::digest(&bytes[..end]); bytes[end..].copy_from_slice(sum.as_bytes());
            assert!(SnapshotView::open(&bytes, SnapshotLimits::default(), || false).is_err(), "offset {offset}");
        }
        let mut bytes = encoded.bytes().to_vec(); bytes.push(0);
        assert!(SnapshotView::open(&bytes, SnapshotLimits::default(), || false).is_err());
    }
    #[test]
    fn hostile_paths_duplicate_members_and_length_mismatch_are_not_serialized() {
        let budget = budget();
        for path in [b"".as_slice(), b"/root", b"a/../b", b"a//b", b"a\0b"] {
            assert!(encode(&[SnapshotEntry { path, observed_bytes: 0, data: SnapshotData::Captured(b"") }], true, &budget).is_err());
        }
        let entry = SnapshotEntry { path: b"a", observed_bytes: 0, data: SnapshotData::Captured(b"") };
        assert!(matches!(encode(&[entry, entry], true, &budget), Err(SnapshotError::Order)));
        assert!(matches!(encode(&[SnapshotEntry { observed_bytes: 3, ..entry }], true, &budget), Err(SnapshotError::InvalidMember)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    #[test]
    fn admission_and_cancellation_release_the_candidate() {
        let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
        assert!(matches!(encode(&[], true, &tiny), Err(SnapshotError::ResourceDenied)));
        let budget = budget();
        let result = SnapshotBytes::encode(owner(), true, "test-v1", &[], SnapshotLimits::default(), &budget,
            ResourceAllocationId::new(1).unwrap(), || budget.accounting().reserved().get() > 0);
        assert!(matches!(result, Err(SnapshotError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
