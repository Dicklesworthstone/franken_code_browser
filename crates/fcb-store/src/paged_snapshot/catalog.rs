#![forbid(unsafe_code)]

//! Reopen an FCBS/1 observation without rereading all source payloads.
//!
//! The catalog is produced ONLY from a fully validated archive directory. Its
//! FULL artifact digest must be retained separately by a trusted host. A checksum
//! inside a file, a filename, or a digest computed from untrusted catalog bytes
//! is NOT that trust. Catalog paths/offsets never authorize filesystem access.
//!
//! Reopening checks archive length, header and stored checksum, not the body.
//! Member loads still hash the bytes returned by the existing loader. Thus a
//! saved scope can be searched without reading excluded members; corruption of
//! an unread body region remains UNCHECKED, not a healthy-archive claim.

use super::*;
use crate::{EnvelopeLimits, EnvelopeReader, EnvelopeSchema, EnvelopeWriter, UnknownPolicy};

pub const CATALOG_SCHEMA: EnvelopeSchema = EnvelopeSchema::with_magic(*b"FCBC", 0x43415431, 1, 0);
pub const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const PREFIX_BYTES: usize = 32 + 8 + 2 + 8 + 8 + 8 + 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogError {
    Archive(PagedSnapshotError), Format, Version, Limits, PinMismatch,
    FullValidationRequired, SourceLayout, ResourceDenied, Canceled,
}
impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Archive(error) => return write!(f, "{error}"),
            Self::Format => "CATALOG_FORMAT", Self::Version => "CATALOG_VERSION",
            Self::Limits => "CATALOG_LIMIT", Self::PinMismatch => "CATALOG_PIN_MISMATCH",
            Self::FullValidationRequired => "CATALOG_FULL_VALIDATION_REQUIRED",
            Self::SourceLayout => "CATALOG_SOURCE_LAYOUT_MISMATCH",
            Self::ResourceDenied => "CATALOG_RESOURCE_DENIED", Self::Canceled => "CATALOG_CANCELED",
        })
    }
}
impl std::error::Error for CatalogError {}
impl From<PagedSnapshotError> for CatalogError {
    fn from(error: PagedSnapshotError) -> Self {
        if error == PagedSnapshotError::Canceled { Self::Canceled } else { Self::Archive(error) }
    }
}
impl From<crate::EnvelopeError> for CatalogError {
    fn from(_: crate::EnvelopeError) -> Self { Self::Format }
}

/// Owned sensitive metadata: native names, source lengths and digests. No source
/// payload. Encoding holds its own reservation alongside the original directory.
pub struct CatalogArtifact {
    bytes: Vec<u8>, digest: Sha256Digest, _lease: ResourceLease,
}
impl CatalogArtifact {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    /// Full-artifact pin for trusted publication state, not the inner checksum.
    pub const fn digest(&self) -> Sha256Digest { self.digest }
}

impl SnapshotDirectory {
    /// True only when this open actually read and hashed the full archive body.
    /// It does not certify an externally mutable file remains unchanged later.
    pub const fn fully_verified_on_open(&self) -> bool { self.catalog_pin.is_none() }
    pub const fn catalog_pin(&self) -> Option<Sha256Digest> { self.catalog_pin }

