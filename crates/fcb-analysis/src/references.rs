#![forbid(unsafe_code)]

//! Exact, whole-token source occurrences for candidate reference navigation.
//!
//! This is NOT compiler reference resolution or a language lexer. Comments and
//! string literals participate. ASCII letters/digits, `_`, and `$` form tokens;
//! non-ASCII non-whitespace/non-control runs are kept intact conservatively so
//! combining marks are not mistaken for identifier boundaries. No claim of
//! complete Unicode identifier recognition, normalization, or name binding is
//! made. Results are labeled `whole-token-text-candidate` on every surface.
//!
//! The complete immutable observation is decoded in bounded windows by
//! fcb-source. Only token coordinates survive a window boundary; there is no
//! source-sized decoded string or provenance map. Original byte ranges,
//! owner/file/revision/query identities, and a source borrow survive publication.
//! Neither lookup nor activation reads a live pathname.

use std::mem::size_of;

use fcb_core::{ByteLength, ByteOffset, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_search::identifier::{IdentifierCursor, MAX_IDENTIFIER_BYTES, token_scalar, validate_identifier};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, SpanKind, StatefulChunkDecoder, detect_encoding};

pub const MAX_REFERENCE_SOURCE_BYTES: usize = 512 * 1024;
pub const MAX_REFERENCE_ITEMS: usize = 4096;
pub const MAX_REFERENCE_NAME_BYTES: usize = MAX_IDENTIFIER_BYTES;
const CANCEL_QUANTUM: usize = 4096;
/// One raw decoder window, with at most three carried bytes from its predecessor.
const DECODE_WINDOW_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceError {
    SourceLimit, InvalidLimits, InvalidName, UnsupportedEncoding, InvalidEvidence,
    ResourceDenied, Canceled, StaleSource, StaleQuery, OwnerMismatch, NotFound,
}
impl ReferenceError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceLimit => "REFERENCE_SOURCE_LIMIT",
            Self::InvalidLimits => "REFERENCE_INVALID_LIMITS",
            Self::InvalidName => "REFERENCE_INVALID_NAME",
            Self::UnsupportedEncoding => "REFERENCE_UNSUPPORTED_ENCODING",
            Self::InvalidEvidence => "REFERENCE_INVALID_EVIDENCE",
            Self::ResourceDenied => "REFERENCE_RESOURCE_DENIED",
            Self::Canceled => "REFERENCE_CANCELED",
            Self::StaleSource => "REFERENCE_STALE_SOURCE",
            Self::StaleQuery => "REFERENCE_STALE_QUERY",
            Self::OwnerMismatch => "REFERENCE_OWNER_MISMATCH",
            Self::NotFound => "REFERENCE_NOT_FOUND",
        }
    }
}
impl std::fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for ReferenceError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceCandidate {
    id: u64,
    original_range: ByteRange,
    decoded_range: DecodedUtf8Range,
    line: u64,
}
impl ReferenceCandidate {
    pub const fn id(&self) -> u64 { self.id }
    pub const fn original_range(&self) -> ByteRange { self.original_range }
    pub const fn decoded_range(&self) -> DecodedUtf8Range { self.decoded_range }
    /// One-based source line, counting LF (including CRLF) line endings.
    pub const fn line(&self) -> u64 { self.line }
    pub const fn evidence_level(&self) -> &'static str { "whole-token-text-candidate" }
}

