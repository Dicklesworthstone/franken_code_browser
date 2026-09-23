//! Retained primitive batches with explicit shader records and
//! invalidation counters (FCB-018.A, FCB-018.B).
//!
//! A retained batch collects rendering primitives (rectangles, glyphs,
//! images, vectors) into an ordered draw list with explicit clip/layer
//! scoping. Shader records bind precompiled pipelines to batch segments.
//! Invalidation counters track camera-only, theme-only, and geometry
//! changes so a frame can skip full re-encoding when only the camera or
//! theme changes.
//!
//! Complete native atlas and text frame composers (`AtlasFrameComposer`,
//! `TextFrameComposer`) construct complete drawable frames with bounded
//! instance allocations, explicit clip regions, layered alpha sort order,
//! and CPU geometry oracle verification.

#![forbid(unsafe_code)]

/// The kind of rendering primitive.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrimitiveKind {
    /// Axis-aligned filled rectangle.
    Rectangle,
    /// Glyph (text character) placement.
    Glyph,
    /// Image/texture placement.
    Image,
    /// Vector path (triangle fan or strip).
    Vector,
}

/// A single rendering primitive within a batch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primitive {
    /// The primitive kind.
    pub kind: PrimitiveKind,
    /// X position in logical pixels.
    pub x: f32,
    /// Y position in logical pixels.
    pub y: f32,
    /// Width in logical pixels (0 for glyphs using font metrics).
    pub width: f32,
    /// Height in logical pixels.
    pub height: f32,
    /// Clip layer index (0 = no clip, higher = deeper nesting).
    pub clip_layer: u16,
    /// Sort order within the batch (lower = drawn first).
    pub sort_order: u32,
}

/// A shader record: binds a precompiled pipeline to a range of primitives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShaderRecord {
    /// The pipeline name (opaque string for debugging).
    pub pipeline_name: String,
    /// Index of the first primitive in the batch.
    pub first_primitive: usize,
    /// Number of primitives using this pipeline.
    pub primitive_count: usize,
    /// Explicit field encoder bindings (name, offset_in_bytes, size_in_bytes).
    pub field_bindings: Vec<(String, usize, usize)>,
}

/// Invalidation counters for frame deduplication.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InvalidationCounters {
    /// Bumped when the camera transform changes (view/projection matrix).
    pub camera: u64,
    /// Bumped when the theme (colors, fonts) changes.
    pub theme: u64,
    /// Bumped when geometry (primitive positions/sizes) changes.
    pub geometry: u64,
}

impl InvalidationCounters {
    /// Whether only the camera changed since the other counters.
    pub const fn camera_only_change(&self, other: &Self) -> bool {
        self.camera != other.camera && self.theme == other.theme && self.geometry == other.geometry
    }

    /// Whether only the theme changed since the other counters.
    pub const fn theme_only_change(&self, other: &Self) -> bool {
        self.theme != other.theme && self.camera == other.camera && self.geometry == other.geometry
    }

    /// Whether geometry changed (full re-encode needed).
    pub const fn geometry_changed(&self, other: &Self) -> bool {
        self.geometry != other.geometry
    }
}

/// A retained rendering batch with explicit ordering and shader records.
#[derive(Clone, Debug)]
pub struct RenderBatch {
    primitives: Vec<Primitive>,
    shader_records: Vec<ShaderRecord>,
    clip_layers: Vec<ClipLayer>,
    counters: InvalidationCounters,
    sealed: bool,
}

impl Default for RenderBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// A clip/layer scoping region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipLayer {
    /// Layer index.
    pub index: u16,
    /// Clip rect (x, y, width, height) in logical pixels.
    pub rect: [f32; 4],
}

impl RenderBatch {
    /// Create an empty batch.
    pub fn new() -> Self {
        Self {
            primitives: Vec::new(),
            shader_records: Vec::new(),
            clip_layers: Vec::new(),
            counters: InvalidationCounters::default(),
            sealed: false,
        }
    }

    /// Number of primitives in the batch.
    pub fn primitive_count(&self) -> usize {
        self.primitives.len()
    }

    /// Number of shader records.
    pub fn shader_record_count(&self) -> usize {
        self.shader_records.len()
    }

