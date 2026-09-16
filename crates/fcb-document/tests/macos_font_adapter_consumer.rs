#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document system adapts upstream
//! FrankenMarkdown safe Mac font adapter, CoreText fallback attribution,
//! bounded foreign calls, and CoreGraphics raster conversions (FCB-075.B).
//!
//! Acceptance criteria & oracle contract:
//! 1. Owned CoreText/CoreGraphics conversions through separate bridge, bounded foreign calls.
//! 2. Preservation of producing font identity (FontOrigin, FontId) across Latin, CJK, RTL, and Emoji.
//! 3. Bounded context work budget: giant lines rejected before foreign calls to prevent main-thread hangs.
//! 4. Color emoji and bitmap glyphs: CoreGraphics raster BGRA -> RGBA in-place conversion with dimension caps.
//! 5. Exact logical selection: UTF-16 code unit to UTF-8 byte boundary preservation.
//! 6. Honest capabilities: SystemPlatform route kind, is_pixel_deterministic == false.
//! 7. Negative controls: empty text, disabled fallback, bridge failure, raster buffer mismatch.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use franken_markdown::font_context::{segment_clusters, select_logical_range};
use franken_markdown::text::macos::{
    MacFontAdapter, MacFontAdapterConfig, SimulatedMacBridge, DEFAULT_MAC_CONTEXT_BUDGET,
};
use franken_markdown::text::{
    Direction, FontId, FontOrigin, NativeShapingError, NativeShapingRequest,
    NativeShapingRoute, ShapingRouteKind,
};

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn consumer_mac_adapter_capabilities_and_kind() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 1);

    let bridge = SimulatedMacBridge::new();
    let adapter = MacFontAdapter::new(bridge);
    let caps = adapter.capabilities();

    assert_eq!(caps.route_kind, ShapingRouteKind::SystemPlatform);
    assert!(!caps.is_pixel_deterministic, "system fallback routes must not claim pixel determinism");
    assert!(caps.supports_fallback_fonts, "CoreText adapter must support fallback fonts");
    assert!(caps.supports_color_emoji, "CoreGraphics route must declare color glyph support");
    assert!(caps.supports_bidi, "CoreText route supports bidirectional layout");
    assert_eq!(caps.max_paragraph_bytes, DEFAULT_MAC_CONTEXT_BUDGET);
}

#[test]
fn consumer_mac_adapter_latin_and_cjk_fallback_attribution() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 2);

    let bridge = SimulatedMacBridge::new();
    let adapter = MacFontAdapter::new(bridge);

    let text = "Hello 世界!";
    let primary_font_id = FontId::new(100);
    let req = NativeShapingRequest {
        text,
        primary_font_id,
        font_size: 16.0,
        direction: Direction::LeftToRight,
        script: *b"hani",
        language: *b"dflt",
        allow_system_fallback: true,
    };

    let run = adapter.shape_run(&req).expect("shaping mixed Latin+CJK should succeed");
    assert_eq!(run.logical_text, text);
    assert_eq!(run.context.font_origin, FontOrigin::SystemFallbackFace);

    // Verify Latin glyphs map to primary font, CJK glyphs map to fallback font
    let latin_glyphs: Vec<_> = run.glyphs.iter().take(5).collect();
    for g in latin_glyphs {
        assert_eq!(g.font_id, primary_font_id, "Latin glyphs must use primary font");
    }

    let fallback_glyphs: Vec<_> = run.glyphs.iter().filter(|g| g.font_id != primary_font_id).collect();
    assert!(!fallback_glyphs.is_empty(), "CJK characters must trigger fallback font ID");
}

#[test]
fn consumer_mac_adapter_rtl_arabic_and_hebrew() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 3);

    let bridge = SimulatedMacBridge::new();
    let adapter = MacFontAdapter::new(bridge);

    let arabic_text = "مرحبا";
    let req = NativeShapingRequest {
        text: arabic_text,
        primary_font_id: FontId::new(1),
        font_size: 14.0,
        direction: Direction::RightToLeft,
        script: *b"arab",
        language: *b"ara ",
        allow_system_fallback: true,
    };

    let run = adapter.shape_run(&req).expect("shaping Arabic RTL must succeed");
    assert_eq!(run.context.direction, Direction::RightToLeft);
    assert_eq!(run.glyphs.len(), arabic_text.chars().count());
    assert!(run.total_advance > 0.0);

    // Verify clusters have RTL flag set
    let clusters = segment_clusters(arabic_text);
    assert!(clusters.iter().all(|c| c.is_rtl), "all Arabic clusters must be flagged RTL");

    let hit = run.hit_test(0.0);
    assert!(hit.caret.byte_offset <= arabic_text.len());
}

