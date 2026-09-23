//! Focused tests for retained primitive batches, shader records, and
//! invalidation counters (FCB-018.A).

#![forbid(unsafe_code)]

use franken_macos::render_batch::{
    AtlasFrameComposer, DrawableTarget, FrameBudgetLimits, FrameCompositionError, FrameRevisions,
    InvalidationCounters, PrimitiveKind, RenderBatch, ShaderRecord, TextFrameComposer,
    compare_frames,
};

#[test]
fn batch_collects_primitives_in_order() {
    let mut batch = RenderBatch::new();
    batch.add_rect(0.0, 0.0, 100.0, 50.0, 0, 1);
    batch.add_glyph(10.0, 20.0, 0, 2);
    batch.add_image(30.0, 40.0, 200.0, 100.0, 0, 3);
    batch.add_vector(0.0, 0.0, 50.0, 50.0, 0, 4);

    assert_eq!(batch.primitive_count(), 4);
    let sorted = batch.sorted_primitives();
    assert_eq!(sorted[0].kind, PrimitiveKind::Rectangle);
    assert_eq!(sorted[1].kind, PrimitiveKind::Glyph);
    assert_eq!(sorted[2].kind, PrimitiveKind::Image);
    assert_eq!(sorted[3].kind, PrimitiveKind::Vector);
}

#[test]
fn sealed_batch_refuses_new_primitives() {
    let mut batch = RenderBatch::new();
    batch.add_rect(0.0, 0.0, 10.0, 10.0, 0, 1);
    batch.seal();
    assert!(batch.is_sealed());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        batch.add_rect(0.0, 0.0, 10.0, 10.0, 0, 2);
    }));
    assert!(result.is_err(), "sealed batch must refuse new primitives");
}

#[test]
fn shader_record_binds_primitive_range() {
    let mut batch = RenderBatch::new();
    batch.add_rect(0.0, 0.0, 10.0, 10.0, 0, 1);
    batch.add_rect(20.0, 20.0, 10.0, 10.0, 0, 2);
    batch.add_glyph(0.0, 0.0, 0, 3);
    batch.bind_shader(ShaderRecord {
        pipeline_name: "rect_pipeline".to_owned(),
        first_primitive: 0,
        primitive_count: 2,
        field_bindings: vec![("position".to_owned(), 0, 8), ("color".to_owned(), 8, 4)],
    });
    batch.bind_shader(ShaderRecord {
        pipeline_name: "glyph_pipeline".to_owned(),
        first_primitive: 2,
        primitive_count: 1,
        field_bindings: vec![("glyph_index".to_owned(), 0, 4)],
    });
    assert_eq!(batch.shader_record_count(), 2);
}

#[test]
fn clip_layers_scope_primitives() {
    let mut batch = RenderBatch::new();
    batch.push_clip_layer(0, [0.0, 0.0, 800.0, 600.0]);
    batch.push_clip_layer(1, [10.0, 10.0, 200.0, 100.0]);
    assert_eq!(batch.clip_layer_count(), 2);

    batch.add_rect(0.0, 0.0, 10.0, 10.0, 0, 1);
    batch.add_rect(15.0, 15.0, 10.0, 10.0, 1, 2);
    let sorted = batch.sorted_primitives();
    assert_eq!(sorted[0].clip_layer, 0, "first rect in base layer");
    assert_eq!(sorted[1].clip_layer, 1, "second rect in clip layer 1");
}

#[test]
fn camera_only_invalidation_is_detected() {
    let prev = InvalidationCounters {
        camera: 1,
        theme: 5,
        geometry: 10,
    };
    let curr = InvalidationCounters {
        camera: 2,
        theme: 5,
        geometry: 10,
    };
    let frame = compare_frames(&prev, &curr);
    assert!(frame.camera_only, "camera-only change detected");
    assert!(!frame.theme_only);
    assert!(!frame.full_reencode);
}

