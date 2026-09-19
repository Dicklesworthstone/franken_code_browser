//! Hierarchical parcels weighted by complete measured text, not file metadata.
//! Pure layout: no discovery, source reading, shaping, or camera-frame work.

use std::collections::BTreeMap;
use fcb_core::Rect2D;
use crate::{LayoutError, pack_ordered, validate_path};

pub const MAX_TEXT_FILES: usize = 20_000;
const MAX_PATH_BYTES: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 131_072;

#[derive(Clone, Copy, Debug)]
pub struct MeasuredFile<'a> {
    pub path: &'a [u8],
    pub area: f64,
}

#[derive(Default)]
struct Branch {
    children: BTreeMap<Vec<u8>, Branch>,
    file: Option<usize>,
    weight: f64,
}

/// Output rectangles correspond exactly to input files. Directory adjacency
/// comes from the shared ordered treemap packer, with no reserved empty slack.
pub fn pack_text_parcels(files: &[MeasuredFile<'_>], aspect: f64) -> Result<Vec<Rect2D>, LayoutError> {
    if files.len() > MAX_TEXT_FILES || !aspect.is_finite() || !(0.25..=4.0).contains(&aspect) {
        return Err(LayoutError::InvalidPath);
    }
    if files.is_empty() { return Ok(Vec::new()); }
    let mut root = Branch::default();
    let mut path_bytes = 0usize;
    let mut node_count = 1usize;
    for (index, file) in files.iter().enumerate() {
        validate_path(file.path)?;
        path_bytes = path_bytes.checked_add(file.path.len()).ok_or(LayoutError::InvalidPath)?;
        if file.path.is_empty() || path_bytes > MAX_PATH_BYTES || file.path.split(|b| *b == b'/').count() > 64
            || !file.area.is_finite() || !(1.0..=1e12).contains(&file.area) {
            return Err(LayoutError::InvalidPath);
        }
        let mut node = &mut root;
        for segment in file.path.split(|b| *b == b'/') {
            if node.file.is_some() { return Err(LayoutError::InvalidPath); }
            if !node.children.contains_key(segment) {
                node_count += 1;
                if node_count > MAX_NODES { return Err(LayoutError::InvalidPath); }
            }
            node = node.children.entry(segment.to_vec()).or_default();
        }
        if node.file.is_some() || !node.children.is_empty() { return Err(LayoutError::InvalidPath); }
        node.file = Some(index);
        node.weight = file.area;
    }
    sum_weights(&mut root);
    let width = (root.weight * aspect).sqrt();
    let world = Rect2D::from_xywh(0.0, 0.0, width, root.weight / width).map_err(LayoutError::from)?;
    let mut output = vec![world; files.len()];
    place(&root, world, &mut output)?;
    Ok(output)
}

fn sum_weights(node: &mut Branch) -> f64 {
    if node.file.is_none() {
        node.weight = node.children.values_mut().map(sum_weights).sum();
    }
    node.weight
}

fn place(node: &Branch, rect: Rect2D, output: &mut [Rect2D]) -> Result<(), LayoutError> {
    if let Some(index) = node.file { output[index] = rect; return Ok(()); }
    let items: Vec<_> = node.children.iter().map(|(name, child)| (name.clone(), child.weight)).collect();
    for (name, child_rect) in pack_ordered(rect, &items)? {
        let child = node.children.get(&name).ok_or(LayoutError::InvalidPath)?;
        place(child, child_rect, output)?;
    }
    Ok(())
}
