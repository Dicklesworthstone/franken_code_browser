#![forbid(unsafe_code)]

//! Exact atlas text hit -> retained Markdown document. The document starts at
//! its beginning or an explicit heading; Markdown syntax hits do not fabricate
//! a rendered-text highlight. Original matched bytes remain separately exact.
use fcb::map::workspace::text_search::AtlasTextOverlay;
use super::*;

impl From<crate::markdown::Error> for Error {
    fn from(error: crate::markdown::Error) -> Self { Self::Document(error) }
}

pub(super) fn write(out: &mut Output, hits: Option<&AtlasTextOverlay<'_, '_, '_>>,
    index: &AtlasIndex<'_>, options: &Options, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<bool, Error> {
    let json = options.workspace.json;
    if json { out.literal(",\"document_preview\":")?; }
    let Some(position) = options.markdown_hit else {
        if json { out.literal("null")?; }
        return Ok(true);
    };
    let hits = hits.ok_or(WorkspaceTextError::MissingHit)?;
    let selected = hits.select_hit(hits.source(), position, generation())?;
    if json {
        out.literal("{\"hit_index\":")?; out.integer(position as u64)?;
        out.literal(",\"node\":")?; node_record(out, index, selected.hit().node())?;
        out.literal(",\"selected_original_range\":")?; out.range(selected.hit().original_range())?;
        out.literal(",\"selected_original_hex\":")?; out.hex(selected.original_bytes())?;
        out.literal(",\"matched_text\":")?; out.quoted(selected.matched_text())?;
        out.literal(",\"rendered_hit_highlight\":false,\"navigation\":\"document-start-or-explicit-heading\",\"document\":")?;
    } else {
        out.literal("Markdown from retained search-hit capture: ")?;
        out.path(&hits.source().atlas().entry(selected.hit().node())?.path().raw().to_path_buf())?;
        out.literal("\n")?;
    }
    let mut stop = || canceled() || hits.source().validate_active().is_err();
    let result = crate::markdown::write_capture(selected.capture(), &options.markdown_view, json, out, budget, &mut stop);
    // Validate even when cancellation was caused by grant revocation. A pending
    // output candidate cannot outlive the authority that admitted its capture.
    hits.validate_delivery(hits.source(), generation())?;
    let entire = result?;
    if canceled() { return Err(AppError::Canceled.into()); }
    if json { out.literal("}")?; }
    Ok(entire)
}
