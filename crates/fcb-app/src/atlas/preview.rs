#![forbid(unsafe_code)]

//! Render exact retained source context without reopening a mutable path.
use fcb::map::workspace::text_preview::AtlasTextPreview;
use fcb::map::workspace::text_search::AtlasTextOverlay;
use super::*;

pub(super) fn json(out: &mut Output, preview: Option<&AtlasTextPreview<'_>>,
    overlay: Option<&AtlasTextOverlay<'_, '_, '_>>, index: &AtlasIndex<'_>,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    let Some(preview) = preview else { return Ok(()); };
    let overlay = overlay.ok_or(AppError::InvalidRange)?;
    preview.validate_delivery(overlay, overlay.source(), generation())?;
    if canceled() { return Err(AppError::Canceled.into()); }
    let hit = preview.hit(); let text = preview.text(); let selected = preview.selected_text_range();
    out.literal(",\"text_preview\":{\"mode\":\"logical-captured-text-not-shaped\",\"additional_source_bytes_read\":\"0\",\"hit_index\":")?;
    out.integer(preview.hit_index() as u64)?;
    out.literal(",\"node\":")?; node_record(out, index, hit.node())?;
    out.literal(",\"file_id\":")?; out.integer(hit.file().get())?;
    out.literal(",\"revision\":")?; out.integer(hit.revision().get())?;
    out.literal(",\"visible_range\":")?; out.range(text.range())?;
    out.literal(",\"selected_original_range\":")?; out.range(hit.original_range())?;
    out.literal(",\"selected_window_utf8_range\":{\"start\":")?; out.integer(selected.start().get())?;
    out.literal(",\"end\":")?; out.integer(selected.end().get())?;
    out.literal("},\"prefix_bytes_omitted\":")?; out.boolean(preview.prefix_bytes_omitted())?;
    out.literal(",\"suffix_bytes_omitted\":")?; out.boolean(preview.suffix_bytes_omitted())?;
    out.literal(",\"first_line\":")?;
    if let Some(line) = text.first_line_number() { out.integer(line)?; } else { out.literal("null")?; }
    out.literal(",\"has_replacements\":")?; out.boolean(text.has_replacements())?;
    out.literal(",\"text\":")?; out.quoted(text.text())?;
    out.literal(",\"original_hex\":")?;
    out.hex(text.extent().range_bytes(text.range()).map_err(AppError::from)?)?;
    out.literal("}")?;
    Ok(())
}

pub(super) fn human(out: &mut Output, preview: Option<&AtlasTextPreview<'_>>,
    overlay: Option<&AtlasTextOverlay<'_, '_, '_>>, canceled: &mut impl FnMut() -> bool)
    -> Result<(), Error> {
    let Some(preview) = preview else { return Ok(()); };
    let overlay = overlay.ok_or(AppError::InvalidRange)?;
    preview.validate_delivery(overlay, overlay.source(), generation())?;
    if canceled() { return Err(AppError::Canceled.into()); }
    out.literal("[retained source context around hit ")?;
    out.literal(&preview.hit_index().to_string())?;
    out.literal("; logical text; no additional file read]\n")?;
    out.human_text(preview.text().text())?;
    if !preview.text().text().ends_with('\n') { out.literal("\n")?; }
    Ok(())
}
