#![forbid(unsafe_code)]

//! Production unit and boundary test suite for background raster misses,
//! prioritization, unified-memory accounting, and GPU-safe churn (FCB-017.B / fcb-bte.2).
//!
//! Acceptance criteria & package oracle:
//! 1. Priority servicing: `SelectedReadingText` strictly processed before `VisibleReadingText`
//!    and `DistantOrMapLabel`. FIFO order preserved within the same priority tier.
//! 2. Deduplication and priority promotion: repeated queries upgrade priority without queue growth.
//! 3. Bounded queue capacity: strict capacity limit; critical reading text pre-empts/evicts
//!    lower-priority map labels under pressure; equal/lower priority rejected when full.
//! 4. Non-blocking layout: `query_or_enqueue` returns `MissPending` with optional validated fallback slot.
//! 5. Unified-memory accounting & zero CPU demotion: texture bytes tracked, discarded on eviction,
//!    recomputed from font source on re-request, zero CPU shadow duplication (`no_cpu_demotion_policy: true`).
//! 6. Two independent arenas/devices: isolation verified; cross-arena references rejected; reset on one
//!    does not invalidate the other.
//! 7. In-flight pinning during background batch: pinned slots cannot be evicted during background rasterization;
//!    requests preserved until GPU completion unpins slots.
//! 8. Retained bounded diagnostic logs: events recorded in bounded ring with monotonic sequences.
//! 9. Negative controls: queue full rejection, invalid dimensions, stale device generation.

use fcb::{
    AtlasLogEventKind, AtlasSlotId, BatchProcessReport, BoundedAtlasConfig,
    BoundedGlyphAtlas, BoundedRasterQueue, EnqueueResult, GlyphAtlasError, GlyphRasterKey,
    GpuGlyphRef, HintingPolicy, RasterLookupResult, RasterMissRequest, RasterMode, RasterPriority,
    RasterScaleTier, RasterizedGlyph, SlotGeneration, SubpixelBin,
};
use fcb_core::ArenaOwnerId;

