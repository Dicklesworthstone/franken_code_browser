#![forbid(unsafe_code)]

//! A global inverse of the existing trusted per-member trigram segments.
//! Construction transposes keys, never reparses source or invents coverage.
//! Query preparation performs three binary range lookups and allocates nothing;
//! each step inspects at most one posting and one fallback member. Results stay
//! in original member order so result limits and source identities are stable.
//!
//! FCBO is a standalone, pinned artifact, not a reference to an FCBI file.
//! Reopening validates all metadata and posting entries. This is NOT disk-page
//! demand loading: the bounded posting table is resident. Candidate selection
//! avoids visiting unrelated members, but every candidate still needs exact
//! source verification. Checksums alone never confer negative-certificate trust.

pub mod paged;

use std::ops::Range;
use super::*;

pub const POSTINGS_SCHEMA: EnvelopeSchema = EnvelopeSchema::with_magic(*b"FCBO", 0x504f5332, 1, 0);
pub const MAX_POSTINGS_BYTES: usize = 24 * 1024 * 1024;

/// One u64 packs a u24 gram in its high word and a member ordinal in its low
/// word. Sorting yields gram-major, then ordinal-major order with no duplicates.
/// Fallback lists are separate because incompatible encodings are not negatives.
pub struct SnapshotPostings {
    owner: ArenaOwnerId,
    archive: Sha256Digest,
    rows: Vec<Row>,
    pairs: Vec<u64>,
    captured: Vec<u32>,
    raw_fallback: Vec<u32>,
    text_fallback: Vec<u32>,
    stats: SavedIndexStats,
    _lease: ResourceLease,
}

impl SnapshotIndex {
    /// Explicit worker-side transposition. Old/new arrays overlap and have
    /// independent reservations; dropping either cannot invalidate the other.
    pub fn invert(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<SnapshotPostings, SnapshotIndexError> {
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        let lease = charge(self.owner, self.rows.len(), self.grams.len(), false, budget, allocation)?;
        let mut rows = reserve(self.rows.len())?;
        let mut pairs = reserve(self.grams.len())?;
        for (ordinal, row) in self.rows.iter().enumerate() {
            if canceled() { return Err(SnapshotIndexError::Canceled); }
            rows.push(row.clone());
            for (i, &gram) in self.grams[row.start..row.start + row.count].iter().enumerate() {
                if i % 4096 == 0 && canceled() { return Err(SnapshotIndexError::Canceled); }
                pairs.push(pack(gram, ordinal));
            }
        }
        pairs.sort_unstable(); // Bounded worker operation; no UI callback claim.
        let (captured, raw_fallback, text_fallback) = fallbacks(&rows, &mut canceled)?;
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        Ok(SnapshotPostings { owner: self.owner, archive: self.archive, rows, pairs,
            captured, raw_fallback, text_fallback, stats: self.stats, _lease: lease })
    }
}

impl SnapshotPostings {
    pub const fn owner(&self) -> ArenaOwnerId { self.owner }
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn stats(&self) -> SavedIndexStats { self.stats }
    pub fn posting_count(&self) -> usize { self.pairs.len() }
    /// Format dispatch ONLY. This check establishes no integrity or trust.
    pub fn is_encoded(bytes: &[u8]) -> bool { bytes.starts_with(b"FCBO") }
    pub fn validate_directory(&self, directory: &SnapshotDirectory) -> Result<(), SnapshotIndexError> {
        if self.owner != directory.owner() { return Err(SnapshotIndexError::OwnerMismatch); }
        if self.archive != directory.digest() || self.rows.len() != directory.len() {
            return Err(SnapshotIndexError::SourceMismatch);
        }
        Ok(())
    }
    pub fn raw_candidates(&self, needle: &[u8]) -> PostingCandidates<'_> { self.candidates(needle, false) }
    pub fn text_candidates(&self, needle: &str) -> PostingCandidates<'_> { self.candidates(needle.as_bytes(), true) }
    pub(crate) fn candidates(&self, needle: &[u8], text: bool) -> PostingCandidates<'_> {
        let short = needle.len() < 3;
        let keys = if short { [0; 3] } else {
            let last = needle.len() - 3;
            [0, last / 2, last].map(|i| u32::from_be_bytes([0, needle[i], needle[i + 1], needle[i + 2]]))
        };
        let lists = keys.map(|key| if short { 0..0 } else { self.gram_range(key) });
        let rarest = (0..3).min_by_key(|&i| lists[i].len()).unwrap_or(0);
        let driver = lists[rarest].clone();
        let fallback = if short { &self.captured } else if text { &self.text_fallback } else { &self.raw_fallback };
        PostingCandidates { index: self, keys, lists, driver, fallback, fallback_next: 0,
            text, short, stats: PostingProbeStats { list_lookups: if short { 0 } else { 3 }, ..Default::default() } }
    }
    fn gram_range(&self, gram: u32) -> Range<usize> {
        let start = self.pairs.partition_point(|pair| *pair < pack(gram, 0));
        // u24 maximum + 1 still fits u32, and this boundary cannot overflow u64.
        let end = self.pairs.partition_point(|pair| *pair < pack(gram + 1, 0));
        start..end
    }

