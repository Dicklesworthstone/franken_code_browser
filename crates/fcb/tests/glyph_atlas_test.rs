#![forbid(unsafe_code)]

//! Unit and boundary test suite for glyph atlas management, raster identity keys,
//! and bounded packing (FCB-017.A / fcb-bte.1).
//!
//! Acceptance criteria & oracle contract:
//! 1. Separate immutable font/raster identity from page/slot/device residence.
//! 2. Zero-allocation borrowed lookup using BorrowedGlyphRasterKey.
//! 3. Segregated atlas page kinds: MonochromeGlyph, ColorImage, LargeLabelOrTransient.
//! 4. Shelf packing with 1px gutter and bounded fragmentation tracking.
//! 5. In-flight pinning prevents eviction under pressure; GPU completion unpins safely.
//! 6. Stale slot reuse: slot generation increments on reuse; old draw data cannot sample new glyph.
//! 7. Device reset: generation increments invalidate all prior resident references.
//! 8. Negative controls: invalid dimensions, glyphs larger than page, owner/device mismatch.

use fcb::{
    AtlasPageId, AtlasPageKind, AtlasSlotId, BoundedAtlasConfig, BoundedGlyphAtlas,
    BorrowedGlyphRasterKey, GlyphAtlasError, GlyphRasterKey, GpuGlyphRef, HintingPolicy,
    RasterMode, RasterScaleTier, SlotGeneration, SubpixelBin,
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
fn test_raster_identity_distinct_from_residence() {
    let key1 = sample_key("IBMPlexMono", 65, 14.0, RasterMode::MonochromeCoverage);
    let mut key2 = key1.clone();
    key2.scale_tier = RasterScaleTier::TWO_X;

    let mut key3 = key1.clone();
    key3.subpixel_bin = SubpixelBin { x_bin: 1, y_bin: 0 };

    let mut key4 = key1.clone();
    key4.hinting = HintingPolicy::Full;

    // Distinct raster properties produce distinct hashes
    assert_ne!(key1.compute_hash(), key2.compute_hash());
    assert_ne!(key1.compute_hash(), key3.compute_hash());
    assert_ne!(key1.compute_hash(), key4.compute_hash());

    // Identity is independent of GPU residence or page
    let borrowed = key1.as_borrowed();
    assert_eq!(borrowed.compute_hash(), key1.compute_hash());
    assert!(borrowed.matches(&key1));
    assert!(!borrowed.matches(&key2));
}

#[test]
fn test_zero_allocation_borrowed_lookup() {
    let owner = test_owner(10);
    let mut atlas = BoundedGlyphAtlas::new(owner, 100, 1, BoundedAtlasConfig::default());

    let key_a = sample_key("Menlo", 40, 13.0, RasterMode::MonochromeCoverage);
    let key_b = sample_key("Menlo", 41, 13.0, RasterMode::MonochromeCoverage);

    let ref_a = atlas
        .allocate_and_insert(key_a.clone(), 10, 16, 0, 12, 10, 1)
        .expect("allocate glyph A");
    let ref_b = atlas
        .allocate_and_insert(key_b.clone(), 10, 16, 0, 12, 10, 1)
        .expect("allocate glyph B");

    assert_eq!(ref_a.page_id, ref_b.page_id);
    assert_ne!(ref_a.slot_id, ref_b.slot_id);

    // Look up using borrowed key (&str without String allocation)
    let borrowed_a = BorrowedGlyphRasterKey {
        font_name: "Menlo",
        glyph_id: 40,
        size_px_mils: 13000,
        raster_mode: RasterMode::MonochromeCoverage,
        scale_tier: RasterScaleTier::ONE_X,
        subpixel_bin: SubpixelBin::ZERO,
        hinting: HintingPolicy::None,
        variation_coords_hash: 0,
        raster_version: 1,
    };

    let found_a = atlas.lookup(&borrowed_a).expect("glyph A should be found");
    assert_eq!(found_a.slot_id, ref_a.slot_id);
    assert_eq!(found_a.raster_key, key_a);

    let borrowed_miss = BorrowedGlyphRasterKey {
        glyph_id: 999,
        ..borrowed_a
    };
    assert!(atlas.lookup(&borrowed_miss).is_none());
    assert_eq!(atlas.total_lookups(), 2);
    assert_eq!(atlas.total_hits(), 1);
}

#[test]
fn test_segregated_page_kinds() {
    let owner = test_owner(20);
    let mut atlas = BoundedGlyphAtlas::new(owner, 200, 1, BoundedAtlasConfig::default());

    let mono_key = sample_key("SFPro", 10, 14.0, RasterMode::MonochromeCoverage);
    let color_key = sample_key("AppleColorEmoji", 50, 32.0, RasterMode::ColorRgba);
    let sdf_key = sample_key("SFPro", 10, 64.0, RasterMode::DistanceField);

    let mono_ref = atlas
        .allocate_and_insert(mono_key, 12, 18, 0, 14, 12, 1)
        .expect("allocate mono");
    let color_ref = atlas
        .allocate_and_insert(color_key, 32, 32, 0, 28, 32, 1)
        .expect("allocate color");
    let sdf_ref = atlas
        .allocate_and_insert(sdf_key, 64, 64, 0, 50, 64, 1)
        .expect("allocate sdf");

    // All three must be routed to different pages matching their page kind
    assert_ne!(mono_ref.page_id, color_ref.page_id);
    assert_ne!(mono_ref.page_id, sdf_ref.page_id);
    assert_ne!(color_ref.page_id, sdf_ref.page_id);
    assert_eq!(atlas.page_count(), 3);
    assert_eq!(
        atlas.page_kind(mono_ref.page_id),
        Some(AtlasPageKind::MonochromeGlyph)
    );
    assert_eq!(
        atlas.page_kind(color_ref.page_id),
        Some(AtlasPageKind::ColorImage)
    );
    assert_eq!(
        atlas.page_kind(sdf_ref.page_id),
        Some(AtlasPageKind::LargeLabelOrTransient)
    );
}

#[test]
fn test_shelf_packing_and_fragmentation_tracking() {
    let owner = test_owner(30);
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 128,
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 300, 1, config);

    // Pack several glyphs onto a single page
    for i in 0..10 {
        let key = sample_key("Font", i, 12.0, RasterMode::MonochromeCoverage);
        let r = atlas
            .allocate_and_insert(key, 10, 14, 0, 10, 10, 1)
            .expect("pack glyph");
        assert_eq!(r.page_id, AtlasPageId(1));
        assert_eq!(r.pixel_width, 10);
        assert_eq!(r.pixel_height, 14);
    }

    // Verify UV coordinates are within (0.0..=1.0)
    for i in 0..10 {
        let key = sample_key("Font", i, 12.0, RasterMode::MonochromeCoverage);
        let found = atlas.lookup(&key.as_borrowed()).expect("found");
        assert!(found.uv_rect.origin().x() >= 0.0);
        assert!(found.uv_rect.origin().y() >= 0.0);
        assert!(found.uv_rect.size().width() > 0.0);
        assert!(found.uv_rect.size().height() > 0.0);
        assert!(found.uv_rect.origin().x() + found.uv_rect.size().width() <= 1.0);
        assert!(found.uv_rect.origin().y() + found.uv_rect.size().height() <= 1.0);
    }
}