fn test_owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).unwrap()
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
fn test_priority_queue_servicing_order() {
    let mut queue = BoundedRasterQueue::new(10);

    let key_distant1 = sample_key("Font", 1, 12.0, RasterMode::MonochromeCoverage);
    let key_distant2 = sample_key("Font", 2, 12.0, RasterMode::MonochromeCoverage);
    let key_visible1 = sample_key("Font", 3, 14.0, RasterMode::MonochromeCoverage);
    let key_visible2 = sample_key("Font", 4, 14.0, RasterMode::MonochromeCoverage);
    let key_selected1 = sample_key("Font", 5, 16.0, RasterMode::MonochromeCoverage);
    let key_selected2 = sample_key("Font", 6, 16.0, RasterMode::MonochromeCoverage);

    // Enqueue in reverse priority order: Distant first, then Visible, then Selected
    queue
        .enqueue(RasterMissRequest {
            key: key_distant1.clone(),
            priority: RasterPriority::DistantOrMapLabel,
            requested_frame: 1,
            estimated_width: 10,
            estimated_height: 10,
            sequence: 0,
        })
        .expect("enqueue distant 1");

    queue
        .enqueue(RasterMissRequest {
            key: key_visible1.clone(),
            priority: RasterPriority::VisibleReadingText,
            requested_frame: 1,
            estimated_width: 12,
            estimated_height: 12,
            sequence: 0,
        })
        .expect("enqueue visible 1");

    queue
        .enqueue(RasterMissRequest {
            key: key_distant2.clone(),
            priority: RasterPriority::DistantOrMapLabel,
            requested_frame: 1,
            estimated_width: 10,
            estimated_height: 10,
            sequence: 0,
        })
        .expect("enqueue distant 2");

    queue
        .enqueue(RasterMissRequest {
            key: key_selected1.clone(),
            priority: RasterPriority::SelectedReadingText,
            requested_frame: 1,
            estimated_width: 14,
            estimated_height: 14,
            sequence: 0,
        })
        .expect("enqueue selected 1");

    queue
        .enqueue(RasterMissRequest {
            key: key_visible2.clone(),
            priority: RasterPriority::VisibleReadingText,
            requested_frame: 1,
            estimated_width: 12,
            estimated_height: 12,
            sequence: 0,
        })
        .expect("enqueue visible 2");

    queue
        .enqueue(RasterMissRequest {
            key: key_selected2.clone(),
            priority: RasterPriority::SelectedReadingText,
            requested_frame: 1,
            estimated_width: 14,
            estimated_height: 14,
            sequence: 0,
        })
        .expect("enqueue selected 2");

    assert_eq!(queue.len(), 6);

    // Servicing order MUST be:
    // 1. Selected 1
    // 2. Selected 2
    // 3. Visible 1
    // 4. Visible 2
    // 5. Distant 1
    // 6. Distant 2
    let pop1 = queue.pop_highest_priority().expect("pop 1");
    assert_eq!(pop1.priority, RasterPriority::SelectedReadingText);
    assert_eq!(pop1.key, key_selected1);

    let pop2 = queue.pop_highest_priority().expect("pop 2");
    assert_eq!(pop2.priority, RasterPriority::SelectedReadingText);
    assert_eq!(pop2.key, key_selected2);

    let pop3 = queue.pop_highest_priority().expect("pop 3");
    assert_eq!(pop3.priority, RasterPriority::VisibleReadingText);
    assert_eq!(pop3.key, key_visible1);

    let pop4 = queue.pop_highest_priority().expect("pop 4");
    assert_eq!(pop4.priority, RasterPriority::VisibleReadingText);
    assert_eq!(pop4.key, key_visible2);

    let pop5 = queue.pop_highest_priority().expect("pop 5");
    assert_eq!(pop5.priority, RasterPriority::DistantOrMapLabel);
    assert_eq!(pop5.key, key_distant1);

    let pop6 = queue.pop_highest_priority().expect("pop 6");
    assert_eq!(pop6.priority, RasterPriority::DistantOrMapLabel);
    assert_eq!(pop6.key, key_distant2);

    assert!(queue.is_empty());
    assert_eq!(queue.stats().serviced_count, 6);
}

#[test]
fn test_deduplication_and_priority_promotion() {
    let mut queue = BoundedRasterQueue::new(5);

    let key = sample_key("Font", 10, 14.0, RasterMode::MonochromeCoverage);

    // Initial query as DistantOrMapLabel
    let res1 = queue
        .enqueue(RasterMissRequest {
            key: key.clone(),
            priority: RasterPriority::DistantOrMapLabel,
            requested_frame: 10,
            estimated_width: 12,
            estimated_height: 14,
            sequence: 0,
        })
        .expect("enqueue distant");
    assert_eq!(res1, EnqueueResult::Enqueued);
    assert_eq!(queue.len(), 1);

    // User selects the text containing this glyph: upgraded to SelectedReadingText
    let res2 = queue
        .enqueue(RasterMissRequest {
            key: key.clone(),
            priority: RasterPriority::SelectedReadingText,
            requested_frame: 15,
            estimated_width: 12,
            estimated_height: 14,
            sequence: 0,
        })
        .expect("enqueue selected promotion");
    assert_eq!(res2, EnqueueResult::Promoted);
    assert_eq!(queue.len(), 1, "queue must not grow on duplicate request");

    // Repeated query with lower priority does not downgrade
    let res3 = queue
        .enqueue(RasterMissRequest {
            key: key.clone(),
            priority: RasterPriority::VisibleReadingText,
            requested_frame: 16,
            estimated_width: 12,
            estimated_height: 14,
            sequence: 0,
        })
        .expect("enqueue visible");
    assert_eq!(res3, EnqueueResult::AlreadyPresent);
    assert_eq!(queue.len(), 1);

    // Popped request has the promoted priority
    let popped = queue.pop_highest_priority().expect("pop");
    assert_eq!(popped.priority, RasterPriority::SelectedReadingText);
    assert_eq!(popped.requested_frame, 16);
    assert_eq!(queue.stats().promoted_count, 1);
}

