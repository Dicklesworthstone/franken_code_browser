#![forbid(unsafe_code)]

//! Admission for text supplied by the existing decoded search engine. This
//! module sizes its bounded working set; it does not implement another matcher.

use super::*;
use fcb::search::SearchMatch;

// The shared scanner decodes 16 KiB windows. Even a window with one mapping
// span per byte fits with its simultaneous Vec/VecDeque maps, decoded text,
// staging bytes and query-sized KMP/ring state inside this conservative bound.
// Source capture and variable-size result strings are reserved separately.
pub(super) const LOWERCASE_SCAN_SCRATCH_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn matched_text_capacity(
    needle: &str,
    scope: &AtlasSearchScope,
) -> Result<usize, AtlasSearchError> {
    if scope.case_sensitive() {
        return Ok(0);
    }
    // Each transformed matching unit can contribute at most one original
    // four-byte scalar. Count only the query's units, never the whole source.
    let units = needle.chars().try_fold(0usize, |count, ch| {
        count
            .checked_add(ch.to_lowercase().count())
            .ok_or(AppError::Admission)
    })?;
    let capacity = units.checked_mul(4).ok_or(AppError::Admission)?;
    // Keep the native wire/reader contract bounded across toolchain Unicode
    // table changes; unsupported expansion growth is an admission error.
    if capacity > needle.len().checked_mul(4).ok_or(AppError::Admission)?
        || capacity > MAX_ATLAS_SEARCH_MATCHED_TEXT_BYTES {
        return Err(AtlasSearchError::InvalidLimits);
    }
    Ok(capacity)
}

pub(super) fn retained_text_bytes(matches: usize, capacity: usize) -> Result<usize, AtlasSearchError> {
    if capacity == 0 {
        return Ok(0);
    }
    size_of::<String>()
        .checked_add(capacity)
        .and_then(|n| n.checked_mul(matches))
        .ok_or(AppError::Admission.into())
}

pub(super) fn scan_scratch_bytes(matches: usize, capacity: usize) -> Result<usize, AtlasSearchError> {
    // The shared result Vec grows geometrically. Include its minimum allocation
    // and temporary result/source-spelling overlap before transferring strings
    // into the separately reserved retained snapshot.
    matches
        .max(4)
        .checked_mul(2)
        .and_then(|n| n.checked_mul(size_of::<SearchMatch>() + capacity))
        .and_then(|n| n.checked_add(LOWERCASE_SCAN_SCRATCH_BYTES))
        .ok_or(AppError::Admission.into())
}
