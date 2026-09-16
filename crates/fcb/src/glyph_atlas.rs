#![forbid(unsafe_code)]

//! Glyph atlas management, raster identity keys, and bounded packing (FCB-017.A / fcb-bte.1).
//!
//! Plan §13.4, §13.5, §4.5, and §27.9:
//! - Separate immutable font/raster identity ([`GlyphRasterKey`]) from mutable GPU
//!   residence ([`GpuGlyphRef`]). Changing an unrelated atlas page does not invalidate
//!   every CPU glyph raster.
//! - Bounded zero-allocation lookup using [`BorrowedGlyphRasterKey`].
//! - Segregated atlas pages by [`AtlasPageKind`]:
//!   - `MonochromeGlyph`: 8-bit coverage masks for crisp reading-size typography at actual scale.
//!   - `ColorImage`: 32-bit RGBA for color emoji and images.
//!   - `LargeLabelOrTransient`: For zoom-ladder or transient display labels.
//! - Measured packing behavior: shelf-based 2D packing with 1-pixel gutter padding
//!   and bounded fragmentation tracking.
//! - Frame-in-flight pinning: resources referenced by in-flight command buffers are
//!   pinned until GPU completion (`pin_slot` / `unpin_slot`). Pinned slots cannot be
//!   evicted or overwritten under pressure.
//! - Monotonic slot generation ([`SlotGeneration`]): increments on every slot reuse so
//!   retained draw data cannot sample a different glyph accidentally. Stale references
//!   fail validation immediately with [`AtlasError::StaleSlotGeneration`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::Hash;

use fcb_core::{ArenaOwnerId, CoreError, Rect2D};

/// Maximum allowable pages per atlas instance to bound total GPU memory.
pub const DEFAULT_MAX_ATLAS_PAGES: usize = 16;

/// Default page dimension in pixels (e.g. 1024x1024).
pub const DEFAULT_ATLAS_PAGE_SIZE: u32 = 1024;

/// Default capacity for the background raster miss priority queue.
pub const DEFAULT_MAX_RASTER_QUEUE_CAPACITY: usize = 1024;

/// Default capacity for the retained diagnostic atlas lifecycle event ring.
pub const DEFAULT_ATLAS_LOG_CAPACITY: usize = 256;

/// Rasterization mode governing pixel representation and channel format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum RasterMode {
    /// 8-bit grayscale coverage mask for crisp reading-size text at actual display scale.
    MonochromeCoverage,
    /// 24-bit horizontal RGB subpixel coverage for LCD screens.
    SubpixelCoverage,
    /// 32-bit RGBA for color emoji and bitmap glyphs.
    ColorRgba,
    /// Scalable distance field representation (SDF / MSDF) for distant or transformed views.
    DistanceField,
}

/// Quantized display scale tier in millipoints (1000 = 1.0x, 2000 = 2.0x).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RasterScaleTier(pub u16);

impl RasterScaleTier {
    pub const ONE_X: Self = Self(1000);
    pub const TWO_X: Self = Self(2000);
    pub const THREE_X: Self = Self(3000);

    #[must_use]
    pub fn from_scale(scale: f32) -> Self {
        let clamped = scale.clamp(0.25, 10.0);
        let mils = (clamped * 1000.0).round() as u16;
        Self(mils)
    }

    #[must_use]
    pub fn as_f32(self) -> f32 {
        self.0 as f32 / 1000.0
    }
}

/// Subpixel phase binning (quantized 0..4 quarter-pixel offsets).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SubpixelBin {
    pub x_bin: u8,
    pub y_bin: u8,
}

impl SubpixelBin {
    pub const ZERO: Self = Self { x_bin: 0, y_bin: 0 };

    #[must_use]
    pub fn from_subpixel(offset_x: f32, offset_y: f32) -> Self {
        let qx = ((offset_x.fract().abs() * 4.0).floor() as u8).min(3);
        let qy = ((offset_y.fract().abs() * 4.0).floor() as u8).min(3);
        Self { x_bin: qx, y_bin: qy }
    }
}

/// Hinting and outline alignment policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum HintingPolicy {
    None,
    VerticalOnly,
    Full,
}

/// Immutable rasterization key identifying the exact CPU-side glyph raster (Plan §13.4).
///
/// Raster keys include only inputs that affect the glyph raster; shaping-run features
/// already reflected in the glyph ID or position do not duplicate keys.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct GlyphRasterKey {
    /// PostScript or family name of the producing font.
    pub font_name: String,
    /// OpenType glyph index produced by shaping.
    pub glyph_id: u32,
    /// Font size in millipoints (e.g. 14.0 pt -> 14000).
    pub size_px_mils: u32,
    /// Raster mode (monochrome mask, color RGBA, etc.).
    pub raster_mode: RasterMode,
    /// Quantized display scale tier.
    pub scale_tier: RasterScaleTier,
    /// Subpixel positioning phase.
    pub subpixel_bin: SubpixelBin,
    /// Hinting policy.
    pub hinting: HintingPolicy,
    /// Hash of variation axis coordinates, or 0 if default.
    pub variation_coords_hash: u64,
    /// Version of the rasterizer implementation.
    pub raster_version: u32,
}

impl GlyphRasterKey {
    /// Return a borrowed view of this key for zero-allocation lookup.
    #[must_use]
    pub fn as_borrowed(&self) -> BorrowedGlyphRasterKey<'_> {
        BorrowedGlyphRasterKey {
            font_name: &self.font_name,
            glyph_id: self.glyph_id,
            size_px_mils: self.size_px_mils,
            raster_mode: self.raster_mode,
            scale_tier: self.scale_tier,
            subpixel_bin: self.subpixel_bin,
            hinting: self.hinting,
            variation_coords_hash: self.variation_coords_hash,
            raster_version: self.raster_version,
        }
    }

    /// Compute deterministic hash value matching [`BorrowedGlyphRasterKey::compute_hash`].
    #[must_use]
    pub fn compute_hash(&self) -> u64 {
        self.as_borrowed().compute_hash()
    }
}