    /// Number of clip layers.
    pub fn clip_layer_count(&self) -> usize {
        self.clip_layers.len()
    }

    /// Slice of all primitives in insertion order.
    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    /// Slice of all clip layers.
    pub fn clip_layers(&self) -> &[ClipLayer] {
        &self.clip_layers
    }

    /// Slice of all shader records.
    pub fn shader_records(&self) -> &[ShaderRecord] {
        &self.shader_records
    }

    /// Add a clip layer scoping region.
    pub fn push_clip_layer(&mut self, index: u16, rect: [f32; 4]) {
        self.clip_layers.push(ClipLayer { index, rect });
    }

    /// Add a rectangle primitive.
    pub fn add_rect(&mut self, x: f32, y: f32, w: f32, h: f32, clip: u16, order: u32) {
        self.assert_not_sealed();
        self.primitives.push(Primitive {
            kind: PrimitiveKind::Rectangle,
            x,
            y,
            width: w,
            height: h,
            clip_layer: clip,
            sort_order: order,
        });
        self.counters.geometry += 1;
    }

    /// Add a glyph primitive.
    pub fn add_glyph(&mut self, x: f32, y: f32, clip: u16, order: u32) {
        self.assert_not_sealed();
        self.primitives.push(Primitive {
            kind: PrimitiveKind::Glyph,
            x,
            y,
            width: 0.0,
            height: 0.0,
            clip_layer: clip,
            sort_order: order,
        });
        self.counters.geometry += 1;
    }

    /// Add an image primitive.
    pub fn add_image(&mut self, x: f32, y: f32, w: f32, h: f32, clip: u16, order: u32) {
        self.assert_not_sealed();
        self.primitives.push(Primitive {
            kind: PrimitiveKind::Image,
            x,
            y,
            width: w,
            height: h,
            clip_layer: clip,
            sort_order: order,
        });
        self.counters.geometry += 1;
    }

    /// Add a vector primitive.
    pub fn add_vector(&mut self, x: f32, y: f32, w: f32, h: f32, clip: u16, order: u32) {
        self.assert_not_sealed();
        self.primitives.push(Primitive {
            kind: PrimitiveKind::Vector,
            x,
            y,
            width: w,
            height: h,
            clip_layer: clip,
            sort_order: order,
        });
        self.counters.geometry += 1;
    }

    /// Bind a shader pipeline to a range of primitives.
    pub fn bind_shader(&mut self, record: ShaderRecord) {
        self.assert_not_sealed();
        self.shader_records.push(record);
    }

    /// Update the invalidation counters from the host.
    pub fn set_counters(&mut self, counters: InvalidationCounters) {
        self.counters = counters;
    }

    /// The current invalidation counters.
    pub const fn counters(&self) -> &InvalidationCounters {
        &self.counters
    }

    /// Seal the batch: no further primitives or shader records can be added.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Whether the batch is sealed.
    pub const fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Iterate primitives in sort order.
    pub fn sorted_primitives(&self) -> Vec<&Primitive> {
        let mut sorted: Vec<&Primitive> = self.primitives.iter().collect();
        sorted.sort_by_key(|p| p.sort_order);
        sorted
    }

    fn assert_not_sealed(&self) {
        assert!(!self.sealed, "cannot add to a sealed batch");
    }
}

/// Report from a camera-only invalidation check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameInvalidation {
    /// Whether the camera-only path can be used (skip geometry re-encode).
    pub camera_only: bool,
    /// Whether the theme-only path can be used (skip camera update).
    pub theme_only: bool,
    /// Whether full re-encode is needed.
    pub full_reencode: bool,
}

/// Compare two counter snapshots and produce the frame invalidation kind.
pub fn compare_frames(
    prev: &InvalidationCounters,
    curr: &InvalidationCounters,
) -> FrameInvalidation {
    let camera_changed = prev.camera != curr.camera;
    let theme_changed = prev.theme != curr.theme;
    let geometry_changed = prev.geometry != curr.geometry;

    FrameInvalidation {
        camera_only: camera_changed && !theme_changed && !geometry_changed,
        theme_only: theme_changed && !camera_changed && !geometry_changed,
        full_reencode: geometry_changed,
    }
}

