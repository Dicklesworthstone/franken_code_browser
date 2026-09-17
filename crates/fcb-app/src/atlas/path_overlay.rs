#![forbid(unsafe_code)]

//! Thin wire projection of the public retained path-query overlay. Search never
//! changes the base layout, and a matching hidden file is not a presented hit.
use std::ffi::OsStr;
use fcb::map::workspace::path_search::{AtlasPathOverlay, WorkspacePathIndex, WorkspacePathSearchError};
use fcb::search::PathSearchOptions;
use super::*;

impl From<WorkspacePathSearchError> for Error {
    fn from(error: WorkspacePathSearchError) -> Self {
        match error {
            WorkspacePathSearchError::Atlas(error) => Self::Atlas(error),
            WorkspacePathSearchError::Path(error) => Self::App(AppError::Path(error)),
            _ => Self::App(AppError::InvalidRange),
        }
    }
}

pub(super) fn build<'a, 'c>(atlas: &'a WorkspaceAtlas<'c>, needle: &OsStr, limit: usize,
    budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool) -> Result<AtlasPathOverlay<'a, 'c>, Error> {
    let paths = WorkspacePathIndex::build(atlas, budget, [allocation(63), allocation(64)], &mut *canceled)?;
    let mut options = PathSearchOptions::new(generation()); options.max_results = limit;
    let native = RawPath::from_path(Path::new(needle));
    Ok(paths.search(native.as_bytes(), options, budget, [allocation(65), allocation(66)], &mut *canceled)?)
}

pub(super) fn json(out: &mut Output, overlay: Option<&AtlasPathOverlay<'_, '_>>,
    index: &AtlasIndex<'_>, focus: AtlasNodeId, camera: Camera2D,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    out.literal(",\"path_search\":")?;
    let Some(overlay) = overlay else { out.literal("null")?; return Ok(()); };
    overlay.validate_delivery(overlay.atlas(), generation())?;
    out.literal("{\"mode\":\"native-path-fuzzy\",\"scope\":\"catalogued-workspace-paths\",\"generation\":")?;
    out.integer(overlay.generation().get())?;
    out.literal(",\"scan_complete\":")?; out.boolean(overlay.is_complete())?;
    out.literal(",\"truncated\":")?; out.boolean(overlay.truncated())?;
    out.literal(",\"matches_seen\":")?; out.integer(overlay.matches_seen() as u64)?;
    out.literal(",\"retained_matches\":")?; out.integer(overlay.hits().len() as u64)?;
    out.literal(",\"counts_describe\":\"retained-matching-files-not-text-occurrences\",\"hits\":[")?;
    for (i, hit) in overlay.hits().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        if i > 0 { out.literal(",")?; }
        out.literal("{\"node\":")?; node_record(out, index, hit.node)?;
        out.literal(",\"file_id\":")?; out.integer(hit.file.get())?;
        out.literal(",\"rank_kind\":")?; out.quoted(&format!("{:?}", hit.rank.kind))?;
        out.literal(",\"logical_rect\":")?;
        if let Some(rect) = projected_selection(index, hit.node, focus, camera)? { rectangle(out, rect)?; }
        else { out.literal("null")?; }
        out.literal("}")?;
    }
    out.literal("]}")?;
    Ok(())
}

pub(super) fn human(out: &mut Output, overlay: Option<&AtlasPathOverlay<'_, '_>>,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    let Some(overlay) = overlay else { return Ok(()); };
    overlay.validate_delivery(overlay.atlas(), generation())?;
    out.literal("Ranked path matches (not source text or presented hit regions):\n")?;
    for hit in overlay.hits() {
        if canceled() { return Err(AppError::Canceled.into()); }
        out.path(&overlay.atlas().entry(hit.node)?.path().raw().to_path_buf())?; out.literal("\n")?;
    }
    out.literal("Matching paths counted: ")?; out.literal(&overlay.matches_seen().to_string())?;
    out.literal("; retained markers: ")?; out.literal(&overlay.hits().len().to_string())?; out.literal("\n")?;
    if !overlay.is_complete() || overlay.truncated() { out.literal("PARTIAL path membership or limited match rows\n")?; }
    Ok(())
}
