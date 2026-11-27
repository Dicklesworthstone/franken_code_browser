#![forbid(unsafe_code)]

use std::sync::Arc;
use super::*;
use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader};
use fcb::search::CompleteCapture;
use fcb_core::{DocumentGeneration, DocumentId};

/// The caller retains the verified live extent or archive member. Charge the
/// overlapping immutable source copy before giving it to the document engine.
#[allow(clippy::too_many_arguments)]
pub(super) fn analyze(settings: &Settings, request: CaptureRequest, path: &[u8], bytes: &[u8],
    state: &mut RunState, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let owner = request.file().owner();
    let _lease = budget.try_reserve_managed(owner, allocation(204),
        ByteLength::new((bytes.len() + size_of::<CompleteCapture>() + 64) as u64))
        .map_err(|_| Failure::new("SYMBOL_RESOURCE_DENIED"))?;
    let capture = CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
        .map_err(|_| Failure::new("SYMBOL_INVALID_RANGE"))?;
    let document = DocumentReader::prepare(&capture,
        DocumentId::new(owner, request.file().get()).map_err(|_| Failure::new("SYMBOL_IDENTITY_EXHAUSTED"))?,
        DocumentGeneration::new(owner, generation().get()).map_err(|_| Failure::new("SYMBOL_IDENTITY_EXHAUSTED"))?,
        DocumentReadOptions { max_source_bytes: MAX_HEADING_SEARCH_SOURCE_BYTES, ..Default::default() },
        budget, allocation(205), &mut *canceled)
        .and_then(|document| {
            document.qualify_heading_source_ranges(budget, allocation(206), &mut *canceled)?;
            Ok(document)
        });
    // Qualify the complete heading inventory before emitting even the first
    // result. Render-only or nested headings cannot support exact navigation.
    let document = match document {
        Ok(document) => document,
        Err(DocumentReadError::Canceled) => return Err(Failure::canceled()),
        Err(error) => {
            state.stats.refused += 1;
            state.notice(request.file(), path, error.code());
            return Ok(());
        }
    };
    state.stats.analyzed += 1;
    state.stats.analysis_bytes += bytes.len() as u64;
    for (ordinal, heading) in document.headings().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        if let Some(needle) = settings.name.as_deref() {
            let matches = match settings.mode {
                SymbolNameMode::Exact => heading.title == needle,
                SymbolNameMode::Prefix => heading.title.starts_with(needle),
                SymbolNameMode::Contains => heading.title.contains(needle),
            };
            if !matches { continue; }
        }
        state.stats.matches += 1;
        if state.stats.emitted >= settings.limit { continue; }
        let range = document.original_span(heading.source_span).map_err(|error| Failure::new(error.code()))?;
        if settings.json {
            if state.stats.emitted > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(request.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(request.revision().get())?;
            out.literal(",\"candidate_id\":")?; out.integer(ordinal as u64 + 1)?;
            out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(path).to_path_buf())?;
            out.literal(",\"name\":")?; out.quoted(&heading.title)?;
            out.literal(",\"kind\":\"heading\",\"heading_slug\":")?; out.quoted(&heading.slug)?;
            out.literal(",\"level\":")?; out.integer(u64::from(heading.level))?;
            out.literal(",\"original_range\":")?; out.range(range)?; out.literal("}")?;
        } else {
            out.path(&RawPath::from_bytes(path).to_path_buf())?;
            out.literal(" ")?; out.human_text(&heading.title)?;
            out.literal(" (#")?; out.human_text(&heading.slug)?; out.literal(") bytes ")?;
            out.literal(&range.start().get().to_string())?; out.literal("..")?;
            out.literal(&range.end().get().to_string())?; out.literal("\n")?;
        }
        state.stats.emitted += 1;
    }
    Ok(())
}
