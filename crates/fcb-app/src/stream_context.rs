#![forbid(unsafe_code)]

//! Shared wire projection of context retained by a streaming search. Decoding
//! uses the existing extent reader and never opens or rereads source paths.

use fcb::{BrowserSession, ByteLength, ByteOffset, ByteRange, QueryGeneration};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentText, ExtentViewError,
    ObservedExtent, ResourceAllocationId, ResourceBudget};
use crate::{AppError, output::Output};

pub(crate) const BEFORE_BYTES: usize = 160;
pub(crate) const AFTER_BYTES: usize = 160;

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
        let start = if self.original.start().get() == 0 { 0 }
            else { self.original.start().get().checked_add(4).ok_or(AppError::InvalidRange)? };
        let end = if self.original.end().get() == self.observed_length.get() { self.original.end().get() }
            else { self.original.end().get().saturating_sub(4) };
        if start > self.matched.start().get() || end < self.matched.end().get() { return Ok(None); }
        Ok(Some(ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| AppError::InvalidRange)?))
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
        // Keep four original bytes outside the visible region for the shared
        // decoder's scalar/CRLF boundary rules. A partial suffix may not even
        // cover the match with those guards; raw bytes remain inspectable then.
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