#[test]
fn test_bounded_queue_capacity_and_eviction() {
    let mut queue = BoundedRasterQueue::new(2);

    let key1 = sample_key("Font", 1, 12.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 12.0, RasterMode::MonochromeCoverage);
    let key3 = sample_key("Font", 3, 12.0, RasterMode::MonochromeCoverage);
    let key_selected = sample_key("Font", 99, 16.0, RasterMode::MonochromeCoverage);

    queue
        .enqueue(RasterMissRequest {
            key: key1.clone(),
            priority: RasterPriority::DistantOrMapLabel,
            requested_frame: 1,
            estimated_width: 10,
            estimated_height: 10,
            sequence: 0,
        })
        .unwrap();

    queue
        .enqueue(RasterMissRequest {
            key: key2.clone(),
            priority: RasterPriority::DistantOrMapLabel,
            requested_frame: 2,
            estimated_width: 10,
            estimated_height: 10,
            sequence: 0,
        })
        .unwrap();

    assert_eq!(queue.len(), 2);

    // 1. Trying to enqueue another distant label when full must be rejected!
    let err = queue
        .enqueue(RasterMissRequest {
            key: key3.clone(),
            priority: RasterPriority::DistantOrMapLabel,
            requested_frame: 3,
            estimated_width: 10,
            estimated_height: 10,
            sequence: 0,
        })
        .expect_err("must be rejected when full with equal/lower priority");
    assert_eq!(err, GlyphAtlasError::RasterQueueFull);

    // 2. Enqueuing SelectedReadingText under capacity pressure evicts oldest lowest-priority item (key1)
    let res = queue
        .enqueue(RasterMissRequest {
            key: key_selected.clone(),
            priority: RasterPriority::SelectedReadingText,
            requested_frame: 4,
            estimated_width: 16,
            estimated_height: 16,
            sequence: 0,
        })
        .expect("must evict distant label to admit selected reading text");
    assert_eq!(res, EnqueueResult::EnqueuedWithEviction);
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.stats().evicted_count, 1);

    // Popping should return SelectedReadingText first, then key2 (key1 was evicted)
    let popped1 = queue.pop_highest_priority().unwrap();
    assert_eq!(popped1.key, key_selected);

    let popped2 = queue.pop_highest_priority().unwrap();
    assert_eq!(popped2.key, key2);

    assert!(queue.is_empty());
}

#[test]
fn test_non_blocking_layout_query_and_batch_processing() {
    let owner = test_owner(100);
    let mut atlas = BoundedGlyphAtlas::new(owner, 1000, 1, BoundedAtlasConfig::default());

    // Allocate a known fallback glyph (e.g. question mark or space)
    let fallback_key = sample_key("Font", 0, 14.0, RasterMode::MonochromeCoverage);
    let fallback_ref = atlas
        .allocate_and_insert(fallback_key, 8, 14, 0, 10, 8, 1)
        .expect("insert fallback");

    let missing_key = sample_key("Font", 100, 14.0, RasterMode::MonochromeCoverage);

    // 1. Layout queries missing glyph: non-blocking, returns MissPending with fallback
    let lookup_res = atlas
        .query_or_enqueue(
            missing_key.clone(),
            RasterPriority::SelectedReadingText,
            2,
            Some(fallback_ref.clone()),
        )
        .expect("query_or_enqueue");

    assert!(matches!(lookup_res, RasterLookupResult::MissPending { .. }));
    if let RasterLookupResult::MissPending {
        key,
        priority,
        fallback_ref: res_fallback,
    } = lookup_res
    {
        assert_eq!(key, missing_key);
        assert_eq!(priority, RasterPriority::SelectedReadingText);
        assert_eq!(res_fallback.unwrap().slot_id, fallback_ref.slot_id);
    }

    assert_eq!(atlas.raster_queue().len(), 1);

    // 2. Background raster worker drains batch
    let report = atlas
        .process_raster_queue_batch(4, 3, |key| {
            assert_eq!(key, &missing_key);
            Ok(RasterizedGlyph {
                pixel_width: 12,
                pixel_height: 16,
                bearing_x: 1,
                bearing_y: 14,
                advance_x: 12,
            })
        })
        .expect("process batch");

    assert_eq!(
        report,
        BatchProcessReport {
            processed_count: 1,
            remaining_in_queue: 0,
            recomputed_count: 0,
        }
    );

    // 3. Next query hits the newly resident glyph
    let hit_res = atlas
        .query_or_enqueue(
            missing_key.clone(),
            RasterPriority::SelectedReadingText,
            4,
            None,
        )
        .expect("query again");

    assert!(matches!(hit_res, RasterLookupResult::Hit(_)));
    if let RasterLookupResult::Hit(r) = hit_res {
        assert_eq!(r.raster_key, missing_key);
        assert_eq!(r.pixel_width, 12);
        assert_eq!(r.pixel_height, 16);
    }
}