    /// Explicit worker operation with no filesystem effects. A boundary-only
    /// reopened catalog cannot mint a new full-validation receipt.
    pub fn encode_catalog(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<CatalogArtifact, CatalogError> {
        if !self.fully_verified_on_open() { return Err(CatalogError::FullValidationRequired); }
        if canceled() { return Err(CatalogError::Canceled); }
        let mut length = FRAME_LEN + PREFIX_BYTES + self.policy.len();
        for entry in &self.entries {
            if canceled() { return Err(CatalogError::Canceled); }
            let extra = match &entry.location { Location::Captured { .. } => 8 + 32,
                Location::Unavailable(reason) => 8 + reason.len() };
            length = length.checked_add(8 + entry.path.len() + 8 + 1 + extra).ok_or(CatalogError::Limits)?;
        }
        if length > MAX_CATALOG_BYTES { return Err(CatalogError::Limits); }
        let charge = length.checked_mul(4).and_then(|n| n.checked_add(size_of::<CatalogArtifact>()))
            .ok_or(CatalogError::Limits)?;
        let lease = budget.try_reserve_managed(self.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| CatalogError::ResourceDenied)?;
        let mut writer = EnvelopeWriter::new(CATALOG_SCHEMA);
        put_digest(&mut writer, self.digest);
        writer.put_u64(self.archive_bytes);
        writer.put_u8(u8::from(self.complete)); writer.put_u8(1); // Unix native relative names.
        writer.put_u64(self.entries.len() as u64);
        writer.put_u64(self.paths.len() as u64); writer.put_u64(self.reasons.len() as u64);
        writer.put_str(&self.policy);
        for entry in &self.entries {
            if canceled() { return Err(CatalogError::Canceled); }
            writer.put_bytes(&self.paths[entry.path.clone()]); writer.put_u64(entry.observed);
            match &entry.location {
                Location::Captured { offset, digest, .. } => {
                    writer.put_u8(1); writer.put_u64(*offset); put_digest(&mut writer, *digest);
                }
                Location::Unavailable(reason) => { writer.put_u8(0); writer.put_bytes(&self.reasons[reason.clone()]); }
            }
        }
        let bytes = writer.finish();
        if bytes.len() != length || bytes.capacity() > charge { return Err(CatalogError::ResourceDenied); }
        let digest = checked_digest(&bytes, &mut canceled)?;
        Ok(CatalogArtifact { bytes, digest, _lease: lease })
    }
}

/// Fresh-owner metadata admitted against a separately retained trusted pin.
/// Only decode_pinned constructs it. It is consumed when attaching an explicit
/// reader, keeping metadata/lease ownership identical to ordinary PagedSnapshot.
pub struct PinnedCatalog { directory: SnapshotDirectory }
impl PinnedCatalog {
    pub fn directory(&self) -> &SnapshotDirectory { &self.directory }