#[test]
fn theme_only_invalidation_is_detected() {
    let prev = InvalidationCounters {
        camera: 1,
        theme: 5,
        geometry: 10,
    };
    let curr = InvalidationCounters {
        camera: 1,
        theme: 6,
        geometry: 10,
    };
    let frame = compare_frames(&prev, &curr);
    assert!(!frame.camera_only);
    assert!(frame.theme_only, "theme-only change detected");
    assert!(!frame.full_reencode);
}

#[test]
fn geometry_change_requires_full_reencode() {
    let prev = InvalidationCounters {
        camera: 1,
        theme: 5,
        geometry: 10,
    };
    let curr = InvalidationCounters {
        camera: 2,
        theme: 6,
        geometry: 11,
    };
    let frame = compare_frames(&prev, &curr);
    assert!(!frame.camera_only);
    assert!(!frame.theme_only);
    assert!(
        frame.full_reencode,
        "geometry change requires full re-encode"
    );
}

#[test]
fn overlapping_primitives_respect_sort_order() {
    let mut batch = RenderBatch::new();
    // Draw a background rect, then a foreground rect on top.
    batch.add_rect(0.0, 0.0, 100.0, 100.0, 0, 1);
    batch.add_rect(25.0, 25.0, 50.0, 50.0, 0, 2);
    let sorted = batch.sorted_primitives();
    // The foreground (higher sort order) comes after the background.
    assert!(sorted[0].sort_order < sorted[1].sort_order);
    // They overlap in the 25-50 x 25-50 region.
    let bg = sorted[0];
    let fg = sorted[1];
    let overlap_x = bg.x + bg.width > fg.x && fg.x + fg.width > bg.x;
    let overlap_y = bg.y + bg.height > fg.y && fg.y + fg.height > bg.y;
    assert!(overlap_x && overlap_y, "primitives overlap in alpha region");
}

// ============================================================================
// FCB-018.B Tests: Complete Native Atlas and Text Frame Composition
// ============================================================================

#[test]
fn test_atlas_frame_composition_and_cpu_geometry_oracle() {
    let target = DrawableTarget::new(1920.0, 1080.0, 2.0, [0.1, 0.1, 0.1, 1.0]).unwrap();
    assert_eq!(target.physical_width(), 3840.0);
    assert_eq!(target.physical_height(), 2160.0);

    let limits = FrameBudgetLimits::default_limits();
    let revisions = FrameRevisions::new(1, 1, 1);

    let mut composer = AtlasFrameComposer::new(target, limits, revisions);
    composer.add_background().unwrap();

    let clip_root = composer
        .push_clip_layer([0.0, 0.0, 1920.0, 1080.0])
        .unwrap();
    assert_eq!(clip_root, 1);

    // Add directory container
    composer
        .add_directory(50.0, 50.0, 800.0, 600.0, clip_root)
        .unwrap();

    // Nested clip layer for directory contents
    let clip_dir = composer
        .push_clip_layer([60.0, 80.0, 780.0, 560.0])
        .unwrap();
    assert_eq!(clip_dir, 2);

    // Add file parcels inside directory
    composer
        .add_file_parcel(70.0, 90.0, 200.0, 100.0, clip_dir)
        .unwrap();
    composer
        .add_file_parcel(280.0, 90.0, 200.0, 100.0, clip_dir)
        .unwrap();
    composer.add_label_glyph(80.0, 120.0, clip_dir).unwrap();

    // Add selection overlay
    composer
        .add_selection_overlay(70.0, 90.0, 200.0, 100.0, clip_dir)
        .unwrap();

    let frame = composer.compose().unwrap();
    assert!(frame.batch.is_sealed());
    assert_eq!(frame.batch.shader_record_count(), 1);
    assert!(frame.upload_bytes > 0);

    // CPU Geometry Oracle verification
    let expected_parcels = [
        [0.0, 0.0, 1920.0, 1080.0],  // Background
        [50.0, 50.0, 800.0, 600.0],  // Directory
        [70.0, 90.0, 200.0, 100.0],  // File 1
        [280.0, 90.0, 200.0, 100.0], // File 2
    ];
    frame.verify_cpu_geometry_oracle(&expected_parcels).unwrap();
}