/// A bounded occurrence collection borrowed from one exact source observation.
/// IDs are collection-local, never persistent symbol identities.
pub struct CapturedReferences<'source> {
    source: &'source [u8],
    request: CaptureRequest,
    generation: QueryGeneration,
    encoding: DetectedEncoding,
    name: String,
    candidates: Vec<ReferenceCandidate>,
    counted: usize,
    limited: bool,
    count_complete: bool,
    _lease: ResourceLease,
}
impl<'source> CapturedReferences<'source> {
    /// Worker-side, bounded complete-capture operation. Up to one additional
    /// occurrence is counted to distinguish an exact cap from truncated output.
    /// A zero cap still establishes whether at least one occurrence exists.
    #[allow(clippy::too_many_arguments)]
    pub fn build(source: &'source [u8], request: CaptureRequest, generation: QueryGeneration,
        name: &str, encoding: Option<DetectedEncoding>, max_items: usize,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<Self, ReferenceError> {
        Self::build_page(source, request, generation, name, encoding, max_items, 0, budget, allocation, canceled)
    }

    /// Decode and validate the SAME complete capture, then skip qualified token
    /// occurrences without retaining them. The cap and cancellation quantum are
    /// unchanged. Candidate IDs remain local to this result window; the count
    /// includes skipped occurrences and at most one later lookahead.
    #[allow(clippy::too_many_arguments)]
    pub fn build_page(source: &'source [u8], request: CaptureRequest, generation: QueryGeneration,
        name: &str, encoding: Option<DetectedEncoding>, max_items: usize, skip_matches: u64,
        budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool) -> Result<Self, ReferenceError> {
        Self::build_window(source, request, generation, name, encoding, max_items, skip_matches,
            false, budget, allocation, canceled)
    }

    /// Count qualified occurrences independently of the retained result window.
    /// Skipped and later occurrences consume matching work, not row storage.
    /// With `count_all_matches`, a successful call validates and scans the whole
    /// capture even when zero rows were requested. Without it, retain the
    /// existing one-lookahead behavior. Candidate IDs remain window-local.
    #[allow(clippy::too_many_arguments)]
    pub fn build_window(source: &'source [u8], request: CaptureRequest, generation: QueryGeneration,
        name: &str, encoding: Option<DetectedEncoding>, max_items: usize, skip_matches: u64,
        count_all_matches: bool, budget: &ResourceBudget, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<Self, ReferenceError> {
        if generation.owner() != request.file().owner() { return Err(ReferenceError::OwnerMismatch); }
        if request.range().is_some() { return Err(ReferenceError::InvalidEvidence); }
        if source.len() > MAX_REFERENCE_SOURCE_BYTES { return Err(ReferenceError::SourceLimit); }
        if max_items > MAX_REFERENCE_ITEMS { return Err(ReferenceError::InvalidLimits); }
        validate_reference_name(name)?;
        if canceled() { return Err(ReferenceError::Canceled); }
        // One decoder window, carried scalar bytes, span/vector growth overlap,
        // decoded text, result storage and the retained name are charged before
        // allocation. Borrowed source bytes retain the caller's source lease.
        // This bound is independent of total source length after one window.
        let charge = source.len().min(DECODE_WINDOW_BYTES).checked_add(4)
            .and_then(|n| n.checked_mul(384))
            .and_then(|n| max_items.checked_mul(size_of::<ReferenceCandidate>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(name.len() + size_of::<Self>()
                + size_of::<ReferenceScan<'_>>() + 4096))
            .ok_or(ReferenceError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ReferenceError::ResourceDenied)?;
        let encoding = encoding.unwrap_or_else(|| detect_encoding(source));
        if encoding == DetectedEncoding::Unsupported { return Err(ReferenceError::UnsupportedEncoding); }
        let mut candidates = Vec::new();
        candidates.try_reserve_exact(max_items).map_err(|_| ReferenceError::ResourceDenied)?;
        if candidates.capacity() > max_items { return Err(ReferenceError::ResourceDenied); }
        let mut owned_name = String::new();
        owned_name.try_reserve_exact(name.len()).map_err(|_| ReferenceError::ResourceDenied)?;
        if owned_name.capacity() > name.len() { return Err(ReferenceError::ResourceDenied); }
        owned_name.push_str(name);
        let mut scan = ReferenceScan {
            cursor: IdentifierCursor::new(name).map_err(|_| ReferenceError::InvalidName)?,
            candidates, max_items, skip_matches, count_all_matches, counted: 0, limited: false,
            token_raw: 0, token_decoded: 0, token_line: 1, line: 1, raw_end: 0, decoded_end: 0,
        };
        let mut decoder = StatefulChunkDecoder::new(encoding);
        for chunk in source.chunks(DECODE_WINDOW_BYTES) {
            if canceled() { return Err(ReferenceError::Canceled); }
            let map = decoder.decode_chunk(chunk).map_err(decoder_error)?;
            if canceled() { return Err(ReferenceError::Canceled); }
            scan.consume(&map, &mut canceled)?;
            // The map and its decoded text die here. No previous map remains
            // alive while the next window's decoder allocations are built.
        }
        if canceled() { return Err(ReferenceError::Canceled); }
        if let Some(map) = decoder.finish().map_err(decoder_error)? {
            scan.consume(&map, &mut canceled)?;
        }
        if scan.raw_end != source.len() as u64 { return Err(ReferenceError::InvalidEvidence); }
        // Only real capture EOF completes a pending token. Chunk ends and
        // canceled work cannot turn a prefix of a longer token into a hit.
        if !scan.matching_stopped() && scan.cursor.finish() {
            scan.record(scan.raw_end, scan.decoded_end)?;
        }
        if canceled() { return Err(ReferenceError::Canceled); }
        Ok(Self { source, request, generation, encoding, name: owned_name, candidates: scan.candidates,
            counted: scan.counted, limited: scan.limited,
            count_complete: count_all_matches || !scan.limited, _lease: lease })
    }

    pub const fn file(&self) -> FileId { self.request.file() }
    pub const fn revision(&self) -> SourceRevision { self.request.revision() }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub fn name(&self) -> &str { &self.name }
    pub fn candidates(&self) -> &[ReferenceCandidate] { &self.candidates }
    /// Observed occurrences, including skipped matches. Legacy builders count
    /// at most one unstored lookahead; an explicit counting window scans to EOF.
    /// Consult is_complete(), separately from output_limited(), for exact totals.
    pub const fn total_matches_counted(&self) -> usize { self.counted }
    /// At least one occurrence follows the retained window. Counting can be
    /// complete while later rows remain omitted from this bounded collection.
    pub const fn output_limited(&self) -> bool { self.limited }
    /// Completeness applies only to the declared whole-token text policy, never
    /// compiler references or the containing repository's membership.
    pub const fn is_complete(&self) -> bool { self.count_complete }
    pub fn candidate(&self, id: u64) -> Option<&ReferenceCandidate> {
        let index = usize::try_from(id.checked_sub(1)?).ok()?;
        self.candidates.get(index)
    }
    pub fn validate_delivery(&self, file: FileId, revision: SourceRevision,
        generation: QueryGeneration) -> Result<(), ReferenceError> {
        if file.owner() != self.file().owner() || revision.owner() != self.file().owner()
            || generation.owner() != self.file().owner() { return Err(ReferenceError::OwnerMismatch); }
        if file != self.file() || revision != self.revision() { return Err(ReferenceError::StaleSource); }
        if generation != self.generation { return Err(ReferenceError::StaleQuery); }
        Ok(())
    }
    pub fn validate_source(&self, source: &[u8], file: FileId, revision: SourceRevision,
        generation: QueryGeneration) -> Result<(), ReferenceError> {
        self.validate_delivery(file, revision, generation)?;
        if self.source != source { return Err(ReferenceError::StaleSource); }
        Ok(())
    }
    pub fn source_bytes(&self, id: u64) -> Result<&'source [u8], ReferenceError> {
        let range = self.candidate(id).ok_or(ReferenceError::NotFound)?.original_range;
        let (start, end) = range.as_usize_bounds().map_err(|_| ReferenceError::InvalidEvidence)?;
        self.source.get(start..end).ok_or(ReferenceError::InvalidEvidence)
    }
}

/// Matching state is constant-sized apart from the admitted result rows. A token
/// can span arbitrarily many decoding windows without retaining its source text
/// or any mapping spans. The shared cursor owns only a query position and flags.
struct ReferenceScan<'name> {
    cursor: IdentifierCursor<'name>,
    candidates: Vec<ReferenceCandidate>,
    max_items: usize,
    skip_matches: u64,
    count_all_matches: bool,
    counted: usize,
    limited: bool,
    token_raw: u64,
    token_decoded: u64,
    token_line: u64,
    line: u64,
    raw_end: u64,
    decoded_end: u64,
}

impl ReferenceScan<'_> {
    fn matching_stopped(&self) -> bool { self.limited && !self.count_all_matches }

    fn consume(&mut self, map: &CaptureEncodingMap, canceled: &mut impl FnMut() -> bool)
        -> Result<(), ReferenceError> {
        // Preserve the old complete-capture validation contract even after the
        // retained window plus lookahead is full. A malformed suffix rejects
        // the whole attempt instead of publishing apparently valid early rows.
        if map.spans().iter().any(|span| matches!(span.kind,
            SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
            return Err(ReferenceError::UnsupportedEncoding);
        }
        if map.base_raw_offset() != self.raw_end || map.base_decoded_offset() != self.decoded_end {
            return Err(ReferenceError::InvalidEvidence);
        }
        self.raw_end = map.base_raw_offset().checked_add(map.total_raw_bytes().get())
            .ok_or(ReferenceError::InvalidEvidence)?;
        self.decoded_end = map.base_decoded_offset().checked_add(map.total_decoded_bytes())
            .ok_or(ReferenceError::InvalidEvidence)?;
        if self.matching_stopped() { return Ok(()); }
        let mut checkpoint = 0usize;
        for (offset, scalar) in map.decoded_text().char_indices() {
            if offset - checkpoint >= CANCEL_QUANTUM {
                if canceled() { return Err(ReferenceError::Canceled); }
                checkpoint = offset;
            }
            let decoded = map.base_decoded_offset().checked_add(offset as u64)
                .ok_or(ReferenceError::InvalidEvidence)?;
            if !self.cursor.in_token() && token_scalar(scalar) {
                self.token_raw = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(decoded))
                    .map_err(|_| ReferenceError::InvalidEvidence)?.get();
                self.token_decoded = decoded;
                self.token_line = self.line;
            }
            if self.cursor.push(scalar) {
                let raw = map.decoded_utf8_to_byte_offset(DecodedUtf8Offset::new(decoded))
                    .map_err(|_| ReferenceError::InvalidEvidence)?.get();
                self.record(raw, decoded)?;
                if self.matching_stopped() { break; }
            }
            if scalar == '\n' {
                self.line = self.line.checked_add(1).ok_or(ReferenceError::InvalidEvidence)?;
            }
        }
        Ok(())
    }

    fn record(&mut self, raw_end: u64, decoded_end: u64) -> Result<(), ReferenceError> {
        self.counted = self.counted.checked_add(1).ok_or(ReferenceError::InvalidEvidence)?;
        if self.counted as u64 <= self.skip_matches { return Ok(()); }
        if self.candidates.len() == self.max_items {
            self.limited = true;
            return Ok(());
        }
        let original_range = ByteRange::new(ByteOffset::new(self.token_raw), ByteOffset::new(raw_end))
            .map_err(|_| ReferenceError::InvalidEvidence)?;
        let decoded_range = DecodedUtf8Range::new(DecodedUtf8Offset::new(self.token_decoded),
            DecodedUtf8Offset::new(decoded_end)).map_err(|_| ReferenceError::InvalidEvidence)?;
        self.candidates.push(ReferenceCandidate { id: self.candidates.len() as u64 + 1,
            original_range, decoded_range, line: self.token_line });
        Ok(())
    }
}

fn decoder_error(error: fcb_source::SourceError) -> ReferenceError {
    match error {
        fcb_source::SourceError::PayloadTooLarge => ReferenceError::ResourceDenied,
        fcb_source::SourceError::RangeOutOfBounds => ReferenceError::InvalidEvidence,
        _ => ReferenceError::UnsupportedEncoding,
    }
}

/// Validate the exact query token without reading source or allocating memory.
pub fn validate_reference_name(name: &str) -> Result<(), ReferenceError> {
    validate_identifier(name).map_err(|_| ReferenceError::InvalidName)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ArenaOwnerId, ByteOffset};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(876).unwrap() }
    fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
    fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).unwrap() }
    fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
    fn build<'a>(bytes: &'a [u8], name: &str, max: usize) -> CapturedReferences<'a> {
        let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
        CapturedReferences::build(bytes, CaptureRequest::new(file(), revision()).unwrap(), generation(),
            name, None, max, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
    }
    #[test]
    fn whole_tokens_not_substrings_and_final_token_is_kept() {
        let refs = build(b"foo foobar _foo foo_ $foo 1foo foo", "foo", 20);
        assert_eq!(refs.candidates().len(), 2);
        assert_eq!(refs.source_bytes(1).unwrap(), b"foo");
        assert_eq!(refs.source_bytes(2).unwrap(), b"foo");
        assert!(refs.is_complete());
        assert_eq!(refs.candidates()[1].original_range().end().get(), 34);
    }
    #[test]
    fn comments_and_literals_are_honestly_text_candidates() {
        let refs = build(b"foo(); // foo\n\"foo\"", "foo", 20);
        assert_eq!(refs.candidates().len(), 3);
        assert_eq!(refs.candidates()[2].line(), 2);
        assert!(refs.candidates().iter().all(|hit| hit.evidence_level() == "whole-token-text-candidate"));
    }
    #[test]
    fn exact_cap_is_complete_but_extra_occurrence_is_truncated() {
        let exact = build(b"foo foo", "foo", 2);
        assert!(exact.is_complete());
        assert_eq!(exact.total_matches_counted(), 2);
        let limited = build(b"foo foo foo foo", "foo", 2);
        assert!(limited.output_limited());
        assert_eq!(limited.candidates().len(), 2);
        assert_eq!(limited.total_matches_counted(), 3);
    }
    #[test]
    fn later_window_preserves_original_ranges_lines_and_collection_local_ids() {
        let bytes = b"foo\nfoo\nfoo";
        let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
        let refs = CapturedReferences::build_page(bytes, CaptureRequest::new(file(), revision()).unwrap(),
            generation(), "foo", None, 1, 2, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        assert!(refs.is_complete());
        assert_eq!(refs.total_matches_counted(), 3);
        assert_eq!(refs.candidates().len(), 1);
        assert_eq!(refs.candidates()[0].id(), 1);
        assert_eq!(refs.candidates()[0].line(), 3);
        assert_eq!(refs.candidates()[0].original_range().start().get(), 8);
        assert_eq!(refs.source_bytes(1).unwrap(), b"foo");
    }

    #[test]
    fn counted_windows_pass_the_row_cap_and_preserve_utf16_source_coordinates() {
        for little in [true, false] {
            let text = format!("\u{feff}😀\n{}", "foo\r\n".repeat(5003));
            let bytes: Vec<_> = text.encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
            let expected: Vec<_> = "foo".encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
            let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
            let refs = CapturedReferences::build_window(&bytes,
                CaptureRequest::new(file(), revision()).unwrap(), generation(), "foo", None,
                2, 5000, true, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
            assert_eq!(refs.total_matches_counted(), 5003);
            assert_eq!(refs.candidates().len(), 2);
            assert!(refs.is_complete() && refs.output_limited());
            for (ordinal, candidate) in refs.candidates().iter().enumerate() {
                assert_eq!(candidate.id(), ordinal as u64 + 1);
                assert_eq!(candidate.line(), 5002 + ordinal as u64);
                assert_eq!(candidate.original_range().start().get(), 50_008 + ordinal as u64 * 10);
                assert_eq!(candidate.original_range().end().get(), 50_014 + ordinal as u64 * 10);
                assert_eq!(refs.source_bytes(candidate.id()).unwrap(), expected);
            }
        }
    }

    #[test]
    fn counted_zero_storage_and_full_width_offsets_still_observe_all_matches() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(1024 * 1024)).unwrap();
        for offset in [0, u64::MAX] {
            let refs = CapturedReferences::build_window(b"foo foo foo",
                CaptureRequest::new(file(), revision()).unwrap(), generation(), "foo", None,
                0, offset, true, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
            assert!(refs.is_complete());
            assert!(refs.candidates().is_empty());
            assert_eq!(refs.total_matches_counted(), 3);
            assert_eq!(refs.output_limited(), offset == 0);
        }
    }

    #[test]
    fn counted_tail_cancellation_releases_admission_and_malformed_tails_are_rejected() {
        let bytes = "foo ".repeat(5000);
        let budget = ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap();
        let request = CaptureRequest::new(file(), revision()).unwrap();
        let allocation = ResourceAllocationId::new(1).unwrap();
        let mut polls = 0;
        let canceled = CapturedReferences::build_window(bytes.as_bytes(), request, generation(),
            "foo", None, 1, 0, true, &budget, allocation, || { polls += 1; polls == 3 });
        assert!(matches!(canceled, Err(ReferenceError::Canceled)));
        assert_eq!(polls, 3);
        let refs = CapturedReferences::build_window(bytes.as_bytes(), request, generation(),
            "foo", None, 1, 0, true, &budget, allocation, || false).unwrap();
        assert_eq!(refs.total_matches_counted(), 5000);
        assert!(refs.is_complete());
        drop(refs);
        for offset in [0, u64::MAX] {
            assert!(matches!(CapturedReferences::build_window(b"foo foo\xff", request, generation(),
                "foo", None, 0, offset, true, &budget, allocation, || false),
                Err(ReferenceError::UnsupportedEncoding)));
        }
    }

    #[test]
    fn zero_cap_counts_lookahead_without_allocating_a_result() {
        let hit = build(b"foo", "foo", 0);
        assert!(hit.output_limited());
        assert!(hit.candidates().is_empty());
        assert_eq!(hit.total_matches_counted(), 1);
        assert!(build(b"foobar", "foo", 0).is_complete());
    }
    #[test]
    fn combining_marks_and_supplementary_scalars_do_not_split_tokens() {
        let refs = build("cafe\u{301} cafe λ λx 𐐀 𐐀x".as_bytes(), "cafe", 20);
        assert_eq!(refs.candidates().len(), 1);
        assert_eq!(build("λ λx".as_bytes(), "λ", 20).candidates().len(), 1);
        assert_eq!(build("𐐀 𐐀x".as_bytes(), "𐐀", 20).candidates().len(), 1);
        assert_eq!(build("cafe\u{301}".as_bytes(), "cafe\u{301}", 20).candidates().len(), 1);
    }
    #[test]
    fn bom_and_crlf_keep_exact_original_coordinates() {
        let bytes = b"\xef\xbb\xbffoo\r\nfoo";
        let refs = build(bytes, "foo", 20);
        assert_eq!(refs.candidates()[0].original_range().start().get(), 3);
        assert_eq!(refs.candidates()[1].original_range().start().get(), 8);
        assert_eq!(refs.candidates()[1].line(), 2);
    }
    #[test]
    fn utf16_both_endians_preserve_surrogate_and_identifier_ranges() {
        for little in [true, false] {
            let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in "𐐀();\r\nfoo foo_ foo".encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            let refs = build(&bytes, "foo", 20);
            assert_eq!(refs.candidates().len(), 2);
            assert_eq!(refs.candidates()[0].line(), 2);
            let expected: Vec<u8> = "foo".encode_utf16().flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect();
            assert_eq!(refs.source_bytes(1).unwrap(), expected.as_slice());
            assert_eq!(refs.candidates()[0].original_range().start().get(), 16);
        }
    }
    #[test]
    fn malformed_text_cannot_publish_a_complete_negative_result() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(1024 * 1024)).unwrap();
        for bytes in [&b"foo\xff"[..], &b"\xff\xfe\x00\xd8"[..]] {
            let result = CapturedReferences::build(bytes, CaptureRequest::new(file(), revision()).unwrap(), generation(),
                "foo", None, 5, &budget, ResourceAllocationId::new(1).unwrap(), || false);
            assert!(matches!(result, Err(ReferenceError::UnsupportedEncoding)));
        }
    }
    #[test]
    fn stale_query_revision_owner_and_reused_bytes_are_rejected() {
        let refs = build(b"foo", "foo", 1);
        assert_eq!(refs.validate_source(b"bar", file(), revision(), generation()), Err(ReferenceError::StaleSource));
        assert_eq!(refs.validate_delivery(file(), SourceRevision::new(owner(), 2).unwrap(), generation()), Err(ReferenceError::StaleSource));
        assert_eq!(refs.validate_delivery(file(), revision(), QueryGeneration::new(owner(), 2).unwrap()), Err(ReferenceError::StaleQuery));
        let other = ArenaOwnerId::new(877).unwrap();
        assert_eq!(refs.validate_delivery(FileId::new(other, 1).unwrap(), revision(), generation()), Err(ReferenceError::OwnerMismatch));
        assert_eq!(refs.source_bytes(0), Err(ReferenceError::NotFound));
        assert_eq!(refs.source_bytes(2), Err(ReferenceError::NotFound));
    }
    #[test]
    fn extent_is_not_promoted_to_complete_capture() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(1024 * 1024)).unwrap();
        let range = ByteRange::new(ByteOffset::new(5), ByteOffset::new(8)).unwrap();
        let request = CaptureRequest::new(file(), revision()).unwrap().with_range(range).unwrap();
        let result = CapturedReferences::build(b"foo", request, generation(), "foo", None, 1,
            &budget, ResourceAllocationId::new(1).unwrap(), || false);
        assert!(matches!(result, Err(ReferenceError::InvalidEvidence)));
    }
    #[test]
    fn invalid_names_limits_admission_and_cancellation_fail_explicitly() {
        let budget = ResourceBudget::new(owner(), ByteLength::new(1024 * 1024)).unwrap();
        let request = CaptureRequest::new(file(), revision()).unwrap();
        for name in ["", "foo bar", "foo.bar", "2foo"] {
            assert!(matches!(CapturedReferences::build(b"foo", request, generation(), name, None, 1,
                &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(ReferenceError::InvalidName)));
        }
        assert!(matches!(CapturedReferences::build(b"foo", request, generation(), "foo", None, MAX_REFERENCE_ITEMS + 1,
            &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(ReferenceError::InvalidLimits)));
        assert!(matches!(CapturedReferences::build(b"foo", request, generation(), "foo", None, 1,
            &budget, ResourceAllocationId::new(1).unwrap(), || true), Err(ReferenceError::Canceled)));
        let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
        assert!(matches!(CapturedReferences::build(b"foo", request, generation(), "foo", None, 1,
            &denied, ResourceAllocationId::new(1).unwrap(), || false), Err(ReferenceError::ResourceDenied)));
    }
    #[test]
    fn bomless_utf16_uses_the_explicit_decoder() {
        let bytes: Vec<u8> = "foo foo_ foo".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let budget = ResourceBudget::new(owner(), ByteLength::new(1024 * 1024)).unwrap();
        let refs = CapturedReferences::build(&bytes, CaptureRequest::new(file(), revision()).unwrap(), generation(),
            "foo", Some(DetectedEncoding::Utf16Le), 10, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        assert_eq!(refs.candidates().len(), 2);
        assert_eq!(refs.candidates()[0].original_range().start().get(), 0);
        assert_eq!(refs.candidates()[0].original_range().end().get(), 6);
    }
    #[test]
    fn source_larger_than_the_outline_guard_remains_navigable() {
        let mut bytes = vec![b' '; 128 * 1024];
        bytes.extend_from_slice(b"foo");
        let refs = build(&bytes, "foo", 2);
        assert_eq!(refs.candidates().len(), 1);
        assert_eq!(refs.candidates()[0].original_range().start().get(), 128 * 1024);
        assert!(refs.is_complete());
    }
    #[test]
    fn oversized_sources_and_foreign_queries_fail_before_decoding() {
        let bytes = vec![b' '; MAX_REFERENCE_SOURCE_BYTES + 1];
        let budget = ResourceBudget::new(owner(), ByteLength::new(1024 * 1024)).unwrap();
        let request = CaptureRequest::new(file(), revision()).unwrap();
        assert!(matches!(CapturedReferences::build(&bytes, request, generation(), "foo", None, 1,
            &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(ReferenceError::SourceLimit)));
        let other = QueryGeneration::new(ArenaOwnerId::new(877).unwrap(), 1).unwrap();
        assert!(matches!(CapturedReferences::build(b"foo", request, other, "foo", None, 1,
            &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(ReferenceError::OwnerMismatch)));
    }
    #[test]
    fn case_sensitive_names_and_empty_sources_are_unambiguous() {
        assert_eq!(build(b"Foo foo FOO", "foo", 20).candidates().len(), 1);
        let empty = build(b"", "foo", 0);
        assert!(empty.is_complete());
        assert_eq!(empty.total_matches_counted(), 0);
        assert!(empty.candidates().is_empty());
    }
    #[test]
    fn cancellation_is_observed_during_long_tokens() {
        let bytes = vec![b'x'; 32 * 1024];
        let budget = ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap();
        let mut polls = 0;
        let result = CapturedReferences::build(&bytes, CaptureRequest::new(file(), revision()).unwrap(), generation(),
            "foo", None, 1, &budget, ResourceAllocationId::new(1).unwrap(), || { polls += 1; polls == 3 });
        assert!(matches!(result, Err(ReferenceError::Canceled)));
        assert_eq!(polls, 3);
    }
}

#[cfg(test)]
mod bounded_window_tests {
    //! The occurrence matcher must not need a whole decoded source map. These
    //! public-route regressions compare bounded decoding against an independent
    //! whole-map oracle, and exercise admission with caller-owned source bytes.

    use crate::references::{CapturedReferences, ReferenceError, MAX_REFERENCE_SOURCE_BYTES};
    use fcb_core::{ArenaOwnerId, ByteLength, DecodedUtf8Offset, DecodedUtf8Range, FileId,
        QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
    use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding};

    const SCRATCH_BUDGET: u64 = 2 * 1024 * 1024;
    const WINDOW: usize = 4096;

    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(987).unwrap() }
    fn request() -> CaptureRequest {
        CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
    }
    fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
    fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(SCRATCH_BUDGET)).unwrap() }

    fn query<'a>(bytes: &'a [u8], name: &str, limit: usize, skip: u64, count: bool)
        -> CapturedReferences<'a> {
        CapturedReferences::build_window(bytes, request(), generation(), name, None, limit, skip, count,
            &budget(), allocation(), || false).unwrap()
    }

    fn encode(text: &str, encoding: DetectedEncoding) -> Vec<u8> {
        match encoding {
            DetectedEncoding::Utf8 { has_bom } => {
                let mut bytes = if has_bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
                bytes.extend_from_slice(text.as_bytes());
                bytes
            }
            DetectedEncoding::Utf16Le | DetectedEncoding::Utf16Be => {
                let little = encoding == DetectedEncoding::Utf16Le;
                let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
                }
                bytes
            }
            DetectedEncoding::Unsupported => unreachable!(),
        }
    }

    fn encodings() -> [DetectedEncoding; 4] {
        [DetectedEncoding::Utf8 { has_bom: false }, DetectedEncoding::Utf8 { has_bom: true },
            DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be]
    }

    // This oracle uses whole-text literal locations and neighboring scalar tests,
    // never the production streaming token cursor or its carried coordinates.
    fn token(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '$')
            || (!c.is_ascii() && !c.is_whitespace() && !c.is_control())
    }
    fn oracle(bytes: &[u8], name: &str) -> Vec<(u64, u64, u64, u64, u64)> {
        let map = CaptureEncodingMap::build(bytes).unwrap();
        let text = map.decoded_text();
        text.match_indices(name).filter_map(|(start, _)| {
            let end = start + name.len();
            if text[..start].chars().next_back().is_some_and(token)
                || text[end..].chars().next().is_some_and(token) { return None; }
            let decoded = DecodedUtf8Range::new(DecodedUtf8Offset::new(start as u64),
                DecodedUtf8Offset::new(end as u64)).unwrap();
            let original = map.decoded_utf8_range_to_byte_range(decoded).unwrap();
            let line = text[..start].bytes().filter(|byte| *byte == b'\n').count() as u64 + 1;
            Some((original.start().get(), original.end().get(), start as u64, end as u64, line))
        }).collect()
    }

    #[test]
    fn scalar_and_token_splits_match_a_whole_capture_oracle() {
        let name = "fo𐐀o\u{301}";
        for encoding in encodings() {
            // UTF-8 byte and UTF-16 unit alignment both move across 4 KiB edges.
            for prefix in [WINDOW / 2 - 8, WINDOW - 8] {
                for delta in 0..16 {
                    let text = format!("{}\r\n{name} {name}x x{name} {name}\r\n{name}\n{name}",
                        " ".repeat(prefix + delta));
                    let bytes = encode(&text, encoding);
                    let expected = oracle(&bytes, name);
                    assert_eq!(expected.len(), 4);
                    let refs = query(&bytes, name, 8, 0, true);
                    assert!(refs.is_complete());
                    assert!(!refs.output_limited());
                    assert_eq!(refs.total_matches_counted(), expected.len());
                    assert_eq!(refs.candidates().len(), expected.len());
                    for (index, (hit, expected)) in refs.candidates().iter().zip(&expected).enumerate() {
                        assert_eq!(hit.id(), index as u64 + 1);
                        assert_eq!((hit.original_range().start().get(), hit.original_range().end().get(),
                            hit.decoded_range().start().get(), hit.decoded_range().end().get(), hit.line()),
                            *expected, "{encoding:?}, prefix={prefix}, delta={delta}");
                        assert_eq!(refs.source_bytes(hit.id()).unwrap(),
                            &bytes[expected.0 as usize..expected.1 as usize]);
                    }
                    let later = query(&bytes, name, 1, 2, false);
                    assert_eq!(later.total_matches_counted(), 4);
                    assert!(later.output_limited() && !later.is_complete());
                    let hit = &later.candidates()[0];
                    assert_eq!(hit.id(), 1);
                    assert_eq!((hit.original_range().start().get(), hit.original_range().end().get()),
                        (expected[2].0, expected[2].1));
                }
            }
        }
    }

    #[test]
    fn maximum_admitted_source_uses_bounded_decoder_reservation() {
        // Source backing belongs to the caller. The 2 MiB analysis admission cannot
        // cover the former 384-times-whole-source map at either source ceiling.
        let mut bytes = vec![b' '; MAX_REFERENCE_SOURCE_BYTES];
        let start = bytes.len() - 3;
        bytes[start..].copy_from_slice(b"foo");
        let refs = query(&bytes, "foo", 1, 0, true);
        assert!(refs.is_complete());
        assert_eq!(refs.total_matches_counted(), 1);
        assert_eq!(refs.candidates()[0].original_range().start().get(), start as u64);
        assert_eq!(refs.source_bytes(1).unwrap(), b"foo");
    }

    #[test]
    fn long_nonmatching_tokens_and_window_ends_never_become_hits() {
        let mut bytes = vec![b' '; WINDOW - 3];
        bytes.extend_from_slice(b"foo"); // A decoder boundary is not a delimiter.
        bytes.extend(std::iter::repeat_n(b'x', WINDOW * 17));
        bytes.extend_from_slice(b"foo foo");
        let refs = query(&bytes, "foo", 1, 0, true);
        assert!(refs.is_complete());
        assert_eq!(refs.total_matches_counted(), 1);
        assert_eq!(refs.candidates()[0].original_range().start().get(), bytes.len() as u64 - 3);
    }

    #[test]
    fn an_interior_bom_at_a_decoder_boundary_stays_inside_the_token() {
        for encoding in encodings() {
            let (bom, unit) = match encoding {
                DetectedEncoding::Utf8 { has_bom } => (usize::from(has_bom) * 3, 1),
                _ => (2, 2),
            };
            let prefix = (WINDOW - bom) / unit - 3;
            let text = format!("{}foo\u{feff} foo", " ".repeat(prefix));
            let bytes = encode(&text, encoding);
            let expected = oracle(&bytes, "foo");
            assert_eq!(expected.len(), 1);
            let refs = query(&bytes, "foo", 4, 0, true);
            assert_eq!(refs.total_matches_counted(), 1);
            assert_eq!(refs.candidates()[0].original_range().start().get(), expected[0].0);
            assert_eq!(refs.candidates()[0].original_range().end().get(), expected[0].1);
        }
    }

    #[test]
    fn later_counted_windows_skip_storage_and_keep_exact_lines() {
        let bytes = format!("{}foo", "foo\n".repeat(20_003)).into_bytes();
        let refs = query(&bytes, "foo", 2, 20_000, true);
        assert!(refs.is_complete() && refs.output_limited());
        assert_eq!(refs.total_matches_counted(), 20_004);
        assert_eq!(refs.candidates().len(), 2);
        for (index, hit) in refs.candidates().iter().enumerate() {
            assert_eq!(hit.id(), index as u64 + 1);
            assert_eq!(hit.original_range().start().get(), 80_000 + index as u64 * 4);
            assert_eq!(hit.line(), 20_001 + index as u64);
        }
        let bounded = query(&bytes, "foo", 2, 20_000, false);
        assert_eq!(bounded.total_matches_counted(), 20_003);
        assert!(bounded.output_limited() && !bounded.is_complete());
        let only_count = query(&bytes, "foo", 0, u64::MAX, true);
        assert!(only_count.is_complete() && !only_count.output_limited());
        assert!(only_count.candidates().is_empty());
        assert_eq!(only_count.total_matches_counted(), 20_004);
    }

    #[test]
    fn malformed_suffixes_after_lookahead_or_at_true_eof_reject_the_attempt() {
        for encoding in encodings() {
            let text = format!("foo foo {}", " ".repeat(WINDOW * 3));
            let valid = encode(&text, encoding);
            let tails: &[&[u8]] = match encoding {
                DetectedEncoding::Utf16Le => &[&[0x00, 0xd8], &[0x61]],
                DetectedEncoding::Utf16Be => &[&[0xd8, 0x00], &[0x00]],
                _ => &[&[0xff], &[0xf0, 0x90, 0x80]],
            };
            for tail in tails {
                let mut bytes = valid.clone();
                bytes.extend_from_slice(tail);
                for count_all in [false, true] {
                    for limit in [0, 1] {
                        let result = CapturedReferences::build_window(&bytes, request(), generation(), "foo",
                            None, limit, 0, count_all, &budget(), allocation(), || false);
                        assert!(matches!(result, Err(ReferenceError::UnsupportedEncoding)),
                            "{encoding:?}, count={count_all}, limit={limit}");
                    }
                }
            }
        }
    }

    #[test]
    fn cancellation_after_lookahead_releases_the_same_admission_for_retry() {
        let bytes = format!("foo foo {}", " ".repeat(WINDOW * 8)).into_bytes();
        let budget = budget();
        let mut polls = 0;
        let result = CapturedReferences::build_window(&bytes, request(), generation(), "foo",
            None, 1, 0, false, &budget, allocation(), || { polls += 1; polls == 7 });
        assert!(matches!(result, Err(ReferenceError::Canceled)));
        assert_eq!(polls, 7);
        let retried = CapturedReferences::build_window(&bytes, request(), generation(), "foo",
            None, 1, 0, false, &budget, allocation(), || false).unwrap();
        assert_eq!(retried.total_matches_counted(), 2);
        assert!(retried.output_limited() && !retried.is_complete());
    }

    #[test]
    fn empty_and_bom_only_sources_have_no_invented_final_token() {
        for encoding in encodings() {
            let bytes = encode("", encoding);
            let refs = query(&bytes, "foo", 0, 0, true);
            assert!(refs.is_complete() && !refs.output_limited());
            assert_eq!(refs.total_matches_counted(), 0);
            assert!(refs.candidates().is_empty());
        }
    }

    #[test]
    fn explicit_bomless_utf16_retains_global_coordinates() {
        for little in [true, false] {
            let text = format!("{}𐐀foo foo", " ".repeat(WINDOW / 2 - 1));
            let bytes: Vec<u8> = text.encode_utf16()
                .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
            let encoding = if little { DetectedEncoding::Utf16Le } else { DetectedEncoding::Utf16Be };
            let refs = CapturedReferences::build_window(&bytes, request(), generation(), "foo",
                Some(encoding), 1, 0, true, &budget(), allocation(), || false).unwrap();
            assert!(refs.is_complete());
            assert_eq!(refs.total_matches_counted(), 1);
            assert_eq!(refs.candidates()[0].original_range().start().get(), bytes.len() as u64 - 6);
            assert_eq!(refs.candidates()[0].decoded_range().end().get(), text.len() as u64);
        }
    }
}