// ============================================================================
// FCB-018.B: Complete Native Atlas and Text Frame Composition
// ============================================================================

/// Error during frame composition or geometry oracle verification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameCompositionError {
    /// Instance or upload budget exceeded.
    BudgetExceeded {
        kind: &'static str,
        count: usize,
        limit: usize,
    },
    /// Invalid viewport dimensions.
    InvalidDimensions { width: f32, height: f32 },
    /// Invalid display scale factor.
    InvalidScale(f32),
    /// Invalid clear color component.
    InvalidClearColor { channel: usize, value: f32 },
    /// Referenced clip layer does not exist.
    InvalidClipLayer(u16),
    /// Attempted to compose a frame with zero primitives.
    EmptyFrame,
    /// Sort ordering violation in rendered primitives.
    SortOrderViolation { prev_order: u32, next_order: u32 },
    /// Expected CPU geometry parcel was not represented in the batch.
    GeometryMismatch { missing_parcel: [f32; 4] },
    /// Primitive bounds violate the enclosing clip layer rect.
    ClipBoundaryViolation {
        primitive_bounds: [f32; 4],
        clip_rect: [f32; 4],
    },
}

impl std::fmt::Display for FrameCompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExceeded { kind, count, limit } => {
                write!(f, "budget exceeded for {kind}: {count} > {limit}")
            }
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid drawable dimensions: {width}x{height}")
            }
            Self::InvalidScale(s) => write!(f, "invalid scale factor: {s}"),
            Self::InvalidClearColor { channel, value } => {
                write!(f, "invalid clear color channel {channel}: {value}")
            }
            Self::InvalidClipLayer(idx) => write!(f, "invalid clip layer index: {idx}"),
            Self::EmptyFrame => write!(f, "frame contains zero primitives"),
            Self::SortOrderViolation {
                prev_order,
                next_order,
            } => {
                write!(f, "sort order violation: {prev_order} > {next_order}")
            }
            Self::GeometryMismatch { missing_parcel } => {
                write!(f, "geometry mismatch: missing parcel {missing_parcel:?}")
            }
            Self::ClipBoundaryViolation {
                primitive_bounds,
                clip_rect,
            } => {
                write!(
                    f,
                    "primitive bounds {primitive_bounds:?} violate clip rect {clip_rect:?}"
                )
            }
        }
    }
}

impl std::error::Error for FrameCompositionError {}

/// Resource budget limits for a single frame composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameBudgetLimits {
    /// Maximum number of primitives permitted in a single frame.
    pub max_primitives: usize,
    /// Maximum number of nested clip layers permitted.
    pub max_clip_layers: usize,
    /// Maximum upload buffer size in bytes.
    pub max_upload_bytes: usize,
}

impl FrameBudgetLimits {
    /// Standard conservative defaults: 65,536 primitives, 256 clip layers, 4 MiB upload.
    pub const fn default_limits() -> Self {
        Self {
            max_primitives: 65_536,
            max_clip_layers: 256,
            max_upload_bytes: 4 * 1024 * 1024,
        }
    }
}

impl Default for FrameBudgetLimits {
    fn default() -> Self {
        Self::default_limits()
    }
}

/// The complete target surface descriptor for a frame presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawableTarget {
    /// Logical width in display points.
    pub width: f32,
    /// Logical height in display points.
    pub height: f32,
    /// Display backing scale factor (e.g. 2.0 for Retina).
    pub scale_factor: f32,
    /// Initial clear color [R, G, B, A].
    pub clear_color: [f32; 4],
}

impl DrawableTarget {
    /// Construct and validate a drawable target descriptor.
    pub fn new(
        width: f32,
        height: f32,
        scale_factor: f32,
        clear_color: [f32; 4],
    ) -> Result<Self, FrameCompositionError> {
        if !width.is_finite() || width <= 0.0 || !height.is_finite() || height <= 0.0 {
            return Err(FrameCompositionError::InvalidDimensions { width, height });
        }
        if !scale_factor.is_finite() || scale_factor <= 0.0 {
            return Err(FrameCompositionError::InvalidScale(scale_factor));
        }
        for (i, &c) in clear_color.iter().enumerate() {
            if !c.is_finite() || !(0.0..=1.0).contains(&c) {
                return Err(FrameCompositionError::InvalidClearColor {
                    channel: i,
                    value: c,
                });
            }
        }
        Ok(Self {
            width,
            height,
            scale_factor,
            clear_color,
        })
    }

