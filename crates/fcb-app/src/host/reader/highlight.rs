#![forbid(unsafe_code)]

//! Clip upstream whole-capture lexical roles to the retained reader's exact page.
//! Arbitrary pages are never lexed as complete programs. UTF-16 captures use
//! the shared bounded decoder before upstream lexing; malformed captures stay
//! explicitly plain. Original bytes remain authoritative for pages and copies.
//! This bounded worker adapter is not an incremental lexer.

use std::{borrow::Cow, mem::size_of, ops::Range, sync::Arc};
use fcb::document::source_highlight::{canonical_language, source_highlight,
    SourceHighlight, SourceHighlightError, SourceTokenRun, MAX_SOURCE_HIGHLIGHT_BYTES};
use fcb::source::{CaptureEncodingMap, MappingSpan, SpanKind, StatefulChunkDecoder};
use fcb_core::{ArenaOwnerId, ResourceAllocationId, ResourceBudget, ResourceLease};
use super::{check, reserve, ByteLength, ByteRange, DetectedEncoding, Output,
    ReaderSession, ReaderSessionError, MAX_RESPONSE_BYTES};

const CHECKPOINT_BYTES: usize = 4096;
pub(super) const MAX_PAGE_RUNS: usize = 8192;
// Leave space for the largest decoded window, its extent, the 8 MiB encoder
// and final response overlap even when the first request is a tiny page.
const WINDOW_HEADROOM: u64 = 128 * 1024 * 1024;
const RUN_JSON_BYTES: usize = 128;
const PAGE_JSON_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Highlighted, Plain, UnsupportedEncoding, SourceLimit, AdmissionDenied,
    InvalidSpans, PageLimit,
}
impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Highlighted => "highlighted", Self::Plain => "plain",
            Self::UnsupportedEncoding => "unsupported_encoding",
            Self::SourceLimit => "source_limit", Self::AdmissionDenied => "admission_denied",
            Self::InvalidSpans => "invalid_spans", Self::PageLimit => "page_limit",
        }
    }
}

#[derive(Clone)]
pub(super) enum CaptureHighlight {
    Plain { language: &'static str, state: State },
    Ready(Arc<HighlightedCapture>),
}
struct Checkpoint { byte: usize, utf16: u64 }
pub(super) struct HighlightedCapture {
    tokens: SourceHighlight,
    encoding: DetectedEncoding,
    checkpoints: Vec<Checkpoint>,
    utf16_length: u64,
    _lease: ResourceLease,
}
struct Page { runs: Range<usize>, start: u64, end: u64 }

impl ReaderSession {
    pub(super) fn prepare_highlight(&mut self, canceled: &mut impl FnMut() -> bool)
        -> Result<(), ReaderSessionError> {
        check(canceled)?;
        if self.highlight.is_some() { return Ok(()); }
        let language = canonical_language(self.path.extension().and_then(|s| s.to_str()).unwrap_or(""));
        let plain = |state| CaptureHighlight::Plain { language, state };
        let candidate = if self.encoding == DetectedEncoding::Unsupported {
            plain(State::UnsupportedEncoding)
        } else if self.capture.bytes().len() > MAX_SOURCE_HIGHLIGHT_BYTES {
            plain(State::SourceLimit)
        } else {
            let ids = self.allocations()?;
            prepare_capture(self.capture.bytes(), self.encoding, language, &self.budget,
                self.owner(), ids, canceled)?
        };
        check(canceled)?;
        self.highlight = Some(candidate);
        Ok(())
    }

