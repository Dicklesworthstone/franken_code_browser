//! Hierarchical parcels weighted by complete measured text, not file metadata.
//! Pure layout: no discovery, source reading, shaping, or camera-frame work.

use std::collections::BTreeMap;
use fcb_core::Rect2D;
use crate::{LayoutError, validate_path};

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
/// is preserved by ordered subdivision, with no reserved empty slack.
/// A bounded lookahead favors portrait or landscape golden-ratio file parcels.
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

const GOLDEN: f64 = 1.618_033_988_749_895;

fn shape_cost(width: f64, height: f64) -> f64 {
    let error = (width / height).ln().abs() - GOLDEN.ln();
    error * error
}

// Each decision examines at most eleven split positions and both axes. Large
// ranges only split within the middle half by count, bounding recursion depth
// even when a generated file outweighs all its siblings. Lookahead is fixed,
// not an exhaustive partition search. Lexicographic sibling order never changes.
fn candidates(count: usize) -> Vec<usize> {
    if count <= 12 { return (1..count).collect(); }
    let mut cuts = Vec::with_capacity(9);
    for step in 0..=8 {
        let cut = count / 4 + step * (count / 2) / 8;
        if cuts.last() != Some(&cut) { cuts.push(cut); }
    }
    cuts
}

fn estimate(width: f64, height: f64, count: usize, area: f64) -> f64 {
    // A group can subdivide further: estimate equal-area rows instead of
    // mistaking its enclosing directory's aspect for every descendant's aspect.
    (1..=count.min(16)).map(|columns| {
        let rows = count as f64 / columns as f64;
        shape_cost(width / columns as f64, height / rows)
    }).fold(f64::INFINITY, f64::min) * area
}

fn choose(weights: &[f64], width: f64, height: f64, depth: u8) -> (f64, usize, bool) {
    // Minimize distortion of source area, not a vote per filename. Otherwise
    // many tiny siblings can sacrifice a large file to an extremely thin strip.
    // Squared log-aspect error penalizes the tail without a corpus-specific cap.
    if weights.len() == 1 { return (weights[0] * shape_cost(width, height), 0, true); }
    if depth == 0 { return (estimate(width, height, weights.len(), weights.iter().sum()), 0, true); }
    // Local prefix/suffix sums avoid subtracting a tiny tail from a huge total.
    let mut prefix = vec![0.0; weights.len() + 1];
    let mut suffix = vec![0.0; weights.len() + 1];
    for i in 0..weights.len() { prefix[i + 1] = prefix[i] + weights[i]; }
    for i in (0..weights.len()).rev() { suffix[i] = suffix[i + 1] + weights[i]; }
    let mut best = (f64::INFINITY, 1, true);
    for cut in candidates(weights.len()) {
        let total = prefix[cut] + suffix[cut];
        let left = prefix[cut] / total;
        let right = suffix[cut] / total;
        for along_x in [true, false] {
            let (aw, ah, bw, bh) = if along_x {
                (width * left, height, width * right, height)
            } else { (width, height * left, width, height * right) };
            let cost = choose(&weights[..cut], aw, ah, depth - 1).0
                + choose(&weights[cut..], bw, bh, depth - 1).0;
            if cost < best.0 { best = (cost, cut, along_x); }
        }
    }
    best
}

fn divide(children: &[&Branch], rect: Rect2D, output: &mut [Rect2D]) -> Result<(), LayoutError> {
    if children.len() == 1 { return place(children[0], rect, output); }
    let weights: Vec<_> = children.iter().map(|child| child.weight).collect();
    let (_, cut, along_x) = choose(&weights, rect.size().width(), rect.size().height(), 2);
    let left: f64 = weights[..cut].iter().sum();
    let right: f64 = weights[cut..].iter().sum();
    let fraction = left / (left + right);
    let (a, b) = if along_x {
        let width = rect.size().width() * fraction;
        (Rect2D::from_xywh(rect.min_x(), rect.min_y(), width, rect.size().height())?,
         Rect2D::from_xywh(rect.min_x() + width, rect.min_y(), rect.size().width() - width, rect.size().height())?)
    } else {
        let height = rect.size().height() * fraction;
        (Rect2D::from_xywh(rect.min_x(), rect.min_y(), rect.size().width(), height)?,
         Rect2D::from_xywh(rect.min_x(), rect.min_y() + height, rect.size().width(), rect.size().height() - height)?)
    };
    divide(&children[..cut], a, output)?;
    divide(&children[cut..], b, output)
}

fn place(node: &Branch, rect: Rect2D, output: &mut [Rect2D]) -> Result<(), LayoutError> {
    if let Some(index) = node.file { output[index] = rect; return Ok(()); }
    let children: Vec<_> = node.children.values().collect();
    divide(&children, rect, output)
}