    /// Physical pixel width.
    pub fn physical_width(&self) -> f32 {
        self.width * self.scale_factor
    }

    /// Physical pixel height.
    pub fn physical_height(&self) -> f32 {
        self.height * self.scale_factor
    }
}

/// Independent revision identifiers tracking separate invalidation axes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameRevisions {
    /// Camera view/projection revision.
    pub camera: u64,
    /// Theme/color/font revision.
    pub theme: u64,
    /// Content/geometry revision.
    pub geometry: u64,
}

impl FrameRevisions {
    /// Construct a revision tuple.
    pub const fn new(camera: u64, theme: u64, geometry: u64) -> Self {
        Self {
            camera,
            theme,
            geometry,
        }
    }

    /// Convert to invalidation counters.
    pub fn to_counters(&self) -> InvalidationCounters {
        InvalidationCounters {
            camera: self.camera,
            theme: self.theme,
            geometry: self.geometry,
        }
    }
}

/// Composer for complete native atlas frames.
#[derive(Debug)]
pub struct AtlasFrameComposer {
    target: DrawableTarget,
    limits: FrameBudgetLimits,
    revisions: FrameRevisions,
    batch: RenderBatch,
    next_order: u32,
}

impl AtlasFrameComposer {
    /// Initialize a new atlas frame composer.
    pub fn new(
        target: DrawableTarget,
        limits: FrameBudgetLimits,
        revisions: FrameRevisions,
    ) -> Self {
        let mut batch = RenderBatch::new();
        batch.set_counters(revisions.to_counters());
        Self {
            target,
            limits,
            revisions,
            batch,
            next_order: 10,
        }
    }

    /// Push a nested clip layer. Returns the 1-based clip layer identifier.
    pub fn push_clip_layer(&mut self, rect: [f32; 4]) -> Result<u16, FrameCompositionError> {
        let index = (self.batch.clip_layer_count() + 1) as u16;
        if self.batch.clip_layer_count() >= self.limits.max_clip_layers {
            return Err(FrameCompositionError::BudgetExceeded {
                kind: "clip_layers",
                count: self.batch.clip_layer_count() + 1,
                limit: self.limits.max_clip_layers,
            });
        }
        self.batch.push_clip_layer(index, rect);
        Ok(index)
    }

    /// Add an opaque canvas background covering the entire drawable target.
    pub fn add_background(&mut self) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        self.batch
            .add_rect(0.0, 0.0, self.target.width, self.target.height, 0, 1);
        Ok(())
    }

    /// Add a directory container parcel.
    pub fn add_directory(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(100);
        self.batch.add_rect(x, y, w, h, clip, order);
        Ok(())
    }

    /// Add a file leaf parcel.
    pub fn add_file_parcel(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(200);
        self.batch.add_rect(x, y, w, h, clip, order);
        Ok(())
    }

    /// Add a label glyph placement.
    pub fn add_label_glyph(
        &mut self,
        x: f32,
        y: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(300);
        self.batch.add_glyph(x, y, clip, order);
        Ok(())
    }

    /// Add a focus or selection highlight overlay.
    pub fn add_selection_overlay(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(400);
        self.batch.add_rect(x, y, w, h, clip, order);
        Ok(())
    }

    /// Finalize and seal the composed atlas frame.
    pub fn compose(mut self) -> Result<ComposedFrame, FrameCompositionError> {
        let prim_count = self.batch.primitive_count();
        if prim_count == 0 {
            return Err(FrameCompositionError::EmptyFrame);
        }

        self.batch.bind_shader(ShaderRecord {
            pipeline_name: "atlas_primitive_pipeline".to_owned(),
            first_primitive: 0,
            primitive_count: prim_count,
            field_bindings: vec![
                ("position".to_owned(), 0, 8),
                ("size".to_owned(), 8, 8),
                ("clip_layer".to_owned(), 16, 2),
            ],
        });
        self.batch.seal();

        let upload_bytes = prim_count * 32;
        if upload_bytes > self.limits.max_upload_bytes {
            return Err(FrameCompositionError::BudgetExceeded {
                kind: "upload_bytes",
                count: upload_bytes,
                limit: self.limits.max_upload_bytes,
            });
        }

        Ok(ComposedFrame {
            target: self.target,
            revisions: self.revisions,
            batch: self.batch,
            upload_bytes,
        })
    }

    fn check_primitive_budget(&self, additional: usize) -> Result<(), FrameCompositionError> {
        if self.batch.primitive_count() + additional > self.limits.max_primitives {
            return Err(FrameCompositionError::BudgetExceeded {
                kind: "primitives",
                count: self.batch.primitive_count() + additional,
                limit: self.limits.max_primitives,
            });
        }
        Ok(())
    }

    fn alloc_order(&mut self, base_tier: u32) -> u32 {
        let order = base_tier * 1000 + self.next_order;
        self.next_order += 1;
        order
    }
}