#[test]
fn test_unified_memory_discard_and_recompute_lifecycle() {
    let owner = test_owner(200);
    // 64x64 page (4096 bytes monochrome). Fits ~4 glyphs of 28x28 (with 2px padding = 30x30).
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 64,
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 2000, 1, config);

    let initial_mem = atlas.memory_accounting();
    assert_eq!(initial_mem.allocated_texture_bytes, 0);
    assert_eq!(initial_mem.discarded_texture_bytes, 0);
    assert_eq!(initial_mem.recomputed_glyph_count, 0);
    assert!(initial_mem.no_cpu_demotion_policy);

    // Allocate 4 glyphs (allocating the single page)
    let key1 = sample_key("Font", 1, 16.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 16.0, RasterMode::MonochromeCoverage);
    let key3 = sample_key("Font", 3, 16.0, RasterMode::MonochromeCoverage);
    let key4 = sample_key("Font", 4, 16.0, RasterMode::MonochromeCoverage);

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

    let mem_after_4 = atlas.memory_accounting();
    assert_eq!(mem_after_4.allocated_texture_bytes, 64 * 64); // 4096 bytes
    assert_eq!(mem_after_4.peak_texture_bytes, 4096);
    assert_eq!(mem_after_4.discarded_texture_bytes, 0);
    assert_eq!(mem_after_4.active_page_count, 1);

    // 5th glyph forces LRU eviction of key1:
    // Its slot texture bytes are discarded directly without CPU shadow buffer duplication!
    let key5 = sample_key("Font", 5, 16.0, RasterMode::MonochromeCoverage);
    atlas
        .allocate_and_insert(key5.clone(), 28, 28, 0, 20, 28, 5)
        .expect("insert 5 via eviction");

    let mem_after_evict = atlas.memory_accounting();
    assert_eq!(mem_after_evict.allocated_texture_bytes, 4096);
    assert_eq!(mem_after_evict.discarded_texture_bytes, 28 * 28); // 784 bytes discarded
    assert_eq!(mem_after_evict.recomputed_glyph_count, 0);

    // Now re-request key1: it is recomputed on demand from font source
    // Evicts slot 1 (key2) to re-insert key1
    atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 6)
        .expect("recompute and insert key1");

    let mem_after_recompute = atlas.memory_accounting();
    assert_eq!(mem_after_recompute.recomputed_glyph_count, 1);
    assert_eq!(
        mem_after_recompute.discarded_texture_bytes,
        2 * 28 * 28
    );

    // Device reset / teardown: all active page texture memory is discarded
    atlas.reset_device_generation(2);
    let mem_after_reset = atlas.memory_accounting();
    assert_eq!(mem_after_reset.allocated_texture_bytes, 0);
    assert_eq!(mem_after_reset.active_page_count, 0);
    assert_eq!(
        mem_after_reset.discarded_texture_bytes,
        28 * 28 * 2 + 4096
    );
}

