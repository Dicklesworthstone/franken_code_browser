#![forbid(unsafe_code)]

//! Focused production unit and boundary tests for FCB-089.B (fcb-npyi.2):
//! - Validate target format/sample/device/colorspace, field offsets/strides and semantic transparent order.
//! - Oracle catches gamma, double alpha, clip/depth flips and incompatible target reuse.
//! - Negative controls demonstrate defect detection.
//! - Emits structured [`ScenarioReceipt`]s with event rings and content digests.

use std::fs;
use std::path::PathBuf;

use fcb_core::ArenaOwnerId;
use fcb_render::{
    host_target::{
        HostDrawableLeaseTracker, RenderTargetDescriptor, TargetColorSpace, TargetLeaseState,
        TargetPixelFormat, TargetSampleCount, TargetValidationError, MAX_TARGET_DIMENSION,
    },
    reference_oracle::{
        CpuReferenceCompositor, OracleDefect, ReferenceOracle, ReferencePixel,
    },
    semantic_order::{
        validate_semantic_order, verify_projection_stability, DepthPolicy, RenderLayer,
        SemanticOrderError,
    },
    shader_abi::{
        GpuFrameUniforms, GpuGlyphRecord, GpuScissorRecord, GpuSolidRectRecord, ShaderAbiError,
    },
    ColorLinearSdr, GlyphCoverage, ScissorRect,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;

const RUN_ID_ENV: &str = "FCB_089_RUN_ID";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-089-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_89_00_02),
        pin: SourcePin::new("0890008900089000890008900089000890008902").expect("pin valid"),
        route: RouteId::new("headless:render:shader-abi").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };

    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

fn sample_owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).expect("valid owner id")
}

// =========================================================================
// 1. Host Target Descriptor & Device/Format/Sample/ColorSpace Validation
// =========================================================================

#[test]
fn test_01_target_format_sample_device_colorspace_validation() {
    let owner = sample_owner(100);
    let device_id = 42;

    // 1. Valid SDR baseline target passes validation.
    let valid_target = RenderTargetDescriptor::new(
        device_id,
        owner,
        1920,
        1080,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        1,
    );
    assert!(valid_target.validate(device_id, owner, false).is_ok());

    // 2. Zero dimensions rejected.
    let zero_w = RenderTargetDescriptor::new(
        device_id,
        owner,
        0,
        1080,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        1,
    );
    assert_eq!(
        zero_w.validate(device_id, owner, false),
        Err(TargetValidationError::ZeroDimension {
            width: 0,
            height: 1080
        })
    );

    // 3. Excessive dimensions rejected.
    let huge = RenderTargetDescriptor::new(
        device_id,
        owner,
        MAX_TARGET_DIMENSION + 1,
        100,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        1,
    );
    assert_eq!(
        huge.validate(device_id, owner, false),
        Err(TargetValidationError::DimensionExceedsMaximum {
            width: MAX_TARGET_DIMENSION + 1,
            height: 100,
            max: MAX_TARGET_DIMENSION,
        })
    );

    // 4. Device mismatch rejected.
    assert_eq!(
        valid_target.validate(999, owner, false),
        Err(TargetValidationError::DeviceMismatch {
            expected_device: 999,
            actual_device: 42,
        })
    );

    // 5. Owner mismatch rejected.
    let foreign_owner = sample_owner(200);
    assert_eq!(
        valid_target.validate(device_id, foreign_owner, false),
        Err(TargetValidationError::OwnerMismatch {
            expected_owner: foreign_owner,
            actual_owner: owner,
        })
    );

    // 6. Unsupported format rejected.
    let unsupported_fmt = RenderTargetDescriptor::new(
        device_id,
        owner,
        800,
        600,
        TargetPixelFormat::Unsupported(99),
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        1,
    );
    assert_eq!(
        unsupported_fmt.validate(device_id, owner, false),
        Err(TargetValidationError::UnsupportedFormat(
            TargetPixelFormat::Unsupported(99)
        ))
    );

    // 7. Unsupported sample count rejected.
    let invalid_sample = RenderTargetDescriptor::new(
        device_id,
        owner,
        800,
        600,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::Unsupported(8),
        TargetColorSpace::Srgb,
        1,
    );
    assert_eq!(
        invalid_sample.validate(device_id, owner, false),
        Err(TargetValidationError::UnsupportedSampleCount(
            TargetSampleCount::Unsupported(8)
        ))
    );

    // 8. Wide-gamut color space without capability rejected.
    let p3_target = RenderTargetDescriptor::new(
        device_id,
        owner,
        800,
        600,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::DisplayP3,
        1,
    );
    assert_eq!(
        p3_target.validate(device_id, owner, false),
        Err(TargetValidationError::WideGamutDisallowedWithoutCapability(
            TargetColorSpace::DisplayP3
        ))
    );

    // 9. Wide-gamut allowed when capability is enabled.
    assert!(p3_target.validate(device_id, owner, true).is_ok());

    // 10. 4x MSAA supported.
    let msaa_target = RenderTargetDescriptor::new(
        device_id,
        owner,
        800,
        600,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::Four,
        TargetColorSpace::Srgb,
        1,
    );
    assert!(msaa_target.validate(device_id, owner, false).is_ok());

    record_receipt(
        "test_01_target_format_sample_device_colorspace_validation",
        Effect::Succeeded,
        "Target validation correctly enforces SDR baseline and device/owner confinement",
    );
}