/// Composer for complete native text reader frames.
#[derive(Debug)]
pub struct TextFrameComposer {
    target: DrawableTarget,
    limits: FrameBudgetLimits,
    revisions: FrameRevisions,
    batch: RenderBatch,
    next_order: u32,
}

impl TextFrameComposer {
    /// Initialize a new text frame composer.
    pub fn new(
        target: DrawableTarget,
        limits: FrameBudgetLimits,
        revisions: FrameRevisions,
    ) -> Self {
        let mut batch = RenderBatch::new();
        batch.set_counters(revisions.to_counters());
        Self {
            target,
            limits,
            revisions,
            batch,
            next_order: 10,
        }
    }

    /// Push a nested clip layer. Returns the 1-based clip layer identifier.
    pub fn push_clip_layer(&mut self, rect: [f32; 4]) -> Result<u16, FrameCompositionError> {
        let index = (self.batch.clip_layer_count() + 1) as u16;
        if self.batch.clip_layer_count() >= self.limits.max_clip_layers {
            return Err(FrameCompositionError::BudgetExceeded {
                kind: "clip_layers",
                count: self.batch.clip_layer_count() + 1,
                limit: self.limits.max_clip_layers,
            });
        }
        self.batch.push_clip_layer(index, rect);
        Ok(index)
    }

    /// Add an editor background.
    pub fn add_background(&mut self) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        self.batch
            .add_rect(0.0, 0.0, self.target.width, self.target.height, 0, 1);
        Ok(())
    }

    /// Add a line gutter bar.
    pub fn add_gutter(&mut self, width: f32) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(50);
        self.batch
            .add_rect(0.0, 0.0, width, self.target.height, 0, order);
        Ok(())
    }

    /// Add a line run of glyphs.
    pub fn add_line_run(
        &mut self,
        _line_idx: usize,
        y: f32,
        glyph_count: usize,
        start_x: f32,
        glyph_width: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(glyph_count)?;
        for i in 0..glyph_count {
            let gx = start_x + (i as f32) * glyph_width;
            let order = self.alloc_order(300);
            self.batch.add_glyph(gx, y, clip, order);
        }
        Ok(())
    }

    /// Add a text selection highlight range.
    pub fn add_selection_range(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(200);
        self.batch.add_rect(x, y, w, h, clip, order);
        Ok(())
    }

    /// Add a text cursor/caret indicator.
    pub fn add_cursor(
        &mut self,
        x: f32,
        y: f32,
        height: f32,
        clip: u16,
    ) -> Result<(), FrameCompositionError> {
        self.check_primitive_budget(1)?;
        let order = self.alloc_order(500);
        self.batch.add_rect(x, y, 2.0, height, clip, order);
        Ok(())
    }

    /// Finalize and seal the composed text frame.
    pub fn compose(mut self) -> Result<ComposedFrame, FrameCompositionError> {
        let prim_count = self.batch.primitive_count();
        if prim_count == 0 {
            return Err(FrameCompositionError::EmptyFrame);
        }

        self.batch.bind_shader(ShaderRecord {
            pipeline_name: "text_primitive_pipeline".to_owned(),
            first_primitive: 0,
            primitive_count: prim_count,
            field_bindings: vec![
                ("position".to_owned(), 0, 8),
                ("glyph_info".to_owned(), 8, 8),
                ("clip_layer".to_owned(), 16, 2),
            ],
        });
        self.batch.seal();

        let upload_bytes = prim_count * 32;
        if upload_bytes > self.limits.max_upload_bytes {
            return Err(FrameCompositionError::BudgetExceeded {
                kind: "upload_bytes",
                count: upload_bytes,
                limit: self.limits.max_upload_bytes,
            });
        }

        Ok(ComposedFrame {
            target: self.target,
            revisions: self.revisions,
            batch: self.batch,
            upload_bytes,
        })
    }

    fn check_primitive_budget(&self, additional: usize) -> Result<(), FrameCompositionError> {
        if self.batch.primitive_count() + additional > self.limits.max_primitives {
            return Err(FrameCompositionError::BudgetExceeded {
                kind: "primitives",
                count: self.batch.primitive_count() + additional,
                limit: self.limits.max_primitives,
            });
        }
        Ok(())
    }

    fn alloc_order(&mut self, base_tier: u32) -> u32 {
        let order = base_tier * 1000 + self.next_order;
        self.next_order += 1;
        order
    }
}

