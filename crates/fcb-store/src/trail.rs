#![forbid(unsafe_code)]

//! Portable user-owned reading trails. This envelope stores references and user
//! rationale, NOT source payloads, live root grants, executable configuration,
//! or inferred compiler facts. Order and repeated visits are intentional.
//! Mutations produce a new immutable document; the old user state is untouched.

use std::mem::size_of;
use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::{EnvelopeLimits, EnvelopeReader, EnvelopeSchema, EnvelopeWriter, Sha256Digest, UnknownPolicy, CHECKSUM_LEN, FRAME_LEN};

pub const TRAIL_SCHEMA: EnvelopeSchema = EnvelopeSchema::with_magic(*b"FCBT", 0x54524c31, 1, 0);
pub const MAX_TRAIL_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRAIL_ITEMS: usize = 1024;
pub const MAX_TRAIL_PATH_BYTES: usize = 16_384;
pub const MAX_RATIONALE_BYTES: usize = 4096;
pub const MAX_TRAIL_TITLE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailError { Limit, Format, Version, Path, Range, Text, MissingItem, ResourceDenied, Canceled }
impl TrailError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limit => "TRAIL_LIMIT", Self::Format => "TRAIL_INVALID_ENVELOPE",
            Self::Version => "TRAIL_UNSUPPORTED_VERSION", Self::Path => "TRAIL_INVALID_PATH",
            Self::Range => "TRAIL_INVALID_RANGE", Self::Text => "TRAIL_INVALID_TEXT",
            Self::MissingItem => "TRAIL_ITEM_NOT_FOUND", Self::ResourceDenied => "TRAIL_RESOURCE_DENIED",
            Self::Canceled => "TRAIL_CANCELED",
        }
    }
}
impl std::fmt::Display for TrailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for TrailError {}
impl From<crate::EnvelopeError> for TrailError { fn from(_: crate::EnvelopeError) -> Self { Self::Format } }

/// Serialized references must be revalidated against the explicitly selected
/// archive and its member bytes before use. Equal path strings alone prove
/// neither source identity nor continuity across captures. Rationale is user
/// text, never evidence of code behavior. Empty ranges are point bookmarks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrailEntry<'a> {
    pub archive: Sha256Digest,
    pub source: Sha256Digest,
    pub source_length: u64,
    pub path: &'a [u8],
    pub range: ByteRange,
    pub rationale: &'a str,
}
impl TrailEntry<'_> {
    fn validate(self) -> Result<(), TrailError> {
        if self.path.is_empty() || self.path.len() > MAX_TRAIL_PATH_BYTES || self.path.contains(&0)
            || self.path.split(|&b| b == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
            return Err(TrailError::Path);
        }
        if self.range.end().get() > self.source_length { return Err(TrailError::Range); }
        if self.rationale.len() > MAX_RATIONALE_BYTES { return Err(TrailError::Text); }
        Ok(())
    }
}

