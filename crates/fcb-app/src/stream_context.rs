#![forbid(unsafe_code)]

//! Shared wire projection of context retained by a streaming search. Decoding
//! uses the existing extent reader and never opens or rereads source paths.

use fcb::{BrowserSession, ByteLength, ByteOffset, ByteRange, QueryGeneration};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentText, ExtentViewError,
    ExtentConsistency, ObservedExtent, ResourceAllocationId, ResourceBudget};
use fcb::store::Sha256;
use crate::{AppError, output::Output};

pub(crate) const BEFORE_BYTES: usize = 160;
pub(crate) const AFTER_BYTES: usize = 160;

/// An engine-resolved, independently leased excerpt from one accepted search
/// observation. Its byte path is provenance only and never authorizes a read.
/// Construction is private: callers cannot substitute text or a live pathname.
pub struct RetainedSearchExcerpt<'a> {
    pub(crate) extent: ObservedExtent,
    pub(crate) path: &'a [u8],
    pub(crate) encoding: DetectedEncoding,
    pub(crate) selection: ByteRange,
}
impl RetainedSearchExcerpt<'_> {
    pub fn extent(&self) -> &ObservedExtent { &self.extent }
    pub fn path_bytes(&self) -> &[u8] { self.path }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub const fn selection(&self) -> ByteRange { self.selection }
}

#[derive(Clone, Copy)]
pub(crate) struct Context<'a> {
    pub request: CaptureRequest,
    pub generation: QueryGeneration,
    pub observed_length: ByteLength,
    pub encoding: Option<DetectedEncoding>,
    pub requested: ByteRange,
    pub original: ByteRange,
    pub matched: ByteRange,
    pub bytes: &'a [u8],
}