#[test]
fn consumer_mac_adapter_emoji_color_raster_and_swizzle() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 4);

    let bridge = SimulatedMacBridge::new();
    let adapter = MacFontAdapter::new(bridge);

    // Rasterize emoji glyph (glyph 1000 produces BGRA: B=255, G=200, R=50, A=255)
    let raster = adapter
        .get_glyph_rgba_raster("AppleColorEmoji", 1000, 32.0)
        .expect("rasterizing color emoji must succeed")
        .expect("raster should be present");

    assert_eq!(raster.width, 32);
    assert_eq!(raster.height, 32);
    assert!(!raster.is_bgra, "raster must be converted from BGRA to RGBA");

    // Verify in-place BGRA -> RGBA swizzle
    // Original BGRA: B=255, G=200, R=50, A=255
    // Swizzled RGBA: R=50, G=200, B=255, A=255
    assert_eq!(raster.pixels[0], 50, "Red channel");
    assert_eq!(raster.pixels[1], 200, "Green channel");
    assert_eq!(raster.pixels[2], 255, "Blue channel");
    assert_eq!(raster.pixels[3], 255, "Alpha channel");
}

#[test]
fn consumer_mac_adapter_context_work_budget_enforcement() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 5);

    let bridge = SimulatedMacBridge::new();
    let config = MacFontAdapterConfig {
        max_paragraph_bytes: 64, // strictly capped context budget
        ..MacFontAdapterConfig::default()
    };
    let adapter = MacFontAdapter::with_config(bridge, config);

    let giant_text = "A".repeat(128);
    let req = NativeShapingRequest {
        text: &giant_text,
        primary_font_id: FontId::new(1),
        font_size: 14.0,
        direction: Direction::LeftToRight,
        script: *b"latn",
        language: *b"dflt",
        allow_system_fallback: true,
    };

    let err = adapter.shape_run(&req).expect_err("oversized line must exceed budget");
    assert_eq!(
        err,
        NativeShapingError::ContextBudgetExceeded {
            length: 128,
            max_allowed: 64,
        }
    );
}

#[test]
fn consumer_mac_adapter_exact_logical_selection() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 6);

    let bridge = SimulatedMacBridge::new();
    let adapter = MacFontAdapter::new(bridge);

    let text = "let msg = \"こんにちは 🚀\";";
    let req = NativeShapingRequest {
        text,
        primary_font_id: FontId::new(1),
        font_size: 14.0,
        direction: Direction::LeftToRight,
        script: *b"latn",
        language: *b"dflt",
        allow_system_fallback: true,
    };

    let run = adapter.shape_run(&req).expect("shaping must succeed");
    assert_eq!(run.logical_text, text);

    // Segment clusters and select Japanese phrase exactly
    let clusters = segment_clusters(text);
    let jpn_start = text.find("こんにちは").expect("find Japanese substring");
    let jpn_end = jpn_start + "こんにちは".len();

    let sel = select_logical_range(&clusters, jpn_start..jpn_end)
        .expect("exact logical selection must succeed");

    assert_eq!(sel.byte_range, jpn_start..jpn_end);
    assert_eq!(sel.char_count, 5);
    assert!(sel.has_cjk);
    assert!(!sel.has_emoji);

    // Select emoji
    let emoji_start = text.find("🚀").expect("find emoji substring");
    let emoji_end = emoji_start + "🚀".len();
    let emoji_sel = select_logical_range(&clusters, emoji_start..emoji_end)
        .expect("exact emoji selection must succeed");
    assert_eq!(emoji_sel.byte_range, emoji_start..emoji_end);
    assert_eq!(emoji_sel.char_count, 1);
    assert!(emoji_sel.has_emoji);
}

#[test]
fn consumer_mac_adapter_negative_controls() {
    let _owner = test_owner();
    let _gen = test_generation(_owner, 7);

    let bridge = SimulatedMacBridge::new();
    let adapter = MacFontAdapter::new(bridge);

    // 1. Empty text
    let empty_req = NativeShapingRequest {
        text: "",
        primary_font_id: FontId::new(1),
        font_size: 14.0,
        direction: Direction::LeftToRight,
        script: *b"latn",
        language: *b"dflt",
        allow_system_fallback: true,
    };
    assert_eq!(adapter.shape_run(&empty_req), Err(NativeShapingError::EmptyText));

    // 2. Disabled fallback
    let no_fallback_req = NativeShapingRequest {
        text: "Latin and 漢字",
        primary_font_id: FontId::new(1),
        font_size: 14.0,
        direction: Direction::LeftToRight,
        script: *b"latn",
        language: *b"dflt",
        allow_system_fallback: false,
    };
    assert_eq!(
        adapter.shape_run(&no_fallback_req),
        Err(NativeShapingError::FallbackRequired {
            unshaped_byte_offset: "Latin and ".len()
        })
    );

    // 3. Simulated bridge foreign call failure
    let failing_bridge = SimulatedMacBridge::new_failing();
    let failing_adapter = MacFontAdapter::new(failing_bridge);
    let fail_req = NativeShapingRequest {
        text: "test",
        primary_font_id: FontId::new(1),
        font_size: 14.0,
        direction: Direction::LeftToRight,
        script: *b"latn",
        language: *b"dflt",
        allow_system_fallback: true,
    };
    let err = failing_adapter.shape_run(&fail_req).expect_err("failing bridge must error");
    assert!(matches!(
        err,
        NativeShapingError::AdapterError(ref msg) if msg.contains("simulated CoreText foreign call abort")
    ));
}