// =========================================================================
// 2. Drawable Lease Lifecycle & Exactly-One Active Drawable Semantics
// =========================================================================

#[test]
fn test_02_drawable_lease_lifecycle_and_single_owner_semantics() {
    let owner = sample_owner(101);
    let device_id = 55;
    let mut tracker = HostDrawableLeaseTracker::new(device_id, owner, false);

    let t1 = RenderTargetDescriptor::new(
        device_id,
        owner,
        1024,
        768,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        1,
    );

    // 1. Acquire first drawable lease.
    assert!(tracker.acquire_drawable(t1).is_ok());
    assert!(tracker.active_lease().is_some());

    // 2. Acquiring a second drawable while active is refused.
    let t2 = RenderTargetDescriptor::new(
        device_id,
        owner,
        1024,
        768,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        2,
    );
    assert_eq!(
        tracker.acquire_drawable(t2),
        Err(TargetValidationError::MultipleActiveDrawables)
    );

    // 3. Premature presentation before encoding and submitting is refused.
    let mut bad_tracker = HostDrawableLeaseTracker::new(device_id, owner, false);
    bad_tracker.acquire_drawable(t1).unwrap();
    assert_eq!(
        bad_tracker.present_drawable(),
        Err(TargetValidationError::TargetNotEncoded)
    );

    // 4. Progress through encode -> submit -> present.
    assert!(tracker.encode_pass().is_ok());
    assert!(tracker.submit_pass().is_ok());
    let presented_count = tracker.present_drawable().expect("present succeeds");
    assert_eq!(presented_count, 1);
    assert!(tracker.active_lease().is_none());

    // 5. Presenting an already released/empty lease is refused.
    assert_eq!(
        tracker.present_drawable(),
        Err(TargetValidationError::TargetNotEncoded)
    );

    // 6. Next frame can now be acquired.
    assert!(tracker.acquire_drawable(t2).is_ok());
    tracker.encode_pass().unwrap();
    tracker.submit_pass().unwrap();
    assert_eq!(tracker.present_drawable().unwrap(), 2);

    record_receipt(
        "test_02_drawable_lease_lifecycle_and_single_owner_semantics",
        Effect::Succeeded,
        "Exactly-one drawable lease lifecycle enforced",
    );
}

// =========================================================================
// 3. Shader Record Layouts, Offsets, Strides, and Explicit Byte Encoding
// =========================================================================

