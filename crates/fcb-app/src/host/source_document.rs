#![forbid(unsafe_code)]
//! One exact source observation plus upstream lexical roles for native drawing.
use std::path::Path;
use fcb::{ByteLength};
use fcb::search::ResourceBudget;
use fcb::document::source_highlight::{source_highlight, SourceHighlightError};
use crate::{AppError, MANAGED_BYTES, allocation, owner};
use crate::output::{Output, MAX_RESPONSE_BYTES};
use super::{HostError, HostResponse, MAX_HOST_TEXT_BYTES};

pub fn read(path: &Path, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, HostError> {
    let source = super::read_text(path, MAX_HOST_TEXT_BYTES, &mut canceled)?;
    render(path, &source, canceled)
}

pub(super) fn render(path: &Path, source: &super::HostText, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, HostError> {
    let budget = ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
    let syntax = source_highlight(source.as_str(), path.extension().and_then(|s| s.to_str()).unwrap_or(""),
        &budget, allocation(95), owner()).map_err(|error| match error {
            SourceHighlightError::Limit => AppError::InputLimit,
            SourceHighlightError::Admission => AppError::Admission,
            SourceHighlightError::InvalidSpans => AppError::InvalidRange,
        })?;
    if canceled() { return Err(AppError::Canceled.into()); }
    // Encoder + retained String + C/native handoff overlap remain admitted.
    let lease = budget.try_reserve_managed(owner(), allocation(96), ByteLength::new((2 * MAX_RESPONSE_BYTES) as u64))
        .map_err(|_| AppError::Admission)?;
    let mut out = Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(97)).map_err(AppError::from)?;
    out.literal("{\"schema\":\"fcb.source-document/1\",\"text\":").map_err(AppError::from)?;
    out.quoted(source.as_str()).map_err(AppError::from)?;
    out.literal(",\"language\":").map_err(AppError::from)?; out.quoted(syntax.language).map_err(AppError::from)?;
    out.literal(",\"syntax_supported\":").map_err(AppError::from)?; out.boolean(syntax.syntax_supported).map_err(AppError::from)?;
    out.literal(",\"runs\":[").map_err(AppError::from)?;
    for (i, run) in syntax.runs.iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        if i != 0 { out.literal(",").map_err(AppError::from)?; }
        out.literal("{\"start\":").map_err(AppError::from)?; out.integer(run.start).map_err(AppError::from)?;
        out.literal(",\"length\":").map_err(AppError::from)?; out.integer(run.length).map_err(AppError::from)?;
        out.literal(",\"role\":").map_err(AppError::from)?; out.quoted(run.role).map_err(AppError::from)?;
        out.literal("}").map_err(AppError::from)?;
    }
    out.literal("]}").map_err(AppError::from)?;
    if canceled() { return Err(AppError::Canceled.into()); }
    let text = std::str::from_utf8(out.as_bytes()).map_err(|_| HostError::InvalidUtf8)?.to_owned();
    Ok(HostResponse { text, exit_code: crate::EXIT_OK, _lease: lease })
}