/// Borrowed glyph raster key enabling zero-allocation atlas cache lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BorrowedGlyphRasterKey<'a> {
    pub font_name: &'a str,
    pub glyph_id: u32,
    pub size_px_mils: u32,
    pub raster_mode: RasterMode,
    pub scale_tier: RasterScaleTier,
    pub subpixel_bin: SubpixelBin,
    pub hinting: HintingPolicy,
    pub variation_coords_hash: u64,
    pub raster_version: u32,
}

impl<'a> BorrowedGlyphRasterKey<'a> {
    /// Compute deterministic hash for this key using standard FNV-1a.
    #[must_use]
    pub fn compute_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in self.font_name.as_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= self.glyph_id as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= self.size_px_mils as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= self.raster_mode as u8 as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= self.scale_tier.0 as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= ((self.subpixel_bin.x_bin as u64) << 8) | (self.subpixel_bin.y_bin as u64);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= self.hinting as u8 as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= self.variation_coords_hash;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= self.raster_version as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash
    }

    /// Check if an owned key matches this borrowed key.
    #[must_use]
    pub fn matches(&self, owned: &GlyphRasterKey) -> bool {
        self.glyph_id == owned.glyph_id
            && self.size_px_mils == owned.size_px_mils
            && self.raster_mode == owned.raster_mode
            && self.scale_tier == owned.scale_tier
            && self.subpixel_bin == owned.subpixel_bin
            && self.hinting == owned.hinting
            && self.variation_coords_hash == owned.variation_coords_hash
            && self.raster_version == owned.raster_version
            && self.font_name == owned.font_name
    }

    /// Convert to an owned [`GlyphRasterKey`].
    #[must_use]
    pub fn to_owned_key(&self) -> GlyphRasterKey {
        GlyphRasterKey {
            font_name: self.font_name.to_string(),
            glyph_id: self.glyph_id,
            size_px_mils: self.size_px_mils,
            raster_mode: self.raster_mode,
            scale_tier: self.scale_tier,
            subpixel_bin: self.subpixel_bin,
            hinting: self.hinting,
            variation_coords_hash: self.variation_coords_hash,
            raster_version: self.raster_version,
        }
    }
}

/// Strongly-typed identifier for an atlas page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AtlasPageId(pub u32);

/// Strongly-typed identifier for a slot within an atlas page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AtlasSlotId(pub u32);

/// Monotonically incrementing generation counter per slot.
///
/// Increments on every slot reuse so retained draw data cannot sample a different
/// glyph accidentally.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SlotGeneration(pub u64);

impl SlotGeneration {
    pub const INITIAL: Self = Self(1);

    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Segregated atlas page kind (Plan §13.5).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum AtlasPageKind {
    /// Grayscale coverage masks (R8Unorm) for common UI/code typography.
    MonochromeGlyph,
    /// RGBA8 color images for emoji and bitmap glyphs.
    ColorImage,
    /// Transient large labels or distance field zoom representations.
    LargeLabelOrTransient,
}

impl AtlasPageKind {
    #[must_use]
    pub fn for_raster_mode(mode: RasterMode) -> Self {
        match mode {
            RasterMode::MonochromeCoverage | RasterMode::SubpixelCoverage => Self::MonochromeGlyph,
            RasterMode::ColorRgba => Self::ColorImage,
            RasterMode::DistanceField => Self::LargeLabelOrTransient,
        }
    }

    /// Number of bytes per pixel for texture memory accounting (Plan §4.5).
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::MonochromeGlyph => 1,
            Self::ColorImage => 4,
            Self::LargeLabelOrTransient => 4,
        }
    }
}

/// Mutable GPU residency reference for an atlas-resident glyph (Plan §13.4).
#[derive(Clone, Debug, PartialEq)]
pub struct GpuGlyphRef {
    pub owner: ArenaOwnerId,
    pub device_id: u64,
    pub device_generation: u64,
    pub page_id: AtlasPageId,
    pub slot_id: AtlasSlotId,
    pub slot_generation: SlotGeneration,
    pub pixel_rect: Rect2D,
    pub uv_rect: Rect2D,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance_x: i32,
    pub raster_key: GlyphRasterKey,
}

/// Errors occurring during atlas packing, lookup, or validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasError {
    OwnerMismatch,
    DeviceMismatch,
    StaleDeviceGeneration,
    StaleSlotGeneration,
    PageNotFound,
    SlotNotFound,
    AtlasFull,
    AllSlotsPinned,
    GlyphTooLargeForPage,
    InvalidDimensions,
    KeyNotFound,
    RasterQueueFull,
    RasterWorkerFailed,
    Core(CoreError),
}

impl fmt::Display for AtlasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnerMismatch => write!(f, "ATLAS_OWNER_MISMATCH"),
            Self::DeviceMismatch => write!(f, "ATLAS_DEVICE_MISMATCH"),
            Self::StaleDeviceGeneration => write!(f, "ATLAS_STALE_DEVICE_GENERATION"),
            Self::StaleSlotGeneration => write!(f, "ATLAS_STALE_SLOT_GENERATION"),
            Self::PageNotFound => write!(f, "ATLAS_PAGE_NOT_FOUND"),
            Self::SlotNotFound => write!(f, "ATLAS_SLOT_NOT_FOUND"),
            Self::AtlasFull => write!(f, "ATLAS_FULL"),
            Self::AllSlotsPinned => write!(f, "ATLAS_ALL_SLOTS_PINNED"),
            Self::GlyphTooLargeForPage => write!(f, "ATLAS_GLYPH_TOO_LARGE_FOR_PAGE"),
            Self::InvalidDimensions => write!(f, "ATLAS_INVALID_DIMENSIONS"),
            Self::KeyNotFound => write!(f, "ATLAS_KEY_NOT_FOUND"),
            Self::RasterQueueFull => write!(f, "ATLAS_RASTER_QUEUE_FULL"),
            Self::RasterWorkerFailed => write!(f, "ATLAS_RASTER_WORKER_FAILED"),
            Self::Core(err) => write!(f, "ATLAS_CORE_ERROR: {err:?}"),
        }
    }
}