    pub fn decode_pinned(bytes: &[u8], trusted_digest: Sha256Digest, owner: ArenaOwnerId,
        limits: SnapshotLimits, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, CatalogError> {
        validate_limits(limits)?;
        if bytes.len() < FRAME_LEN + PREFIX_BYTES || bytes.len() > MAX_CATALOG_BYTES { return Err(CatalogError::Limits); }
        if checked_digest(bytes, &mut canceled)? != trusted_digest { return Err(CatalogError::PinMismatch); }
        if bytes[10..16] != [0; 6] { return Err(CatalogError::Version); }
        let payload = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| CatalogError::Format)?))
            .map_err(|_| CatalogError::Limits)?;
        if payload != bytes.len() - FRAME_LEN { return Err(CatalogError::Format); }
        let mut reader = EnvelopeReader::open(bytes, CATALOG_SCHEMA, EnvelopeLimits {
            max_document_bytes: MAX_CATALOG_BYTES, max_payload_bytes: MAX_CATALOG_BYTES - FRAME_LEN,
            max_field_bytes: limits.max_path_bytes.max(128),
        }, UnknownPolicy::Strict)?;
        let digest = get_digest(&mut reader)?;
        let archive_bytes = reader.get_u64()?;
        if archive_bytes < (FRAME_LEN + 18) as u64 || archive_bytes > limits.max_document_bytes as u64 {
            return Err(CatalogError::Limits);
        }
        let complete = match reader.get_u8()? { 0 => false, 1 => true, _ => return Err(CatalogError::Format) };
        if reader.get_u8()? != 1 { return Err(CatalogError::Version); }
        let count = number(&mut reader)?;
        let path_count = number(&mut reader)?;
        let reason_count = number(&mut reader)?;
        if count > limits.max_files || path_count > limits.max_total_path_bytes
            || reason_count > count.checked_mul(96).ok_or(CatalogError::Limits)? {
            return Err(CatalogError::Limits);
        }
        let policy = field(&mut reader, 128)?;
        if policy.is_empty() || !policy.iter().all(u8::is_ascii_graphic) { return Err(CatalogError::Format); }
        let policy = std::str::from_utf8(policy).map_err(|_| CatalogError::Format)?;
        let charge = count.checked_mul(size_of::<Entry>()).and_then(|n| n.checked_add(path_count))
            .and_then(|n| n.checked_add(reason_count)).and_then(|n| n.checked_add(size_of::<Self>() + policy.len() + 128))
            .ok_or(CatalogError::Limits)?;
        if canceled() { return Err(CatalogError::Canceled); }
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| CatalogError::ResourceDenied)?;
        let mut entries: Vec<Entry> = reserve(count)?;
        let mut paths = reserve(path_count)?;
        let mut reasons = reserve(reason_count)?;
        let mut source_bytes = 0u64;
        let mut captured = 0;
        // Independently reconstruct FCBS/1 framing. Offsets must be exactly at
        // their member payload, not overlapping headers, other members or footer.
        let mut cursor = (HEADER_LEN + 18 + policy.len()) as u64;
        for _ in 0..count {
            if canceled() { return Err(CatalogError::Canceled); }
            let path = field(&mut reader, limits.max_path_bytes)?;
            if path.is_empty() || path.contains(&0)
                || path.split(|&b| b == b'/').any(|p| p.is_empty() || p == b"." || p == b"..") {
                return Err(PagedSnapshotError::Path.into());
            }
            if entries.last().is_some_and(|previous| &paths[previous.path.clone()] >= path) {
                return Err(PagedSnapshotError::Order.into());
            }
            let start = paths.len();
            let end = start.checked_add(path.len()).ok_or(CatalogError::Limits)?;
            if end > path_count { return Err(CatalogError::Limits); }
            paths.extend_from_slice(path);
            let observed = reader.get_u64()?;
            cursor = cursor.checked_add(25 + path.len() as u64).ok_or(CatalogError::Limits)?;
            let location = match reader.get_u8()? {
                1 => {
                    let length = usize::try_from(observed).map_err(|_| CatalogError::Limits)?;
                    if length > limits.max_file_bytes { return Err(CatalogError::Limits); }
                    let offset = reader.get_u64()?;
                    if offset != cursor { return Err(CatalogError::SourceLayout); }
                    let digest = get_digest(&mut reader)?;
                    cursor = cursor.checked_add(observed).ok_or(CatalogError::Limits)?;
                    source_bytes = source_bytes.checked_add(observed).ok_or(CatalogError::Limits)?;
                    if source_bytes > limits.max_source_bytes as u64 { return Err(CatalogError::Limits); }
                    captured += 1;
                    Location::Captured { offset, length, digest }
                }
                0 => {
                    let reason = field(&mut reader, 96)?;
                    if reason.is_empty() || !reason.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_') {
                        return Err(PagedSnapshotError::Member.into());
                    }
                    let start = reasons.len(); let end = start.checked_add(reason.len()).ok_or(CatalogError::Limits)?;
                    if end > reason_count { return Err(CatalogError::Limits); }
                    reasons.extend_from_slice(reason);
                    cursor = cursor.checked_add(reason.len() as u64).ok_or(CatalogError::Limits)?;
                    Location::Unavailable(start..end)
                }
                _ => return Err(CatalogError::Format),
            };
            if cursor > archive_bytes - CHECKSUM_LEN as u64 { return Err(CatalogError::SourceLayout); }
            entries.push(Entry { path: start..end, observed, location });
        }
        reader.finish()?;
        if cursor != archive_bytes - CHECKSUM_LEN as u64 || paths.len() != path_count || reasons.len() != reason_count {
            return Err(CatalogError::SourceLayout);
        }
        if canceled() { return Err(CatalogError::Canceled); }
        Ok(Self { directory: SnapshotDirectory { owner, entries, paths, reasons, policy: policy.to_owned(), complete,
            captured, source_bytes, archive_bytes, digest, validation: SnapshotIoStats::default(),
            catalog_pin: Some(trusted_digest), retained_charge: charge, _lease: lease } })
    }
}