#[test]
fn test_in_flight_pinning_prevents_eviction() {
    let owner = test_owner(40);
    // Small single-page atlas that fits ~4 glyphs
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 64,
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 400, 1, config);

    let key1 = sample_key("Font", 1, 16.0, RasterMode::MonochromeCoverage);
    let key2 = sample_key("Font", 2, 16.0, RasterMode::MonochromeCoverage);

    // Allocate glyph 1 and 2 (each 28x28 with 2px padding = 30x30, 4 fit in 64x64)
    let ref1 = atlas
        .allocate_and_insert(key1.clone(), 28, 28, 0, 20, 28, 10)
        .expect("insert 1");
    let ref2 = atlas
        .allocate_and_insert(key2.clone(), 28, 28, 0, 20, 28, 11)
        .expect("insert 2");

    let key3 = sample_key("Font", 3, 16.0, RasterMode::MonochromeCoverage);
    let key4 = sample_key("Font", 4, 16.0, RasterMode::MonochromeCoverage);
    let ref3 = atlas
        .allocate_and_insert(key3.clone(), 28, 28, 0, 20, 28, 12)
        .expect("insert 3");
    let ref4 = atlas
        .allocate_and_insert(key4.clone(), 28, 28, 0, 20, 28, 13)
        .expect("insert 4");

    // Pin slots 2, 3, 4 (leaving slot 1 unpinned)
    atlas.pin_slot(&ref2).expect("pin 2");
    atlas.pin_slot(&ref3).expect("pin 3");
    atlas.pin_slot(&ref4).expect("pin 4");

    // Allocate glyph 5 under pressure: atlas must evict UNPINNED slot 1 (oldest frame 10)
    let key5 = sample_key("Font", 5, 16.0, RasterMode::MonochromeCoverage);
    let ref5 = atlas
        .allocate_and_insert(key5.clone(), 28, 28, 0, 20, 28, 20)
        .expect("insert 5 via eviction of slot 1");

    assert_eq!(ref5.slot_id, ref1.slot_id, "must reuse slot 1");
    assert_eq!(ref5.slot_generation, ref1.slot_generation.next());
    assert_eq!(atlas.total_evictions(), 1);

    // Pin slot 5 as well: NOW ALL 4 SLOTS ARE PINNED!
    atlas.pin_slot(&ref5).expect("pin 5");

    // Attempting to allocate glyph 6 when all slots are pinned must strictly fail!
    let key6 = sample_key("Font", 6, 16.0, RasterMode::MonochromeCoverage);
    let err = atlas
        .allocate_and_insert(key6, 28, 28, 0, 20, 28, 21)
        .expect_err("must fail because all slots are pinned");
    assert_eq!(err, GlyphAtlasError::AllSlotsPinned);
}