pub struct TrailBytes { bytes: Vec<u8>, _lease: ResourceLease }
impl TrailBytes {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub fn digest(&self) -> Sha256Digest {
        Sha256Digest::new(self.bytes[self.bytes.len() - CHECKSUM_LEN..].try_into().expect("encoded envelope"))
    }
    /// Exact admission precedes writer allocation. The 4x reservation covers
    /// growing writer capacity and final document overlap. Source bytes are not
    /// copied. An optional parent digest records ancestry, not authentication.
    pub fn encode(owner: ArenaOwnerId, title: &str, parent: Option<Sha256Digest>, entries: &[TrailEntry<'_>],
        budget: &ResourceBudget, allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<Self, TrailError> {
        if title.len() > MAX_TRAIL_TITLE_BYTES || entries.len() > MAX_TRAIL_ITEMS { return Err(TrailError::Limit); }
        let mut size = FRAME_LEN + 2 + if parent.is_some() { 40 } else { 0 } + 8 + title.len() + 8;
        let mut selected = 0u64;
        for &entry in entries {
            if canceled() { return Err(TrailError::Canceled); }
            entry.validate()?;
            selected = selected.checked_add(entry.range.len().get()).ok_or(TrailError::Limit)?;
            // evidence byte, two length-tagged digests, length/start/end, path/note lengths.
            size = size.checked_add(121).and_then(|n| n.checked_add(entry.path.len()))
                .and_then(|n| n.checked_add(entry.rationale.len())).ok_or(TrailError::Limit)?;
        }
        if size > MAX_TRAIL_BYTES { return Err(TrailError::Limit); }
        if canceled() { return Err(TrailError::Canceled); }
        let charge = size.checked_mul(4).and_then(|n| n.checked_add(size_of::<Self>())).ok_or(TrailError::Limit)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| TrailError::ResourceDenied)?;
        let mut writer = EnvelopeWriter::new(TRAIL_SCHEMA);
        writer.put_u8(1); // Native Unix relative paths; never a restored filesystem grant.
        writer.put_u8(u8::from(parent.is_some()));
        if let Some(parent) = parent { writer.put_bytes(parent.as_bytes()); }
        writer.put_str(title); writer.put_u64(entries.len() as u64);
        for entry in entries {
            if canceled() { return Err(TrailError::Canceled); }
            writer.put_u8(1); // Explicit user selection, not a parser/relationship claim.
            writer.put_bytes(entry.archive.as_bytes()); writer.put_bytes(entry.source.as_bytes());
            writer.put_u64(entry.source_length);
            writer.put_u64(entry.range.start().get()); writer.put_u64(entry.range.end().get());
            writer.put_bytes(entry.path); writer.put_str(entry.rationale);
        }
        let bytes = writer.finish();
        if bytes.len() != size || bytes.capacity() > charge { return Err(TrailError::ResourceDenied); }
        if canceled() { return Err(TrailError::Canceled); }
        Ok(Self { bytes, _lease: lease })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TrailView<'a> {
    entries: EnvelopeReader<'a>, title: &'a str, parent: Option<Sha256Digest>,
    count: usize, reference_bytes: u64, digest: Sha256Digest,
}
impl<'a> TrailView<'a> {
    /// Validate the complete bounded envelope before exposing any user data.
    /// Input bytes and their charge are owned by the caller; this is zero-copy.
    pub fn open(bytes: &'a [u8], mut canceled: impl FnMut() -> bool) -> Result<Self, TrailError> {
        if canceled() { return Err(TrailError::Canceled); }
        if !(FRAME_LEN..=MAX_TRAIL_BYTES).contains(&bytes.len()) { return Err(TrailError::Limit); }
        let length = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| TrailError::Format)?))
            .map_err(|_| TrailError::Limit)?;
        if length != bytes.len() - FRAME_LEN { return Err(TrailError::Format); }
        if bytes[10..16] != [0; 6] { return Err(TrailError::Version); }
        let mut reader = EnvelopeReader::open(bytes, TRAIL_SCHEMA, EnvelopeLimits {
            max_document_bytes: MAX_TRAIL_BYTES, max_payload_bytes: MAX_TRAIL_BYTES - FRAME_LEN,
            max_field_bytes: MAX_TRAIL_PATH_BYTES,
        }, UnknownPolicy::Strict)?;
        if reader.get_u8()? != 1 { return Err(TrailError::Version); }
        let parent = match reader.get_u8()? { 0 => None, 1 => Some(digest(&mut reader)?), _ => return Err(TrailError::Version) };
        let title = std::str::from_utf8(field(&mut reader)?).map_err(|_| TrailError::Text)?;
        if title.len() > MAX_TRAIL_TITLE_BYTES { return Err(TrailError::Limit); }
        let count = usize::try_from(reader.get_u64()?).map_err(|_| TrailError::Limit)?;
        if count > MAX_TRAIL_ITEMS { return Err(TrailError::Limit); }
        let entries = reader;
        let mut reference_bytes = 0u64;
        for _ in 0..count {
            if canceled() { return Err(TrailError::Canceled); }
            let entry = read_entry(&mut reader)?; entry.validate()?;
            reference_bytes = reference_bytes.checked_add(entry.range.len().get()).ok_or(TrailError::Limit)?;
        }
        reader.finish()?;
        if canceled() { return Err(TrailError::Canceled); }
        Ok(Self { entries, title, parent, count, reference_bytes,
            digest: Sha256Digest::new(bytes[bytes.len() - CHECKSUM_LEN..].try_into().map_err(|_| TrailError::Format)?) })
    }
    pub const fn len(self) -> usize { self.count }
    pub const fn is_empty(self) -> bool { self.count == 0 }
    pub const fn title(self) -> &'a str { self.title }
    pub const fn parent_digest(self) -> Option<Sha256Digest> { self.parent }
    pub const fn digest(self) -> Sha256Digest { self.digest }
    /// Sum of referenced byte lengths INCLUDING intentional repeated visits.
    /// This is not payload residency, unique-source coverage or a token estimate.
    pub const fn reference_bytes(self) -> u64 { self.reference_bytes }
    pub fn entries(self) -> impl ExactSizeIterator<Item = Result<TrailEntry<'a>, TrailError>> {
        let mut reader = self.entries;
        (0..self.count).map(move |_| read_entry(&mut reader))
    }
    pub fn entry(self, ordinal: usize) -> Result<TrailEntry<'a>, TrailError> {
        if ordinal >= self.count { return Err(TrailError::MissingItem); }
        self.entries().nth(ordinal).ok_or(TrailError::MissingItem)?
    }
}
fn field<'a>(reader: &mut EnvelopeReader<'a>) -> Result<&'a [u8], TrailError> {
    let mut peek = *reader;
    let len = usize::try_from(peek.get_u64()?).map_err(|_| TrailError::Limit)?;
    if len > peek.remaining() { return Err(TrailError::Format); }
    Ok(reader.get_bytes()?)
}
fn digest(reader: &mut EnvelopeReader<'_>) -> Result<Sha256Digest, TrailError> {
    Ok(Sha256Digest::new(field(reader)?.try_into().map_err(|_| TrailError::Format)?))
}
fn read_entry<'a>(reader: &mut EnvelopeReader<'a>) -> Result<TrailEntry<'a>, TrailError> {
    if reader.get_u8()? != 1 { return Err(TrailError::Version); }
    let archive = digest(reader)?; let source = digest(reader)?;
    let source_length = reader.get_u64()?;
    let start = reader.get_u64()?; let end = reader.get_u64()?;
    let range = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| TrailError::Range)?;
    let path = field(reader)?;
    let rationale = std::str::from_utf8(field(reader)?).map_err(|_| TrailError::Text)?;
    Ok(TrailEntry { archive, source, source_length, path, range, rationale })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sha256;
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(801).unwrap() }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
    fn entry() -> TrailEntry<'static> {
        TrailEntry { archive: Sha256Digest::new([1; 32]), source: Sha256Digest::new([2; 32]), source_length: u64::MAX,
            path: b"src/\xff\\name.rs", range: ByteRange::new(ByteOffset::new(u64::MAX - 9), ByteOffset::new(u64::MAX)).unwrap(),
            rationale: "User note: verify this branch\n\u{202e}" }
    }
    fn encoded(entries: &[TrailEntry<'_>], parent: Option<Sha256Digest>, budget: &ResourceBudget) -> TrailBytes {
        TrailBytes::encode(owner(), "investigation", parent, entries, budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
    }
    #[test]
    fn ordered_repeated_visits_full_width_offsets_and_notes_roundtrip() {
        let budget = budget(); let entries = [entry(), TrailEntry { rationale: "second visit", ..entry() }];
        let encoded = encoded(&entries, Some(Sha256Digest::new([3; 32])), &budget);
        let view = TrailView::open(encoded.bytes(), || false).unwrap();
        assert_eq!(view.entries().collect::<Result<Vec<_>, _>>().unwrap(), entries);
        assert_eq!(view.reference_bytes(), 18); assert_eq!(view.title(), "investigation");
        assert_eq!(view.entry(1).unwrap().rationale, "second visit");
        assert_eq!(view.entry(2), Err(TrailError::MissingItem));
        assert_eq!(view.digest(), encoded.digest());
        drop(encoded); assert_eq!(budget.accounting().reserved().get(), 0);
    }
    #[test]
    fn empty_trails_and_point_bookmarks_are_not_missing_source() {
        let budget = budget(); let bytes = encoded(&[], None, &budget);
        assert!(TrailView::open(bytes.bytes(), || false).unwrap().is_empty()); drop(bytes);
        let point = TrailEntry { source_length: 0, range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(0)).unwrap(), ..entry() };
        let bytes = encoded(&[point], None, &budget);
        assert_eq!(TrailView::open(bytes.bytes(), || false).unwrap().reference_bytes(), 0);
    }
    #[test]
    fn checksum_truncation_and_unknown_evidence_are_rejected() {
        let budget = budget(); let bytes = encoded(&[entry()], None, &budget);
        for index in 0..bytes.bytes().len() {
            let mut bad = bytes.bytes().to_vec(); bad[index] ^= 1;
            assert!(TrailView::open(&bad, || false).is_err());
            assert!(TrailView::open(&bytes.bytes()[..index], || false).is_err());
        }
        let mut bad = bytes.bytes().to_vec();
        // First evidence byte: header + native/parent tags + title length/data + count.
        bad[24 + 2 + 8 + "investigation".len() + 8] = 2;
        let end = bad.len() - CHECKSUM_LEN; let checksum = Sha256::digest(&bad[..end]);
        bad[end..].copy_from_slice(checksum.as_bytes());
        assert_eq!(TrailView::open(&bad, || false).unwrap_err(), TrailError::Version);
    }
    #[test]
    fn invalid_paths_ranges_and_reference_sum_overflow_are_refused() {
        let budget = budget(); let allocation = ResourceAllocationId::new(1).unwrap();
        for path in [b"/x".as_slice(), b"a/../b", b"", b"a\0b"] {
            assert!(TrailBytes::encode(owner(), "", None, &[TrailEntry { path, ..entry() }], &budget, allocation, || false).is_err());
        }
        let huge = TrailEntry { range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(u64::MAX)).unwrap(), ..entry() };
        assert!(matches!(TrailBytes::encode(owner(), "", None, &[huge, huge], &budget, allocation, || false), Err(TrailError::Limit)));
        assert!(matches!(TrailBytes::encode(owner(), "", None, &[TrailEntry { source_length: 0, ..entry() }], &budget, allocation, || false), Err(TrailError::Range)));
    }
    #[test]
    fn resource_denial_and_cancellation_do_not_retain_candidate_memory() {
        let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
        assert!(matches!(TrailBytes::encode(owner(), "", None, &[], &tiny, ResourceAllocationId::new(1).unwrap(), || false), Err(TrailError::ResourceDenied)));
        let budget = budget();
        assert!(matches!(TrailBytes::encode(owner(), "", None, &[entry()], &budget, ResourceAllocationId::new(1).unwrap(),
            || budget.accounting().reserved().get() > 0), Err(TrailError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
