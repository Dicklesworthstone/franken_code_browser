#![forbid(unsafe_code)]

//! The relationship owner lends its existing workspace-wide parcel geometry.
//! This is a bounded serialization view, not a graph layout or a live frame.
//! Both evidence projections include this module so their wire contracts agree.
use super::{AtlasSearchError, AtlasSession, FileId, Output};

pub(super) fn header(out: &mut Output, atlas: &AtlasSession, filter: Option<FileId>)
    -> Result<(), AtlasSearchError> {
    let index = atlas.atlas().index().map_err(|_| AtlasSearchError::WrongAtlas)?;
    let world = index.bounds_in(index.root_node(), index.root_node())
        .map_err(|_| AtlasSearchError::WrongAtlas)?;
    out.literal(",\"geometry_projection\":\"retained-atlas-parcels-v1\",\"world\":")?;
    rectangle(out, world)?;
    out.literal(",\"filter_parcel\":")?;
    optional_parcel(out, atlas, filter)
}

pub(super) fn row(out: &mut Output, atlas: &AtlasSession, source: FileId, target: Option<FileId>)
    -> Result<(), AtlasSearchError> {
    out.literal(",\"source_parcel\":")?;
    parcel(out, atlas, source)?;
    out.literal(",\"target_parcel\":")?;
    optional_parcel(out, atlas, target)
}

fn optional_parcel(out: &mut Output, atlas: &AtlasSession, file: Option<FileId>)
    -> Result<(), AtlasSearchError> {
    if let Some(file) = file { parcel(out, atlas, file)?; } else { out.literal("null")?; }
    Ok(())
}

fn parcel(out: &mut Output, atlas: &AtlasSession, file: FileId) -> Result<(), AtlasSearchError> {
    // Search receipts are bound to atlas(), not the separately prepared view().
    let retained = atlas.atlas();
    let node = retained.node_for_file(file).map_err(|_| AtlasSearchError::WrongAtlas)?;
    let index = retained.index().map_err(|_| AtlasSearchError::WrongAtlas)?;
    let bounds = index.bounds_in(node, index.root_node()).map_err(|_| AtlasSearchError::WrongAtlas)?;
    rectangle(out, bounds)
}

fn rectangle(out: &mut Output, rect: fcb::Rect2D) -> Result<(), AtlasSearchError> {
    out.literal("{")?;
    for (ordinal, (key, value)) in [("x", rect.min_x()), ("y", rect.min_y()),
        ("w", rect.size().width()), ("h", rect.size().height())].into_iter().enumerate() {
        if !value.is_finite() || value < 0.0 { return Err(AtlasSearchError::WrongAtlas); }
        if ordinal != 0 { out.literal(",")?; }
        // Relationship schemas keep numeric wire values as strings, including
        // these finite coordinates; full-width identities retain their parser.
        out.quoted(key)?; out.literal(":")?; out.quoted(&value.to_string())?;
    }
    out.literal("}")?;
    Ok(())
}