#[test]
fn test_03_shader_record_layouts_offsets_and_serialization() {
    // 1. SolidRect Record Layout: 48 bytes, 16-byte alignment.
    assert_eq!(GpuSolidRectRecord::RECORD_SIZE, 48);
    assert_eq!(GpuSolidRectRecord::RECORD_SIZE % 16, 0);

    let color = ColorLinearSdr::premultiplied(0.2, 0.4, 0.6, 0.8).unwrap();
    let rect = GpuSolidRectRecord::new(10.0, 20.0, 100.0, 200.0, color, 3, 0.25).unwrap();

    let mut buf = [0u8; 48];
    let written = rect.encode(&mut buf).expect("encode succeeds");
    assert_eq!(written, 48);

    // Verify exact byte offsets in Little-Endian format.
    assert_eq!(f32::from_le_bytes(buf[0..4].try_into().unwrap()), 10.0);
    assert_eq!(f32::from_le_bytes(buf[4..8].try_into().unwrap()), 20.0);
    assert_eq!(f32::from_le_bytes(buf[8..12].try_into().unwrap()), 100.0);
    assert_eq!(f32::from_le_bytes(buf[12..16].try_into().unwrap()), 200.0);
    assert_eq!(f32::from_le_bytes(buf[16..20].try_into().unwrap()), 0.2);
    assert_eq!(f32::from_le_bytes(buf[20..24].try_into().unwrap()), 0.4);
    assert_eq!(f32::from_le_bytes(buf[24..28].try_into().unwrap()), 0.6);
    assert_eq!(f32::from_le_bytes(buf[28..32].try_into().unwrap()), 0.8);
    assert_eq!(u32::from_le_bytes(buf[32..36].try_into().unwrap()), 3);
    assert_eq!(f32::from_le_bytes(buf[36..40].try_into().unwrap()), 0.25);
    // Padding bytes are zeroed for 16-byte alignment.
    assert_eq!(u32::from_le_bytes(buf[40..44].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(buf[44..48].try_into().unwrap()), 0);

    // Decode round-trip.
    let decoded = GpuSolidRectRecord::decode(&buf).expect("decode succeeds");
    assert_eq!(decoded.x, 10.0);
    assert_eq!(decoded.y, 20.0);
    assert_eq!(decoded.width, 100.0);
    assert_eq!(decoded.height, 200.0);
    assert_eq!(decoded.color_r, 0.2);
    assert_eq!(decoded.clip_index, 3);
    assert_eq!(decoded.depth, 0.25);

    // 2. Buffer too small error.
    let mut small_buf = [0u8; 32];
    assert_eq!(
        rect.encode(&mut small_buf),
        Err(ShaderAbiError::BufferTooSmall {
            required: 48,
            provided: 32,
        })
    );

    // 3. Glyph Record Layout: 64 bytes, 16-byte alignment.
    assert_eq!(GpuGlyphRecord::RECORD_SIZE, 64);
    assert_eq!(GpuGlyphRecord::RECORD_SIZE % 16, 0);

    let coverage = GlyphCoverage::from_linear(0.75).unwrap();
    let glyph = GpuGlyphRecord::new(
        (5.0, 15.0, 12.0, 18.0),
        (0.1, 0.2, 0.05, 0.08),
        color,
        coverage,
        1,
        0.1,
    )
    .unwrap();

    let mut glyph_buf = [0u8; 64];
    assert_eq!(glyph.encode(&mut glyph_buf).unwrap(), 64);
    let decoded_glyph = GpuGlyphRecord::decode(&glyph_buf).unwrap();
    assert_eq!(decoded_glyph.dest_x, 5.0);
    assert_eq!(decoded_glyph.dest_w, 12.0);
    assert_eq!(decoded_glyph.uv_x, 0.1);
    assert_eq!(decoded_glyph.coverage_linear, 0.75);

    // 4. Scissor Record Layout: 16 bytes.
    let scissor = GpuScissorRecord::new(10, 20, 300, 400);
    let mut sc_buf = [0u8; 16];
    assert_eq!(scissor.encode(&mut sc_buf).unwrap(), 16);
    let dec_sc = GpuScissorRecord::decode(&sc_buf).unwrap();
    assert_eq!(dec_sc.x, 10);
    assert_eq!(dec_sc.height, 400);

    // 5. Frame Uniforms Layout: 32 bytes.
    let uniforms = GpuFrameUniforms::new(1920.0, 1080.0, 2.0, 4, 0, 0).unwrap();
    let mut uni_buf = [0u8; 32];
    assert_eq!(uniforms.encode(&mut uni_buf).unwrap(), 32);
    let dec_uni = GpuFrameUniforms::decode(&uni_buf).unwrap();
    assert_eq!(dec_uni.viewport_width, 1920.0);
    assert_eq!(dec_uni.scale_factor, 2.0);

    record_receipt(
        "test_03_shader_record_layouts_offsets_and_serialization",
        Effect::Succeeded,
        "Shader record 16-byte alignment and Little-Endian round-trip verified",
    );
}

// =========================================================================
// 4. Semantic Transparent Order and Depth Policies
// =========================================================================

#[test]
fn test_04_semantic_transparent_order_and_depth_policies() {
    // 1. Valid 2D painter's order (monotonically non-decreasing z-index).
    let l1 = RenderLayer::new(1, 0, 0.5, (0.0, 0.0, 100.0, 100.0), 0.5, true).unwrap();
    let l2 = RenderLayer::new(2, 1, 0.4, (50.0, 50.0, 100.0, 100.0), 0.5, true).unwrap();
    let layers_2d = [l1, l2];
    assert!(validate_semantic_order(&layers_2d, DepthPolicy::PainterBackToFront).is_ok());

    // 2. Inverted 2D painter's order detected.
    let inv_2d = [l2, l1];
    assert!(matches!(
        validate_semantic_order(&inv_2d, DepthPolicy::PainterBackToFront),
        Err(SemanticOrderError::DepthInversion { .. })
    ));

    // 3. Valid LessEqual depth ordering (foreground has smaller depth).
    assert!(validate_semantic_order(&layers_2d, DepthPolicy::DepthTestLessEqual).is_ok());

    // 4. Inverted LessEqual depth ordering detected (foreground has larger depth).
    let l2_inverted_depth =
        RenderLayer::new(2, 1, 0.9, (50.0, 50.0, 100.0, 100.0), 0.5, true).unwrap();
    let bad_depth = [l1, l2_inverted_depth];
    assert!(matches!(
        validate_semantic_order(&bad_depth, DepthPolicy::DepthTestLessEqual),
        Err(SemanticOrderError::DepthInversion { .. })
    ));

    // 5. Hidden surface clickability defect detection:
    let l_under =
        RenderLayer::new(10, 0, 0.8, (20.0, 20.0, 50.0, 50.0), 1.0, true).unwrap();
    let l_opaque =
        RenderLayer::new(11, 1, 0.2, (0.0, 0.0, 200.0, 200.0), 1.0, true).unwrap();
    let occluded_stack = [l_under, l_opaque];
    assert_eq!(
        validate_semantic_order(&occluded_stack, DepthPolicy::DepthTestLessEqual),
        Err(SemanticOrderError::HiddenSurfaceClickable {
            occluded_id: 10,
            occluder_id: 11,
        })
    );

    // 6. Projection stability check:
    let l1_city = RenderLayer::new(1, 0, 0.6, (0.0, 0.0, 100.0, 100.0), 0.5, true).unwrap();
    let l2_city = RenderLayer::new(2, 1, 0.3, (50.0, 50.0, 100.0, 100.0), 0.5, true).unwrap();
    let city_layers = [l1_city, l2_city];
    assert!(verify_projection_stability(&layers_2d, &city_layers).is_ok());

    // 7. Projection order flip defect detection:
    let l2_flipped_city =
        RenderLayer::new(2, 1, 0.8, (50.0, 50.0, 100.0, 100.0), 0.5, true).unwrap();
    let bad_city = [l1_city, l2_flipped_city];
    assert!(matches!(
        verify_projection_stability(&layers_2d, &bad_city),
        Err(SemanticOrderError::ProjectionOrderFlip { .. })
    ));

    record_receipt(
        "test_04_semantic_transparent_order_and_depth_policies",
        Effect::Succeeded,
        "Transparent layer sorting, occlusion masking, and projection stability verified",
    );
}

// =========================================================================
// 5. CPU Reference Compositor and Agreement
// =========================================================================

#[test]
fn test_05_cpu_reference_compositor_matches_linear_sdr_math() {
    // 1. Reference premultiplied source-over blending:
    let src = ReferencePixel {
        r: 0.5,
        g: 0.0,
        b: 0.0,
        a: 0.5,
    };
    let dst = ReferencePixel {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };
    let blended = CpuReferenceCompositor::blend_source_over(src, dst);
    assert!((blended.r - 0.5).abs() < 1e-6);
    assert!((blended.b - 0.5).abs() < 1e-6);
    assert!((blended.a - 1.0).abs() < 1e-6);

    // 2. Linear glyph coverage application:
    let text_color = ReferencePixel {
        r: 0.8,
        g: 0.8,
        b: 0.8,
        a: 1.0,
    };
    let cov = GlyphCoverage::from_linear(0.5).unwrap();
    let text_covered = CpuReferenceCompositor::apply_glyph_coverage(text_color, cov);
    assert!((text_covered.r - 0.4).abs() < 1e-6);
    assert!((text_covered.a - 0.5).abs() < 1e-6);

    record_receipt(
        "test_05_cpu_reference_compositor_matches_linear_sdr_math",
        Effect::Succeeded,
        "CPU reference compositor matches linear SDR source-over blending",
    );
}

// =========================================================================
// 6. Negative Controls: Defect Detection Oracles
// =========================================================================

#[test]
fn test_06_negative_controls_defect_detection_oracles() {
    // 1. Gamma Defect Oracle:
    let cov = GlyphCoverage::from_linear(0.5).unwrap();
    let gamma_corrupted = 0.5_f32.powf(2.2);
    assert!(matches!(
        ReferenceOracle::verify_glyph_coverage(cov, gamma_corrupted, 0.01),
        Err(OracleDefect::GammaDecodedLinearCoverage { .. })
    ));
    assert!(ReferenceOracle::verify_glyph_coverage(cov, 0.5, 0.001).is_ok());

    // 2. Double Premultiply Defect Oracle:
    let expected = ColorLinearSdr::premultiplied(0.4, 0.2, 0.1, 0.5).unwrap();
    let double_premul = ColorLinearSdr::premultiplied(0.2, 0.1, 0.05, 0.5).unwrap();
    assert!(matches!(
        ReferenceOracle::verify_no_double_premultiply(expected, double_premul, 0.01),
        Err(OracleDefect::DoublePremultiplyDetected { .. })
    ));
    assert!(ReferenceOracle::verify_no_double_premultiply(expected, expected, 0.001).is_ok());

    // 3. Scissor Clip Violation Oracle:
    let scissor = ScissorRect::new(100, 100, 200, 200);
    assert_eq!(
        ReferenceOracle::verify_scissor_clip(50, 150, &scissor),
        Err(OracleDefect::ScissorClipViolation {
            pixel_x: 50,
            pixel_y: 150,
            clip_x: 100,
            clip_y: 100,
            clip_w: 200,
            clip_h: 200,
        })
    );
    assert!(ReferenceOracle::verify_scissor_clip(150, 150, &scissor).is_ok());

    // 4. Depth Flip Oracle:
    assert!(matches!(
        ReferenceOracle::verify_depth_order(0.8, 0.2),
        Err(OracleDefect::DepthOrderFlipped { .. })
    ));
    assert!(ReferenceOracle::verify_depth_order(0.2, 0.8).is_ok());

    // 5. Incompatible Target Reuse Oracle:
    let owner = sample_owner(999);
    let mut target = RenderTargetDescriptor::new(
        1,
        owner,
        800,
        600,
        TargetPixelFormat::Bgra8UnormSrgb,
        TargetSampleCount::One,
        TargetColorSpace::Srgb,
        1,
    );
    assert!(ReferenceOracle::verify_target_lease(&target).is_ok());

    target.mark_encoded().unwrap();
    assert_eq!(
        ReferenceOracle::verify_target_lease(&target),
        Err(OracleDefect::IncompatibleTargetReuse {
            state: TargetLeaseState::Encoded
        })
    );

    target.mark_submitted().unwrap();
    assert_eq!(
        ReferenceOracle::verify_target_lease(&target),
        Err(OracleDefect::IncompatibleTargetReuse {
            state: TargetLeaseState::Submitted
        })
    );

    target.mark_presented().unwrap();
    assert_eq!(
        ReferenceOracle::verify_target_lease(&target),
        Err(OracleDefect::IncompatibleTargetReuse {
            state: TargetLeaseState::Presented
        })
    );

    record_receipt(
        "test_06_negative_controls_defect_detection_oracles",
        Effect::Succeeded,
        "Oracles accurately detect gamma, double alpha, clip violation, depth flip, and target reuse defects",
    );
}
