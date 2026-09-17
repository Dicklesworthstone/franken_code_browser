#![forbid(unsafe_code)]

//! Reusable FCBS-bound substring segments produced by the existing index engine.
//! No matcher, decoder, source grant, database, implicit filesystem access or
//! environment lookup is added here. Querying still verifies candidate bytes.
//!
//! TRUST BOUNDARY: a disk checksum proves internal consistency, not that an
//! attacker listed every gram. `decode_pinned` therefore requires the digest of
//! an artifact previously built/accepted by a trusted host, supplied separately
//! from the untrusted index. NEVER derive that expected digest from the index
//! being opened. A self-consistent forged omission cannot acquire a negative
//! certificate merely by recomputing its embedded checksum. An untrusted index
//! without such a pin must be rebuilt from its independently validated snapshot.

mod refresh;
mod postings;
pub use refresh::{RefreshStats, SnapshotRefresh};
pub use postings::{SnapshotPostings, PostingCandidates, PostingProbeStats, PostingStep,
    POSTINGS_SCHEMA, MAX_POSTINGS_BYTES};

use std::{io::{Read, Seek}, mem::size_of, sync::Arc};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, ResourceAllocationId, ResourceBudget,
    ResourceLease, SourceRevision};
use fcb_store::{EnvelopeLimits, EnvelopeReader, EnvelopeSchema, EnvelopeWriter, Sha256,
    Sha256Digest, UnknownPolicy, FRAME_LEN};
use super::{CaptureRequest, CompleteCapture, EphemeralIndex, IndexError, IndexLimits,
    ManifestLimits, MembershipState, SearchDocument, SearchManifest, SearchManifestId,
    SegmentCoverage};
use super::paged_snapshot::{PagedMemberData, PagedSnapshot, PagedSnapshotError, SnapshotDirectory};