#[test]
fn test_two_independent_arenas_and_devices() {
    let owner_a = test_owner(300);
    let owner_b = test_owner(400);

    let mut atlas_a = BoundedGlyphAtlas::new(owner_a, 3000, 1, BoundedAtlasConfig::default());
    let mut atlas_b = BoundedGlyphAtlas::new(owner_b, 4000, 1, BoundedAtlasConfig::default());

    let key = sample_key("Font", 10, 14.0, RasterMode::MonochromeCoverage);

    let ref_a = atlas_a
        .allocate_and_insert(key.clone(), 12, 16, 0, 12, 12, 1)
        .expect("allocate on A");
    let ref_b = atlas_b
        .allocate_and_insert(key.clone(), 12, 16, 0, 12, 12, 1)
        .expect("allocate on B");

    // Both are initially valid within their own domain
    assert_eq!(atlas_a.validate_ref(&ref_a), Ok(()));
    assert_eq!(atlas_b.validate_ref(&ref_b), Ok(()));

    // Cross validation must be strictly rejected
    assert_eq!(
        atlas_a.validate_ref(&ref_b),
        Err(GlyphAtlasError::OwnerMismatch)
    );
    assert_eq!(
        atlas_b.validate_ref(&ref_a),
        Err(GlyphAtlasError::OwnerMismatch)
    );

    // Device reset on atlas A does not affect atlas B
    atlas_a.reset_device_generation(2);
    assert_eq!(
        atlas_a.validate_ref(&ref_a),
        Err(GlyphAtlasError::StaleDeviceGeneration)
    );
    assert_eq!(
        atlas_b.validate_ref(&ref_b),
        Ok(()),
        "Atlas B must remain completely valid"
    );
}

#[test]
fn test_in_flight_pinning_during_background_batch() {
    let owner = test_owner(500);
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 64,
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 5000, 1, config);

    let key1 = sample_key("Font", 1, 16.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 16.0, RasterMode::MonochromeCoverage);
    let key3 = sample_key("Font", 3, 16.0, RasterMode::MonochromeCoverage);
    let key4 = sample_key("Font", 4, 16.0, RasterMode::MonochromeCoverage);

    // Fill page with 4 glyphs (28x28 in 64x64)
    let ref1 = atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 1)
        .unwrap();
    let ref2 = atlas
        .allocate_and_insert(key2.clone(), 28, 28, 0, 20, 28, 2)
        .unwrap();
    let ref3 = atlas
        .allocate_and_insert(key3.clone(), 28, 28, 0, 20, 28, 3)
        .unwrap();
    let ref4 = atlas
        .allocate_and_insert(key4.clone(), 28, 28, 0, 20, 28, 4)
        .unwrap();

    // Pin ALL 4 slots (e.g. 4 glyphs sampled by in-flight GPU frames)
    atlas.pin_slot(&ref1).unwrap();
    atlas.pin_slot(&ref2).unwrap();
    atlas.pin_slot(&ref3).unwrap();
    atlas.pin_slot(&ref4).unwrap();

    // Queue 2 background raster requests
    let key_pending1 = sample_key("Font", 10, 16.0, RasterMode::MonochromeCoverage);
    let key_pending2 = sample_key("Font", 11, 16.0, RasterMode::MonochromeCoverage);

    atlas
        .query_or_enqueue(
            key_pending1.clone(),
            RasterPriority::SelectedReadingText,
            5,
            None,
        )
        .unwrap();
    atlas
        .query_or_enqueue(
            key_pending2.clone(),
            RasterPriority::VisibleReadingText,
            5,
            None,
        )
        .unwrap();

    assert_eq!(atlas.raster_queue().len(), 2);

    // Run batch processor: cannot allocate because all slots are pinned!
    // Processing stops and requests remain in the queue
    let report = atlas
        .process_raster_queue_batch(2, 6, |_| {
            Ok(RasterizedGlyph {
                pixel_width: 28,
                pixel_height: 28,
                bearing_x: 0,
                bearing_y: 20,
                advance_x: 28,
            })
        })
        .expect("process batch under pin pressure");

    assert_eq!(report.processed_count, 0);
    assert_eq!(report.remaining_in_queue, 2, "requests must be preserved");

    // GPU completes frame 1: unpin slot 1
    atlas.unpin_slot(&ref1).unwrap();

    // Run batch processor again: can now evict slot 1 for the highest priority request (key_pending1)
    let report2 = atlas
        .process_raster_queue_batch(2, 7, |_| {
            Ok(RasterizedGlyph {
                pixel_width: 28,
                pixel_height: 28,
                bearing_x: 0,
                bearing_y: 20,
                advance_x: 28,
            })
        })
        .expect("process batch after partial unpin");

    assert_eq!(report2.processed_count, 1);
    assert_eq!(report2.remaining_in_queue, 1);

    // The newly resident glyph is key_pending1
    assert!(atlas.lookup(&key_pending1.as_borrowed()).is_some());
    assert!(atlas.lookup(&key_pending2.as_borrowed()).is_none());
}

