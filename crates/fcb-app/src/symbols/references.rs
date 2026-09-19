#![forbid(unsafe_code)]

//! Reference-candidate CLI adapter. Source admission, capture, workspace and
//! snapshot traversal stay with the ordinary symbol-navigation command.

use super::*;
use fcb::search::symbols::{CapturedReferences, ReferenceError};

#[allow(clippy::too_many_arguments)]
pub(super) fn analyze(settings: &Settings, request: CaptureRequest, path: &[u8],
    language: Option<SymbolLanguage>, bytes: &[u8], state: &mut RunState,
    out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<(), Failure> {
    // Name syntax was validated before the source was opened. Refuse rather
    // than converting a missing/invalid name into a complete negative result.
    let name = settings.name.as_deref().ok_or_else(|| Failure::new("REFERENCE_INVALID_NAME"))?;
    let capacity = settings.limit.saturating_sub(state.stats.emitted);
    let references = match CapturedReferences::build(bytes, request, generation(), name,
        settings.encoding, capacity, budget, allocation(204), &mut *canceled) {
        Ok(references) => references,
        Err(ReferenceError::Canceled) => return Err(Failure::canceled()),
        Err(error) if matches!(error, ReferenceError::InvalidName | ReferenceError::InvalidLimits) =>
            return Err(Failure::new(error.code())),
        Err(error) => {
            state.stats.refused += 1;
            state.notice(request.file(), path, error.code());
            return Ok(());
        }
    };
    state.stats.analyzed += 1;
    state.stats.analysis_bytes += bytes.len() as u64;
    // Includes at most one unstored lookahead per file, never an invented total.
    // Even with no remaining display capacity, an absent name is distinguished
    // from another matching file by the engine's zero-capacity scan.
    state.stats.matches += references.total_matches_counted();
    if references.output_limited() {
        state.stats.limited += 1;
        state.notice(request.file(), path, "REFERENCE_ITEM_LIMIT");
    }
    for candidate in references.candidates() {
        if canceled() { return Err(Failure::canceled()); }
        if settings.json {
            if state.stats.emitted > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(request.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(request.revision().get())?;
            out.literal(",\"candidate_id\":")?; out.integer(candidate.id())?;
            out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(path).to_path_buf())?;
            out.literal(",\"name\":")?; out.quoted(references.name())?;
            out.literal(",\"kind\":\"reference-candidate\",\"evidence\":")?; out.quoted(candidate.evidence_level())?;
            out.literal(",\"language\":")?;
            match language { Some(language) => out.quoted(language.name())?, None => out.literal("null")? }
            out.literal(",\"encoding\":")?; out.quoted(references.encoding().name())?;
            out.literal(",\"line\":")?; out.integer(candidate.line())?;
            out.literal(",\"original_range\":")?; out.range(candidate.original_range())?;
            out.literal(",\"name_range\":")?; out.range(candidate.original_range())?;
            out.literal("}")?;
        } else {
            out.path(&RawPath::from_bytes(path).to_path_buf())?;
            out.literal(" reference-candidate ")?; out.human_text(references.name())?;
            out.literal(" at line ")?; out.literal(&candidate.line().to_string())?;
            out.literal(" bytes ")?; out.literal(&candidate.original_range().start().get().to_string())?;
            out.literal("..")?; out.literal(&candidate.original_range().end().get().to_string())?;
            out.literal(" [whole-token-text-candidate]\n")?;
        }
        state.stats.emitted += 1;
    }
    Ok(())
}