impl Context<'_> {
    pub(crate) fn visible_range(&self) -> Result<Option<ByteRange>, AppError> {
        self.validate()?;
        // The shared decoder floors a requested start to a scalar/CRLF
        // boundary. Eight preceding bytes leave at least four after flooring,
        // so the exact displayed selection can be decoded again in the Desk.
        let start = if self.original.start().get() == 0 { 0 }
            else { self.original.start().get().checked_add(8).ok_or(AppError::InvalidRange)? };
        let end = if self.original.end().get() == self.observed_length.get() { self.original.end().get() }
            else { self.original.end().get().saturating_sub(4) };
        if start > self.matched.start().get() || end < self.matched.end().get() { return Ok(None); }
        Ok(Some(ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| AppError::InvalidRange)?))
    }

    /// Resolve the same lossless text range advertised by the context receipt,
    /// then retain its original bytes and original observation evidence. No
    /// filesystem authority enters this operation. Unsupported partial evidence
    /// is refused by the existing extent contract, never upgraded or relabeled.
    pub(crate) fn retain<'a>(&self, path: &'a [u8], final_length: Option<ByteLength>,
        consistency: ExtentConsistency, budget: &ResourceBudget,
        ids: [ResourceAllocationId; 3], canceled: &mut impl FnMut() -> bool)
        -> Result<RetainedSearchExcerpt<'a>, AppError> {
        if canceled() { return Err(AppError::Canceled); }
        self.validate()?;
        let encoding = self.encoding.filter(|encoding| *encoding != DetectedEncoding::Unsupported)
            .ok_or(AppError::InvalidRange)?;
        let selection = match self.decode(budget, [ids[0], ids[1]], canceled)? {
            Decoded::Available(text) if !text.has_replacements() && !text.range().is_empty() => text.range(),
            _ => return Err(AppError::InvalidRange),
        };
        // The display projection used a copied byte sequence. Durable reading
        // keeps the original evidence, whose EOF may be unknown after a change
        // or metadata failure. Require the same suffix context as ExtentView so
        // this capability never advertises a selection that Desk must refuse.
        let eof_known = self.original == self.requested
            && self.original.end().get() == self.observed_length.get()
            && matches!(consistency, ExtentConsistency::HostSupplied | ExtentConsistency::UnchangedMetadata);
        if self.original.start().get() > selection.start().get().saturating_sub(4)
            || (!eof_known && self.original.end().get().checked_sub(selection.end().get()).is_none_or(|n| n < 4)) {
            return Err(AppError::InvalidRange);
        }
        // Decoded scratch is dropped before retaining the separately leased
        // original bytes. A caller may import these after source replacement.
        if canceled() { return Err(AppError::Canceled); }
        let request = self.request.with_range(self.requested).map_err(|_| AppError::InvalidRange)?;
        let charge = self.bytes.len().checked_add(4096).ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(request.file().owner(), ids[2], ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let extent = ObservedExtent::restore_recorded(request, self.original, self.observed_length,
            final_length, consistency, self.bytes, lease)?;
        if canceled() { return Err(AppError::Canceled); }
        Ok(RetainedSearchExcerpt { extent, path, encoding, selection })
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.original.len().get() != self.bytes.len() as u64 || self.matched.is_empty()
            || self.requested.start() > self.original.start() || self.requested.end() < self.original.end()
            || self.original.start() > self.matched.start() || self.original.end() < self.matched.end()
            || self.requested.end().get() > self.observed_length.get()
            || self.request.file().owner() != self.generation.owner() {
            return Err(AppError::InvalidRange);
        }
        Ok(())
    }
    fn decode(&self, budget: &ResourceBudget, ids: [ResourceAllocationId; 2],
        canceled: &mut impl FnMut() -> bool) -> Result<Decoded, AppError> {
        let Some(encoding) = self.encoding.filter(|encoding| *encoding != DetectedEncoding::Unsupported) else {
            return Ok(Decoded::Unavailable("encoding-unavailable"));
        };
        // Keep eight preceding and four following original bytes for the
        // shared decoder's scalar/CRLF rules. The extra prefix survives start
        // boundary flooring so the advertised range can be decoded again.
        // A partial suffix may not cover the match with those guards; raw bytes
        // remain inspectable then.
        let Some(visible) = self.visible_range()? else {
            return Ok(Decoded::Unavailable("insufficient-scalar-context"));
        };
        let request = self.request.with_range(self.original).map_err(|_| AppError::InvalidRange)?;
        let extent = ObservedExtent::from_bytes(request, self.observed_length, self.bytes, budget, ids[0])?;
        let view = BrowserSession::new(request.file().owner()).open_extent(extent)?;
        let text = match view.decode(visible, encoding, self.generation, budget, ids[1], &mut *canceled) {
            Ok(text) => text,
            Err(ExtentViewError::ContextUnavailable) => return Ok(Decoded::Unavailable("insufficient-scalar-context")),
            Err(error) => return Err(error.into()),
        };
        let matched = match text.source_to_text(self.matched) {
            Ok(matched) => matched,
            Err(_) => return Ok(Decoded::Unavailable("match-boundary-unavailable")),
        };
        let selected = match text.text_selection(matched) {
            Ok(selected) => selected,
            Err(_) => return Ok(Decoded::Unavailable("match-boundary-unavailable")),
        };
        let local_start = usize::try_from(self.matched.start().get() - self.original.start().get())
            .map_err(|_| AppError::InvalidRange)?;
        let local_end = usize::try_from(self.matched.end().get() - self.original.start().get())
            .map_err(|_| AppError::InvalidRange)?;
        if selected.original_range != self.matched
            || self.bytes.get(local_start..local_end) != Some(selected.original_bytes) {
            return Ok(Decoded::Unavailable("match-boundary-unavailable"));
        }
        Ok(Decoded::Available(text))
    }
}