    pub(super) fn encode_highlight(&self, out: &mut Output, range: ByteRange, text: &str,
        replacements: bool, canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
        check(canceled)?;
        let (language, state, ready) = match &self.highlight {
            Some(CaptureHighlight::Plain { language, state }) => (*language, *state, None),
            Some(CaptureHighlight::Ready(ready)) => (ready.tokens.language, State::Highlighted, Some(ready)),
            None => ("plain", State::AdmissionDenied, None),
        };
        let (mut state, mut page) = if let Some(ready) = ready {
            match ready.page(self.capture.bytes(), range, text, replacements, canceled)? {
                Ok(page) => (state, Some(page)), Err(state) => (state, None),
            }
        } else { (state, None) };
        if let Some(candidate) = &page {
            // Preflight optional JSON before writing any of it: never truncate
            // colored spans or fail source reading to fit syntax presentation.
            let charge = candidate.runs.len() * RUN_JSON_BYTES + PAGE_JSON_BYTES + 2;
            if out.as_bytes().len().checked_add(charge).is_none_or(|n| n > MAX_RESPONSE_BYTES) {
                state = State::PageLimit;
                page = None;
            }
        }
        out.literal(",\"highlight\":{\"state\":")?; out.quoted(state.name())?;
        out.literal(",\"language\":")?; out.quoted(language)?;
        out.literal(",\"coordinate_space\":\"window-utf16\",\"source_context\":\"complete-capture\",\"runs\":[")?;
        if let (Some(ready), Some(page)) = (ready, page) {
            for (index, run) in ready.tokens.runs[page.runs].iter().enumerate() {
                if index % 64 == 0 { check(canceled)?; }
                if index != 0 { out.literal(",")?; }
                let start = run.start.max(page.start);
                let end = (run.start + run.length).min(page.end);
                out.literal("{\"start\":")?; out.integer(start - page.start)?;
                out.literal(",\"length\":")?; out.integer(end - start)?;
                out.literal(",\"role\":")?; out.quoted(run.role)?; out.literal("}")?;
            }
        }
        out.literal("]}")?;
        check(canceled)
    }
}

/// The UTF-8 route borrows captured backing. UTF-16 decoding retains only one
/// bounded UTF-8 buffer and one small chunk map at a time; neither survives the
/// lexer call. Sparse checkpoints are needed only for variable-width UTF-8.
struct LexicalText<'a> { text: Cow<'a, str>, _lease: Option<ResourceLease> }

fn prepare_capture(bytes: &[u8], encoding: DetectedEncoding, language: &'static str,
    budget: &ResourceBudget, owner: ArenaOwnerId, ids: [ResourceAllocationId; 4],
    canceled: &mut impl FnMut() -> bool) -> Result<CaptureHighlight, ReaderSessionError> {
    let plain = |state| CaptureHighlight::Plain { language, state };
    let [headroom_id, tokens_id, checkpoints_id, decode_id] = ids;
    let Ok(_headroom) = budget.try_reserve_managed(owner, headroom_id, ByteLength::new(WINDOW_HEADROOM))
        else { return Ok(plain(State::AdmissionDenied)); };
    let source = match lexical_text(bytes, encoding, budget, owner, decode_id, canceled)? {
        Ok(source) => source, Err(state) => return Ok(plain(state)),
    };
    if language == "plain" { return Ok(plain(State::Plain)); }
    // The existing upstream operation is bounded but opaque; cancellation
    // brackets it, not a deadline. No lexical implementation is copied here.
    check(canceled)?;
    let tokens = source_highlight(&source.text, language, budget, tokens_id, owner);
    check(canceled)?;
    let tokens = match tokens {
        Err(SourceHighlightError::Limit) => return Ok(plain(State::SourceLimit)),
        Err(SourceHighlightError::Admission) => return Ok(plain(State::AdmissionDenied)),
        Err(SourceHighlightError::InvalidSpans) => return Ok(plain(State::InvalidSpans)),
        Ok(tokens) if !tokens.syntax_supported => return Ok(plain(State::Plain)),
        Ok(tokens) => tokens,
    };
    let capacity = if encoding.is_utf8() { source.text.len() / CHECKPOINT_BYTES + 1 } else { 0 };
    let charge = capacity * size_of::<Checkpoint>()
        + size_of::<HighlightedCapture>() + 2 * size_of::<usize>();
    let Ok(lease) = budget.try_reserve_managed(owner, checkpoints_id, ByteLength::new(charge as u64))
        else { return Ok(plain(State::AdmissionDenied)); };
    let Ok(mut checkpoints) = reserve::<Checkpoint>(capacity)
        else { return Ok(plain(State::AdmissionDenied)); };
    let utf16_length = if encoding.is_utf8() {
        let bom = encoding.bom_bytes_len() as usize;
        checkpoints.push(Checkpoint { byte: bom, utf16: 0 });
        let (mut next, mut utf16) = (CHECKPOINT_BYTES, 0);
        for (byte, scalar) in source.text.char_indices() {
            if byte >= next {
                check(canceled)?;
                checkpoints.push(Checkpoint { byte: bom + byte, utf16 });
                next += CHECKPOINT_BYTES;
            }
            utf16 += scalar.len_utf16() as u64;
        }
        utf16
    } else {
        // The shared decoder has validated every code unit and the final EOF.
        // Original UTF-16 units therefore map directly, excluding its BOM.
        (bytes.len() as u64 - encoding.bom_bytes_len()) / 2
    };
    check(canceled)?;
    Ok(CaptureHighlight::Ready(Arc::new(HighlightedCapture {
        tokens, encoding, checkpoints, utf16_length, _lease: lease,
    })))
}