impl std::error::Error for AtlasError {}

impl From<CoreError> for AtlasError {
    fn from(err: CoreError) -> Self {
        Self::Core(err)
    }
}

/// A horizontal shelf within an atlas page for 2D skyline bin packing.
#[derive(Clone, Debug)]
struct AtlasShelf {
    y: u32,
    height: u32,
    current_x: u32,
}

/// A resident slot inside an atlas page.
#[derive(Clone, Debug)]
pub struct AtlasSlot {
    pub slot_id: AtlasSlotId,
    pub generation: SlotGeneration,
    pub pixel_rect: Rect2D,
    pub uv_rect: Rect2D,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub is_occupied: bool,
    pub in_flight_pins: u32,
    pub last_used_frame: u64,
    pub current_key: Option<GlyphRasterKey>,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance_x: i32,
}

impl AtlasSlot {
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.in_flight_pins > 0
    }
}

/// A single texture page in the paged atlas.
#[derive(Clone, Debug)]
pub struct AtlasPage {
    pub page_id: AtlasPageId,
    pub kind: AtlasPageKind,
    pub width: u32,
    pub height: u32,
    pub allocated_pixels: u64,
    pub wasted_pixels: u64,
    shelves: Vec<AtlasShelf>,
    slots: Vec<AtlasSlot>,
    next_slot_id: u32,
}

impl AtlasPage {
    #[must_use]
    pub fn new(page_id: AtlasPageId, kind: AtlasPageKind, size: u32) -> Self {
        Self {
            page_id,
            kind,
            width: size,
            height: size,
            allocated_pixels: 0,
            wasted_pixels: 0,
            shelves: Vec::new(),
            slots: Vec::new(),
            next_slot_id: 0,
        }
    }

    /// Total texture bytes backing this page in GPU memory (Plan §4.5).
    #[must_use]
    pub const fn byte_size(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * (self.kind.bytes_per_pixel() as u64)
    }

    /// Calculate current fragmentation ratio (unusable/wasted space / total capacity).
    #[must_use]
    pub fn fragmentation_ratio(&self) -> f32 {
        let total = (self.width as f64) * (self.height as f64);
        if total == 0.0 {
            return 0.0;
        }
        let used = self.allocated_pixels as f64;
        let free_or_wasted = (total - used).max(0.0);
        (free_or_wasted / total) as f32
    }

    /// Total number of slots currently allocated on this page.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Number of slots currently in-flight pinned.
    #[must_use]
    pub fn pinned_slot_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_pinned()).count()
    }

    /// Allocate a rectangle using shelf packing with a 1-pixel gutter to prevent bleed.
    fn allocate_rect(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 || w > self.width || h > self.height {
            return None;
        }

        // 1px padding on each side: 2px total horizontal and vertical
        let padded_w = w.checked_add(2)?;
        let padded_h = h.checked_add(2)?;

        if padded_w > self.width || padded_h > self.height {
            return None;
        }

        // 1. Try to find an existing shelf that can fit this glyph
        for shelf in &mut self.shelves {
            if shelf.height >= padded_h && (self.width - shelf.current_x) >= padded_w {
                let x = shelf.current_x + 1;
                let y = shelf.y + 1;
                shelf.current_x += padded_w;
                self.allocated_pixels += (w as u64) * (h as u64);
                return Some((x, y));
            }
        }

        // 2. Start a new shelf
        let next_y = self
            .shelves
            .last()
            .map(|s| s.y + s.height)
            .unwrap_or(0);

        if self.height - next_y >= padded_h {
            let shelf_h = padded_h.max(16); // Minimum shelf height
            let new_shelf = AtlasShelf {
                y: next_y,
                height: shelf_h,
                current_x: padded_w,
            };
            let x = 1;
            let y = next_y + 1;
            self.shelves.push(new_shelf);
            self.allocated_pixels += (w as u64) * (h as u64);
            Some((x, y))
        } else {
            None
        }
    }
}

/// Priority levels for background glyph rasterization misses (Plan §13.5).
///
/// Selected reading text takes strict precedence over ordinary visible text,
/// which takes strict precedence over distant or overview map labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum RasterPriority {
    /// Distant zoom level labels, minimap, overview, or speculative prefetch.
    DistantOrMapLabel = 0,
    /// Text visible within the active reading viewport.
    VisibleReadingText = 1,
    /// Actively selected text, cursor line, or immediate keyboard focus target.
    SelectedReadingText = 2,
}

/// A background rasterization miss request queued when a glyph is absent from the atlas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RasterMissRequest {
    pub key: GlyphRasterKey,
    pub priority: RasterPriority,
    pub requested_frame: u64,
    pub estimated_width: u32,
    pub estimated_height: u32,
    pub sequence: u64,
}

/// Outcome of enqueuing a background raster miss request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueResult {
    /// New request admitted to the queue.
    Enqueued,
    /// New high-priority request admitted by evicting a lower-priority request.
    EnqueuedWithEviction,
    /// Request already existed in the queue and was upgraded to a higher priority.
    Promoted,
    /// Request already existed with equal or higher priority; requested frame refreshed.
    AlreadyPresent,
}

/// Cumulative statistics for the background raster queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RasterQueueStats {
    pub enqueued_count: u64,
    pub promoted_count: u64,
    pub evicted_count: u64,
    pub serviced_count: u64,
}

/// Bounded, deduplicated priority queue for background glyph raster misses (Plan §13.5).
///
/// Ordering guarantees:
/// 1. Higher priority tiers are serviced strictly before lower priority tiers
///    (`SelectedReadingText` > `VisibleReadingText` > `DistantOrMapLabel`).
/// 2. FIFO order is preserved among requests within the same priority tier.
/// 3. Request deduplication: repeated queries for an already-enqueued key upgrade
///    the existing request if the new query has higher priority, without queue growth.
/// 4. Capacity bounds: when the queue is at capacity, high-priority requests
///    evict the oldest lowest-priority request; lower-priority requests are rejected.
#[derive(Clone, Debug)]
pub struct BoundedRasterQueue {
    max_capacity: usize,
    requests: Vec<RasterMissRequest>,
    next_sequence: u64,
    enqueued_count: u64,
    promoted_count: u64,
    evicted_count: u64,
    serviced_count: u64,
}