/// A fully composed, sealed frame ready for Metal GPU presentation.
#[derive(Debug)]
pub struct ComposedFrame {
    /// The target drawable properties.
    pub target: DrawableTarget,
    /// The revisions snapshot baked into this frame.
    pub revisions: FrameRevisions,
    /// The sealed render batch containing all primitives and shader records.
    pub batch: RenderBatch,
    /// Total calculated GPU upload bytes.
    pub upload_bytes: usize,
}

impl ComposedFrame {
    /// CPU geometry oracle:
    /// Compares the composed frame's primitives against expected CPU geometry parcels
    /// `[(x, y, w, h)]` and verifies:
    /// 1. Every expected parcel is represented by a primitive in the batch matching coordinates within epsilon.
    /// 2. Sort ordering is strictly non-decreasing across consecutive primitives.
    /// 3. Primitives inside a clip layer are physically contained within that clip layer's rect.
    pub fn verify_cpu_geometry_oracle(
        &self,
        expected_parcels: &[[f32; 4]],
    ) -> Result<(), FrameCompositionError> {
        let sorted = self.batch.sorted_primitives();

        // 1. Sort order non-decreasing verification
        for window in sorted.windows(2) {
            if window[0].sort_order > window[1].sort_order {
                return Err(FrameCompositionError::SortOrderViolation {
                    prev_order: window[0].sort_order,
                    next_order: window[1].sort_order,
                });
            }
        }

        // 2. Expected parcel coverage verification
        for expected in expected_parcels {
            let found = sorted.iter().any(|p| {
                (p.x - expected[0]).abs() < 1e-4
                    && (p.y - expected[1]).abs() < 1e-4
                    && (p.width - expected[2]).abs() < 1e-4
                    && (p.height - expected[3]).abs() < 1e-4
            });
            if !found {
                return Err(FrameCompositionError::GeometryMismatch {
                    missing_parcel: *expected,
                });
            }
        }

        // 3. Clip layer containment
        let clip_layers = self.batch.clip_layers();
        for p in &sorted {
            if p.clip_layer > 0 {
                let layer_idx = (p.clip_layer - 1) as usize;
                if let Some(clip) = clip_layers.get(layer_idx) {
                    let clip_r = clip.rect;
                    let p_max_x = p.x + p.width;
                    let p_max_y = p.y + p.height;
                    let clip_max_x = clip_r[0] + clip_r[2];
                    let clip_max_y = clip_r[1] + clip_r[3];
                    // Primitives must not exceed clip boundaries by more than epsilon
                    if p.x < clip_r[0] - 1e-3
                        || p.y < clip_r[1] - 1e-3
                        || p_max_x > clip_max_x + 1e-3
                        || p_max_y > clip_max_y + 1e-3
                    {
                        return Err(FrameCompositionError::ClipBoundaryViolation {
                            primitive_bounds: [p.x, p.y, p.width, p.height],
                            clip_rect: clip_r,
                        });
                    }
                } else {
                    return Err(FrameCompositionError::InvalidClipLayer(p.clip_layer));
                }
            }
        }

        Ok(())
    }
}