    pub fn encode(&self, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<IndexArtifact, SnapshotIndexError> {
        let length = encoded_length(self.rows.len(), self.pairs.len())?;
        let bytes = length.checked_mul(4).and_then(|n| n.checked_add(size_of::<IndexArtifact>()))
            .ok_or(SnapshotIndexError::Limits)?;
        let lease = budget.try_reserve_managed(self.owner, allocation, ByteLength::new(bytes as u64))
            .map_err(|_| SnapshotIndexError::ResourceDenied)?;
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        let mut writer = EnvelopeWriter::new(POSTINGS_SCHEMA);
        put_digest(&mut writer, self.archive);
        writer.put_u32(SEMANTICS); writer.put_u64(self.rows.len() as u64); writer.put_u64(self.pairs.len() as u64);
        for row in &self.rows {
            if canceled() { return Err(SnapshotIndexError::Canceled); }
            writer.put_u8(row.coverage.tag()); writer.put_u64(row.length);
            put_digest(&mut writer, row.digest); writer.put_u64(row.count as u64);
        }
        for (i, &pair) in self.pairs.iter().enumerate() {
            if i % 4096 == 0 && canceled() { return Err(SnapshotIndexError::Canceled); }
            writer.put_u64(pair);
        }
        let bytes = writer.finish();
        if bytes.len() != length { return Err(SnapshotIndexError::Format); }
        if bytes.capacity() > lease.info().bytes().get() as usize { return Err(SnapshotIndexError::ResourceDenied); }
        let digest = Sha256::digest(&bytes);
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        Ok(IndexArtifact { bytes, digest, _lease: lease })
    }

    /// trusted_digest comes from a separately retained trusted build receipt,
    /// not by hashing these input bytes to manufacture a pin. A valid structure
    /// still cannot prove that omitted grams were never present in source.
    pub fn decode_pinned(bytes: &[u8], trusted_digest: Sha256Digest, directory: &SnapshotDirectory,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<Self, SnapshotIndexError> {
        Self::decode(bytes, trusted_digest, directory, false, budget, allocation, canceled)
    }
    fn decode(bytes: &[u8], trusted_digest: Sha256Digest, directory: &SnapshotDirectory,
        conversion_headroom: bool, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, SnapshotIndexError> {
        if bytes.len() < FRAME_LEN + PREFIX_BYTES || bytes.len() > MAX_POSTINGS_BYTES { return Err(SnapshotIndexError::Limits); }
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        if Sha256::digest(bytes) != trusted_digest { return Err(SnapshotIndexError::PinMismatch); }
        if bytes[10..16] != [0; 6] { return Err(SnapshotIndexError::Version); }
        let payload = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| SnapshotIndexError::Format)?))
            .map_err(|_| SnapshotIndexError::Limits)?;
        if payload != bytes.len() - FRAME_LEN { return Err(SnapshotIndexError::Format); }
        let limits = EnvelopeLimits { max_document_bytes: MAX_POSTINGS_BYTES,
            max_payload_bytes: MAX_POSTINGS_BYTES - FRAME_LEN, max_field_bytes: MAX_POSTINGS_BYTES };
        let mut reader = EnvelopeReader::open(bytes, POSTINGS_SCHEMA, limits, UnknownPolicy::Strict)?;
        let archive = get_digest(&mut reader)?;
        if archive != directory.digest() { return Err(SnapshotIndexError::SourceMismatch); }
        if reader.get_u32()? != SEMANTICS { return Err(SnapshotIndexError::Version); }
        let count = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        let total = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
        if count != directory.len() || count > MAX_INDEX_MEMBERS || total > MAX_INDEX_GRAMS
            || encoded_length(count, total)? != bytes.len() { return Err(SnapshotIndexError::Limits); }
        let lease = charge(directory.owner(), count, total, conversion_headroom, budget, allocation)?;
        let mut rows = reserve(count)?;
        let mut pairs = reserve(total)?;
        let mut stats = SavedIndexStats { members: count, unique_grams: total, ..Default::default() };
        let mut expected_total = 0usize;
        for ordinal in 0..count {
            if canceled() { return Err(SnapshotIndexError::Canceled); }
            let coverage = Coverage::parse(reader.get_u8()?)?;
            let length = reader.get_u64()?;
            let digest = get_digest(&mut reader)?;
            let n = usize::try_from(reader.get_u64()?).map_err(|_| SnapshotIndexError::Limits)?;
            if n > total.saturating_sub(expected_total) || (!coverage.indexed() && n != 0)
                || n as u64 > length.saturating_sub(2) { return Err(SnapshotIndexError::Format); }
            expected_total += n;
            let member = directory.member(ordinal).ok_or(SnapshotIndexError::SourceMismatch)?;
            if length != member.observed_bytes { return Err(SnapshotIndexError::SourceMismatch); }
            match member.data {
                PagedMemberData::Captured { digest: expected, .. } if coverage != Coverage::Unavailable && digest == expected => {},
                PagedMemberData::Unavailable(_) if coverage == Coverage::Unavailable && digest == Sha256Digest::new([0; 32]) => {},
                _ => return Err(SnapshotIndexError::SourceMismatch),
            }
            match coverage {
                Coverage::Unavailable => stats.unavailable_files += 1,
                Coverage::Uncovered => stats.uncovered_files += 1,
                _ => stats.indexed_files += 1,
            }
            // start temporarily counts actual posting occurrences per member.
            rows.push(Row { coverage, length, digest, start: 0, count: n });
        }
        if expected_total != total { return Err(SnapshotIndexError::Format); }
        let mut previous = None;
        for i in 0..total {
            if i % 4096 == 0 && canceled() { return Err(SnapshotIndexError::Canceled); }
            let pair = reader.get_u64()?;
            let ordinal = ordinal(pair);
            if pair >> 32 > 0x00ff_ffff || ordinal >= count || previous.is_some_and(|p| p >= pair) {
                return Err(SnapshotIndexError::Format);
            }
            let row = &mut rows[ordinal];
            if !row.coverage.indexed() || row.start == row.count { return Err(SnapshotIndexError::Format); }
            row.start += 1;
            previous = Some(pair); pairs.push(pair);
        }
        if rows.iter().any(|row| row.start != row.count) { return Err(SnapshotIndexError::Format); }
        reader.finish()?;
        let (captured, raw_fallback, text_fallback) = fallbacks(&rows, &mut canceled)?;
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        Ok(Self { owner: directory.owner(), archive, rows, pairs, captured, raw_fallback, text_fallback, stats, _lease: lease })
    }

