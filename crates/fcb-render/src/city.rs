#![forbid(unsafe_code)]

//! Retained City geometry, orthographic projection, and CPU picking.
//!
//! A host supplies immutable atlas footprints and observed byte counts. This
//! module does not read sources, lay out files, run a clock, or own a renderer.
//! Preparation is bounded worker work; changing a camera and picking allocate
//! nothing. The uploaded instances and the picking hierarchy use the SAME f32
//! geometry. Ground coordinates are x-right/y-down, with positive z upward.

use std::mem::size_of;

use fcb_core::{ByteLength, LayoutRevision, PresentedFrameId, Rect2D,
    ResourceAllocationId, ResourceBudget, ResourceLease};

pub const MAX_CITY_RECORDS: usize = 20_000;
pub const METRIC_NAME: &str = "capped-log-observed-bytes";
pub const METRIC_CAP_BYTES: u64 = 1 << 30;
pub const MAX_TILT_RADIANS: f64 = 75.0 * std::f64::consts::PI / 180.0;
pub const DEFAULT_TILT_RADIANS: f64 = 0.70;
const MAX_VIEWPORT_POINTS: f64 = 65_536.0;
const MAX_HEIGHT_FRACTION: f64 = 0.12;
const KNOWN_HEIGHT_FLOOR: f64 = 0.02;
const NO_NODE: u32 = u32::MAX;
const PICK_STACK: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CityError {
    TooManyRecords, InvalidRecord, DuplicateFile, InvalidCamera, InvalidPoint,
    PrecisionLost, ResourceDenied, Canceled, OwnerMismatch, StaleFrame,
}
impl CityError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooManyRecords => "CITY_RECORD_LIMIT",
            Self::InvalidRecord => "CITY_INVALID_RECORD",
            Self::DuplicateFile => "CITY_DUPLICATE_FILE",
            Self::InvalidCamera => "CITY_INVALID_CAMERA",
            Self::InvalidPoint => "CITY_INVALID_POINT",
            Self::PrecisionLost => "CITY_PRECISION_LOST",
            Self::ResourceDenied => "CITY_RESOURCE_DENIED",
            Self::Canceled => "CITY_CANCELED",
            Self::OwnerMismatch => "CITY_OWNER_MISMATCH",
            Self::StaleFrame => "CITY_STALE_FRAME",
        }
    }
}
impl std::fmt::Display for CityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for CityError {}

