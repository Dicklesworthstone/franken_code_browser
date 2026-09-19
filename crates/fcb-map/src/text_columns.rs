//! Dense measured-text packing, independent of hierarchy parcel weights.
//!
//! The caller measures complete shaped text tiles at a common width. Packing
//! preserves those dimensions and input order; it never clips or samples text.

use fcb_core::Rect2D;

pub const MAX_TEXT_TILES: usize = 65_536;
pub const MAX_TEXT_COLUMNS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextColumnsError {
    InvalidColumns,
    InvalidDimension,
    TooManyTiles,
    GeometryOverflow,
    AllocationDenied,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextTilePosition {
    pub input_index: usize,
    pub rect: Rect2D,
}

/// Place each tile in the currently shortest column, breaking ties leftward.
///
/// Identical inputs produce identical rectangles; appending tiles never moves
/// the existing prefix. Changing measured heights can change later placement.
/// Work is bounded by `MAX_TEXT_TILES * MAX_TEXT_COLUMNS`; no I/O or shaping
/// occurs here. The returned vector contains every input tile or an error.
pub fn pack_text_columns(
    heights: &[f64],
    columns: usize,
    width: f64,
    gap: f64,
) -> Result<Vec<TextTilePosition>, TextColumnsError> {
    if !(1..=MAX_TEXT_COLUMNS).contains(&columns) {
        return Err(TextColumnsError::InvalidColumns);
    }
    if heights.len() > MAX_TEXT_TILES {
        return Err(TextColumnsError::TooManyTiles);
    }
    if !width.is_finite() || width <= 0.0 || !gap.is_finite() || gap < 0.0
        || heights.iter().any(|height| !height.is_finite() || *height <= 0.0)
    {
        return Err(TextColumnsError::InvalidDimension);
    }
    let mut positions = Vec::new();
    positions.try_reserve_exact(heights.len())
        .map_err(|_| TextColumnsError::AllocationDenied)?;
    if heights.is_empty() {
        return Ok(positions);
    }
    let mut xs = [0.0; MAX_TEXT_COLUMNS];
    let mut bottoms = [0.0; MAX_TEXT_COLUMNS];
    let mut occupied = [false; MAX_TEXT_COLUMNS];
    // Repeated endpoint addition avoids a rounded multiplication making
    // neighboring columns overlap at very large coordinates.
    for column in 1..columns {
        xs[column] = add_gap(positive_end(xs[column - 1], width)?, gap)?;
    }
    for (input_index, &height) in heights.iter().enumerate() {
        let mut column = 0;
        for candidate in 1..columns {
            if bottoms[candidate] < bottoms[column] {
                column = candidate;
            }
        }
        let y = if occupied[column] { add_gap(bottoms[column], gap)? } else { 0.0 };
        let bottom = positive_end(y, height)?;
        positive_end(xs[column], width)?;
        let rect = Rect2D::from_xywh(xs[column], y, width, height)
            .map_err(|_| TextColumnsError::GeometryOverflow)?;
        positions.push(TextTilePosition { input_index, rect });
        bottoms[column] = bottom;
        occupied[column] = true;
    }
    Ok(positions)
}

fn positive_end(start: f64, length: f64) -> Result<f64, TextColumnsError> {
    let end = start + length;
    if !end.is_finite() || end <= start {
        return Err(TextColumnsError::GeometryOverflow);
    }
    Ok(end)
}

fn add_gap(end: f64, gap: f64) -> Result<f64, TextColumnsError> {
    if gap == 0.0 { Ok(end) } else { positive_end(end, gap) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interiors_overlap;

    #[test]
    fn variable_heights_fill_shortest_column_without_losing_tiles() {
        let heights = [100.0, 20.0, 30.0, 10.0, 90.0];
        let tiles = pack_text_columns(&heights, 2, 80.0, 4.0).unwrap();
        let origins = [(0.0, 0.0), (84.0, 0.0), (84.0, 24.0), (84.0, 58.0), (84.0, 72.0)];
        assert_eq!(tiles.len(), heights.len());
        for (i, tile) in tiles.iter().enumerate() {
            assert_eq!(tile.input_index, i);
            assert_eq!((tile.rect.min_x(), tile.rect.min_y()), origins[i]);
            assert_eq!(tile.rect.size().width(), 80.0);
            assert_eq!(tile.rect.size().height(), heights[i]);
            for prior in &tiles[..i] { assert!(!interiors_overlap(prior.rect, tile.rect)); }
        }
    }

    #[test]
    fn ties_are_leftward_and_appending_preserves_placement() {
        let initial = pack_text_columns(&[20.0, 20.0, 20.0], 2, 5.0, 0.0).unwrap();
        assert_eq!(initial[2].rect.min_x(), 0.0);
        assert_eq!(initial[2].rect.min_y(), 20.0);
        assert_eq!(initial, pack_text_columns(&[20.0, 20.0, 20.0], 2, 5.0, 0.0).unwrap());
        let extended = pack_text_columns(&[20.0, 20.0, 20.0, 40.0], 2, 5.0, 0.0).unwrap();
        assert_eq!(initial, extended[..3]);
    }

    #[test]
    fn empty_and_budget_boundary() {
        assert!(pack_text_columns(&[], 1, 1.0, 0.0).unwrap().is_empty());
        let heights = vec![1.0; MAX_TEXT_TILES];
        let tiles = pack_text_columns(&heights, 1, 1.0, 0.0).unwrap();
        assert_eq!(tiles.len(), MAX_TEXT_TILES);
        assert_eq!(tiles.last().unwrap().rect.max_y(), MAX_TEXT_TILES as f64);
        assert_eq!(pack_text_columns(&vec![1.0; MAX_TEXT_TILES + 1], 1, 1.0, 0.0), Err(TextColumnsError::TooManyTiles));
    }

    #[test]
    fn rejects_invalid_dimensions_and_columns_even_when_empty() {
        for columns in [0, MAX_TEXT_COLUMNS + 1, usize::MAX] {
            assert_eq!(pack_text_columns(&[], columns, 1.0, 0.0), Err(TextColumnsError::InvalidColumns));
        }
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(pack_text_columns(&[bad], 1, 1.0, 0.0), Err(TextColumnsError::InvalidDimension));
            assert_eq!(pack_text_columns(&[], 1, bad, 0.0), Err(TextColumnsError::InvalidDimension));
        }
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(pack_text_columns(&[], 1, 1.0, bad), Err(TextColumnsError::InvalidDimension));
        }
        assert_eq!(pack_text_columns(&[1.0; MAX_TEXT_COLUMNS], MAX_TEXT_COLUMNS, 1.0, 0.0).unwrap().len(), MAX_TEXT_COLUMNS);
    }

    #[test]
    fn rejects_overflow_and_unrepresentable_positive_extent() {
        for (heights, columns, width, gap) in [
            (vec![f64::MAX, f64::MAX], 1, 1.0, 0.0),
            (vec![1.0, 1.0], 2, f64::MAX, 0.0),
            (vec![f64::MAX, 1.0], 1, 1.0, 0.0),
            (vec![1.0, 1.0], 2, f64::MAX, f64::MAX),
        ] {
            assert_eq!(pack_text_columns(&heights, columns, width, gap), Err(TextColumnsError::GeometryOverflow));
        }
    }
}