impl BoundedRasterQueue {
    #[must_use]
    pub fn new(max_capacity: usize) -> Self {
        Self {
            max_capacity,
            requests: Vec::with_capacity(max_capacity.min(1024)),
            next_sequence: 1,
            enqueued_count: 0,
            promoted_count: 0,
            evicted_count: 0,
            serviced_count: 0,
        }
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.max_capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    #[must_use]
    pub fn contains(&self, key: &BorrowedGlyphRasterKey<'_>) -> bool {
        self.requests.iter().any(|r| key.matches(&r.key))
    }

    #[must_use]
    pub const fn stats(&self) -> RasterQueueStats {
        RasterQueueStats {
            enqueued_count: self.enqueued_count,
            promoted_count: self.promoted_count,
            evicted_count: self.evicted_count,
            serviced_count: self.serviced_count,
        }
    }

    pub fn clear(&mut self) {
        self.requests.clear();
    }

    /// Enqueue a raster request with deduplication, priority promotion, and capacity bounding.
    pub fn enqueue(&mut self, mut req: RasterMissRequest) -> Result<EnqueueResult, AtlasError> {
        // 1. Deduplication & priority promotion
        for item in &mut self.requests {
            if item.key == req.key {
                item.requested_frame = item.requested_frame.max(req.requested_frame);
                if req.priority > item.priority {
                    item.priority = req.priority;
                    self.promoted_count = self.promoted_count.saturating_add(1);
                    return Ok(EnqueueResult::Promoted);
                } else {
                    return Ok(EnqueueResult::AlreadyPresent);
                }
            }
        }

        // Assign sequence
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        req.sequence = seq;

        // 2. Capacity check
        if self.requests.len() >= self.max_capacity {
            if self.max_capacity == 0 {
                return Err(AtlasError::RasterQueueFull);
            }
            // Find lowest priority candidate (lowest priority, then oldest sequence)
            let mut lowest_idx = 0;
            for i in 1..self.requests.len() {
                let curr = &self.requests[i];
                let lowest = &self.requests[lowest_idx];
                if curr.priority < lowest.priority
                    || (curr.priority == lowest.priority && curr.sequence < lowest.sequence)
                {
                    lowest_idx = i;
                }
            }

            if self.requests[lowest_idx].priority < req.priority {
                self.requests.remove(lowest_idx);
                self.evicted_count = self.evicted_count.saturating_add(1);
                self.requests.push(req);
                self.enqueued_count = self.enqueued_count.saturating_add(1);
                Ok(EnqueueResult::EnqueuedWithEviction)
            } else {
                Err(AtlasError::RasterQueueFull)
            }
        } else {
            self.requests.push(req);
            self.enqueued_count = self.enqueued_count.saturating_add(1);
            Ok(EnqueueResult::Enqueued)
        }
    }

    /// Pop the highest priority request. Ties are broken in FIFO sequence order.
    pub fn pop_highest_priority(&mut self) -> Option<RasterMissRequest> {
        if self.requests.is_empty() {
            return None;
        }
        let mut best_idx = 0;
        for i in 1..self.requests.len() {
            let curr = &self.requests[i];
            let best = &self.requests[best_idx];
            if curr.priority > best.priority
                || (curr.priority == best.priority && curr.sequence < best.sequence)
            {
                best_idx = i;
            }
        }
        self.serviced_count = self.serviced_count.saturating_add(1);
        Some(self.requests.remove(best_idx))
    }
}

/// Result of querying the atlas for a glyph raster (Plan §13.5).
///
/// If absent, background rasterization is queued without blocking layout.
#[derive(Clone, Debug, PartialEq)]
pub enum RasterLookupResult {
    /// Glyph is resident in the GPU atlas.
    Hit(GpuGlyphRef),
    /// Glyph was a cache miss; background rasterization has been queued.
    /// Non-blocking: caller may use the optional fallback glyph reference.
    MissPending {
        key: GlyphRasterKey,
        priority: RasterPriority,
        fallback_ref: Option<GpuGlyphRef>,
    },
}

/// Rasterized glyph metrics and geometry ready for atlas allocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RasterizedGlyph {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance_x: i32,
}

/// Summary report from processing a batch of queued raster requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchProcessReport {
    pub processed_count: usize,
    pub remaining_in_queue: usize,
    pub recomputed_count: usize,
}

/// Unified memory texture accounting for Apple Silicon / UMA architectures (Plan §4.5).
///
/// On unified memory, demoting an evicted GPU texture to a CPU shadow buffer
/// duplicates physical memory without relieving pressure. FCB enforces a strict
/// discard/recompute policy: evicted textures are released immediately and
/// recomputed on demand from font sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnifiedMemoryAccounting {
    /// Total bytes currently allocated in GPU texture backings across all active pages.
    pub allocated_texture_bytes: u64,
    /// Peak texture bytes allocated since atlas creation.
    pub peak_texture_bytes: u64,
    /// Cumulative texture bytes discarded via slot eviction or device reset (never copied to CPU).
    pub discarded_texture_bytes: u64,
    /// Total count of glyphs recomputed from font source after prior eviction.
    pub recomputed_glyph_count: u64,
    /// Number of active texture pages.
    pub active_page_count: usize,
    /// Invariant: true guarantees zero CPU shadow buffer demotion (Plan §4.5).
    pub no_cpu_demotion_policy: bool,
}

/// Retained diagnostic event kinds for bounded atlas lifecycle logging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasLogEventKind {
    PageAllocated,
    SlotAllocated,
    SlotEvicted,
    SlotPinned,
    SlotUnpinned,
    RasterMissQueued,
    RasterPriorityPromoted,
    RasterServiced,
    DeviceReset,
}