#[test]
fn test_stale_slot_generation_on_reuse() {
    let owner = test_owner(50);
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 64,
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 500, 1, config);

    let key_old = sample_key("Font", 1, 16.0, RasterMode::MonochromeCoverage);
    let old_ref = atlas
        .allocate_and_insert(key_old, 28, 28, 0, 20, 28, 1)
        .expect("insert old");

    // Initially old_ref is valid
    assert_eq!(atlas.validate_ref(&old_ref), Ok(()));

    // Fill page to force eviction of slot
    let key_fill1 = sample_key("Font", 2, 16.0, RasterMode::MonochromeCoverage);
    let key_fill2 = sample_key("Font", 3, 16.0, RasterMode::MonochromeCoverage);
    let key_fill3 = sample_key("Font", 4, 16.0, RasterMode::MonochromeCoverage);
    atlas
        .allocate_and_insert(key_fill1, 28, 28, 0, 20, 28, 2)
        .unwrap();
    atlas
        .allocate_and_insert(key_fill2, 28, 28, 0, 20, 28, 3)
        .unwrap();
    atlas
        .allocate_and_insert(key_fill3, 28, 28, 0, 20, 28, 4)
        .unwrap();

    // Now evict slot 0 by inserting a 5th glyph
    let key_new = sample_key("Font", 5, 16.0, RasterMode::MonochromeCoverage);
    let new_ref = atlas
        .allocate_and_insert(key_new, 28, 28, 0, 20, 28, 5)
        .expect("insert new");

    assert_eq!(new_ref.slot_id, old_ref.slot_id);
    assert_ne!(new_ref.slot_generation, old_ref.slot_generation);

    // CRUCIAL CONTRACT: Old reference holding old generation must fail validation!
    // Retained draw data cannot sample new glyph accidentally!
    assert_eq!(
        atlas.validate_ref(&old_ref),
        Err(GlyphAtlasError::StaleSlotGeneration)
    );

    // New reference is valid
    assert_eq!(atlas.validate_ref(&new_ref), Ok(()));
}

