#![forbid(unsafe_code)]

//! Clip upstream whole-capture lexical roles to the retained reader's exact page.
//! Arbitrary pages are never lexed as complete programs. UTF-16 and malformed
//! captures remain readable through the existing decoder with explicit plain
//! presentation. This bounded worker adapter is not an incremental lexer.

use std::{mem::size_of, ops::Range, sync::Arc};
use fcb::document::source_highlight::{canonical_language, source_highlight,
    SourceHighlight, SourceHighlightError, SourceTokenRun, MAX_SOURCE_HIGHLIGHT_BYTES};
use fcb_core::ResourceLease;
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
        let candidate = if !matches!(self.encoding, DetectedEncoding::Utf8 { .. }) {
            plain(State::UnsupportedEncoding)
        } else if self.capture.bytes().len() > MAX_SOURCE_HIGHLIGHT_BYTES {
            plain(State::SourceLimit)
        } else {
            let [headroom_id, tokens_id, checkpoints_id] = self.allocations()?;
            let bytes = self.capture.bytes();
            let bom = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 };
            // A valid page inside a malformed observation cannot acquire a
            // stronger lexical claim than its complete source context.
            let source = std::str::from_utf8(&bytes[bom..]);
            check(canceled)?;
            match source {
                Err(_) => plain(State::UnsupportedEncoding),
                Ok(_) if language == "plain" => plain(State::Plain),
                Ok(source) => {
                    let headroom = self.budget.try_reserve_managed(self.owner(), headroom_id,
                        ByteLength::new(WINDOW_HEADROOM));
                    match headroom {
                        Err(_) => plain(State::AdmissionDenied),
                        Ok(_headroom) => {
                            // The existing upstream operation is bounded but
                            // opaque; cancellation brackets it, not a deadline.
                            check(canceled)?;
                            let tokens = source_highlight(source, language, &self.budget, tokens_id, self.owner());
                            check(canceled)?;
                            match tokens {
                                Err(SourceHighlightError::Limit) => plain(State::SourceLimit),
                                Err(SourceHighlightError::Admission) => plain(State::AdmissionDenied),
                                Err(SourceHighlightError::InvalidSpans) => plain(State::InvalidSpans),
                                Ok(tokens) if !tokens.syntax_supported => plain(State::Plain),
                                Ok(tokens) => {
                                    let capacity = source.len() / CHECKPOINT_BYTES + 1;
                                    let charge = capacity * size_of::<Checkpoint>()
                                        + size_of::<HighlightedCapture>() + 2 * size_of::<usize>();
                                    let lease = self.budget.try_reserve_managed(self.owner(), checkpoints_id,
                                        ByteLength::new(charge as u64));
                                    match lease {
                                        Err(_) => plain(State::AdmissionDenied),
                                        Ok(lease) => match reserve::<Checkpoint>(capacity) {
                                            Err(_) => plain(State::AdmissionDenied),
                                            Ok(mut checkpoints) => {
                                                checkpoints.push(Checkpoint { byte: bom, utf16: 0 });
                                                let (mut next, mut utf16) = (CHECKPOINT_BYTES, 0);
                                                for (byte, scalar) in source.char_indices() {
                                                    if byte >= next {
                                                        check(canceled)?;
                                                        checkpoints.push(Checkpoint { byte: bom + byte, utf16 });
                                                        next += CHECKPOINT_BYTES;
                                                    }
                                                    utf16 += scalar.len_utf16() as u64;
                                                }
                                                check(canceled)?;
                                                CaptureHighlight::Ready(Arc::new(HighlightedCapture {
                                                    tokens, checkpoints, utf16_length: utf16, _lease: lease,
                                                }))
                                            }
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
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

impl HighlightedCapture {
    fn page(&self, bytes: &[u8], range: ByteRange, text: &str, replacements: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Result<Page, State>, ReaderSessionError> {
        check(canceled)?;
        if replacements { return Ok(Err(State::UnsupportedEncoding)); }
        let (Ok(start), Ok(end)) = (usize::try_from(range.start().get()), usize::try_from(range.end().get()))
            else { return Ok(Err(State::InvalidSpans)); };
        if bytes.get(start..end) != Some(text.as_bytes()) { return Ok(Err(State::InvalidSpans)); }
        let checkpoint = self.checkpoints.partition_point(|point| point.byte <= start);
        if checkpoint == 0 { return Ok(Err(State::InvalidSpans)); }
        let checkpoint = &self.checkpoints[checkpoint - 1];
        let Some(prefix) = bytes.get(checkpoint.byte..start).and_then(|slice| std::str::from_utf8(slice).ok())
            else { return Ok(Err(State::InvalidSpans)); };
        let start = checkpoint.utf16 + utf16_length(prefix, canceled)?;
        let end = start + utf16_length(text, canceled)?;
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
