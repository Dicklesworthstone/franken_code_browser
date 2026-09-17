#![forbid(unsafe_code)]

//! FCBD/1 stores the existing globally sorted posting pairs in independently
//! verified 16 KiB pages. The trusted pin hashes the COMPLETE metadata envelope,
//! including every page digest, rather than the unread payload. Reopening reads
//! only the header and manifest; every page is verified before it can support a
//! candidate or negative certificate. Unread backing integrity remains unknown.
//! This changes storage access, not substring semantics or exact verification.

mod cursor;
pub use cursor::PagedPostingCandidates;

use std::io::{self, SeekFrom};
use super::*;

pub const POSTING_PAGE_PAIRS: usize = 2048;
pub const POSTING_PAGE_BYTES: usize = POSTING_PAGE_PAIRS * 8;
pub const MAX_PAGED_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PAGED_POSTINGS_BYTES: usize = 24 * 1024 * 1024;
pub const PAGED_MANIFEST_SCHEMA: EnvelopeSchema = EnvelopeSchema::with_magic(*b"FCPM", 0x504f5333, 1, 0);
const HEADER_BYTES: usize = 32;
const MANIFEST_PREFIX: usize = 64;
const PAGE_META_BYTES: usize = 56;
const MAX_IO_CALLS: u64 = 131_072;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagedPostingError {
    Index(SnapshotIndexError), Io, ReadCallLimit, ChangedPage, InvalidRead,
}
impl std::fmt::Display for PagedPostingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index(error) => write!(f, "{error}"),
            Self::Io => f.write_str("PAGED_INDEX_IO"),
            Self::ReadCallLimit => f.write_str("PAGED_INDEX_READ_CALL_LIMIT"),
            Self::ChangedPage => f.write_str("PAGED_INDEX_CHANGED_PAGE"),
            Self::InvalidRead => f.write_str("PAGED_INDEX_INVALID_READ"),
        }
    }
}
impl std::error::Error for PagedPostingError {}
impl From<SnapshotIndexError> for PagedPostingError { fn from(e: SnapshotIndexError) -> Self { Self::Index(e) } }
impl From<fcb_store::EnvelopeError> for PagedPostingError {
    fn from(_: fcb_store::EnvelopeError) -> Self { SnapshotIndexError::Format.into() }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PostingPageIo {
    pub manifest_bytes_read: u64,
    pub page_bytes_read: u64,
    pub read_calls: u64,
    pub page_loads: u64,
    pub cache_hits: u64,
    pub evictions: u64,
}
#[derive(Clone, Copy, Debug)]
struct PageMeta { first: u64, last: u64, count: usize, digest: Sha256Digest }

/// This is an explicit source-derived export. The digest is a METADATA pin
/// covering all page digests; it is not a fresh checksum of unread disk pages.
pub struct PagedIndexArtifact {
    bytes: Vec<u8>, pin: Sha256Digest, manifest_bytes: usize, _lease: ResourceLease,
}
impl PagedIndexArtifact {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub const fn digest(&self) -> Sha256Digest { self.pin }
    pub const fn manifest_bytes(&self) -> usize { self.manifest_bytes }
}

impl SnapshotPostings {
    /// Encode exactly the SAME pair table into verified pages. Construction
    /// still uses the resident production builder. This is not out-of-core sort.
    pub fn encode_paged(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<PagedIndexArtifact, PagedPostingError> {
        let pages = self.pairs.len().div_ceil(POSTING_PAGE_PAIRS);
        let manifest_len = manifest_length(self.rows.len(), pages)?;
        let total_len = file_length(manifest_len, self.pairs.len())?;
        let charge = total_len.checked_add(manifest_len.checked_mul(4).ok_or(SnapshotIndexError::Limits)?)
            .and_then(|n| n.checked_add(POSTING_PAGE_BYTES + size_of::<PagedIndexArtifact>()))
            .ok_or(SnapshotIndexError::Limits)?;
        if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
        let lease = budget.try_reserve_managed(self.owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| SnapshotIndexError::ResourceDenied)?;
        let mut writer = EnvelopeWriter::new(PAGED_MANIFEST_SCHEMA);
        put_digest(&mut writer, self.archive);
        writer.put_u32(SEMANTICS); writer.put_u32(POSTING_PAGE_PAIRS as u32);
        writer.put_u64(self.rows.len() as u64); writer.put_u64(self.pairs.len() as u64); writer.put_u64(pages as u64);
        for row in &self.rows {
            if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            writer.put_u8(row.coverage.tag()); writer.put_u64(row.length);
            put_digest(&mut writer, row.digest); writer.put_u64(row.count as u64);
        }
        let mut scratch = [0u8; POSTING_PAGE_BYTES];
        for page in self.pairs.chunks(POSTING_PAGE_PAIRS) {
            if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            for (slot, pair) in page.iter().enumerate() { scratch[slot * 8..slot * 8 + 8].copy_from_slice(&pair.to_le_bytes()); }
            writer.put_u64(page[0]); writer.put_u64(page[page.len() - 1]); writer.put_u64(page.len() as u64);
            put_digest(&mut writer, Sha256::digest(&scratch[..page.len() * 8]));
        }
        let manifest = writer.finish();
        if manifest.len() != manifest_len { return Err(SnapshotIndexError::Format.into()); }
        let pin = Sha256::digest(&manifest);
        let mut bytes = reserve(total_len)?;
        bytes.extend_from_slice(b"FCBD"); bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(manifest_len as u64).to_le_bytes());
        bytes.extend_from_slice(&(total_len as u64).to_le_bytes()); bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&manifest);
        for page in self.pairs.chunks(POSTING_PAGE_PAIRS) {
            if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            for pair in page { bytes.extend_from_slice(&pair.to_le_bytes()); }
        }
        if bytes.len() != total_len { return Err(SnapshotIndexError::Format.into()); }
        if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
        Ok(PagedIndexArtifact { bytes, pin, manifest_bytes: manifest_len, _lease: lease })
    }
}

