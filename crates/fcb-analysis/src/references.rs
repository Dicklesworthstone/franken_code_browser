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
//! The complete immutable observation is decoded once by fcb-source. Original
//! byte ranges, owner/file/revision/query identities, and a source borrow survive
//! publication. Neither lookup nor activation reads a live pathname.

use std::mem::size_of;

use fcb_core::{ByteLength, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId,
    QueryGeneration, ResourceAllocationId, ResourceBudget, ResourceLease, SourceRevision};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, SpanKind, detect_encoding};

pub const MAX_REFERENCE_SOURCE_BYTES: usize = 512 * 1024;
pub const MAX_REFERENCE_ITEMS: usize = 4096;
pub const MAX_REFERENCE_NAME_BYTES: usize = 1024;
const CANCEL_QUANTUM: usize = 4096;

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
        mut canceled: impl FnMut() -> bool) -> Result<Self, ReferenceError> {
        if generation.owner() != request.file().owner() { return Err(ReferenceError::OwnerMismatch); }
        if request.range().is_some() { return Err(ReferenceError::InvalidEvidence); }
        if source.len() > MAX_REFERENCE_SOURCE_BYTES { return Err(ReferenceError::SourceLimit); }
        if max_items > MAX_REFERENCE_ITEMS { return Err(ReferenceError::InvalidLimits); }
        validate_name(name)?;
        if canceled() { return Err(ReferenceError::Canceled); }
        // Covers decoder spans (including old/new Vec capacity overlap), decoded
        // text, result storage and the retained name before any owned allocation.
        // Borrowed source bytes retain the caller's separate source reservation.
        let charge = source.len().checked_add(1).and_then(|n| n.checked_mul(384))
            .and_then(|n| max_items.checked_mul(size_of::<ReferenceCandidate>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(name.len() + size_of::<Self>() + 4096))
            .ok_or(ReferenceError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ReferenceError::ResourceDenied)?;
        let encoding = encoding.unwrap_or_else(|| detect_encoding(source));
        if encoding == DetectedEncoding::Unsupported { return Err(ReferenceError::UnsupportedEncoding); }
        let map = CaptureEncodingMap::build_with_encoding(source, encoding)
            .map_err(|_| ReferenceError::UnsupportedEncoding)?;
        if map.spans().iter().any(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
            return Err(ReferenceError::UnsupportedEncoding);
        }
        if canceled() { return Err(ReferenceError::Canceled); }
        let mut candidates = Vec::new();
        candidates.try_reserve_exact(max_items).map_err(|_| ReferenceError::ResourceDenied)?;
        if candidates.capacity() > max_items { return Err(ReferenceError::ResourceDenied); }
        let mut owned_name = String::new();
        owned_name.try_reserve_exact(name.len()).map_err(|_| ReferenceError::ResourceDenied)?;
        if owned_name.capacity() > name.len() { return Err(ReferenceError::ResourceDenied); }
        owned_name.push_str(name);
        let text = map.decoded_text();
        let mut start = None;
        let mut token_line = 1u64;
        let mut line = 1u64;
        let mut checkpoint = 0usize;
        let mut counted = 0usize;
        let mut limited = false;
        // One pass over scalars; source fragments are compared only at complete
        // token boundaries. A long token cannot cause repeated prefix rescans.
        for (offset, scalar) in text.char_indices().chain(std::iter::once((text.len(), '\0'))) {
            if offset - checkpoint >= CANCEL_QUANTUM {
                if canceled() { return Err(ReferenceError::Canceled); }
                checkpoint = offset;
            }
            if token_scalar(scalar) {
                if start.is_none() { start = Some(offset); token_line = line; }
            } else if let Some(begin) = start.take() {
                if text.get(begin..offset) == Some(name) {
                    counted += 1;
                    if candidates.len() == max_items { limited = true; break; }
                    let decoded_range = DecodedUtf8Range::new(DecodedUtf8Offset::new(begin as u64),
                        DecodedUtf8Offset::new(offset as u64)).map_err(|_| ReferenceError::InvalidEvidence)?;
                    let original_range = map.decoded_utf8_range_to_byte_range(decoded_range)
                        .map_err(|_| ReferenceError::InvalidEvidence)?;
                    candidates.push(ReferenceCandidate { id: candidates.len() as u64 + 1,
                        original_range, decoded_range, line: token_line });
                }
            }
            if scalar == '\n' { line += 1; }
        }
        if canceled() { return Err(ReferenceError::Canceled); }
        Ok(Self { source, request, generation, encoding, name: owned_name, candidates,
            counted, limited, _lease: lease })
    }

    pub const fn file(&self) -> FileId { self.request.file() }
    pub const fn revision(&self) -> SourceRevision { self.request.revision() }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub fn name(&self) -> &str { &self.name }
    pub fn candidates(&self) -> &[ReferenceCandidate] { &self.candidates }
    /// Observed occurrences, including at most one unstored lookahead. This is
    /// not an exhaustive count when output_limited() is true.
    pub const fn total_matches_counted(&self) -> usize { self.counted }
    pub const fn output_limited(&self) -> bool { self.limited }
    /// Completeness applies only to the declared whole-token text policy, never
    /// compiler references or the containing repository's membership.
    pub const fn is_complete(&self) -> bool { !self.limited }
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

fn token_scalar(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$')
        || (!c.is_ascii() && !c.is_whitespace() && !c.is_control())
}
fn validate_name(name: &str) -> Result<(), ReferenceError> {
    if name.is_empty() || name.len() > MAX_REFERENCE_NAME_BYTES
        || !name.chars().all(token_scalar) || name.as_bytes()[0].is_ascii_digit() {
        return Err(ReferenceError::InvalidName);
    }
    Ok(())
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