#[test]
fn test_retained_bounded_diagnostic_logs() {
    let owner = test_owner(600);
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 64,
        log_capacity: 4, // Small log capacity to test ring bounded overflow
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 6000, 1, config);

    let key = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);
    let r = atlas
        .allocate_and_insert(key, 10, 10, 0, 8, 10, 100)
        .expect("insert");

    atlas.pin_slot(&r).expect("pin");
    atlas.unpin_slot(&r).expect("unpin");
    atlas.reset_device_generation(2);

    let logs = atlas.log_events();
    assert!(logs.len() <= 4, "log must not exceed configured capacity");
    // Verify last recorded event was DeviceReset
    let last = logs.back().expect("last log event");
    assert_eq!(last.kind, AtlasLogEventKind::DeviceReset);
    assert_eq!(last.detail, 2);
}

#[test]
fn test_negative_controls_and_worker_failure() {
    let owner = test_owner(700);
    let mut atlas = BoundedGlyphAtlas::new(owner, 7000, 1, BoundedAtlasConfig::default());

    let key = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);

    // 1. Worker failure in batch processing surfaces without corrupting state
    atlas
        .query_or_enqueue(key.clone(), RasterPriority::SelectedReadingText, 1, None)
        .unwrap();

    let res = atlas.process_raster_queue_batch(1, 2, |_| {
        Err(GlyphAtlasError::RasterWorkerFailed)
    });
    assert_eq!(res, Err(GlyphAtlasError::RasterWorkerFailed));

    // 2. Invalid dimensions in batch processing
    atlas
        .query_or_enqueue(key.clone(), RasterPriority::SelectedReadingText, 3, None)
        .unwrap();

    let res_dims = atlas.process_raster_queue_batch(1, 4, |_| {
        Ok(RasterizedGlyph {
            pixel_width: 0,
            pixel_height: 10,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 0,
        })
    });
    assert_eq!(res_dims, Err(GlyphAtlasError::InvalidDimensions));

    // 3. Stale fallback slot is safely ignored
    let stale_fallback = GpuGlyphRef {
        owner,
        device_id: 7000,
        device_generation: 999, // Stale generation
        page_id: fcb::AtlasPageId(1),
        slot_id: AtlasSlotId(0),
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

    let query_res = atlas
        .query_or_enqueue(
            sample_key("Font", 2, 14.0, RasterMode::MonochromeCoverage),
            RasterPriority::VisibleReadingText,
            5,
            Some(stale_fallback),
        )
        .unwrap();

    assert!(matches!(query_res, RasterLookupResult::MissPending { .. }));
    if let RasterLookupResult::MissPending { fallback_ref, .. } = query_res {
        assert!(
            fallback_ref.is_none(),
            "stale fallback slot must be rejected safely"
        );
    }
}
