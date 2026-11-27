//! Bounded path marshaling shared by live, captured and indexed repository searches.

use std::ffi::c_char;
use fcb_app::host::atlas_search::{AtlasSearchError, AtlasSearchPathKind, AtlasSearchScope,
    MAX_SEARCH_REGION_WIRE_BYTES, MAX_SEARCH_SCOPE_PATH_BYTES};

/// The caller enforces its endpoint's null policy. Every non-null pointer must
/// be readable through its first NUL or the kind's allowance plus one byte.
/// No foreign memory is retained by the resulting predicate.
pub(super) unsafe fn with_path<E: From<AtlasSearchError>>(scope: AtlasSearchScope,
    kind: AtlasSearchPathKind, pointer: *const c_char, invalid: E, overlong: E,
) -> Result<AtlasSearchScope, E> {
    let limit = if kind == AtlasSearchPathKind::Visible {
        MAX_SEARCH_REGION_WIRE_BYTES
    } else {
        MAX_SEARCH_SCOPE_PATH_BYTES
    };
    let bytes = if pointer.is_null() {
        &[][..]
    } else {
        let Some(length) = (0..=limit).find(|&offset| unsafe { *pointer.add(offset) == 0 }) else {
            return Err(overlong);
        };
        unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) }
    };
    if kind == AtlasSearchPathKind::Visible {
        let csv = std::str::from_utf8(bytes).map_err(|_| invalid)?;
        scope.with_path(kind, csv).map_err(E::from)
    } else {
        scope.with_raw_path(kind, bytes).map_err(E::from)
    }
}