#[test]
fn test_text_frame_composition_and_cpu_geometry_oracle() {
    let target = DrawableTarget::new(1200.0, 800.0, 2.0, [0.05, 0.05, 0.05, 1.0]).unwrap();
    let limits = FrameBudgetLimits::default_limits();
    let revisions = FrameRevisions::new(10, 2, 5);

    let mut composer = TextFrameComposer::new(target, limits, revisions);
    composer.add_background().unwrap();
    composer.add_gutter(60.0).unwrap();

    let clip_text = composer
        .push_clip_layer([60.0, 0.0, 1140.0, 800.0])
        .unwrap();
    composer
        .add_selection_range(100.0, 50.0, 250.0, 20.0, clip_text)
        .unwrap();
    composer
        .add_line_run(1, 65.0, 30, 100.0, 8.0, clip_text)
        .unwrap();
    composer.add_cursor(340.0, 50.0, 20.0, clip_text).unwrap();

    let frame = composer.compose().unwrap();
    assert!(frame.batch.is_sealed());

    let expected_parcels = [
        [0.0, 0.0, 1200.0, 800.0],  // Background
        [0.0, 0.0, 60.0, 800.0],    // Gutter
        [100.0, 50.0, 250.0, 20.0], // Selection
        [340.0, 50.0, 2.0, 20.0],   // Cursor
    ];
    frame.verify_cpu_geometry_oracle(&expected_parcels).unwrap();
}

#[test]
fn test_overlapping_alpha_layer_ordering_guarantee() {
    let target = DrawableTarget::new(800.0, 600.0, 1.0, [0.0, 0.0, 0.0, 1.0]).unwrap();
    let limits = FrameBudgetLimits::default_limits();
    let revisions = FrameRevisions::new(1, 1, 1);

    let mut composer = AtlasFrameComposer::new(target, limits, revisions);
    // Background layer (base tier 0)
    composer.add_background().unwrap();
    // Directory layer (base tier 100)
    composer.add_directory(10.0, 10.0, 500.0, 500.0, 0).unwrap();
    // File layer (base tier 200)
    composer
        .add_file_parcel(20.0, 20.0, 200.0, 200.0, 0)
        .unwrap();
    // Glyph layer (base tier 300)
    composer.add_label_glyph(30.0, 30.0, 0).unwrap();
    // Overlay layer (base tier 400)
    composer
        .add_selection_overlay(20.0, 20.0, 200.0, 200.0, 0)
        .unwrap();

    let frame = composer.compose().unwrap();
    let sorted = frame.batch.sorted_primitives();

    // Verify strictly ascending sort order across semantic layers
    for window in sorted.windows(2) {
        assert!(
            window[0].sort_order < window[1].sort_order,
            "sort order must strictly increase from background to overlay: {} < {}",
            window[0].sort_order,
            window[1].sort_order
        );
    }
}

#[test]
fn test_independent_invalidation_axes_camera_and_theme() {
    let rev_base = FrameRevisions::new(1, 1, 1);
    let rev_camera = FrameRevisions::new(2, 1, 1);
    let rev_theme = FrameRevisions::new(1, 2, 1);
    let rev_geom = FrameRevisions::new(2, 2, 2);

    let inv_camera = compare_frames(&rev_base.to_counters(), &rev_camera.to_counters());
    assert!(inv_camera.camera_only);
    assert!(!inv_camera.theme_only);
    assert!(!inv_camera.full_reencode);

    let inv_theme = compare_frames(&rev_base.to_counters(), &rev_theme.to_counters());
    assert!(!inv_theme.camera_only);
    assert!(inv_theme.theme_only);
    assert!(!inv_theme.full_reencode);

    let inv_geom = compare_frames(&rev_base.to_counters(), &rev_geom.to_counters());
    assert!(!inv_geom.camera_only);
    assert!(!inv_geom.theme_only);
    assert!(inv_geom.full_reencode);
}