fn lexical_text<'a>(bytes: &'a [u8], encoding: DetectedEncoding, budget: &ResourceBudget,
    owner: ArenaOwnerId, allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
    -> Result<Result<LexicalText<'a>, State>, ReaderSessionError> {
    check(canceled)?;
    let bom = encoding.bom_bytes_len() as usize;
    let Some(content) = bytes.get(bom..) else { return Ok(Err(State::UnsupportedEncoding)); };
    if encoding.is_utf8() {
        return Ok(std::str::from_utf8(content).map(|text| LexicalText { text: Cow::Borrowed(text), _lease: None })
            .map_err(|_| State::UnsupportedEncoding));
    }
    if !encoding.is_utf16() { return Ok(Err(State::UnsupportedEncoding)); }
    // A BMP code unit may expand to three UTF-8 bytes. Keep the upstream input
    // bound on decoded bytes too; source-size admission alone is insufficient.
    let capacity = (content.len() / 2 * 3).min(MAX_SOURCE_HIGHLIGHT_BYTES);
    let scratch = (CHECKPOINT_BYTES + 8) * (4 * size_of::<MappingSpan>() + 16)
        + size_of::<StatefulChunkDecoder>() + size_of::<CaptureEncodingMap>();
    let Ok(lease) = budget.try_reserve_managed(owner, allocation,
        ByteLength::new((capacity + scratch + size_of::<String>()) as u64))
        else { return Ok(Err(State::AdmissionDenied)); };
    let mut text = String::new();
    if text.try_reserve_exact(capacity).is_err() || text.capacity() > capacity {
        return Ok(Err(State::AdmissionDenied));
    }
    let mut decoder = StatefulChunkDecoder::new(encoding);
    for chunk in bytes.chunks(CHECKPOINT_BYTES) {
        check(canceled)?;
        let Ok(map) = decoder.decode_chunk(chunk) else { return Ok(Err(State::InvalidSpans)); };
        if let Err(state) = append_decoded(&mut text, &map, capacity) { return Ok(Err(state)); }
    }
    check(canceled)?;
    let Ok(tail) = decoder.finish() else { return Ok(Err(State::InvalidSpans)); };
    if let Some(map) = tail {
        if let Err(state) = append_decoded(&mut text, &map, capacity) { return Ok(Err(state)); }
    }
    check(canceled)?;
    Ok(Ok(LexicalText { text: Cow::Owned(text), _lease: Some(lease) }))
}

fn append_decoded(text: &mut String, map: &CaptureEncodingMap, capacity: usize) -> Result<(), State> {
    // A valid local page cannot receive a stronger lexical claim than the
    // complete captured source. Genuine U+FFFD characters remain valid text.
    if map.spans().iter().any(|span| span.kind == SpanKind::ReplacementMalformed) {
        return Err(State::UnsupportedEncoding);
    }
    if map.decoded_text().len() > capacity - text.len() { return Err(State::SourceLimit); }
    text.push_str(map.decoded_text());
    Ok(())
}