/// File IDs are local to the owner-qualified layout. They never grant source
/// access. A host must preserve its catalog-to-reader authorization separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Record {
    pub file_id: u64,
    pub footprint: Rect2D,
    pub observed_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    file_id: u64,
    footprint: Rect2D,
    observed_bytes: Option<u64>,
    rect: [f32; 4],
    height: f32,
}
impl Instance {
    pub const fn file_id(self) -> u64 { self.file_id }
    pub const fn footprint(self) -> Rect2D { self.footprint }
    pub const fn observed_bytes(self) -> Option<u64> { self.observed_bytes }
    /// Canonical scene-rebased x, y, width, height. Upload these values directly;
    /// do not reconstruct them from the original f64 footprint in another host.
    pub const fn rect(self) -> [f32; 4] { self.rect }
    pub const fn height(self) -> f32 { self.height }
    fn bounds(self) -> Bounds {
        let [x, y, width, height] = self.rect;
        // Addition must occur in f32, exactly as the unit-cube vertex shader.
        Bounds { min: [f64::from(x), f64::from(y), 0.0],
            max: [f64::from(x + width), f64::from(y + height), f64::from(self.height)] }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub records: usize,
    pub known_files: usize,
    pub unknown_files: usize,
    pub capped_files: usize,
    pub bvh_nodes: usize,
    /// Conservative admission includes the preparation scratch index, even
    /// after that scratch has been released. This is not a process RSS bound.
    pub managed_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct Bounds { min: [f64; 3], max: [f64; 3] }
impl Bounds {
    fn union(self, other: Self) -> Self {
        Self { min: std::array::from_fn(|i| self.min[i].min(other.min[i])),
            max: std::array::from_fn(|i| self.max[i].max(other.max[i])) }
    }
    fn projection(self, axis: [f64; 3]) -> (f64, f64) {
        let mut low = 0.0;
        let mut high = 0.0;
        for (i, value) in axis.into_iter().enumerate() {
            let a = self.min[i] * value;
            let b = self.max[i] * value;
            low += a.min(b); high += a.max(b);
        }
        (low, high)
    }
}

#[derive(Clone, Copy, Debug)]
struct Node { bounds: Bounds, left: u32, right: u32, instance: u32 }

/// One immutable layout, metric projection and balanced AABB hierarchy.
/// Layout revisions must not be reused for different source geography. Its
/// reservation is also retained by frames, preventing allocation-key reuse
/// while an old display receipt remains live.
pub struct Scene {
    layout: LayoutRevision,
    origin: [f64; 2],
    bounds: Option<Rect2D>,
    max_height: f64,
    instances: Vec<Instance>,
    nodes: Vec<Node>,
    stats: Stats,
    lease: ResourceLease,
}
impl Scene {
    pub fn new(layout: LayoutRevision, records: &[Record], budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<Self, CityError> {
        if records.len() > MAX_CITY_RECORDS { return Err(CityError::TooManyRecords); }
        check(&mut canceled)?;
        let mut bounds = None;
        for record in records {
            check(&mut canceled)?;
            let rect = record.footprint;
            if record.file_id == 0 || rect.size().is_empty()
                || !rect.max_x().is_finite() || !rect.max_y().is_finite()
                || rect.max_x() <= rect.min_x() || rect.max_y() <= rect.min_y() {
                return Err(CityError::InvalidRecord);
            }
            bounds = Some(match bounds {
                None => rect,
                Some(previous) => Rect2D::union(previous, rect).map_err(|_| CityError::InvalidRecord)?,
            });
        }
        let origin = bounds.map_or([0.0, 0.0], |r|
            [r.min_x() + r.size().width() * 0.5, r.min_y() + r.size().height() * 0.5]);
        let max_height = bounds.map_or(0.0, |r|
            r.size().width().max(r.size().height()) * MAX_HEIGHT_FRACTION);
        if !origin.iter().all(|v| v.is_finite()) || !max_height.is_finite() {
            return Err(CityError::InvalidRecord);
        }
        let node_count = records.len().checked_mul(2).and_then(|n| n.checked_sub(1)).unwrap_or(0);
        let charge = records.len().checked_mul(size_of::<Instance>() + size_of::<usize>())
            .and_then(|n| node_count.checked_mul(size_of::<Node>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<Self>() + size_of::<Vec<usize>>()))
            .ok_or(CityError::ResourceDenied)?;
        let managed_bytes = u64::try_from(charge).map_err(|_| CityError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(layout.owner(), allocation, ByteLength::new(managed_bytes))
            .map_err(|_| CityError::ResourceDenied)?;
        let mut instances = reserved::<Instance>(records.len())?;
        let mut indexes = reserved::<usize>(records.len())?;
        let mut nodes = reserved::<Node>(node_count)?;
        let mut stats = Stats { records: records.len(), managed_bytes, ..Stats::default() };
        let logarithm_cap = (METRIC_CAP_BYTES as f64).ln_1p();
        for record in records {
            check(&mut canceled)?;
            let rect = [(record.footprint.min_x() - origin[0]) as f32,
                (record.footprint.min_y() - origin[1]) as f32,
                record.footprint.size().width() as f32, record.footprint.size().height() as f32];
            if !rect.iter().all(|v| v.is_finite()) || rect[2] <= 0.0 || rect[3] <= 0.0
                || !(rect[0] + rect[2]).is_finite() || !(rect[1] + rect[3]).is_finite()
                || rect[0] + rect[2] <= rect[0] || rect[1] + rect[3] <= rect[1] {
                return Err(CityError::PrecisionLost);
            }
            let height = match record.observed_bytes {
                None => { stats.unknown_files += 1; 0.0 }
                Some(bytes) => {
                    stats.known_files += 1;
                    stats.capped_files += usize::from(bytes > METRIC_CAP_BYTES);
                    let fraction = (bytes.min(METRIC_CAP_BYTES) as f64).ln_1p() / logarithm_cap;
                    (max_height * (KNOWN_HEIGHT_FLOOR + (1.0 - KNOWN_HEIGHT_FLOOR) * fraction)) as f32
                }
            };
            if !height.is_finite() || (record.observed_bytes.is_some() && height <= 0.0) {
                return Err(CityError::PrecisionLost);
            }
            instances.push(Instance { file_id: record.file_id, footprint: record.footprint,
                observed_bytes: record.observed_bytes, rect, height });
        }
        instances.sort_unstable_by_key(|instance| instance.file_id);
        if instances.windows(2).any(|pair| pair[0].file_id == pair[1].file_id) {
            return Err(CityError::DuplicateFile);
        }
        indexes.extend(0..instances.len());
        if !indexes.is_empty() { build(&instances, &mut indexes, &mut nodes, &mut canceled)?; }
        stats.bvh_nodes = nodes.len();
        check(&mut canceled)?;
        Ok(Self { layout, origin, bounds, max_height, instances, nodes, stats, lease })
    }

    pub const fn layout(&self) -> LayoutRevision { self.layout }
    pub const fn origin(&self) -> [f64; 2] { self.origin }
    pub const fn bounds(&self) -> Option<Rect2D> { self.bounds }
    /// Upper end of the named metric scale, not a line count or source fact.
    pub const fn max_height(&self) -> f64 { self.max_height }
    pub fn instances(&self) -> &[Instance] { &self.instances }
    pub const fn stats(&self) -> Stats { self.stats }

    /// Fit the complete canonical scene at yaw zero and the documented default
    /// tilt. Heights participate in fitting; a tall building cannot be clipped
    /// merely because its flat footprint fits. Empty scenes use a neutral view.
    pub fn fit_camera(&self, width: f64, height: f64, padding_points: f64) -> Result<Camera, CityError> {
        let initial = Camera::new(width, height, self.origin[0], self.origin[1], 1.0,
            0.0, DEFAULT_TILT_RADIANS)?;
        if !padding_points.is_finite() || padding_points < 0.0
            || padding_points * 2.0 >= width || padding_points * 2.0 >= height {
            return Err(CityError::InvalidCamera);
        }
        let Some(root) = self.nodes.first() else { return Ok(initial); };
        let basis = initial.basis();
        let (left, right) = root.bounds.projection(basis.right);
        let (top, bottom) = root.bounds.projection(basis.down);
        let scale = ((width - 2.0 * padding_points) / (right - left))
            .min((height - 2.0 * padding_points) / (bottom - top));
        let rotated_x = left + (right - left) * 0.5;
        let rotated_y = (top + (bottom - top) * 0.5) / initial.tilt.cos();
        Camera::new(width, height, self.origin[0] + rotated_x,
            self.origin[1] + rotated_y, scale, initial.yaw, initial.tilt)
    }

    /// Prepare one immutable camera receipt. Native drawing and CPU picking
    /// must use the same accepted receipt; preparation itself is not presentation.
    pub fn frame(&self, id: PresentedFrameId, camera: Camera) -> Result<Frame, CityError> {
        if id.owner() != self.layout.owner() { return Err(CityError::OwnerMismatch); }
        let basis = camera.basis();
        let fallback = Bounds { min: [-0.5, -0.5, 0.0], max: [0.5, 0.5, 0.0] };
        let bounds = self.nodes.first().map_or(fallback, |node| node.bounds);
        // Depth is derived from the whole immutable scene, independent of pan,
        // zoom and the visible subset. Generous padding protects f32 rounding.
        let (far, near) = bounds.projection(basis.toward);
        let extent = (0..3).map(|i| bounds.max[i] - bounds.min[i]).fold(0.0, f64::max);
        let padding = (near - far).max(extent).max(f64::MIN_POSITIVE) * 0.01;
        let depth_span = near - far + 2.0 * padding;
        if !depth_span.is_finite() || depth_span <= 0.0 { return Err(CityError::PrecisionLost); }
        let center = [camera.center[0] - self.origin[0], camera.center[1] - self.origin[1]];
        let sx = 2.0 * camera.scale / camera.width;
        let sy = 2.0 * camera.scale / camera.height;
        let matrix64 = [
            sx * basis.right[0], -sy * basis.down[0], -basis.toward[0] / depth_span, 0.0,
            sx * basis.right[1], -sy * basis.down[1], -basis.toward[1] / depth_span, 0.0,
            0.0, -sy * basis.down[2], -basis.toward[2] / depth_span, 0.0,
            -sx * (basis.right[0] * center[0] + basis.right[1] * center[1]),
            sy * (basis.down[0] * center[0] + basis.down[1] * center[1]),
            (near + padding) / depth_span, 1.0,
        ];
        let matrix = matrix64.map(|value| value as f32);
        if !matrix.iter().all(|value| value.is_finite()) { return Err(CityError::PrecisionLost); }
        let inverse = inverse_linear(matrix)?;
        Ok(Frame { id, layout: self.layout, camera, origin: self.origin, matrix, inverse,
            lease: self.lease.clone() })
    }

    /// Pick the nearest visible cuboid surface, including unknown flat planes.
    /// Coordinates are logical viewport points. Outside-viewport requests miss;
    /// NaN/Inf are errors. Equal depth32Float values choose the smaller file ID,
    /// matching ascending instance upload with a strict-less GPU depth test.
    pub fn pick(&self, frame: &Frame, x: f64, y: f64) -> Result<Option<Pick>, CityError> {
        if frame.layout != self.layout || frame.lease.info() != self.lease.info() {
            return Err(CityError::StaleFrame);
        }
        if !x.is_finite() || !y.is_finite() { return Err(CityError::InvalidPoint); }
        if x < 0.0 || y < 0.0 || x >= frame.camera.width || y >= frame.camera.height
            || self.nodes.is_empty() { return Ok(None); }
        let clip = [2.0 * x / frame.camera.width - 1.0, 1.0 - 2.0 * y / frame.camera.height];
        let start = frame.unproject([clip[0], clip[1], 0.0]);
        let end = frame.unproject([clip[0], clip[1], 1.0]);
        let direction = std::array::from_fn(|i| end[i] - start[i]);
        if !start.iter().chain(end.iter()).all(|value| value.is_finite()) {
            return Err(CityError::PrecisionLost);
        }
        let mut stack = [0usize; PICK_STACK];
        let mut stack_len = 1;
        let mut best: Option<Pick> = None;
        while stack_len > 0 {
            stack_len -= 1;
            let node = self.nodes[stack[stack_len]];
            let Some(depth) = intersect(node.bounds, start, direction) else { continue; };
            if best.is_some_and(|hit| depth as f32 > hit.depth) { continue; }
            if node.instance != NO_NODE {
                let index = node.instance as usize;
                let instance = self.instances[index];
                let depth = depth as f32;
                if best.is_none_or(|hit| depth < hit.depth
                    || (depth == hit.depth && instance.file_id < hit.file_id)) {
                    best = Some(Pick { file_id: instance.file_id, instance_index: index, depth });
                }
            } else {
                // Median splits guarantee depth <= 16 for MAX_CITY_RECORDS;
                // the fixed stack also bounds traversal storage without heaps.
                if stack_len + 2 > PICK_STACK { return Err(CityError::PrecisionLost); }
                stack[stack_len] = node.right as usize;
                stack[stack_len + 1] = node.left as usize;
                stack_len += 2;
            }
        }
        Ok(best)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    width: f64, height: f64, center: [f64; 2], scale: f64, yaw: f64, tilt: f64,
}
impl Camera {
    pub fn new(width: f64, height: f64, center_x: f64, center_y: f64,
        scale: f64, yaw: f64, tilt: f64) -> Result<Self, CityError> {
        if ![width, height, center_x, center_y, scale, yaw, tilt].iter().all(|v| v.is_finite())
            || width <= 0.0 || height <= 0.0 || width > MAX_VIEWPORT_POINTS || height > MAX_VIEWPORT_POINTS
            || scale <= 0.0 || !(0.0..=MAX_TILT_RADIANS).contains(&tilt) {
            return Err(CityError::InvalidCamera);
        }
        Ok(Self { width, height, center: [center_x, center_y], scale,
            yaw: yaw.rem_euclid(std::f64::consts::TAU), tilt })
    }
    pub const fn width(self) -> f64 { self.width }
    pub const fn height(self) -> f64 { self.height }
    pub const fn center(self) -> [f64; 2] { self.center }
    pub const fn scale(self) -> f64 { self.scale }
    pub const fn yaw(self) -> f64 { self.yaw }
    pub const fn tilt(self) -> f64 { self.tilt }

    /// Positive deltas drag ground-plane content in the gesture direction.
    pub fn pan(self, delta_x: f64, delta_y: f64) -> Result<Self, CityError> {
        if !delta_x.is_finite() || !delta_y.is_finite() { return Err(CityError::InvalidCamera); }
        let delta = self.ground_delta(delta_x, delta_y);
        Self::new(self.width, self.height, self.center[0] - delta[0], self.center[1] - delta[1],
            self.scale, self.yaw, self.tilt)
    }
    /// Preserve the z=0 ground location under the logical pointer.
    pub fn zoom_at(self, anchor_x: f64, anchor_y: f64, factor: f64) -> Result<Self, CityError> {
        if !anchor_x.is_finite() || !anchor_y.is_finite() || !factor.is_finite() || factor <= 0.0 {
            return Err(CityError::InvalidCamera);
        }
        let old = self.ground_delta(anchor_x - self.width * 0.5, anchor_y - self.height * 0.5);
        let mut next = Self::new(self.width, self.height, self.center[0], self.center[1],
            self.scale * factor, self.yaw, self.tilt)?;
        let new = next.ground_delta(anchor_x - self.width * 0.5, anchor_y - self.height * 0.5);
        next.center = [self.center[0] + old[0] - new[0], self.center[1] + old[1] - new[1]];
        Self::new(next.width, next.height, next.center[0], next.center[1], next.scale, next.yaw, next.tilt)
    }
    /// Deliberate orbit keeps ground center and scale. Pitch is clamped so
    /// ground-plane dragging remains invertible; yaw wraps without a hard stop.
    pub fn orbit(self, delta_yaw: f64, delta_tilt: f64) -> Result<Self, CityError> {
        if !delta_yaw.is_finite() || !delta_tilt.is_finite() { return Err(CityError::InvalidCamera); }
        Self::new(self.width, self.height, self.center[0], self.center[1], self.scale,
            self.yaw + delta_yaw.rem_euclid(std::f64::consts::TAU),
            (self.tilt + delta_tilt).clamp(0.0, MAX_TILT_RADIANS))
    }
    pub fn with_viewport(self, width: f64, height: f64) -> Result<Self, CityError> {
        Self::new(width, height, self.center[0], self.center[1], self.scale, self.yaw, self.tilt)
    }
    fn ground_delta(self, x: f64, y: f64) -> [f64; 2] {
        let (sine, cosine) = self.yaw.sin_cos();
        let x = x / self.scale;
        let y = y / (self.scale * self.tilt.cos());
        [cosine * x + sine * y, -sine * x + cosine * y]
    }
    fn basis(self) -> Basis {
        let (sine, cosine) = self.yaw.sin_cos();
        let (tilt_sine, tilt_cosine) = self.tilt.sin_cos();
        Basis { right: [cosine, -sine, 0.0],
            down: [tilt_cosine * sine, tilt_cosine * cosine, -tilt_sine],
            toward: [tilt_sine * sine, tilt_sine * cosine, tilt_cosine] }
    }
}
struct Basis { right: [f64; 3], down: [f64; 3], toward: [f64; 3] }

/// An immutable presentation candidate. A host acknowledges it only after the
/// associated instance buffer and matrix have actually reached the drawable.
#[derive(Clone, Debug)]
pub struct Frame {
    id: PresentedFrameId,
    layout: LayoutRevision,
    camera: Camera,
    origin: [f64; 2],
    matrix: [f32; 16],
    inverse: [[f64; 3]; 3],
    lease: ResourceLease,
}
impl Frame {
    pub const fn id(&self) -> PresentedFrameId { self.id }
    pub const fn layout(&self) -> LayoutRevision { self.layout }
    pub const fn camera(&self) -> Camera { self.camera }
    /// Column-major matrix for Metal (x/y -1..1, z 0..1, w=1). Its input is
    /// the canonical scene-rebased instance coordinates, never original paths.
    pub const fn matrix(&self) -> [f32; 16] { self.matrix }
    /// Reference projection into logical viewport x/y and Metal depth. This
    /// uses the actual f32 matrix coefficients, not an unrounded second camera.
    pub fn project(&self, world: [f64; 3]) -> Result<[f64; 3], CityError> {
        if !world.iter().all(|value| value.is_finite()) { return Err(CityError::InvalidPoint); }
        let local = [world[0] - self.origin[0], world[1] - self.origin[1], world[2]];
        let clip: [f64; 3] = std::array::from_fn(|row| f64::from(self.matrix[12 + row])
            + (0..3).map(|column| f64::from(self.matrix[column * 4 + row]) * local[column]).sum::<f64>());
        let point = [(clip[0] + 1.0) * self.camera.width * 0.5,
            (1.0 - clip[1]) * self.camera.height * 0.5, clip[2]];
        if !point.iter().all(|value| value.is_finite()) { return Err(CityError::PrecisionLost); }
        Ok(point)
    }
    fn unproject(&self, clip: [f64; 3]) -> [f64; 3] {
        let translated: [f64; 3] = std::array::from_fn(|row| clip[row] - f64::from(self.matrix[12 + row]));
        std::array::from_fn(|row| (0..3).map(|column| self.inverse[row][column] * translated[column]).sum())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pick { file_id: u64, instance_index: usize, depth: f32 }
impl Pick {
    pub const fn file_id(self) -> u64 { self.file_id }
    pub const fn instance_index(self) -> usize { self.instance_index }
    pub const fn depth(self) -> f32 { self.depth }
}

fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), CityError> {
    if canceled() { Err(CityError::Canceled) } else { Ok(()) }
}
fn reserved<T>(capacity: usize) -> Result<Vec<T>, CityError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| CityError::ResourceDenied)?;
    if values.capacity() > capacity { return Err(CityError::ResourceDenied); }
    Ok(values)
}
fn build(instances: &[Instance], indexes: &mut [usize], nodes: &mut Vec<Node>,
    canceled: &mut impl FnMut() -> bool) -> Result<u32, CityError> {
    check(canceled)?;
    let bounds = indexes.iter().skip(1).fold(instances[indexes[0]].bounds(),
        |bounds, &index| bounds.union(instances[index].bounds()));
    let slot = nodes.len();
    nodes.push(Node { bounds, left: NO_NODE, right: NO_NODE, instance: NO_NODE });
    if indexes.len() == 1 { nodes[slot].instance = indexes[0] as u32; }
    else {
        let axis = usize::from(bounds.max[1] - bounds.min[1] > bounds.max[0] - bounds.min[0]);
        let middle = indexes.len() / 2;
        indexes.select_nth_unstable_by(middle, |&a, &b| {
            let a_bounds = instances[a].bounds(); let b_bounds = instances[b].bounds();
            let a_center = a_bounds.min[axis] + (a_bounds.max[axis] - a_bounds.min[axis]) * 0.5;
            let b_center = b_bounds.min[axis] + (b_bounds.max[axis] - b_bounds.min[axis]) * 0.5;
            a_center.total_cmp(&b_center).then_with(|| instances[a].file_id.cmp(&instances[b].file_id))
        });
        let (left, right) = indexes.split_at_mut(middle);
        let left = build(instances, left, nodes, canceled)?;
        let right = build(instances, right, nodes, canceled)?;
        nodes[slot].left = left;
        nodes[slot].right = right;
    }
    Ok(slot as u32)
}
fn intersect(bounds: Bounds, start: [f64; 3], direction: [f64; 3]) -> Option<f64> {
    let mut enter: f64 = 0.0;
    let mut exit: f64 = 1.0;
    for axis in 0..3 {
        if direction[axis] == 0.0 {
            if start[axis] < bounds.min[axis] || start[axis] > bounds.max[axis] { return None; }
        } else {
            let first = (bounds.min[axis] - start[axis]) / direction[axis];
            let last = (bounds.max[axis] - start[axis]) / direction[axis];
            enter = enter.max(first.min(last)); exit = exit.min(first.max(last));
            if enter > exit { return None; }
        }
    }
    Some(enter)
}
fn inverse_linear(matrix: [f32; 16]) -> Result<[[f64; 3]; 3], CityError> {
    let a = f64::from(matrix[0]); let b = f64::from(matrix[4]); let c = f64::from(matrix[8]);
    let d = f64::from(matrix[1]); let e = f64::from(matrix[5]); let f = f64::from(matrix[9]);
    let g = f64::from(matrix[2]); let h = f64::from(matrix[6]); let i = f64::from(matrix[10]);
    let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !determinant.is_finite() || determinant == 0.0 { return Err(CityError::PrecisionLost); }
    let inverse = [[(e * i - f * h) / determinant, (c * h - b * i) / determinant, (b * f - c * e) / determinant],
        [(f * g - d * i) / determinant, (a * i - c * g) / determinant, (c * d - a * f) / determinant],
        [(d * h - e * g) / determinant, (b * g - a * h) / determinant, (a * e - b * d) / determinant]];
    if !inverse.iter().flatten().all(|value| value.is_finite()) { return Err(CityError::PrecisionLost); }
    Ok(inverse)
}