/// Owns one explicitly supplied seekable index handle. Rows/fallback metadata
/// are resident; posting payload is a fixed-size round-robin verified-page cache.
/// A cached verified page may safely outlive later backing-file modification.
/// Re-loading an evicted modified page fails, never silently replacing evidence.
pub struct PagedPostings<R: Read + Seek> {
    input: R,
    owner: ArenaOwnerId,
    archive: Sha256Digest,
    pin: Sha256Digest,
    rows: Vec<Row>,
    pages: Vec<PageMeta>,
    captured: Vec<u32>, raw_fallback: Vec<u32>, text_fallback: Vec<u32>,
    total: usize, body_offset: u64, file_bytes: u64,
    cache: Vec<u8>, slots: Vec<Option<usize>>, replacement: usize,
    stats: SavedIndexStats, io: PostingPageIo,
    failed: Option<PagedPostingError>, _lease: ResourceLease,
}
impl<R: Read + Seek> PagedPostings<R> {
    /// The pin MUST come from the trusted encode/build receipt, independently
    /// of this file. Canonical header fields are re-derived from pinned metadata.
    /// No posting page is read by open. 1..=16 cache pages are admitted up front.
    pub fn open_pinned(mut input: R, pin: Sha256Digest, directory: &SnapshotDirectory,
        cache_pages: usize, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, PagedPostingError> {
        if !(1..=16).contains(&cache_pages) { return Err(SnapshotIndexError::Limits.into()); }
        let mut io = PostingPageIo::default();
        seek_to(&mut input, 0)?;
        let mut header = [0; HEADER_BYTES];
        read_exact(&mut input, &mut header, &mut io, false, &mut canceled)?;
        if &header[..4] != b"FCBD" || header[4..8] != 1u32.to_le_bytes() || header[24..] != [0; 8] {
            return Err(SnapshotIndexError::Version.into());
        }
        let manifest_len = usize::try_from(u64::from_le_bytes(header[8..16].try_into().unwrap()))
            .map_err(|_| SnapshotIndexError::Limits)?;
        let file_bytes = u64::from_le_bytes(header[16..24].try_into().unwrap());
        if !(FRAME_LEN + MANIFEST_PREFIX..=MAX_PAGED_MANIFEST_BYTES).contains(&manifest_len)
            || file_bytes > MAX_PAGED_POSTINGS_BYTES as u64 { return Err(SnapshotIndexError::Limits.into()); }
        let charge = manifest_len.checked_mul(4)
            .and_then(|n| n.checked_add(cache_pages * (POSTING_PAGE_BYTES + size_of::<Option<usize>>())))
            .and_then(|n| n.checked_add(size_of::<Self>())).ok_or(SnapshotIndexError::Limits)?;
        let lease = budget.try_reserve_managed(directory.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| SnapshotIndexError::ResourceDenied)?;
        let mut manifest = reserve(manifest_len)?; manifest.resize(manifest_len, 0);
        read_exact(&mut input, &mut manifest, &mut io, false, &mut canceled)?;
        if Sha256::digest(&manifest) != pin { return Err(SnapshotIndexError::PinMismatch.into()); }
        if manifest[10..16] != [0; 6] { return Err(SnapshotIndexError::Version.into()); }
        let payload = u64::from_le_bytes(manifest[16..24].try_into().unwrap());
        if payload != (manifest_len - FRAME_LEN) as u64 { return Err(SnapshotIndexError::Format.into()); }
        let limits = EnvelopeLimits { max_document_bytes: MAX_PAGED_MANIFEST_BYTES,
            max_payload_bytes: MAX_PAGED_MANIFEST_BYTES - FRAME_LEN, max_field_bytes: MAX_PAGED_MANIFEST_BYTES };
        let mut reader = EnvelopeReader::open(&manifest, PAGED_MANIFEST_SCHEMA, limits, UnknownPolicy::Strict)?;
        let archive = get_digest(&mut reader)?;
        if archive != directory.digest() { return Err(SnapshotIndexError::SourceMismatch.into()); }
        if reader.get_u32()? != SEMANTICS || reader.get_u32()? != POSTING_PAGE_PAIRS as u32 {
            return Err(SnapshotIndexError::Version.into());
        }
        let count = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        let total = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        let page_count = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        if count != directory.len() || count > MAX_INDEX_MEMBERS || total > MAX_INDEX_GRAMS
            || page_count != total.div_ceil(POSTING_PAGE_PAIRS) || manifest_length(count, page_count)? != manifest_len
            || file_length(manifest_len, total)? as u64 != file_bytes { return Err(SnapshotIndexError::Limits.into()); }
        let mut rows = reserve(count)?;
        let mut stats = SavedIndexStats { members: count, unique_grams: total, ..Default::default() };
        let mut sum = 0usize;
        for ordinal in 0..count {
            if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            let coverage = Coverage::parse(reader.get_u8()?)?;
            let length = reader.get_u64()?;
            let digest = get_digest(&mut reader)?;
            let n = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
            if n > total.saturating_sub(sum) || (!coverage.indexed() && n != 0) || n as u64 > length.saturating_sub(2) {
                return Err(SnapshotIndexError::Format.into());
            }
            sum += n;
            let member = directory.member(ordinal).ok_or(SnapshotIndexError::SourceMismatch)?;
            if length != member.observed_bytes { return Err(SnapshotIndexError::SourceMismatch.into()); }
            match member.data {
                PagedMemberData::Captured { digest: expected, .. } if coverage != Coverage::Unavailable && digest == expected => {},
                PagedMemberData::Unavailable(_) if coverage == Coverage::Unavailable && digest == Sha256Digest::new([0; 32]) => {},
                _ => return Err(SnapshotIndexError::SourceMismatch.into()),
            }
            match coverage { Coverage::Unavailable => stats.unavailable_files += 1,
                Coverage::Uncovered => stats.uncovered_files += 1, _ => stats.indexed_files += 1 }
            rows.push(Row { coverage, length, digest, start: 0, count: n });
        }
        if sum != total { return Err(SnapshotIndexError::Format.into()); }
        let mut pages: Vec<PageMeta> = reserve(page_count)?;
        for page in 0..page_count {
            if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            let first = reader.get_u64()?; let last = reader.get_u64()?;
            let n = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
            let digest = get_digest(&mut reader)?;
            if n != (total - page * POSTING_PAGE_PAIRS).min(POSTING_PAGE_PAIRS)
                || first > last || (n == 1) != (first == last)
                || pages.last().is_some_and(|old| old.last >= first)
                || !valid_pair(first, &rows) || !valid_pair(last, &rows) { return Err(SnapshotIndexError::Format.into()); }
            pages.push(PageMeta { first, last, count: n, digest });
        }
        reader.finish()?;
        // Seek length checks storage shape without reading an unselected page.
        if input.seek(SeekFrom::End(0)).map_err(|_| PagedPostingError::Io)? != file_bytes {
            return Err(SnapshotIndexError::Format.into());
        }
        let (captured, raw_fallback, text_fallback) = fallbacks(&rows, &mut canceled)?;
        let mut cache = reserve(cache_pages * POSTING_PAGE_BYTES)?; cache.resize(cache_pages * POSTING_PAGE_BYTES, 0);
        let mut slots = reserve(cache_pages)?; slots.resize(cache_pages, None);
        if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
        Ok(Self { input, owner: directory.owner(), archive, pin, rows, pages, captured, raw_fallback, text_fallback,
            total, body_offset: (HEADER_BYTES + manifest_len) as u64, file_bytes, cache, slots, replacement: 0,
            stats, io, failed: None, _lease: lease })
    }
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn digest(&self) -> Sha256Digest { self.pin }
    pub const fn stats(&self) -> SavedIndexStats { self.stats }
    pub const fn io_stats(&self) -> PostingPageIo { self.io }
    pub const fn file_bytes(&self) -> u64 { self.file_bytes }
    pub fn cache_capacity_bytes(&self) -> usize { self.cache.len() }
    pub fn resident_pages(&self) -> usize { self.slots.iter().filter(|slot| slot.is_some()).count() }
    pub fn validate_directory(&self, directory: &SnapshotDirectory) -> Result<(), SnapshotIndexError> {
        if self.owner != directory.owner() { return Err(SnapshotIndexError::OwnerMismatch); }
        if self.archive != directory.digest() || self.rows.len() != directory.len() { return Err(SnapshotIndexError::SourceMismatch); }
        Ok(())
    }
    pub fn raw_candidates(&mut self, needle: &[u8]) -> PagedPostingCandidates<'_> { self.candidates(needle, false) }
    pub fn text_candidates(&mut self, needle: &str) -> PagedPostingCandidates<'_> { self.candidates(needle.as_bytes(), true) }
    pub(crate) fn candidates(&mut self, needle: &[u8], text: bool) -> PagedPostingCandidates<'_> {
        PagedPostingCandidates::new(self, needle, text)
    }
    fn load_page(&mut self, page: usize, slot: usize, canceled: &mut dyn FnMut() -> bool) -> Result<(), PagedPostingError> {
        let meta = self.pages[page];
        if self.slots[slot].take().is_some() { self.io.evictions += 1; }
        seek_to(&mut self.input, self.body_offset + (page * POSTING_PAGE_BYTES) as u64)?;
        let start = slot * POSTING_PAGE_BYTES;
        let bytes = &mut self.cache[start..start + meta.count * 8];
        read_exact(&mut self.input, bytes, &mut self.io, true, canceled)?;
        if Sha256::digest(bytes) != meta.digest { return Err(PagedPostingError::ChangedPage); }
        let mut previous = None;
        for (i, word) in bytes.chunks_exact(8).enumerate() {
            if i % 256 == 0 && canceled() { return Err(SnapshotIndexError::Canceled.into()); }
            let pair = u64::from_le_bytes(word.try_into().unwrap());
            if !valid_pair(pair, &self.rows) || previous.is_some_and(|old| old >= pair)
                || (i == 0 && pair != meta.first) || (i + 1 == meta.count && pair != meta.last) {
                return Err(SnapshotIndexError::Format.into());
            }
            previous = Some(pair);
        }
        if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
        self.slots[slot] = Some(page); self.io.page_loads += 1;
        Ok(())
    }
}