/// One recorded atlas lifecycle event with frame timestamp and detail payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasLogEvent {
    pub sequence: u64,
    pub frame: u64,
    pub kind: AtlasLogEventKind,
    pub detail: u64,
}

/// Fixed-capacity ring buffer of diagnostic atlas lifecycle events.
#[derive(Clone, Debug)]
pub struct AtlasLogRing {
    capacity: usize,
    events: VecDeque<AtlasLogEvent>,
    next_sequence: u64,
    total_recorded: u64,
}

impl AtlasLogRing {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            events: VecDeque::with_capacity(capacity.min(512)),
            next_sequence: 1,
            total_recorded: 0,
        }
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[must_use]
    pub const fn total_recorded(&self) -> u64 {
        self.total_recorded
    }

    pub fn events(&self) -> &VecDeque<AtlasLogEvent> {
        &self.events
    }

    pub fn record(&mut self, frame: u64, kind: AtlasLogEventKind, detail: u64) {
        if self.capacity == 0 {
            return;
        }
        if self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.total_recorded = self.total_recorded.saturating_add(1);
        self.events.push_back(AtlasLogEvent {
            sequence: seq,
            frame,
            kind,
            detail,
        });
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// Configuration options for the bounded glyph atlas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundedAtlasConfig {
    /// Maximum number of texture pages admitted across all page kinds.
    pub max_pages: usize,
    /// Width and height of each square atlas page in pixels.
    pub page_size: u32,
    /// Maximum capacity of the background raster miss priority queue.
    pub max_raster_queue_capacity: usize,
    /// Maximum capacity of the retained diagnostic lifecycle event ring.
    pub log_capacity: usize,
}

impl Default for BoundedAtlasConfig {
    fn default() -> Self {
        Self {
            max_pages: DEFAULT_MAX_ATLAS_PAGES,
            page_size: DEFAULT_ATLAS_PAGE_SIZE,
            max_raster_queue_capacity: DEFAULT_MAX_RASTER_QUEUE_CAPACITY,
            log_capacity: DEFAULT_ATLAS_LOG_CAPACITY,
        }
    }
}

impl BoundedAtlasConfig {
    #[must_use]
    pub const fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }

    #[must_use]
    pub const fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = page_size;
        self
    }

    #[must_use]
    pub const fn with_raster_queue_capacity(mut self, capacity: usize) -> Self {
        self.max_raster_queue_capacity = capacity;
        self
    }

    #[must_use]
    pub const fn with_log_capacity(mut self, capacity: usize) -> Self {
        self.log_capacity = capacity;
        self
    }
}

/// Paged, bounded glyph atlas separating raster identity from GPU residence (Plan §13.4, §13.5).
pub struct BoundedGlyphAtlas {
    owner: ArenaOwnerId,
    device_id: u64,
    device_generation: u64,
    config: BoundedAtlasConfig,
    pages: Vec<AtlasPage>,
    /// Index from key hash to list of entries for zero-allocation borrowed lookup.
    by_hash: BTreeMap<u64, Vec<(GlyphRasterKey, GpuGlyphRef)>>,
    next_page_id: u32,
    total_evictions: u64,
    total_lookups: u64,
    total_hits: u64,
    raster_queue: BoundedRasterQueue,
    allocated_texture_bytes: u64,
    peak_texture_bytes: u64,
    discarded_texture_bytes: u64,
    recomputed_glyph_count: u64,
    evicted_keys_history: BTreeSet<u64>,
    log: AtlasLogRing,
}