pub const INDEX_SCHEMA: EnvelopeSchema = EnvelopeSchema::with_magic(*b"FCBI", 0x504f5331, 1, 0);
pub const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INDEX_GRAMS: usize = 2 * 1024 * 1024;
pub const MAX_INDEX_MEMBERS: usize = 65_536;
const SEMANTICS: u32 = 1; // Raw contiguous byte trigrams, sorted unique u24 in u32.
const ROW_BYTES: usize = 1 + 8 + 32 + 8;
const PREFIX_BYTES: usize = 32 + 4 + 8 + 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotIndexError {
    Limits, Format, Version, PinMismatch, SourceMismatch, OwnerMismatch,
    ResourceDenied, Canceled, Build(IndexError), Archive(PagedSnapshotError),
}
impl std::fmt::Display for SnapshotIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(error) => write!(f, "{error}"), Self::Archive(error) => write!(f, "{error}"),
            _ => f.write_str(match self {
                Self::Limits => "SAVED_INDEX_LIMIT", Self::Format => "SAVED_INDEX_FORMAT",
                Self::Version => "SAVED_INDEX_VERSION", Self::PinMismatch => "SAVED_INDEX_PIN_MISMATCH",
                Self::SourceMismatch => "SAVED_INDEX_SOURCE_MISMATCH", Self::OwnerMismatch => "SAVED_INDEX_OWNER_MISMATCH",
                Self::ResourceDenied => "SAVED_INDEX_RESOURCE_DENIED", Self::Canceled => "SAVED_INDEX_CANCELED",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for SnapshotIndexError {}
impl From<IndexError> for SnapshotIndexError { fn from(error: IndexError) -> Self { Self::Build(error) } }
impl From<PagedSnapshotError> for SnapshotIndexError { fn from(error: PagedSnapshotError) -> Self { Self::Archive(error) } }
impl From<fcb_store::EnvelopeError> for SnapshotIndexError {
    fn from(_: fcb_store::EnvelopeError) -> Self { Self::Format }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexDecision {
    /// A complete, compatible segment proves the literal cannot occur.
    Excluded,
    /// Compatible segment; still requires exact verification, not a hit.
    Verify,
    /// Missing/limited/incompatible indexing must not narrow the source universe.
    Fallback,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SavedIndexStats {
    pub members: usize,
    pub indexed_files: usize,
    pub uncovered_files: usize,
    pub unavailable_files: usize,
    pub unique_grams: usize,
    pub build_source_bytes: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Coverage { Unavailable, Uncovered, Utf8, Bytes }
impl Coverage {
    fn tag(self) -> u8 { match self { Self::Unavailable => 0, Self::Uncovered => 1, Self::Utf8 => 2, Self::Bytes => 3 } }
    fn parse(tag: u8) -> Result<Self, SnapshotIndexError> {
        match tag { 0 => Ok(Self::Unavailable), 1 => Ok(Self::Uncovered), 2 => Ok(Self::Utf8),
            3 => Ok(Self::Bytes), _ => Err(SnapshotIndexError::Format) }
    }
    fn indexed(self) -> bool { matches!(self, Self::Utf8 | Self::Bytes) }
}
#[derive(Clone, Debug)]
struct Row { coverage: Coverage, length: u64, digest: Sha256Digest, start: usize, count: usize }

/// Immutable per-file segments. Metadata probes are O(files); `invert` builds
/// an independently owned global posting table for selective repeated queries.
/// Source payloads are never retained here.
pub struct SnapshotIndex {
    owner: ArenaOwnerId,
    archive: Sha256Digest,
    rows: Vec<Row>,
    grams: Vec<u32>,
    stats: SavedIndexStats,
    _lease: ResourceLease,
}
impl SnapshotIndex {
    /// Build from digest-verified members, one at a time. Quota refusal produces
    /// an uncovered record rather than removing a file. IDs here are private
    /// temporary index IDs, not exported/persisted identities or source handles.
    /// allocations = [retained index, member load, capture copy, engine scratch].
    pub fn build<R: Read + Seek>(archive: &mut PagedSnapshot<R>, limits: IndexLimits,
        budget: &ResourceBudget, allocations: [ResourceAllocationId; 4],
        canceled: impl FnMut() -> bool) -> Result<Self, SnapshotIndexError> {
        refresh::construct(archive, limits, budget, allocations, None, canceled)
            .map(SnapshotRefresh::into_index)
    }
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn stats(&self) -> SavedIndexStats { self.stats }
    pub fn validate_directory(&self, directory: &SnapshotDirectory) -> Result<(), SnapshotIndexError> {
        if self.owner != directory.owner() { return Err(SnapshotIndexError::OwnerMismatch); }
        if self.archive != directory.digest() || self.rows.len() != directory.len() { return Err(SnapshotIndexError::SourceMismatch); }
        Ok(())
    }
    /// Exact UTF-8 only. UTF-16, malformed encodings and absent text declarations
    /// must use the ordinary decoder/scanner, even when their raw grams differ.
    pub fn text_decision(&self, ordinal: usize, text: Option<&str>) -> IndexDecision {
        let Some(row) = self.rows.get(ordinal) else { return IndexDecision::Fallback; };
        let Some(text) = text else { return IndexDecision::Fallback; };
        if row.coverage != Coverage::Utf8 { return IndexDecision::Fallback; }
        self.probe(row, text.as_bytes())
    }
    /// Original-byte hosts may use this with the SAME raw needle they verify.
    /// IndexedNeedle in the paged adapter binds this pattern to its exact matcher.
    pub fn raw_decision(&self, ordinal: usize, bytes: &[u8]) -> IndexDecision {
        let Some(row) = self.rows.get(ordinal) else { return IndexDecision::Fallback; };
        if !row.coverage.indexed() { return IndexDecision::Fallback; }
        self.probe(row, bytes)
    }
    fn probe(&self, row: &Row, bytes: &[u8]) -> IndexDecision {
        if bytes.len() < 3 { return IndexDecision::Fallback; }
        let last = bytes.len() - 3;
        let grams = &self.grams[row.start..row.start + row.count];
        for offset in [0, last / 2, last] {
            let key = u32::from_be_bytes([0, bytes[offset], bytes[offset + 1], bytes[offset + 2]]);
            if grams.binary_search(&key).is_err() { return IndexDecision::Excluded; }
        }
        IndexDecision::Verify
    }
    /// The returned digest identifies the FULL encoded artifact, not just its
    /// embedded checksum. Retain that value in the host's trusted publication
    /// record before using negative certificates after reopening.
    pub fn encode(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<IndexArtifact, SnapshotIndexError> {
        let length = wire_length(self.rows.len(), self.grams.len())?;
        let charge = length.checked_mul(4).and_then(|n| n.checked_add(size_of::<IndexArtifact>()))
            .ok_or(SnapshotIndexError::Limits)?;
        let lease = budget.try_reserve_managed(self.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| SnapshotIndexError::ResourceDenied)?;
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        let mut writer = EnvelopeWriter::new(INDEX_SCHEMA);
        put_digest(&mut writer, self.archive);
        writer.put_u32(SEMANTICS); writer.put_u64(self.rows.len() as u64); writer.put_u64(self.grams.len() as u64);
        for row in &self.rows {
            if canceled() { return Err(SnapshotIndexError::Canceled); }
            writer.put_u8(row.coverage.tag()); writer.put_u64(row.length); put_digest(&mut writer, row.digest);
            writer.put_u64(row.count as u64);
            for (i, &gram) in self.grams[row.start..row.start + row.count].iter().enumerate() {
                if i % 4096 == 0 && canceled() { return Err(SnapshotIndexError::Canceled); }
                writer.put_u32(gram);
            }
        }
        let bytes = writer.finish();
        if bytes.len() != length || bytes.capacity() > charge { return Err(SnapshotIndexError::ResourceDenied); }
        let digest = Sha256::digest(&bytes);
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        Ok(IndexArtifact { bytes, digest, _lease: lease })
    }
    /// Open an artifact under a separately retained TRUSTED full-document digest.
    /// FCBO global postings are converted under a pre-admitted overlap reservation
    /// for compatibility with refresh. Query hosts should decode SnapshotPostings
    /// directly instead, retaining its global lookup and avoiding this conversion.
    /// Structural validation alone does not certify semantic completeness. The
    /// caller must NOT set trusted_digest = hash(untrusted_bytes) just to pass.
    pub fn decode_pinned(bytes: &[u8], trusted_digest: Sha256Digest, directory: &SnapshotDirectory,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotIndexError> {
        if SnapshotPostings::is_encoded(bytes) {
            return SnapshotPostings::decode_segments(bytes, trusted_digest, directory, budget, allocation, canceled);
        }
        if bytes.len() < FRAME_LEN + PREFIX_BYTES || bytes.len() > MAX_INDEX_BYTES { return Err(SnapshotIndexError::Limits); }
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        if Sha256::digest(bytes) != trusted_digest { return Err(SnapshotIndexError::PinMismatch); }
        if bytes[10..16] != [0; 6] { return Err(SnapshotIndexError::Version); }
        let payload = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| SnapshotIndexError::Format)?))
            .map_err(|_| SnapshotIndexError::Limits)?;
        if payload != bytes.len() - FRAME_LEN { return Err(SnapshotIndexError::Format); }
        let limits = EnvelopeLimits { max_document_bytes: MAX_INDEX_BYTES, max_payload_bytes: MAX_INDEX_BYTES - FRAME_LEN,
            max_field_bytes: MAX_INDEX_BYTES };
        let mut reader = EnvelopeReader::open(bytes, INDEX_SCHEMA, limits, UnknownPolicy::Strict)?;
        let archive = get_digest(&mut reader)?;
        if archive != directory.digest() { return Err(SnapshotIndexError::SourceMismatch); }
        if reader.get_u32()? != SEMANTICS { return Err(SnapshotIndexError::Version); }
        let count = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        let total = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        if count != directory.len() || count > MAX_INDEX_MEMBERS || total > MAX_INDEX_GRAMS
            || wire_length(count, total)? != bytes.len() { return Err(SnapshotIndexError::Limits); }
        let lease = reserve_charge(directory.owner(), count, total, budget, allocation)?;
        let mut rows = reserve(count)?;
        let mut grams = reserve(total)?;
        let mut stats = SavedIndexStats { members: count, unique_grams: total, ..SavedIndexStats::default() };
        for ordinal in 0..count {
            if canceled() { return Err(SnapshotIndexError::Canceled); }
            let coverage = Coverage::parse(reader.get_u8()?)?;
            let length = reader.get_u64()?;
            let digest = get_digest(&mut reader)?;
            let n = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
            if n > total.saturating_sub(grams.len()) || (!coverage.indexed() && n != 0)
                || n as u64 > length.saturating_sub(2) { return Err(SnapshotIndexError::Format); }
            let member = directory.member(ordinal).ok_or(SnapshotIndexError::SourceMismatch)?;
            if length != member.observed_bytes { return Err(SnapshotIndexError::SourceMismatch); }
            match member.data {
                PagedMemberData::Captured { digest: expected, .. } if coverage != Coverage::Unavailable && digest == expected => {},
                PagedMemberData::Unavailable(_) if coverage == Coverage::Unavailable && digest == Sha256Digest::new([0; 32]) => {},
                _ => return Err(SnapshotIndexError::SourceMismatch),
            }
            let start = grams.len();
            let mut previous = None;
            for i in 0..n {
                if i % 4096 == 0 && canceled() { return Err(SnapshotIndexError::Canceled); }
                let gram = reader.get_u32()?;
                if gram > 0x00ff_ffff || previous.is_some_and(|last| last >= gram) { return Err(SnapshotIndexError::Format); }
                previous = Some(gram); grams.push(gram);
            }
            match coverage { Coverage::Unavailable => stats.unavailable_files += 1,
                Coverage::Uncovered => stats.uncovered_files += 1, _ => stats.indexed_files += 1 }
            rows.push(Row { coverage, length, digest, start, count: n });
        }
        if grams.len() != total { return Err(SnapshotIndexError::Format); }
        reader.finish()?;
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        Ok(Self { owner: directory.owner(), archive, rows, grams, stats, _lease: lease })
    }
}

pub struct IndexArtifact { bytes: Vec<u8>, digest: Sha256Digest, _lease: ResourceLease }
impl IndexArtifact {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub const fn digest(&self) -> Sha256Digest { self.digest }
}
fn wire_length(count: usize, grams: usize) -> Result<usize, SnapshotIndexError> {
    let length = count.checked_mul(ROW_BYTES).and_then(|n| n.checked_add(FRAME_LEN + PREFIX_BYTES))
        .and_then(|n| grams.checked_mul(4).and_then(|g| n.checked_add(g))).ok_or(SnapshotIndexError::Limits)?;
    if length > MAX_INDEX_BYTES { return Err(SnapshotIndexError::Limits); }
    Ok(length)
}
fn reserve_charge(owner: ArenaOwnerId, count: usize, grams: usize, budget: &ResourceBudget,
    allocation: ResourceAllocationId) -> Result<ResourceLease, SnapshotIndexError> {
    let charge = count.checked_mul(size_of::<Row>()).and_then(|n| grams.checked_mul(4).and_then(|g| n.checked_add(g)))
        .and_then(|n| n.checked_add(size_of::<SnapshotIndex>())).ok_or(SnapshotIndexError::Limits)?;
    budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64)).map_err(|_| SnapshotIndexError::ResourceDenied)
}
fn reserve<T>(capacity: usize) -> Result<Vec<T>, SnapshotIndexError> {
    let mut values = Vec::new(); values.try_reserve_exact(capacity).map_err(|_| SnapshotIndexError::ResourceDenied)?;
    if values.capacity() > capacity { return Err(SnapshotIndexError::ResourceDenied); }
    Ok(values)
}
fn put_digest(writer: &mut EnvelopeWriter, digest: Sha256Digest) {
    for &byte in digest.as_bytes() { writer.put_u8(byte); }
}
fn get_digest(reader: &mut EnvelopeReader<'_>) -> Result<Sha256Digest, SnapshotIndexError> {
    let mut bytes = [0; 32];
    for byte in &mut bytes { *byte = reader.get_u8()?; }
    Ok(Sha256Digest::new(bytes))
}