enum Decoded { Available(ExtentText), Unavailable(&'static str) }

pub(crate) fn encode(out: &mut Output, context: Context<'_>, budget: &ResourceBudget,
    ids: [ResourceAllocationId; 2], canceled: &mut impl FnMut() -> bool) -> Result<(), AppError> {
    if canceled() { return Err(AppError::Canceled); }
    context.validate()?;
    let decoded = context.decode(budget, ids, canceled)?;
    out.literal(",\"context\":{\"state\":\"retained\",\"requested_range\":")?;
    out.range(context.requested)?;
    out.literal(",\"original_range\":")?; out.range(context.original)?;
    out.literal(",\"match_range\":")?; out.range(context.matched)?;
    out.literal(",\"complete\":")?; out.boolean(context.original == context.requested)?;
    out.literal(",\"original_hex\":")?; out.hex(context.bytes)?;
    out.literal(",\"extent_sha256\":")?; out.quoted(&Sha256::digest(context.bytes).to_hex())?;
    match decoded {
        Decoded::Available(text) => {
            let matched = text.source_to_text(context.matched)?;
            out.literal(",\"text\":")?; out.quoted(text.text())?;
            out.literal(",\"text_original_range\":")?; out.range(text.range())?;
            out.literal(",\"match_decoded_range\":{\"start\":")?; out.integer(matched.start().get())?;
            out.literal(",\"end\":")?; out.integer(matched.end().get())?; out.literal("}")?;
            out.literal(",\"text_has_replacements\":")?; out.boolean(text.has_replacements())?;
            out.literal(",\"text_unavailable_reason\":null")?;
        }
        Decoded::Unavailable(reason) => {
            out.literal(",\"text\":null,\"text_original_range\":null,\"match_decoded_range\":null,\"text_has_replacements\":null,\"text_unavailable_reason\":")?;
            out.quoted(reason)?;
        }
    }
    out.literal("}")?;
    if canceled() { return Err(AppError::Canceled); }
    Ok(())
}

pub(crate) fn human(out: &mut Output, context: Context<'_>, budget: &ResourceBudget,
    ids: [ResourceAllocationId; 2], canceled: &mut impl FnMut() -> bool) -> Result<(), AppError> {
    if canceled() { return Err(AppError::Canceled); }
    context.validate()?;
    let decoded = context.decode(budget, ids, canceled)?;
    out.literal("    Same-scan context")?;
    if context.original != context.requested { out.literal(" (partial)")?; }
    out.literal(": ")?;
    match decoded {
        Decoded::Available(text) => out.human_text(text.text())?,
        Decoded::Unavailable(reason) => { out.literal("text unavailable: ")?; out.literal(reason)?; }
    }
    out.literal("\n")?;
    if canceled() { return Err(AppError::Canceled); }
    Ok(())
}


#[cfg(test)]
mod retained_excerpt_tests {
    use super::*;
    use fcb::{ArenaOwnerId, FileId, SourceRevision};

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
    }
    fn setup() -> (ResourceBudget, [ResourceAllocationId; 3], CaptureRequest, QueryGeneration) {
        let owner = ArenaOwnerId::new(751).unwrap();
        (ResourceBudget::new(owner, ByteLength::new(8 * 1024 * 1024)).unwrap(),
            [1, 2, 3].map(|id| ResourceAllocationId::new(id).unwrap()),
            CaptureRequest::new(FileId::new(owner, 41).unwrap(), SourceRevision::new(owner, 42).unwrap()).unwrap(),
            QueryGeneration::new(owner, 7).unwrap())
    }

    #[test]
    fn retained_excerpt_cancel_and_lossy_decode_release_every_reservation() {
        let (budget, ids, request, generation) = setup();
        let bytes = b"before needle after";
        let context = Context { request, generation, observed_length: ByteLength::new(bytes.len() as u64),
            encoding: Some(DetectedEncoding::Utf8 { has_bom: false }), requested: range(0, bytes.len() as u64),
            original: range(0, bytes.len() as u64), matched: range(7, 13), bytes };
        let mut polls = 0;
        let accepted = context.retain(b"label.rs", Some(context.observed_length), ExtentConsistency::UnchangedMetadata,
            &budget, ids, &mut || { polls += 1; false }).unwrap();
        assert_eq!(accepted.selection(), range(0, bytes.len() as u64));
        drop(accepted);
        assert_eq!(budget.accounting().reserved().get(), 0);
        for stop in 1..=polls {
            let mut seen = 0;
            let rejected = context.retain(b"label.rs", Some(context.observed_length), ExtentConsistency::UnchangedMetadata,
                &budget, ids, &mut || { seen += 1; seen >= stop });
            assert!(rejected.is_err(), "cancel poll {stop} must not publish");
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
        let malformed = b"before needle \xfffter";
        let context = Context { bytes: malformed, ..context };
        assert!(context.retain(b"label.rs", Some(context.observed_length), ExtentConsistency::UnchangedMetadata,
            &budget, ids, &mut || false).is_err());
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    #[test]
    fn unicode_prefix_guards_survive_boundary_flooring_and_desk_roundtrip() {
        for encoding in [DetectedEncoding::Utf8 { has_bom: false }, DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
            let (budget, ids, request, generation) = setup();
            let text = "a😀".repeat(80);
            let bytes: Vec<u8> = match encoding {
                DetectedEncoding::Utf8 { .. } => text.into_bytes(),
                DetectedEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                DetectedEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
                _ => unreachable!(),
            };
            let base = 100;
            let step = if encoding.is_utf16() { 6 } else { 5 };
            let unit = if encoding.is_utf16() { 2 } else { 1 };
            let original = range(base, base + bytes.len() as u64);
            let length = ByteLength::new(original.end().get() + 100);
            let context = Context { request, generation, observed_length: length, encoding: Some(encoding),
                requested: original, original, matched: range(base + 40 * step, base + 40 * step + unit), bytes: &bytes };
            let retained = context.retain(b"unicode.rs", Some(length), ExtentConsistency::UnchangedMetadata,
                &budget, ids, &mut || false).unwrap();
            // A four-byte requested prefix lands inside the emoji and floors
            // back to one UTF-8 byte or two UTF-16 bytes of remaining context.
            // The new projection leaves a safe, exact scalar-aligned selection.
            assert_eq!(retained.selection().start().get(), base + if encoding.is_utf16() { 8 } else { 6 });
            let mut desk = crate::host::desk::DeskSession::new(ArenaOwnerId::new(752).unwrap(),
                crate::host::desk::DeskLimits::default()).unwrap();
            let kept = desk.import_search_excerpt(0, 1, &retained, || false).unwrap();
            let decoded = desk.model().read_observed_passage(1, kept.passage,
                ResourceAllocationId::new(900).unwrap(), || false).unwrap();
            assert_eq!(decoded.range(), retained.selection());
            assert!(!decoded.has_replacements());
            let selected = retained.selection();
            assert_eq!(decoded.extent().range_bytes(selected).unwrap(),
                &bytes[(selected.start().get() - base) as usize..(selected.end().get() - base) as usize]);
            drop(retained);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }

    #[test]
    fn changed_or_unknown_eof_cannot_publish_a_passage_without_suffix_context() {
        let (budget, ids, request, generation) = setup();
        let bytes = b"before needle after";
        let length = ByteLength::new(bytes.len() as u64);
        let whole = range(0, length.get());
        let encoding = DetectedEncoding::Utf8 { has_bom: false };
        let context = Context { request, generation, observed_length: length, encoding: Some(encoding),
            requested: whole, original: whole, matched: range(7, 13), bytes };
        for (consistency, final_length) in [
            (ExtentConsistency::ChangedDuringRead, Some(ByteLength::new(length.get() + 1))),
            (ExtentConsistency::MetadataUnavailable, None),
        ] {
            assert!(context.retain(b"label.rs", final_length, consistency, &budget, ids, &mut || false).is_err());
            assert_eq!(budget.accounting().reserved().get(), 0);
            // Negative control bypasses this module's preflight and proves the
            // existing Desk decoder rejects the same impossible EOF selection
            // without publishing a note or upgrading the observation evidence.
            let lease = budget.try_reserve_managed(request.file().owner(), ids[2],
                ByteLength::new(bytes.len() as u64 + 4096)).unwrap();
            let extent = ObservedExtent::restore_recorded(request.with_range(whole).unwrap(), whole,
                length, final_length, consistency, bytes, lease).unwrap();
            let candidate = RetainedSearchExcerpt { extent, path: b"label.rs", encoding, selection: whole };
            let mut desk = crate::host::desk::DeskSession::new(ArenaOwnerId::new(752).unwrap(),
                crate::host::desk::DeskLimits::default()).unwrap();
            assert!(desk.import_search_excerpt(0, 1, &candidate, || false).is_err());
            assert_eq!(desk.model().revision(), 0);
            assert!(desk.model().observed_passages().is_empty());
            drop(candidate);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }

    #[test]
    fn partial_context_preserves_short_read_evidence_and_refuses_consistency_upgrade() {
        let (budget, ids, request, generation) = setup();
        let bytes = [b'x'; 80];
        let context = Context { request, generation, observed_length: ByteLength::new(100),
            encoding: Some(DetectedEncoding::Utf8 { has_bom: false }), requested: range(0, 100),
            original: range(0, 80), matched: range(20, 26), bytes: &bytes };
        assert!(context.retain(b"partial.rs", Some(ByteLength::new(100)), ExtentConsistency::UnchangedMetadata,
            &budget, ids, &mut || false).is_err());
        assert_eq!(budget.accounting().reserved().get(), 0);
        let retained = context.retain(b"partial.rs", Some(ByteLength::new(80)), ExtentConsistency::ShortRead,
            &budget, ids, &mut || false).unwrap();
        assert_eq!(retained.extent().range(), range(0, 80));
        assert_eq!(retained.extent().request().range(), Some(range(0, 100)));
        assert!(!retained.extent().request_filled());
        assert_eq!(retained.extent().consistency(), ExtentConsistency::ShortRead);
        assert_eq!(retained.extent().final_length(), Some(ByteLength::new(80)));
        assert!(retained.selection().end() < ByteOffset::new(80));
        assert_eq!(retained.extent().request().revision().get(), 42);
        drop(retained);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
