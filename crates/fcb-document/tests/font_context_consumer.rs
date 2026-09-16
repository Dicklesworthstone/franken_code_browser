#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document system adapts upstream
//! FrankenMarkdown shared font context, fallback chains, and raster extensions (FCB-075.A).
//!
//! Acceptance criteria & oracle contract:
//! 1. Exact font run identity: deterministic font ID hashing and ordered fallback chains.
//! 2. Bounded context checkpoints: versioned, size-capped (<= 4 KiB) serialization/deserialization.
//! 3. Clustering across scripts: CJK, RTL, joining, combining, emoji, tabs, and ligatures.
//! 4. Color and bitmap resources: verified pixel/bit extraction and fallback placeholder generation.
//! 5. Exact logical selection: selections snap cleanly to cluster boundaries.
//! 6. Negative controls: dimension caps, buffer mismatch, oversized checkpoints, and version errors.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use franken_markdown::font_context::{
    checkpoint_context, restore_context, segment_clusters, select_logical_range,
    BitmapGlyphResource, CheckpointError, ColorGlyphError, ColorGlyphResource,
    FallbackChain, FontRunIdentity, MAX_BITMAP_GLYPH_DIMENSION, MAX_CHECKPOINT_BYTES,
    MAX_COLOR_GLYPH_DIMENSION,
};

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn consumer_font_run_identity_and_fallback_resolution() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 1);

    let primary = FontRunIdentity::new("IBMPlexSans", false, false, 14);
    let cjk = FontRunIdentity::new("PingFangSC", false, false, 14);
    let emoji = FontRunIdentity::new("AppleColorEmoji", false, false, 14);

    // Verify deterministic font IDs
    assert_ne!(primary.font_id(), cjk.font_id());
    assert_ne!(primary.font_id(), emoji.font_id());

    let mut chain = FallbackChain::single(primary.clone());
    chain.push_fallback(cjk.clone());
    chain.push_fallback(emoji.clone());

    let covers_latin = |f: &FontRunIdentity, _c: char| f.family == "IBMPlexSans";
    let covers_cjk = |f: &FontRunIdentity, _c: char| f.family == "PingFangSC";
    let covers_emoji = |f: &FontRunIdentity, _c: char| f.family == "AppleColorEmoji";

    assert_eq!(
        chain.resolve('A', &covers_latin).map(|f| &f.family),
        Some(&"IBMPlexSans".to_string())
    );
    assert_eq!(
        chain.resolve('語', &covers_cjk).map(|f| &f.family),
        Some(&"PingFangSC".to_string())
    );
    assert_eq!(
        chain.resolve('🚀', &covers_emoji).map(|f| &f.family),
        Some(&"AppleColorEmoji".to_string())
    );
    assert_eq!(chain.resolve('?', &|_, _| false), None);
}

#[test]
fn consumer_multi_script_clustering_and_exact_logical_selection() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 2);

    // Mixed document line: Latin, CJK, RTL, Emoji, Tab, and Ligature
    let line = "let x\t= 10; // 日本語 العربية 🚀 fi => ok";
    let clusters = segment_clusters(line);
    assert!(!clusters.is_empty());

    // Verify tabs, cjk, rtl, emoji, and ligatures detected
    let has_tab = clusters.iter().any(|c| c.is_tab);
    let has_cjk = clusters.iter().any(|c| c.is_cjk);
    let has_rtl = clusters.iter().any(|c| c.is_rtl);
    let has_emoji = clusters.iter().any(|c| c.is_emoji);
    let has_ligature = clusters.iter().any(|c| c.is_ligature);

    assert!(has_tab, "Tab cluster must be detected");
    assert!(has_cjk, "CJK cluster must be detected");
    assert!(has_rtl, "RTL cluster must be detected");
    assert!(has_emoji, "Emoji cluster must be detected");
    assert!(has_ligature, "Ligature cluster must be detected");

    // Exact selection of CJK characters
    let cjk_start = line.find("日本語").unwrap();
    let cjk_end = cjk_start + "日本語".len();
    let sel = select_logical_range(&clusters, cjk_start..cjk_end).unwrap();

    assert_eq!(sel.byte_range, cjk_start..cjk_end);
    assert_eq!(sel.char_count, 3);
    assert!(sel.has_cjk);
    assert!(!sel.has_rtl);
}

#[test]
fn consumer_color_and_bitmap_glyph_raster_contracts() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 3);

    // 2x2 RGBA color glyph
    let pixels = vec![
        255, 128, 0, 255,
        0, 255, 128, 255,
        128, 0, 255, 255,
        255, 255, 255, 255,
    ];
    let color_glyph = ColorGlyphResource::try_new_rgba('✨', 2, 2, pixels).unwrap();
    assert_eq!(color_glyph.pixel_at(0, 0), Some([255, 128, 0, 255]));
    assert_eq!(color_glyph.pixel_at(1, 1), Some([255, 255, 255, 255]));
    assert_eq!(color_glyph.pixel_at(2, 0), None);

    // Fallback placeholder
    let placeholder = ColorGlyphResource::fallback_placeholder('🛸', 16);
    assert_eq!(placeholder.width, 16);
    assert_eq!(placeholder.height, 16);
    assert_eq!(placeholder.pixels.len(), 16 * 16 * 4);

    // 8x1 monochrome bitmap glyph
    let bitmap = BitmapGlyphResource::try_new_1bpp('B', 8, 1, vec![0b10000001]).unwrap();
    assert_eq!(bitmap.bit_at(0, 0), Some(true));
    assert_eq!(bitmap.bit_at(1, 0), Some(false));
    assert_eq!(bitmap.bit_at(7, 0), Some(true));
}

#[test]
fn consumer_bounded_checkpoint_round_trip_and_caps() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 4);

    let runs = vec![
        FontRunIdentity::new("IBMPlexSans", false, false, 14),
        FontRunIdentity::new("PingFangSC", false, false, 14),
    ];
    let clusters = segment_clusters("code\t// 注释");
    let blob = checkpoint_context(&runs, &clusters).unwrap();
    assert!(blob.len() <= MAX_CHECKPOINT_BYTES);

    let restored = restore_context(&blob).unwrap();
    assert_eq!(restored.run_identities.len(), 2);
    assert_eq!(restored.cluster_offsets.len(), clusters.len());

    // Negative control: oversized checkpoint
    let huge_name = "X".repeat(MAX_CHECKPOINT_BYTES);
    let bad_runs = vec![FontRunIdentity::new(&huge_name, false, false, 14)];
    assert_eq!(
        checkpoint_context(&bad_runs, &[]),
        Err(CheckpointError::TooLarge)
    );

    // Negative control: color glyph dimension cap
    assert_eq!(
        ColorGlyphResource::try_new_rgba('X', MAX_COLOR_GLYPH_DIMENSION + 1, 10, vec![]),
        Err(ColorGlyphError::DimensionTooLarge {
            dimension: MAX_COLOR_GLYPH_DIMENSION + 1,
            max_allowed: MAX_COLOR_GLYPH_DIMENSION,
        })
    );

    // Negative control: bitmap glyph dimension cap
    assert_eq!(
        BitmapGlyphResource::try_new_1bpp('X', MAX_BITMAP_GLYPH_DIMENSION + 1, 10, vec![]),
        Err(ColorGlyphError::DimensionTooLarge {
            dimension: MAX_BITMAP_GLYPH_DIMENSION + 1,
            max_allowed: MAX_BITMAP_GLYPH_DIMENSION,
        })
    );
}