impl HighlightedCapture {
    fn page(&self, bytes: &[u8], range: ByteRange, text: &str, replacements: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Result<Page, State>, ReaderSessionError> {
        check(canceled)?;
        if replacements { return Ok(Err(State::UnsupportedEncoding)); }
        let (Ok(start), Ok(end)) = (usize::try_from(range.start().get()), usize::try_from(range.end().get()))
            else { return Ok(Err(State::InvalidSpans)); };
        let (start, end) = if self.encoding.is_utf16() {
            let bom = self.encoding.bom_bytes_len() as usize;
            let little = self.encoding == DetectedEncoding::Utf16Le;
            let Some(raw) = bytes.get(start..end) else { return Ok(Err(State::InvalidSpans)); };
            if start < bom || !start.is_multiple_of(2) || !end.is_multiple_of(2)
                || !matches_utf16(raw, text, little, canceled)? {
                return Ok(Err(State::InvalidSpans));
            }
            // Even an empty range must be a scalar boundary, as on UTF-8.
            // Complete-capture decoding has already established valid pairs.
            if start == end && bytes.get(start..start + 2).is_some_and(|pair| {
                let pair = [pair[0], pair[1]];
                let unit = if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) };
                (0xdc00..=0xdfff).contains(&unit)
            }) { return Ok(Err(State::InvalidSpans)); }
            (((start - bom) / 2) as u64, ((end - bom) / 2) as u64)
        } else {
            if bytes.get(start..end) != Some(text.as_bytes()) { return Ok(Err(State::InvalidSpans)); }
            let checkpoint = self.checkpoints.partition_point(|point| point.byte <= start);
            if checkpoint == 0 { return Ok(Err(State::InvalidSpans)); }
            let checkpoint = &self.checkpoints[checkpoint - 1];
            let Some(prefix) = bytes.get(checkpoint.byte..start).and_then(|slice| std::str::from_utf8(slice).ok())
                else { return Ok(Err(State::InvalidSpans)); };
            let start = checkpoint.utf16 + utf16_length(prefix, canceled)?;
            (start, start + utf16_length(text, canceled)?)
        };
        if end > self.utf16_length { return Ok(Err(State::InvalidSpans)); }
        if start == end { return Ok(Ok(Page { runs: 0..0, start, end })); }
        let runs = &self.tokens.runs;
        let first = runs.partition_point(|run| run.start.saturating_add(run.length) <= start);
        let last = runs.partition_point(|run| run.start < end);
        if first > last { return Ok(Err(State::InvalidSpans)); }
        if last - first > MAX_PAGE_RUNS { return Ok(Err(State::PageLimit)); }
        let mut next = start;
        for (index, run) in runs[first..last].iter().enumerate() {
            if index % 64 == 0 { check(canceled)?; }
            let Some(run_end) = run.start.checked_add(run.length) else { return Ok(Err(State::InvalidSpans)); };
            let clipped_start = run.start.max(start);
            let clipped_end = run_end.min(end);
            if clipped_start != next || clipped_end <= clipped_start || !known_role(run) {
                return Ok(Err(State::InvalidSpans));
            }
            next = clipped_end;
        }
        check(canceled)?;
        if next != end { return Ok(Err(State::InvalidSpans)); }
        Ok(Ok(Page { runs: first..last, start, end }))
    }
}

fn matches_utf16(raw: &[u8], text: &str, little: bool, canceled: &mut impl FnMut() -> bool)
    -> Result<bool, ReaderSessionError> {
    if !raw.len().is_multiple_of(2) { return Ok(false); }
    let mut units = text.encode_utf16();
    for (index, pair) in raw.chunks_exact(2).enumerate() {
        if index.is_multiple_of(CHECKPOINT_BYTES / 2) { check(canceled)?; }
        let pair = [pair[0], pair[1]];
        let unit = if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) };
        if units.next() != Some(unit) { return Ok(false); }
    }
    check(canceled)?;
    Ok(units.next().is_none())
}

fn utf16_length(text: &str, canceled: &mut impl FnMut() -> bool) -> Result<u64, ReaderSessionError> {
    let (mut next, mut length) = (0, 0);
    for (byte, scalar) in text.char_indices() {
        if byte >= next { check(canceled)?; next = byte + CHECKPOINT_BYTES; }
        length += scalar.len_utf16() as u64;
    }
    check(canceled)?;
    Ok(length)
}
fn known_role(run: &SourceTokenRun) -> bool {
    matches!(run.role, "plain" | "keyword" | "type" | "function" | "string"
        | "number" | "comment" | "operator" | "punctuation")
}

#[cfg(test)]
mod tests;