impl BoundedGlyphAtlas {
    /// Create a new bounded glyph atlas bound to an owner, device, and device generation.
    #[must_use]
    pub fn new(
        owner: ArenaOwnerId,
        device_id: u64,
        device_generation: u64,
        config: BoundedAtlasConfig,
    ) -> Self {
        let max_q = config.max_raster_queue_capacity;
        let log_cap = config.log_capacity;
        Self {
            owner,
            device_id,
            device_generation,
            config,
            pages: Vec::new(),
            by_hash: BTreeMap::new(),
            next_page_id: 1,
            total_evictions: 0,
            total_lookups: 0,
            total_hits: 0,
            raster_queue: BoundedRasterQueue::new(max_q),
            allocated_texture_bytes: 0,
            peak_texture_bytes: 0,
            discarded_texture_bytes: 0,
            recomputed_glyph_count: 0,
            evicted_keys_history: BTreeSet::new(),
            log: AtlasLogRing::new(log_cap),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn device_id(&self) -> u64 {
        self.device_id
    }

    pub const fn device_generation(&self) -> u64 {
        self.device_generation
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub const fn total_evictions(&self) -> u64 {
        self.total_evictions
    }

    pub const fn total_lookups(&self) -> u64 {
        self.total_lookups
    }

    pub const fn total_hits(&self) -> u64 {
        self.total_hits
    }

    /// Retrieve the kind of an atlas page by its ID.
    #[must_use]
    pub fn page_kind(&self, page_id: AtlasPageId) -> Option<AtlasPageKind> {
        let idx = (page_id.0 as usize).saturating_sub(1);
        self.pages.get(idx).map(|p| p.kind)
    }

    /// Read-only access to the background raster miss queue.
    #[must_use]
    pub fn raster_queue(&self) -> &BoundedRasterQueue {
        &self.raster_queue
    }

    /// Mutable access to the background raster miss queue.
    pub fn raster_queue_mut(&mut self) -> &mut BoundedRasterQueue {
        &mut self.raster_queue
    }

    /// Read current unified memory accounting counters (Plan §4.5).
    #[must_use]
    pub fn memory_accounting(&self) -> UnifiedMemoryAccounting {
        UnifiedMemoryAccounting {
            allocated_texture_bytes: self.allocated_texture_bytes,
            peak_texture_bytes: self.peak_texture_bytes,
            discarded_texture_bytes: self.discarded_texture_bytes,
            recomputed_glyph_count: self.recomputed_glyph_count,
            active_page_count: self.pages.len(),
            no_cpu_demotion_policy: true,
        }
    }

    /// Read retained diagnostic lifecycle events.
    #[must_use]
    pub fn log_events(&self) -> &VecDeque<AtlasLogEvent> {
        self.log.events()
    }

    /// Invalidate entire atlas upon host device reset or GPU teardown (Plan §14.9).
    pub fn reset_device_generation(&mut self, new_generation: u64) {
        self.device_generation = new_generation;
        self.discarded_texture_bytes = self
            .discarded_texture_bytes
            .saturating_add(self.allocated_texture_bytes);
        self.allocated_texture_bytes = 0;
        self.pages.clear();
        self.by_hash.clear();
        self.raster_queue.clear();
        self.next_page_id = 1;
        self.log.record(0, AtlasLogEventKind::DeviceReset, new_generation);
    }

    /// Look up an existing resident glyph using a borrowed key without heap allocation.
    pub fn lookup(&mut self, key: &BorrowedGlyphRasterKey<'_>) -> Option<&GpuGlyphRef> {
        self.total_lookups = self.total_lookups.saturating_add(1);
        let hash = key.compute_hash();
        if let Some(entries) = self.by_hash.get(&hash) {
            for (k, r) in entries {
                if key.matches(k) {
                    self.total_hits = self.total_hits.saturating_add(1);
                    return Some(r);
                }
            }
        }
        None
    }

    /// Insert or allocate a glyph into the atlas, returning its resident [`GpuGlyphRef`].
    pub fn allocate_and_insert(
        &mut self,
        key: GlyphRasterKey,
        pixel_width: u32,
        pixel_height: u32,
        bearing_x: i32,
        bearing_y: i32,
        advance_x: i32,
        current_frame: u64,
    ) -> Result<GpuGlyphRef, AtlasError> {
        if pixel_width == 0 || pixel_height == 0 {
            return Err(AtlasError::InvalidDimensions);
        }
        if pixel_width > self.config.page_size || pixel_height > self.config.page_size {
            return Err(AtlasError::GlyphTooLargeForPage);
        }

        let borrowed = key.as_borrowed();
        let hash = borrowed.compute_hash();

        // Check if already present
        if let Some(entries) = self.by_hash.get_mut(&hash) {
            for (k, r) in entries.iter_mut() {
                if borrowed.matches(k) {
                    // Update last used frame
                    let page_idx = (r.page_id.0 as usize).saturating_sub(1);
                    if let Some(page) = self.pages.get_mut(page_idx) {
                        let slot_idx = r.slot_id.0 as usize;
                        if let Some(slot) = page.slots.get_mut(slot_idx) {
                            slot.last_used_frame = current_frame;
                        }
                    }
                    return Ok(r.clone());
                }
            }
        }

        let page_kind = AtlasPageKind::for_raster_mode(key.raster_mode);

        // 1. Try to pack into an existing page of matching kind
        for page_idx in 0..self.pages.len() {
            if self.pages[page_idx].kind == page_kind {
                if let Some((x, y)) = self.pages[page_idx].allocate_rect(pixel_width, pixel_height) {
                    return self.create_slot_and_ref(
                        page_idx,
                        x,
                        y,
                        pixel_width,
                        pixel_height,
                        bearing_x,
                        bearing_y,
                        advance_x,
                        key,
                        hash,
                        current_frame,
                    );
                }
            }
        }

        // 2. Try to allocate a new page if under maximum page budget
        if self.pages.len() < self.config.max_pages {
            let page_id = AtlasPageId(self.next_page_id);
            self.next_page_id = self.next_page_id.saturating_add(1);
            let mut page = AtlasPage::new(page_id, page_kind, self.config.page_size);
            if let Some((x, y)) = page.allocate_rect(pixel_width, pixel_height) {
                let page_bytes = page.byte_size();
                self.allocated_texture_bytes =
                    self.allocated_texture_bytes.saturating_add(page_bytes);
                self.peak_texture_bytes =
                    self.peak_texture_bytes.max(self.allocated_texture_bytes);
                self.log.record(
                    current_frame,
                    AtlasLogEventKind::PageAllocated,
                    page_id.0 as u64,
                );
                let page_idx = self.pages.len();
                self.pages.push(page);
                return self.create_slot_and_ref(
                    page_idx,
                    x,
                    y,
                    pixel_width,
                    pixel_height,
                    bearing_x,
                    bearing_y,
                    advance_x,
                    key,
                    hash,
                    current_frame,
                );
            }
        }

        // 3. Atlas is full: perform LRU eviction of unpinned slots
        self.evict_lru_and_reuse(
            page_kind,
            pixel_width,
            pixel_height,
            bearing_x,
            bearing_y,
            advance_x,
            key,
            hash,
            current_frame,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_slot_and_ref(
        &mut self,
        page_idx: usize,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        bearing_x: i32,
        bearing_y: i32,
        advance_x: i32,
        key: GlyphRasterKey,
        hash: u64,
        current_frame: u64,
    ) -> Result<GpuGlyphRef, AtlasError> {
        let page = &mut self.pages[page_idx];
        let slot_id = AtlasSlotId(page.next_slot_id);
        page.next_slot_id = page.next_slot_id.saturating_add(1);

        let pixel_rect = Rect2D::from_xywh(x as f64, y as f64, w as f64, h as f64)?;
        let uv_rect = Rect2D::from_xywh(
            (x as f64) / (page.width as f64),
            (y as f64) / (page.height as f64),
            (w as f64) / (page.width as f64),
            (h as f64) / (page.height as f64),
        )?;
        let page_id = page.page_id;

        let slot = AtlasSlot {
            slot_id,
            generation: SlotGeneration::INITIAL,
            pixel_rect,
            uv_rect,
            pixel_width: w,
            pixel_height: h,
            is_occupied: true,
            in_flight_pins: 0,
            last_used_frame: current_frame,
            current_key: Some(key.clone()),
            bearing_x,
            bearing_y,
            advance_x,
        };

        page.slots.push(slot);

        let glyph_ref = GpuGlyphRef {
            owner: self.owner,
            device_id: self.device_id,
            device_generation: self.device_generation,
            page_id,
            slot_id,
            slot_generation: SlotGeneration::INITIAL,
            pixel_rect,
            uv_rect,
            pixel_width: w,
            pixel_height: h,
            bearing_x,
            bearing_y,
            advance_x,
            raster_key: key.clone(),
        };

        self.by_hash
            .entry(hash)
            .or_default()
            .push((key, glyph_ref.clone()));

        if self.evicted_keys_history.remove(&hash) {
            self.recomputed_glyph_count = self.recomputed_glyph_count.saturating_add(1);
        }

        self.log.record(
            current_frame,
            AtlasLogEventKind::SlotAllocated,
            slot_id.0 as u64,
        );

        Ok(glyph_ref)
    }

    #[allow(clippy::too_many_arguments)]
    fn evict_lru_and_reuse(
        &mut self,
        page_kind: AtlasPageKind,
        w: u32,
        h: u32,
        bearing_x: i32,
        bearing_y: i32,
        advance_x: i32,
        key: GlyphRasterKey,
        hash: u64,
        current_frame: u64,
    ) -> Result<GpuGlyphRef, AtlasError> {
        // Find the oldest unpinned slot that can accommodate this glyph
        let mut best_candidate: Option<(usize, usize, u64)> = None; // (page_idx, slot_idx, last_used_frame)

        for (page_idx, page) in self.pages.iter().enumerate() {
            if page.kind != page_kind {
                continue;
            }
            for (slot_idx, slot) in page.slots.iter().enumerate() {
                if !slot.is_pinned()
                    && slot.last_used_frame < current_frame
                    && slot.pixel_width >= w
                    && slot.pixel_height >= h
                {
                    match best_candidate {
                        None => best_candidate = Some((page_idx, slot_idx, slot.last_used_frame)),
                        Some((_, _, oldest_frame)) if slot.last_used_frame < oldest_frame => {
                            best_candidate = Some((page_idx, slot_idx, slot.last_used_frame));
                        }
                        _ => {}
                    }
                }
            }
        }

        let (page_idx, slot_idx, _) = match best_candidate {
            Some(c) => c,
            None => {
                // Check if any slot is unpinned at all
                let any_unpinned = self.pages.iter().any(|p| {
                    p.kind == page_kind && p.slots.iter().any(|s| !s.is_pinned())
                });
                if any_unpinned {
                    return Err(AtlasError::AtlasFull);
                } else {
                    return Err(AtlasError::AllSlotsPinned);
                }
            }
        };

        // Evict and reuse the selected slot
        self.total_evictions = self.total_evictions.saturating_add(1);

        let page = &mut self.pages[page_idx];
        let slot = &mut page.slots[slot_idx];

        // Discard texture bytes for this slot from unified memory (Plan §4.5)
        let slot_bytes = (w as u64) * (h as u64) * (page_kind.bytes_per_pixel() as u64);
        self.discarded_texture_bytes = self.discarded_texture_bytes.saturating_add(slot_bytes);

        // 1. Remove old key from index and track in eviction history for recomputation metrics
        if let Some(ref old_key) = slot.current_key {
            let old_hash = old_key.compute_hash();
            if let Some(entries) = self.by_hash.get_mut(&old_hash) {
                entries.retain(|(k, _)| k != old_key);
            }
            if self.evicted_keys_history.len() >= 4096 {
                if let Some(&first) = self.evicted_keys_history.iter().next() {
                    self.evicted_keys_history.remove(&first);
                }
            }
            self.evicted_keys_history.insert(old_hash);
        }

        // 2. Increment slot generation strictly!
        let new_generation = slot.generation.next();
        slot.generation = new_generation;
        slot.current_key = Some(key.clone());
        slot.last_used_frame = current_frame;
        slot.bearing_x = bearing_x;
        slot.bearing_y = bearing_y;
        slot.advance_x = advance_x;

        let glyph_ref = GpuGlyphRef {
            owner: self.owner,
            device_id: self.device_id,
            device_generation: self.device_generation,
            page_id: page.page_id,
            slot_id: slot.slot_id,
            slot_generation: new_generation,
            pixel_rect: slot.pixel_rect,
            uv_rect: slot.uv_rect,
            pixel_width: w,
            pixel_height: h,
            bearing_x,
            bearing_y,
            advance_x,
            raster_key: key.clone(),
        };

        if self.evicted_keys_history.remove(&hash) {
            self.recomputed_glyph_count = self.recomputed_glyph_count.saturating_add(1);
        }

        self.by_hash
            .entry(hash)
            .or_default()
            .push((key, glyph_ref.clone()));

        self.log.record(
            current_frame,
            AtlasLogEventKind::SlotEvicted,
            slot.slot_id.0 as u64,
        );

        Ok(glyph_ref)
    }

    /// Pin a slot referenced by an in-flight command buffer.
    pub fn pin_slot(&mut self, glyph_ref: &GpuGlyphRef) -> Result<(), AtlasError> {
        self.validate_ref(glyph_ref)?;
        let page_idx = (glyph_ref.page_id.0 as usize).saturating_sub(1);
        let page = self.pages.get_mut(page_idx).ok_or(AtlasError::PageNotFound)?;
        let slot_idx = glyph_ref.slot_id.0 as usize;
        let slot = page.slots.get_mut(slot_idx).ok_or(AtlasError::SlotNotFound)?;
        slot.in_flight_pins = slot.in_flight_pins.saturating_add(1);
        self.log.record(
            0,
            AtlasLogEventKind::SlotPinned,
            glyph_ref.slot_id.0 as u64,
        );
        Ok(())
    }

    /// Unpin a slot when GPU terminal execution completes.
    pub fn unpin_slot(&mut self, glyph_ref: &GpuGlyphRef) -> Result<(), AtlasError> {
        self.validate_ref(glyph_ref)?;
        let page_idx = (glyph_ref.page_id.0 as usize).saturating_sub(1);
        let page = self.pages.get_mut(page_idx).ok_or(AtlasError::PageNotFound)?;
        let slot_idx = glyph_ref.slot_id.0 as usize;
        let slot = page.slots.get_mut(slot_idx).ok_or(AtlasError::SlotNotFound)?;
        slot.in_flight_pins = slot.in_flight_pins.saturating_sub(1);
        self.log.record(
            0,
            AtlasLogEventKind::SlotUnpinned,
            glyph_ref.slot_id.0 as u64,
        );
        Ok(())
    }

    /// Validate that a retained [`GpuGlyphRef`] matches current device, page, slot,
    /// and slot generation.
    ///
    /// If the slot has been reused following an eviction, returns [`AtlasError::StaleSlotGeneration`].
    pub fn validate_ref(&self, glyph_ref: &GpuGlyphRef) -> Result<(), AtlasError> {
        if glyph_ref.owner != self.owner {
            return Err(AtlasError::OwnerMismatch);
        }
        if glyph_ref.device_id != self.device_id {
            return Err(AtlasError::DeviceMismatch);
        }
        if glyph_ref.device_generation != self.device_generation {
            return Err(AtlasError::StaleDeviceGeneration);
        }

        let page_idx = (glyph_ref.page_id.0 as usize).saturating_sub(1);
        let page = self.pages.get(page_idx).ok_or(AtlasError::PageNotFound)?;

        let slot_idx = glyph_ref.slot_id.0 as usize;
        let slot = page.slots.get(slot_idx).ok_or(AtlasError::SlotNotFound)?;

        if slot.generation != glyph_ref.slot_generation {
            return Err(AtlasError::StaleSlotGeneration);
        }

        if slot.current_key.as_ref() != Some(&glyph_ref.raster_key) {
            return Err(AtlasError::StaleSlotGeneration);
        }

        Ok(())
    }

    /// Query a glyph raster key, returning an immediate hit or enqueuing a background miss (Plan §13.5).
    ///
    /// Never blocks layout. If absent from atlas, enqueues request with the specified priority
    /// and returns [`RasterLookupResult::MissPending`] with optional validated fallback slot.
    pub fn query_or_enqueue(
        &mut self,
        key: GlyphRasterKey,
        priority: RasterPriority,
        current_frame: u64,
        fallback_ref: Option<GpuGlyphRef>,
    ) -> Result<RasterLookupResult, AtlasError> {
        self.query_or_enqueue_with_estimated_dims(key, priority, current_frame, 0, 0, fallback_ref)
    }

    /// Query a glyph raster key with estimated geometry dimensions.
    pub fn query_or_enqueue_with_estimated_dims(
        &mut self,
        key: GlyphRasterKey,
        priority: RasterPriority,
        current_frame: u64,
        estimated_width: u32,
        estimated_height: u32,
        fallback_ref: Option<GpuGlyphRef>,
    ) -> Result<RasterLookupResult, AtlasError> {
        let borrowed = key.as_borrowed();
        let hash = borrowed.compute_hash();

        // 1. Check if resident
        if let Some(r) = self.lookup(&borrowed) {
            let r_clone = r.clone();
            return Ok(RasterLookupResult::Hit(r_clone));
        }

        // 2. Not resident: validate optional fallback ref
        let valid_fallback = fallback_ref.and_then(|f| {
            if self.validate_ref(&f).is_ok() {
                Some(f)
            } else {
                None
            }
        });

        // 3. Enqueue background raster miss request
        let miss_req = RasterMissRequest {
            key: key.clone(),
            priority,
            requested_frame: current_frame,
            estimated_width,
            estimated_height,
            sequence: 0,
        };
        let enq_res = self.raster_queue.enqueue(miss_req)?;
        match enq_res {
            EnqueueResult::Promoted => {
                self.log.record(
                    current_frame,
                    AtlasLogEventKind::RasterPriorityPromoted,
                    hash,
                );
            }
            EnqueueResult::Enqueued | EnqueueResult::EnqueuedWithEviction => {
                self.log.record(
                    current_frame,
                    AtlasLogEventKind::RasterMissQueued,
                    hash,
                );
            }
            EnqueueResult::AlreadyPresent => {}
        }

        Ok(RasterLookupResult::MissPending {
            key,
            priority,
            fallback_ref: valid_fallback,
        })
    }

    /// Process a batch of pending background raster requests in strict priority order.
    ///
    /// If in-flight GPU frames have pinned all available slots, stops processing
    /// and preserves remaining requests in the queue until slots become unpinned.
    pub fn process_raster_queue_batch<F>(
        &mut self,
        max_batch: usize,
        current_frame: u64,
        mut rasterizer: F,
    ) -> Result<BatchProcessReport, AtlasError>
    where
        F: FnMut(&GlyphRasterKey) -> Result<RasterizedGlyph, AtlasError>,
    {
        let mut processed_count = 0;
        let initial_recomputed = self.recomputed_glyph_count;

        for _ in 0..max_batch {
            let req = match self.raster_queue.pop_highest_priority() {
                Some(r) => r,
                None => break,
            };

            let glyph = rasterizer(&req.key)?;

            let insert_res = self.allocate_and_insert(
                req.key.clone(),
                glyph.pixel_width,
                glyph.pixel_height,
                glyph.bearing_x,
                glyph.bearing_y,
                glyph.advance_x,
                current_frame,
            );

            match insert_res {
                Ok(_) => {
                    processed_count += 1;
                    self.log.record(
                        current_frame,
                        AtlasLogEventKind::RasterServiced,
                        req.key.compute_hash(),
                    );
                }
                Err(AtlasError::AllSlotsPinned) | Err(AtlasError::AtlasFull) => {
                    // Cannot allocate because GPU is currently sampling all slots or
                    // all unpinned slots are active in the current frame.
                    // Put request back so it will be retried in a future frame!
                    let _ = self.raster_queue.enqueue(req);
                    break;
                }
                Err(err) => return Err(err),
            }
        }

        let recomputed_count =
            (self.recomputed_glyph_count.saturating_sub(initial_recomputed)) as usize;

        Ok(BatchProcessReport {
            processed_count,
            remaining_in_queue: self.raster_queue.len(),
            recomputed_count,
        })
    }
}
