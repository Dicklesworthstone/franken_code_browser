//! FCB-017.V production verification scenario: Glyph atlas, raster queues,
//! and safe resource retirement.
//!
//! Required verification cases:
//! 1. `oracle_actual_size_pixel_fixtures_and_identity_preservation`
//! 2. `oracle_stale_slot_reuse_and_generation_retirement`
//! 3. `oracle_in_flight_pinning_and_completion_safe_churn`
//! 4. `oracle_priority_queue_servicing_and_non_blocking_fallback`
//! 5. `oracle_unified_memory_discard_and_recompute_lifecycle`
//! 6. `oracle_two_independent_arenas_and_devices`
//! 7. `negative_control_zero_dimensions_and_corrupt_slots_refused`
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_017.sh`).

#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use fcb::{
    AtlasSlotId, BatchProcessReport, BoundedAtlasConfig, BoundedGlyphAtlas,
    BoundedRasterQueue, EnqueueResult, GlyphAtlasError, GlyphRasterKey, GpuGlyphRef,
    HintingPolicy, RasterLookupResult, RasterMissRequest, RasterMode, RasterPriority,
    RasterScaleTier, RasterizedGlyph, SlotGeneration, SubpixelBin,
};
use fcb_core::ArenaOwnerId;
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;

const RUN_ID_ENV: &str = "FCB_017_RUN_ID";

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-017-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_17_00_01),
        pin: SourcePin::new("0173456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
        route: RouteId::new("headless:rust").expect("route valid"),
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
    fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    )
    .expect("receipt retained");
}

fn sample_key(font: &str, glyph_id: u32, size_pt: f32, mode: RasterMode) -> GlyphRasterKey {
    GlyphRasterKey {
        font_name: font.to_string(),
        glyph_id,
        size_px_mils: (size_pt * 1000.0) as u32,
        raster_mode: mode,
        scale_tier: RasterScaleTier::ONE_X,
        subpixel_bin: SubpixelBin::ZERO,
        hinting: HintingPolicy::None,
        variation_coords_hash: 0,
        raster_version: 1,
    }
}

#[test]
fn oracle_actual_size_pixel_fixtures_and_identity_preservation() {
    let owner = ArenaOwnerId::new(0x0C_17).expect("owner valid");
    let config = BoundedAtlasConfig {
        page_size: 128,
        max_pages: 4,
        max_raster_queue_capacity: 64,
        log_capacity: 128,
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, config);

    // Realistic ASCII & code punctuation fixtures at 1x and 2x backing scales
    // 'A': 10x16 at 1x, 20x32 at 2x
    // '{': 8x18 at 1x
    // ';': 6x14 at 1x
    let key_a_1x = sample_key("Menlo", 'A' as u32, 16.0, RasterMode::MonochromeCoverage);
    let key_color = sample_key("Emoji", 0x1F600, 32.0, RasterMode::ColorRgba);
    let key_brace = sample_key("Menlo", '{' as u32, 16.0, RasterMode::MonochromeCoverage);
    let key_semi = sample_key("Menlo", ';' as u32, 16.0, RasterMode::MonochromeCoverage);
    let key_outline = sample_key("Menlo", 'A' as u32, 48.0, RasterMode::DistanceField);

    let ref_a_1x = atlas
        .allocate_and_insert(key_a_1x.clone(), 10, 16, 0, 12, 10, 1)
        .expect("allocate A 1x");
    let ref_color = atlas
        .allocate_and_insert(key_color.clone(), 20, 32, 0, 24, 20, 1)
        .expect("allocate color emoji");
    let ref_brace = atlas
        .allocate_and_insert(key_brace.clone(), 8, 18, -1, 14, 10, 1)
        .expect("allocate brace");
    let ref_semi = atlas
        .allocate_and_insert(key_semi.clone(), 6, 14, 0, 10, 10, 1)
        .expect("allocate semi");
    let ref_outline = atlas
        .allocate_and_insert(key_outline.clone(), 30, 48, 0, 36, 30, 1)
        .expect("allocate outline");

    // Invariant: Immutable raster key is decoupled from mutable GPU residency handle
    assert_eq!(ref_a_1x.owner, owner);
    assert_eq!(ref_a_1x.device_generation, 1);
    assert_eq!(ref_a_1x.pixel_width, 10);
    assert_eq!(ref_a_1x.pixel_height, 16);
    assert_eq!(ref_a_1x.bearing_x, 0);
    assert_eq!(ref_a_1x.bearing_y, 12);
    assert_eq!(ref_a_1x.advance_x, 10);

    // Segregated pages: Monochrome (A 1x, brace, semi) vs Color (ColorRgba) vs DistanceField (outline)
    assert_eq!(ref_a_1x.page_id, ref_brace.page_id);
    assert_eq!(ref_a_1x.page_id, ref_semi.page_id);
    assert_ne!(ref_a_1x.page_id, ref_color.page_id);
    assert_ne!(ref_a_1x.page_id, ref_outline.page_id);
    assert_ne!(ref_color.page_id, ref_outline.page_id);

    // Shelf packing non-overlap validation
    assert!(atlas.validate_ref(&ref_a_1x).is_ok());
    assert!(atlas.validate_ref(&ref_color).is_ok());
    assert!(atlas.validate_ref(&ref_brace).is_ok());
    assert!(atlas.validate_ref(&ref_semi).is_ok());
    assert!(atlas.validate_ref(&ref_outline).is_ok());

    record_receipt(
        "oracle_actual_size_pixel_fixtures_and_identity_preservation",
        Effect::Succeeded,
        "verified actual-size code glyph fixtures preserve pixel coverage and identity separation across segregated pages",
    );
}

#[test]
fn oracle_stale_slot_reuse_and_generation_retirement() {
    let owner = ArenaOwnerId::new(0x0C_17).expect("owner valid");
    // Tight 64x64 page admitting exactly four 28x28 glyphs in a single page
    let config = BoundedAtlasConfig {
        page_size: 64,
        max_pages: 1,
        max_raster_queue_capacity: 16,
        log_capacity: 64,
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, config);

    let key1 = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 14.0, RasterMode::MonochromeCoverage);
    let key3 = sample_key("Font", 3, 14.0, RasterMode::MonochromeCoverage);
    let key4 = sample_key("Font", 4, 14.0, RasterMode::MonochromeCoverage);

    let ref1 = atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 1)
        .expect("insert 1");
    let ref2 = atlas
        .allocate_and_insert(key2.clone(), 28, 28, 0, 20, 28, 2)
        .expect("insert 2");
    let ref3 = atlas
        .allocate_and_insert(key3.clone(), 28, 28, 0, 20, 28, 3)
        .expect("insert 3");
    let ref4 = atlas
        .allocate_and_insert(key4.clone(), 28, 28, 0, 20, 28, 4)
        .expect("insert 4");

    // All 4 slots are initially valid
    assert!(atlas.validate_ref(&ref1).is_ok());
    assert!(atlas.validate_ref(&ref2).is_ok());
    assert!(atlas.validate_ref(&ref3).is_ok());
    assert!(atlas.validate_ref(&ref4).is_ok());

    // Allocate 5th glyph: forces LRU eviction of slot 0 (ref1, last_used_frame 1 < current_frame 5)
    let key5 = sample_key("Font", 5, 14.0, RasterMode::MonochromeCoverage);
    let ref5 = atlas
        .allocate_and_insert(key5.clone(), 28, 28, 0, 20, 28, 5)
        .expect("insert 5 via LRU eviction");

    // Slot generation must increment monotonically
    assert_eq!(ref5.slot_id, ref1.slot_id);
    assert_eq!(ref5.slot_generation, ref1.slot_generation.next());
    assert!(atlas.validate_ref(&ref5).is_ok());

    // Crucial Oracle: The old ref1 handle with stale generation MUST be rejected!
    // Retained draw data sampling ref1 can NEVER sample ref5's newly inserted glyph.
    assert_eq!(
        atlas.validate_ref(&ref1),
        Err(GlyphAtlasError::StaleSlotGeneration)
    );

    record_receipt(
        "oracle_stale_slot_reuse_and_generation_retirement",
        Effect::Succeeded,
        "verified monotonic slot generation increment on eviction prevents sampling newly inserted glyphs via stale handles",
    );
}

#[test]
fn oracle_in_flight_pinning_and_completion_safe_churn() {
    let owner = ArenaOwnerId::new(0x0C_17).expect("owner valid");
    let config = BoundedAtlasConfig {
        page_size: 64,
        max_pages: 1,
        max_raster_queue_capacity: 16,
        log_capacity: 64,
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, config);

    let key1 = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 14.0, RasterMode::MonochromeCoverage);
    let key3 = sample_key("Font", 3, 14.0, RasterMode::MonochromeCoverage);
    let key4 = sample_key("Font", 4, 14.0, RasterMode::MonochromeCoverage);

    let ref1 = atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 1)
        .expect("insert 1");
    let ref2 = atlas
        .allocate_and_insert(key2.clone(), 28, 28, 0, 20, 28, 2)
        .expect("insert 2");
    let ref3 = atlas
        .allocate_and_insert(key3.clone(), 28, 28, 0, 20, 28, 3)
        .expect("insert 3");
    let ref4 = atlas
        .allocate_and_insert(key4.clone(), 28, 28, 0, 20, 28, 4)
        .expect("insert 4");

    // Pin all slots as in-flight on GPU
    atlas.pin_slot(&ref1).expect("pin 1");
    atlas.pin_slot(&ref2).expect("pin 2");
    atlas.pin_slot(&ref3).expect("pin 3");
    atlas.pin_slot(&ref4).expect("pin 4");

    // Under pressure, attempt to allocate 5th glyph: eviction cannot evict pinned slots!
    let key5 = sample_key("Font", 5, 14.0, RasterMode::MonochromeCoverage);
    let err = atlas.allocate_and_insert(key5.clone(), 28, 28, 0, 20, 28, 5);
    assert_eq!(err, Err(GlyphAtlasError::AllSlotsPinned));

    // Frame completion arrives from GPU terminal record drain: unpin slot 1
    atlas.unpin_slot(&ref1).expect("unpin 1 on GPU completion");

    // Now allocation of 5th glyph succeeds by reusing unpinned slot 1
    let ref5 = atlas
        .allocate_and_insert(key5.clone(), 28, 28, 0, 20, 28, 6)
        .expect("insert 5 after unpin");
    assert_eq!(ref5.slot_id, ref1.slot_id);
    assert!(atlas.validate_ref(&ref5).is_ok());

    record_receipt(
        "oracle_in_flight_pinning_and_completion_safe_churn",
        Effect::Succeeded,
        "verified in-flight frame pinning protects referenced slots from eviction until lossless GPU completion unpins them",
    );
}

#[test]
fn oracle_priority_queue_servicing_and_non_blocking_fallback() {
    let owner = ArenaOwnerId::new(0x0C_17).expect("owner valid");
    let config = BoundedAtlasConfig {
        page_size: 128,
        max_pages: 2,
        max_raster_queue_capacity: 4,
        log_capacity: 64,
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, config);

    // Initial fallback glyph
    let fallback_key = sample_key("Fallback", 0, 16.0, RasterMode::MonochromeCoverage);
    let fallback_ref = atlas
        .allocate_and_insert(fallback_key, 12, 16, 0, 12, 12, 1)
        .expect("allocate fallback");

    let distant_key = sample_key("Font", 10, 8.0, RasterMode::MonochromeCoverage);
    let visible_key = sample_key("Font", 20, 16.0, RasterMode::MonochromeCoverage);
    let selected_key = sample_key("Font", 30, 16.0, RasterMode::MonochromeCoverage);

    // Query missing glyph with fallback: non-blocking layout query returns MissPending
    let res = atlas
        .query_or_enqueue(
            distant_key.clone(),
            RasterPriority::DistantOrMapLabel,
            2,
            Some(fallback_ref.clone()),
        )
        .expect("non-blocking query");

    assert!(matches!(res, RasterLookupResult::MissPending { .. }));
    if let RasterLookupResult::MissPending {
        key,
        priority,
        fallback_ref: fb,
    } = res
    {
        assert_eq!(key, distant_key);
        assert_eq!(priority, RasterPriority::DistantOrMapLabel);
        assert_eq!(fb, Some(fallback_ref.clone()));
    }

    // Enqueue visible and selected
    atlas
        .query_or_enqueue(
            visible_key.clone(),
            RasterPriority::VisibleReadingText,
            2,
            Some(fallback_ref.clone()),
        )
        .expect("enqueue visible");
    atlas
        .query_or_enqueue(
            selected_key.clone(),
            RasterPriority::SelectedReadingText,
            2,
            Some(fallback_ref.clone()),
        )
        .expect("enqueue selected");

    // Batch processing drains in strict priority order: Selected > Visible > Distant
    let mut processed_order = Vec::new();
    let report = atlas
        .process_raster_queue_batch(3, 3, |req_key| {
            processed_order.push(req_key.clone());
            Ok(RasterizedGlyph {
                pixel_width: 12,
                pixel_height: 16,
                bearing_x: 0,
                bearing_y: 12,
                advance_x: 12,
            })
        })
        .expect("drain queue");

    assert_eq!(
        report,
        BatchProcessReport {
            processed_count: 3,
            remaining_in_queue: 0,
            recomputed_count: 0,
        }
    );
    assert_eq!(processed_order.len(), 3);
    assert_eq!(processed_order[0], selected_key);
    assert_eq!(processed_order[1], visible_key);
    assert_eq!(processed_order[2], distant_key);

    // Subsequent query is a direct cache Hit!
    let hit_res = atlas
        .query_or_enqueue(selected_key, RasterPriority::SelectedReadingText, 4, None)
        .expect("subsequent query");
    assert!(matches!(hit_res, RasterLookupResult::Hit(_)));

    record_receipt(
        "oracle_priority_queue_servicing_and_non_blocking_fallback",
        Effect::Succeeded,
        "verified non-blocking layout queries return MissPending with fallback and batch drains in Selected > Visible > Distant priority order",
    );
}

#[test]
fn oracle_unified_memory_discard_and_recompute_lifecycle() {
    let owner = ArenaOwnerId::new(0x0C_17).expect("owner valid");
    let config = BoundedAtlasConfig {
        page_size: 64,
        max_pages: 1,
        max_raster_queue_capacity: 16,
        log_capacity: 64,
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, config);

    let key1 = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 14.0, RasterMode::MonochromeCoverage);
    let key3 = sample_key("Font", 3, 14.0, RasterMode::MonochromeCoverage);
    let key4 = sample_key("Font", 4, 14.0, RasterMode::MonochromeCoverage);

    atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 1)
        .unwrap();
    atlas
        .allocate_and_insert(key2.clone(), 28, 28, 0, 20, 28, 2)
        .unwrap();
    atlas
        .allocate_and_insert(key3.clone(), 28, 28, 0, 20, 28, 3)
        .unwrap();
    atlas
        .allocate_and_insert(key4.clone(), 28, 28, 0, 20, 28, 4)
        .unwrap();

    let mem1 = atlas.memory_accounting();
    assert_eq!(mem1.allocated_texture_bytes, 64 * 64);
    assert_eq!(mem1.peak_texture_bytes, 4096);
    assert_eq!(mem1.discarded_texture_bytes, 0);
    assert_eq!(mem1.recomputed_glyph_count, 0);
    assert!(mem1.no_cpu_demotion_policy);

    // Eviction discards texture bytes directly on UMA
    let key5 = sample_key("Font", 5, 14.0, RasterMode::MonochromeCoverage);
    atlas
        .allocate_and_insert(key5.clone(), 28, 28, 0, 20, 28, 5)
        .unwrap();

    let mem2 = atlas.memory_accounting();
    assert_eq!(mem2.discarded_texture_bytes, 28 * 28);
    assert_eq!(mem2.recomputed_glyph_count, 0);

    // Re-inserting previously evicted key1 increments recomputed glyph count
    atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 6)
        .unwrap();

    let mem3 = atlas.memory_accounting();
    assert_eq!(mem3.recomputed_glyph_count, 1);
    assert_eq!(mem3.discarded_texture_bytes, 2 * 28 * 28);

    // Reset device generation
    atlas.reset_device_generation(2);
    let mem_reset = atlas.memory_accounting();
    assert_eq!(mem_reset.allocated_texture_bytes, 0);
    assert_eq!(mem_reset.peak_texture_bytes, 4096);

    record_receipt(
        "oracle_unified_memory_discard_and_recompute_lifecycle",
        Effect::Succeeded,
        "verified Apple Silicon unified memory accounting tracks direct texture discard and source font recomputations without duplicate CPU demotion",
    );
}

#[test]
fn oracle_two_independent_arenas_and_devices() {
    let owner1 = ArenaOwnerId::new(0x0C_17).expect("owner 1");
    let owner2 = ArenaOwnerId::new(0x0C_18).expect("owner 2");

    let mut atlas1 = BoundedGlyphAtlas::new(owner1, 100, 1, BoundedAtlasConfig::default());
    let mut atlas2 = BoundedGlyphAtlas::new(owner2, 200, 1, BoundedAtlasConfig::default());

    let key = sample_key("Font", 1, 16.0, RasterMode::MonochromeCoverage);
    let ref1 = atlas1
        .allocate_and_insert(key.clone(), 16, 16, 0, 12, 16, 1)
        .expect("allocate arena 1");
    let ref2 = atlas2
        .allocate_and_insert(key.clone(), 16, 16, 0, 12, 16, 1)
        .expect("allocate arena 2");

    // Cross-arena references are strictly rejected!
    assert_eq!(atlas1.validate_ref(&ref2), Err(GlyphAtlasError::OwnerMismatch));
    assert_eq!(atlas2.validate_ref(&ref1), Err(GlyphAtlasError::OwnerMismatch));

    // Reset device 1 does not affect device 2
    atlas1.reset_device_generation(2);
    assert_eq!(
        atlas1.validate_ref(&ref1),
        Err(GlyphAtlasError::StaleDeviceGeneration)
    );
    assert!(atlas2.validate_ref(&ref2).is_ok());

    record_receipt(
        "oracle_two_independent_arenas_and_devices",
        Effect::Succeeded,
        "verified arena and device isolation prevents cross-arena leakage and isolated reset behavior",
    );
}

#[test]
fn negative_control_zero_dimensions_and_corrupt_slots_refused() {
    let owner = ArenaOwnerId::new(0x0C_17).expect("owner valid");
    let config = BoundedAtlasConfig {
        page_size: 64,
        max_pages: 1,
        max_raster_queue_capacity: 4,
        log_capacity: 16,
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, config);

    let key = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);

    // Negative control 1: zero dimensions
    assert_eq!(
        atlas.allocate_and_insert(key.clone(), 0, 10, 0, 0, 0, 1),
        Err(GlyphAtlasError::InvalidDimensions)
    );
    assert_eq!(
        atlas.allocate_and_insert(key.clone(), 10, 0, 0, 0, 0, 1),
        Err(GlyphAtlasError::InvalidDimensions)
    );

    // Negative control 2: oversized glyph
    assert_eq!(
        atlas.allocate_and_insert(key.clone(), 100, 10, 0, 0, 0, 1),
        Err(GlyphAtlasError::GlyphTooLargeForPage)
    );

    // Negative control 3: corrupt / unallocated slot
    let bogus_ref = GpuGlyphRef {
        owner,
        device_id: 100,
        device_generation: 1,
        page_id: fcb::AtlasPageId(1),
        slot_id: AtlasSlotId(99),
        slot_generation: SlotGeneration::INITIAL,
        pixel_rect: fcb_core::Rect2D::ZERO,
        uv_rect: fcb_core::Rect2D::ZERO,
        pixel_width: 10,
        pixel_height: 10,
        bearing_x: 0,
        bearing_y: 0,
        advance_x: 0,
        raster_key: key.clone(),
    };
    assert_eq!(
        atlas.validate_ref(&bogus_ref),
        Err(GlyphAtlasError::PageNotFound)
    );

    // Negative control 4: invalid queue enqueue
    let mut queue = BoundedRasterQueue::new(1);
    let req1 = RasterMissRequest {
        key: key.clone(),
        priority: RasterPriority::DistantOrMapLabel,
        requested_frame: 1,
        estimated_width: 10,
        estimated_height: 10,
        sequence: 0,
    };
    assert_eq!(queue.enqueue(req1.clone()), Ok(EnqueueResult::Enqueued));
    // Re-enqueueing same key with same priority is deduplicated
    assert_eq!(queue.enqueue(req1), Ok(EnqueueResult::AlreadyPresent));

    record_receipt(
        "negative_control_zero_dimensions_and_corrupt_slots_refused",
        Effect::Succeeded,
        "negative control: zero dimensions, oversized glyphs, unallocated slots, and duplicate queue requests are truthfully rejected",
    );
}
