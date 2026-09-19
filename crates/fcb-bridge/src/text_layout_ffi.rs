//! Thin native marshaling for the shared measured-text layout algorithm.

use std::{ffi::c_char, fmt::Write};
use fcb_map::text_columns::{MAX_TEXT_TILES, pack_text_columns};
use super::{reply, string_out};

/// Returns owned JSON rectangles in input order, or null on refusal.
/// Every successful response includes all tiles; empty input returns `[]`.
/// Release a non-null response exactly once through `fcb_free_string`.
///
/// # Safety
/// For nonzero count, `heights` must point to that many initialized, aligned
/// readable f64 values in one allocation, unchanged until this call returns.
/// A null pointer is permitted only for count zero. Call from a host worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_text_tile_positions(
    heights: *const f64,
    count: u64,
    columns: u64,
    width: f64,
    gap: f64,
) -> *mut c_char {
    reply(|| {
        let count = usize::try_from(count).ok()?;
        if count > MAX_TEXT_TILES { return None; }
        let columns = usize::try_from(columns).ok()?;
        let heights = if count == 0 { &[] } else {
            if heights.is_null() || !heights.is_aligned() { return None; }
            // SAFETY: caller guarantees live immutable backing; the bounded
            // count also keeps slice byte length below isize::MAX.
            unsafe { std::slice::from_raw_parts(heights, count) }
        };
        let tiles = pack_text_columns(heights, columns, width, gap).ok()?;
        let mut json = String::new();
        // Scientific formatting keeps even subnormal/MAX finite coordinates
        // compact. Reserve the bounded response before beginning publication.
        json.try_reserve_exact(count.checked_mul(128)?.checked_add(2)?).ok()?;
        json.push('[');
        for (index, tile) in tiles.iter().enumerate() {
            if index > 0 { json.push(','); }
            let rect = tile.rect;
            write!(json, "[{:e},{:e},{:e},{:e}]", rect.min_x(), rect.min_y(), rect.size().width(), rect.size().height()).ok()?;
        }
        json.push(']');
        string_out(&json)
    })
}