// Private, sealed-by-module storage interface: public callers cannot inject an
// unchecked candidate universe. A cursor borrows the typed validated index.
trait PairSource {
    fn bracket(&self, value: u64) -> Range<usize>;
    fn pair(&mut self, index: usize, loads: &mut usize, canceled: &mut dyn FnMut() -> bool)
        -> Result<Option<u64>, PagedPostingError>;
    fn coverage(&self, ordinal: usize) -> Coverage;
    fn fallback(&self, text: bool, short: bool) -> &[u32];
    fn captured_count(&self) -> usize;
}
impl<R: Read + Seek> PairSource for PagedPostings<R> {
    fn bracket(&self, value: u64) -> Range<usize> {
        let page = self.pages.partition_point(|meta| meta.last < value);
        if page == self.pages.len() { return self.total..self.total; }
        let start = page * POSTING_PAGE_PAIRS;
        // A fence can answer an absent-key boundary without reading that page.
        if value <= self.pages[page].first { start..start }
        else { start..start + self.pages[page].count }
    }
    fn pair(&mut self, index: usize, loads: &mut usize, canceled: &mut dyn FnMut() -> bool)
        -> Result<Option<u64>, PagedPostingError> {
        if let Some(error) = self.failed { return Err(error); }
        if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
        if index >= self.total { return Err(SnapshotIndexError::Format.into()); }
        let page = index / POSTING_PAGE_PAIRS;
        let slot = match self.slots.iter().position(|stored| *stored == Some(page)) {
            Some(slot) => { self.io.cache_hits += 1; slot }
            None => {
                if *loads == 0 { return Ok(None); }
                *loads -= 1;
                let slot = self.replacement; self.replacement = (self.replacement + 1) % self.slots.len();
                if let Err(error) = self.load_page(page, slot, canceled) {
                    if error != PagedPostingError::Index(SnapshotIndexError::Canceled) { self.failed = Some(error); }
                    return Err(error);
                }
                slot
            }
        };
        let start = slot * POSTING_PAGE_BYTES + (index % POSTING_PAGE_PAIRS) * 8;
        Ok(Some(u64::from_le_bytes(self.cache[start..start + 8].try_into().unwrap())))
    }
    fn coverage(&self, ordinal: usize) -> Coverage { self.rows[ordinal].coverage }
    fn fallback(&self, text: bool, short: bool) -> &[u32] {
        if short { &self.captured } else if text { &self.text_fallback } else { &self.raw_fallback }
    }
    fn captured_count(&self) -> usize { self.captured.len() }
}
fn valid_pair(pair: u64, rows: &[Row]) -> bool {
    pair >> 32 <= 0x00ff_ffff && rows.get(ordinal(pair)).is_some_and(|row| row.coverage.indexed() && row.count > 0)
}
fn manifest_length(rows: usize, pages: usize) -> Result<usize, SnapshotIndexError> {
    let length = rows.checked_mul(ROW_BYTES).and_then(|n| pages.checked_mul(PAGE_META_BYTES).and_then(|p| n.checked_add(p)))
        .and_then(|n| n.checked_add(FRAME_LEN + MANIFEST_PREFIX)).ok_or(SnapshotIndexError::Limits)?;
    if rows > MAX_INDEX_MEMBERS || length > MAX_PAGED_MANIFEST_BYTES { return Err(SnapshotIndexError::Limits); }
    Ok(length)
}
fn file_length(manifest: usize, pairs: usize) -> Result<usize, SnapshotIndexError> {
    let length = pairs.checked_mul(8).and_then(|n| n.checked_add(HEADER_BYTES + manifest)).ok_or(SnapshotIndexError::Limits)?;
    if pairs > MAX_INDEX_GRAMS || length > MAX_PAGED_POSTINGS_BYTES { return Err(SnapshotIndexError::Limits); }
    Ok(length)
}
fn seek_to(input: &mut impl Seek, position: u64) -> Result<(), PagedPostingError> {
    if input.seek(SeekFrom::Start(position)).map_err(|_| PagedPostingError::Io)? != position {
        return Err(PagedPostingError::InvalidRead);
    }
    Ok(())
}
fn read_exact(input: &mut impl Read, target: &mut [u8], stats: &mut PostingPageIo, page: bool,
    canceled: &mut dyn FnMut() -> bool) -> Result<(), PagedPostingError> {
    let mut offset = 0;
    let mut calls = 0;
    while offset < target.len() {
        if canceled() { return Err(SnapshotIndexError::Canceled.into()); }
        if calls == MAX_IO_CALLS { return Err(PagedPostingError::ReadCallLimit); }
        calls += 1; stats.read_calls += 1;
        let end = target.len().min(offset + 64 * 1024);
        match input.read(&mut target[offset..end]) {
            Ok(0) => return Err(PagedPostingError::InvalidRead),
            Ok(n) if n <= end - offset => {
                offset += n;
                if page { stats.page_bytes_read += n as u64; } else { stats.manifest_bytes_read += n as u64; }
            }
            Ok(_) => return Err(PagedPostingError::InvalidRead),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(PagedPostingError::Io),
        }
    }
    Ok(())
}