#[test]
fn test_negative_control_oracle_detects_geometry_drift() {
    let target = DrawableTarget::new(800.0, 600.0, 1.0, [0.0, 0.0, 0.0, 1.0]).unwrap();
    let mut composer = AtlasFrameComposer::new(
        target,
        FrameBudgetLimits::default_limits(),
        FrameRevisions::default(),
    );
    composer
        .add_file_parcel(100.0, 100.0, 50.0, 50.0, 0)
        .unwrap();
    let frame = composer.compose().unwrap();

    // Planted defect in expected CPU geometry: wrong coordinate (100.5 instead of 100.0)
    let corrupted_expected = [[100.5, 100.0, 50.0, 50.0]];
    let result = frame.verify_cpu_geometry_oracle(&corrupted_expected);

    assert_eq!(
        result,
        Err(FrameCompositionError::GeometryMismatch {
            missing_parcel: [100.5, 100.0, 50.0, 50.0]
        }),
        "oracle must detect planted coordinate drift"
    );
}

#[test]
fn test_negative_control_oracle_detects_clip_violation() {
    let target = DrawableTarget::new(800.0, 600.0, 1.0, [0.0, 0.0, 0.0, 1.0]).unwrap();
    let mut composer = AtlasFrameComposer::new(
        target,
        FrameBudgetLimits::default_limits(),
        FrameRevisions::default(),
    );
    // Clip layer restricted to (0, 0, 100, 100)
    let clip = composer.push_clip_layer([0.0, 0.0, 100.0, 100.0]).unwrap();

    // Planted defect: parcel placed at (50, 50, 80, 80), max_x = 130 > clip_max_x = 100
    composer
        .add_file_parcel(50.0, 50.0, 80.0, 80.0, clip)
        .unwrap();
    let frame = composer.compose().unwrap();

    let result = frame.verify_cpu_geometry_oracle(&[[50.0, 50.0, 80.0, 80.0]]);
    assert_eq!(
        result,
        Err(FrameCompositionError::ClipBoundaryViolation {
            primitive_bounds: [50.0, 50.0, 80.0, 80.0],
            clip_rect: [0.0, 0.0, 100.0, 100.0],
        }),
        "oracle must catch primitive exceeding clip boundaries"
    );
}

#[test]
fn test_negative_control_budget_exceeded_refusal() {
    let target = DrawableTarget::new(800.0, 600.0, 1.0, [0.0, 0.0, 0.0, 1.0]).unwrap();
    let tight_limits = FrameBudgetLimits {
        max_primitives: 3,
        max_clip_layers: 2,
        max_upload_bytes: 1024,
    };

    let mut composer = AtlasFrameComposer::new(target, tight_limits, FrameRevisions::default());
    composer.add_file_parcel(0.0, 0.0, 10.0, 10.0, 0).unwrap();
    composer.add_file_parcel(10.0, 0.0, 10.0, 10.0, 0).unwrap();
    composer.add_file_parcel(20.0, 0.0, 10.0, 10.0, 0).unwrap();

    // 4th primitive must be refused by budget gate
    let overflow = composer.add_file_parcel(30.0, 0.0, 10.0, 10.0, 0);
    assert_eq!(
        overflow,
        Err(FrameCompositionError::BudgetExceeded {
            kind: "primitives",
            count: 4,
            limit: 3,
        })
    );
}

#[test]
fn test_invalid_target_dimensions_rejected() {
    // Zero width
    assert!(DrawableTarget::new(0.0, 100.0, 1.0, [0.0, 0.0, 0.0, 1.0]).is_err());
    // Negative height
    assert!(DrawableTarget::new(100.0, -50.0, 1.0, [0.0, 0.0, 0.0, 1.0]).is_err());
    // NaN scale factor
    assert!(DrawableTarget::new(100.0, 100.0, f32::NAN, [0.0, 0.0, 0.0, 1.0]).is_err());
    // Out of range clear color
    assert!(DrawableTarget::new(100.0, 100.0, 1.0, [1.5, 0.0, 0.0, 1.0]).is_err());
}