impl<R: Read + Seek> PagedSnapshot<R> {
    /// Attach a host-admitted reader to trusted saved metadata. Reads 24 header
    /// bytes, 32 footer bytes and an EOF probe; uses three seeks. This does NOT
    /// recompute the archive checksum. Unread body corruption is unknown. Every
    /// subsequent load uses the existing per-member hash check before publication.
    /// No fallback, pathname opening, implicit catalog lookup or mutation occurs.
    pub fn open_pinned(mut input: R, catalog: PinnedCatalog, mut canceled: impl FnMut() -> bool)
        -> Result<Self, CatalogError> {
        if canceled() { return Err(CatalogError::Canceled); }
        let mut directory = catalog.directory;
        let end = input.seek(SeekFrom::End(0)).map_err(|_| PagedSnapshotError::Io)?;
        if end != directory.archive_bytes { return Err(CatalogError::SourceLayout); }
        if input.seek(SeekFrom::Start(0)).map_err(|_| PagedSnapshotError::Io)? != 0 {
            return Err(PagedSnapshotError::Io.into());
        }
        let mut scan = Scan { input: &mut input, hash: Sha256::new(), position: 0,
            end, stats: SnapshotIoStats::default() };
        let mut header = [0; HEADER_LEN];
        scan.exact(&mut header, false, &mut canceled)?;
        if header[..4] != SNAPSHOT_SCHEMA.magic
            || u32::from_le_bytes(header[4..8].try_into().unwrap()) != SNAPSHOT_SCHEMA.schema_id
            || u16::from_le_bytes(header[8..10].try_into().unwrap()) != SNAPSHOT_SCHEMA.major
            || header[10..16] != [0; 6]
            || u64::from_le_bytes(header[16..24].try_into().unwrap()) != end - FRAME_LEN as u64 {
            return Err(CatalogError::SourceLayout);
        }
        let footer = end - CHECKSUM_LEN as u64;
        if scan.input.seek(SeekFrom::Start(footer)).map_err(|_| PagedSnapshotError::Io)? != footer {
            return Err(PagedSnapshotError::Io.into());
        }
        scan.position = footer;
        let mut checksum = [0; CHECKSUM_LEN];
        scan.exact(&mut checksum, false, &mut canceled)?;
        scan.eof(&mut canceled)?;
        if checksum != *directory.digest.as_bytes() { return Err(CatalogError::SourceLayout); }
        directory.validation = scan.stats;
        if canceled() { return Err(CatalogError::Canceled); }
        Ok(Self { input, directory, loads: SnapshotIoStats::default() })
    }
}

fn checked_digest(bytes: &[u8], canceled: &mut impl FnMut() -> bool) -> Result<Sha256Digest, CatalogError> {
    let mut hash = Sha256::new();
    for part in bytes.chunks(SNAPSHOT_READ_BYTES) {
        if canceled() { return Err(CatalogError::Canceled); }
        hash.update(part);
    }
    if canceled() { return Err(CatalogError::Canceled); }
    Ok(hash.finalize())
}
fn put_digest(writer: &mut EnvelopeWriter, digest: Sha256Digest) {
    for &byte in digest.as_bytes() { writer.put_u8(byte); }
}
fn get_digest(reader: &mut EnvelopeReader<'_>) -> Result<Sha256Digest, CatalogError> {
    let mut bytes = [0; 32];
    for byte in &mut bytes { *byte = reader.get_u8()?; }
    Ok(Sha256Digest::new(bytes))
}
fn number(reader: &mut EnvelopeReader<'_>) -> Result<usize, CatalogError> {
    usize::try_from(reader.get_u64()?).map_err(|_| CatalogError::Limits)
}
fn field<'a>(reader: &mut EnvelopeReader<'a>, maximum: usize) -> Result<&'a [u8], CatalogError> {
    let mut peek = *reader;
    let length = number(&mut peek)?;
    if length > maximum || length > peek.remaining() { return Err(CatalogError::Limits); }
    Ok(reader.get_bytes()?)
}