    /// Compatibility for the existing refresh engine. Decodes a pinned inverse
    /// and transposes back without source I/O. The SINGLE reservation is enlarged
    /// before allocation to cover both layouts at once; it follows returned data.
    pub(super) fn decode_segments(bytes: &[u8], pin: Sha256Digest, directory: &SnapshotDirectory,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<SnapshotIndex, SnapshotIndexError> {
        let mut inverse = Self::decode(bytes, pin, directory, true, budget, allocation, &mut canceled)?;
        inverse.pairs.sort_unstable_by_key(|&pair| (ordinal(pair), pair >> 32));
        let mut grams = reserve(inverse.pairs.len())?;
        for (i, &pair) in inverse.pairs.iter().enumerate() {
            if i % 4096 == 0 && canceled() { return Err(SnapshotIndexError::Canceled); }
            grams.push((pair >> 32) as u32);
        }
        let mut start = 0;
        for row in &mut inverse.rows {
            row.start = start; start += row.count;
        }
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        Ok(SnapshotIndex { owner: inverse.owner, archive: inverse.archive, rows: inverse.rows,
            grams, stats: inverse.stats, _lease: inverse._lease })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PostingProbeStats {
    pub list_lookups: usize,
    pub posting_entries_visited: usize,
    pub membership_lookups: usize,
    pub candidates_emitted: usize,
    pub fallback_emitted: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostingStep {
    /// One posting was examined, but did not survive intersection/semantics.
    Pending,
    Candidate { ordinal: usize, decision: IndexDecision },
    Finished,
}

/// Allocation-free intersection of three complete gram lists, ordered by source
/// member. The smallest list is the driver; other lists use binary membership
/// checks, not scans across skipped ordinals. Fallback membership merges into
/// the same ordering. Empty/short needles do not claim index exclusions.
pub struct PostingCandidates<'index> {
    index: &'index SnapshotPostings,
    keys: [u32; 3],
    lists: [Range<usize>; 3],
    driver: Range<usize>,
    fallback: &'index [u32],
    fallback_next: usize,
    text: bool,
    short: bool,
    stats: PostingProbeStats,
}
impl PostingCandidates<'_> {
    pub const fn stats(&self) -> PostingProbeStats { self.stats }
    pub fn is_finished(&self) -> bool { self.driver.is_empty() && self.fallback_next == self.fallback.len() }
    /// Proven total only after the cursor finishes. A truncated query must not
    /// report unvisited candidates as index-excluded source members.
    pub fn excluded_files(&self) -> Option<usize> {
        self.is_finished().then(|| self.index.captured.len() - self.stats.candidates_emitted - self.stats.fallback_emitted)
    }
    pub fn step(&mut self) -> PostingStep {
        let posting = (!self.driver.is_empty()).then(|| ordinal(self.index.pairs[self.driver.start]));
        let fallback = self.fallback.get(self.fallback_next).map(|&n| n as usize);
        if let Some(file) = fallback.filter(|file| posting.is_none_or(|p| *file <= p)) {
            self.fallback_next += 1;
            if posting == Some(file) { self.driver.start += 1; self.stats.posting_entries_visited += 1; }
            self.stats.fallback_emitted += 1;
            return PostingStep::Candidate { ordinal: file, decision: IndexDecision::Fallback };
        }
        let Some(file) = posting else { return PostingStep::Finished; };
        self.driver.start += 1;
        self.stats.posting_entries_visited += 1;
        if self.text && self.index.rows[file].coverage != Coverage::Utf8 { return PostingStep::Pending; }
        debug_assert!(!self.short);
        for (key, list) in self.keys.iter().zip(&self.lists) {
            self.stats.membership_lookups += 1;
            if self.index.pairs[list.clone()].binary_search(&pack(*key, file)).is_err() { return PostingStep::Pending; }
        }
        self.stats.candidates_emitted += 1;
        PostingStep::Candidate { ordinal: file, decision: IndexDecision::Verify }
    }
}

fn pack(gram: u32, ordinal: usize) -> u64 { (u64::from(gram) << 32) | ordinal as u64 }
fn ordinal(pair: u64) -> usize { (pair as u32) as usize }
fn encoded_length(count: usize, postings: usize) -> Result<usize, SnapshotIndexError> {
    let length = count.checked_mul(ROW_BYTES).and_then(|n| n.checked_add(FRAME_LEN + PREFIX_BYTES))
        .and_then(|n| postings.checked_mul(8).and_then(|p| n.checked_add(p))).ok_or(SnapshotIndexError::Limits)?;
    if count > MAX_INDEX_MEMBERS || postings > MAX_INDEX_GRAMS || length > MAX_POSTINGS_BYTES { return Err(SnapshotIndexError::Limits); }
    Ok(length)
}
fn charge(owner: ArenaOwnerId, count: usize, postings: usize, conversion: bool,
    budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<ResourceLease, SnapshotIndexError> {
    encoded_length(count, postings)?;
    let bytes = count.checked_mul(size_of::<Row>() + 3 * size_of::<u32>())
        .and_then(|n| postings.checked_mul(if conversion { 12 } else { 8 }).and_then(|p| n.checked_add(p)))
        .and_then(|n| n.checked_add(size_of::<SnapshotPostings>() + size_of::<SnapshotIndex>()))
        .ok_or(SnapshotIndexError::Limits)?;
    budget.try_reserve_managed(owner, allocation, ByteLength::new(bytes as u64)).map_err(|_| SnapshotIndexError::ResourceDenied)
}
fn fallbacks(rows: &[Row], canceled: &mut impl FnMut() -> bool)
    -> Result<(Vec<u32>, Vec<u32>, Vec<u32>), SnapshotIndexError> {
    let mut captured = reserve(rows.len())?;
    let mut raw = reserve(rows.len())?;
    let mut text = reserve(rows.len())?;
    for (ordinal, row) in rows.iter().enumerate() {
        if canceled() { return Err(SnapshotIndexError::Canceled); }
        if row.coverage != Coverage::Unavailable { captured.push(ordinal as u32); }
        if row.coverage == Coverage::Uncovered { raw.push(ordinal as u32); }
        if matches!(row.coverage, Coverage::Uncovered | Coverage::Bytes) { text.push(ordinal as u32); }
    }
    Ok((captured, raw, text))
}