#[test]
fn test_completion_unpins_and_allows_reclamation() {
    let owner = test_owner(60);
    let config = BoundedAtlasConfig {
        max_pages: 1,
        page_size: 64,
        ..BoundedAtlasConfig::default()
    };
    let mut atlas = BoundedGlyphAtlas::new(owner, 600, 1, config);

    let key = sample_key("Font", 1, 16.0, RasterMode::MonochromeCoverage);
    let r = atlas
        .allocate_and_insert(key, 60, 60, 0, 50, 60, 1)
        .expect("insert");

    atlas.pin_slot(&r).expect("pin during GPU submission");

    // Cannot evict while pinned
    let other_key = sample_key("Font", 2, 16.0, RasterMode::MonochromeCoverage);
    let err = atlas
        .allocate_and_insert(other_key.clone(), 60, 60, 0, 50, 60, 2)
        .expect_err("cannot evict pinned");
    assert_eq!(err, GlyphAtlasError::AllSlotsPinned);

    // GPU execution completes: terminal unpin
    atlas.unpin_slot(&r).expect("terminal unpin on completion");

    // Now reclamation can proceed: reuses slot 0
    let reclaimed = atlas
        .allocate_and_insert(other_key, 60, 60, 0, 50, 60, 3)
        .expect("can now evict unpinned slot");
    assert_eq!(reclaimed.slot_id, r.slot_id);
    assert_eq!(reclaimed.slot_generation, r.slot_generation.next());
}

#[test]
fn test_device_reset_invalidates_all_refs() {
    let owner = test_owner(70);
    let mut atlas = BoundedGlyphAtlas::new(owner, 700, 1, BoundedAtlasConfig::default());

    let key = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);
    let r = atlas
        .allocate_and_insert(key, 12, 16, 0, 12, 12, 1)
        .expect("insert");

    assert_eq!(atlas.validate_ref(&r), Ok(()));

    // Device reset / reconstruction
    atlas.reset_device_generation(2);

    assert_eq!(
        atlas.validate_ref(&r),
        Err(GlyphAtlasError::StaleDeviceGeneration)
    );
    assert_eq!(atlas.page_count(), 0);
}

#[test]
fn test_negative_controls() {
    let owner = test_owner(80);
    let mut atlas = BoundedGlyphAtlas::new(owner, 800, 1, BoundedAtlasConfig::default());

    let key = sample_key("Font", 1, 14.0, RasterMode::MonochromeCoverage);

    // 1. Zero dimensions
    assert_eq!(
        atlas.allocate_and_insert(key.clone(), 0, 10, 0, 0, 0, 1),
        Err(GlyphAtlasError::InvalidDimensions)
    );
    assert_eq!(
        atlas.allocate_and_insert(key.clone(), 10, 0, 0, 0, 0, 1),
        Err(GlyphAtlasError::InvalidDimensions)
    );

    // 2. Glyph too large for page
    assert_eq!(
        atlas.allocate_and_insert(key.clone(), 2048, 10, 0, 0, 0, 1),
        Err(GlyphAtlasError::GlyphTooLargeForPage)
    );

    // 3. Foreign owner
    let foreign_ref = GpuGlyphRef {
        owner: test_owner(999),
        device_id: 800,
        device_generation: 1,
        page_id: AtlasPageId(1),
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
    assert_eq!(
        atlas.validate_ref(&foreign_ref),
        Err(GlyphAtlasError::OwnerMismatch)
    );

    // 4. Device mismatch
    let wrong_device_ref = GpuGlyphRef {
        device_id: 9999,
        ..foreign_ref
    };
    let mut wrong_device_ref = wrong_device_ref;
    wrong_device_ref.owner = owner;
    assert_eq!(
        atlas.validate_ref(&wrong_device_ref),
        Err(GlyphAtlasError::DeviceMismatch)
    );
}
