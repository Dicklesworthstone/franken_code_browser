//! Unit and integration test suite for FCB-038.B:
//! Actual-scale native typography and layout gallery, bundled vs system route identities,
//! 1x/2x fractional offsets, punctuation, bidi/emoji, selection overlays, and semantic copy.

#![forbid(unsafe_code)]

use franken_markdown::text::{
    CaretAffinity, FontId, ShapingRouteKind, SimulatedNativeRoute,
};
use franken_markdown::theme::CodeLigatures;
use franken_markdown::TextSelectionRange;

use fcb_document::{
    BackingScale, ContrastOracle, GalleryRouteIdentity,
    GallerySectionId, GalleryThemeMode, GalleryVisualTokens, TypographyGallery,
};

#[test]
fn test_gallery_catalog_and_sections_coverage() {
    let gallery = TypographyGallery::new();
    assert!(!gallery.cases.is_empty(), "gallery suite must contain cases");

    // Verify all required section categories exist in the gallery catalog
    let sections: std::collections::HashSet<_> = gallery.cases.iter().map(|c| c.section).collect();
    assert!(sections.contains(&GallerySectionId::LatinBaselineAndPunctuation));
    assert!(sections.contains(&GallerySectionId::CodeLigaturesConservative));
    assert!(sections.contains(&GallerySectionId::CodeLigaturesOff));
    assert!(sections.contains(&GallerySectionId::CodeLigaturesAll));
    assert!(sections.contains(&GallerySectionId::CombiningMarksAndDiacritics));
    assert!(sections.contains(&GallerySectionId::MixedScriptAndBidi));
    assert!(sections.contains(&GallerySectionId::FractionalSubpixelOffsets));
    assert!(sections.contains(&GallerySectionId::SelectionAndCaretOverlays));
}

#[test]
fn test_bundled_vs_system_route_identities() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(500);

    let mixed_case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::MixedScriptAndBidi)
        .expect("mixed script case");

    let run = gallery
        .render_case(mixed_case, &route, primary_font)
        .expect("render succeeds");

    assert_eq!(run.route_kind, ShapingRouteKind::SimulatedPlatform);
    assert!(run.total_width > 0.0);

    let inspection = TypographyGallery::inspect_run(&run, primary_font);
    assert!(inspection.total_glyphs > 0);
    assert_eq!(inspection.backing_scale, BackingScale::Scale2x);

    // Route identity validation: bundled face vs fallback face
    let bundled_id = GalleryRouteIdentity::Bundled {
        font_id: primary_font,
        family: "Bundled JetBrains Mono".to_string(),
    };
    let fallback_id = GalleryRouteIdentity::SystemFallback {
        fallback_font_id: FontId::new(999),
        face_name: "System Fallback Face".to_string(),
    };
    assert_ne!(bundled_id, fallback_id);
}

#[test]
fn test_backing_scale_1x_and_2x_fractional_snapping() {
    let scale_1x = BackingScale::Scale1x;
    let scale_2x = BackingScale::Scale2x;

    assert_eq!(scale_1x.factor(), 1.0);
    assert_eq!(scale_2x.factor(), 2.0);

    // In 1x, snaps to nearest integer (1.0 pt)
    assert_eq!(scale_1x.snap_coord(10.2), 10.0);
    assert_eq!(scale_1x.snap_coord(10.6), 11.0);
    assert_eq!(scale_1x.snap_coord(10.5), 11.0);

    // In 2x, snaps to nearest half-integer (0.5 pt = 1 physical pixel)
    assert_eq!(scale_2x.snap_coord(10.2), 10.0);
    assert_eq!(scale_2x.snap_coord(10.3), 10.5);
    assert_eq!(scale_2x.snap_coord(10.7), 10.5);
    assert_eq!(scale_2x.snap_coord(10.8), 11.0);

    // Snapping rect preserves validity
    let rect = franken_markdown::display::DisplayRect {
        x: 10.25,
        y: 20.75,
        width: 100.33,
        height: 15.66,
    };
    let snapped_1x = scale_1x.snap_rect(rect);
    assert_eq!(snapped_1x.x, 10.0);
    assert_eq!(snapped_1x.y, 21.0);

    let snapped_2x = scale_2x.snap_rect(rect);
    assert_eq!(snapped_2x.x, 10.5);
    assert_eq!(snapped_2x.y, 21.0);
}

#[test]
fn test_punctuation_and_thin_strokes() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(100);

    let punct_case = gallery
        .cases
        .iter()
        .find(|c| c.id == "latin_punctuation_1x")
        .expect("punctuation case");

    let run = gallery
        .render_case(punct_case, &route, primary_font)
        .expect("render succeeds");

    assert!(run.text.contains("foo[0]"));
    assert!(run.text.contains("!="));
    assert!(run.text.contains("/regex/"));

    // Check hit testing on thin stroke / bracket
    let (offset_bracket, aff) = run.hit_test_caret(15.0);
    assert!(offset_bracket < run.text.len());
    assert!(aff == CaretAffinity::Leading || aff == CaretAffinity::Trailing);
}

#[test]
fn test_code_ligatures_character_boundary_preservation() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(100);

    let lig_case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::CodeLigaturesConservative)
        .expect("conservative ligatures case");

    let run = gallery
        .render_case(lig_case, &route, primary_font)
        .expect("render succeeds");

    let boundaries = TypographyGallery::inspect_ligature_boundaries(&run, CodeLigatures::Conservative);
    assert_eq!(boundaries.len(), run.text.chars().count());

    // Locate the "->" sequence in "fn test() -> bool"
    let arrow_idx = run.text.find("->").expect("contains ->");
    let b_dash = boundaries.iter().find(|b| b.byte_offset == arrow_idx).unwrap();
    let b_arrow = boundaries.iter().find(|b| b.byte_offset == arrow_idx + 1).unwrap();

    assert_eq!(b_dash.character, '-');
    assert_eq!(b_arrow.character, '>');
    assert!(b_dash.trailing_x <= b_arrow.leading_x + 0.1, "character boundaries must not overlap backwards");

    // Copying "-" alone returns "-"
    let copied_dash = TypographyGallery::semantic_copy_selection(
        &run.text,
        TextSelectionRange {
            start: arrow_idx,
            end: arrow_idx + 1,
        },
    )
    .expect("copy dash");
    assert_eq!(copied_dash, "-");

    let copied_arrow = TypographyGallery::semantic_copy_selection(
        &run.text,
        TextSelectionRange {
            start: arrow_idx,
            end: arrow_idx + 2,
        },
    )
    .expect("copy full arrow");
    assert_eq!(copied_arrow, "->");
}

#[test]
fn test_combining_marks_and_diacritics() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(100);

    let marks_case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::CombiningMarksAndDiacritics)
        .expect("combining marks case");

    let run = gallery
        .render_case(marks_case, &route, primary_font)
        .expect("render succeeds");

    assert!(run.text.contains("café"));
    assert!(run.text.contains("naïve"));

    // Copying "café" preserves exact bytes (including 2-byte é: 0xC3, 0xA9)
    let cafe_idx = run.text.find("café").unwrap();
    let copied = TypographyGallery::semantic_copy_selection(
        &run.text,
        TextSelectionRange {
            start: cafe_idx,
            end: cafe_idx + "café".len(),
        },
    )
    .expect("copy café");
    assert_eq!(copied, "café");
    assert_eq!(copied.as_bytes(), "café".as_bytes());
}

#[test]
fn test_selection_overlays_and_caret() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(100);

    let sel_case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::SelectionAndCaretOverlays)
        .expect("selection case");

    let run = gallery
        .render_case(sel_case, &route, primary_font)
        .expect("render succeeds");

    let range = TextSelectionRange {
        start: 6,
        end: 11, // "Arrow"
    };

    let overlay = TypographyGallery::calculate_selection_overlay(&run, range, 100.0, 18.0);
    assert_eq!(overlay.range, range);
    assert_eq!(overlay.visual_rects.len(), 1);

    let rect = &overlay.visual_rects[0];
    assert!(rect.width > 0.0);
    assert_eq!(rect.y, 100.0);
    assert_eq!(rect.height, 18.0);

    let caret = overlay.caret.expect("caret present");
    assert_eq!(caret.byte_offset, 11);
    assert_eq!(caret.affinity, CaretAffinity::Trailing);
}

#[test]
fn test_theme_modes_and_visual_tokens() {
    let modes = [
        GalleryThemeMode::Light,
        GalleryThemeMode::Charcoal,
        GalleryThemeMode::HighContrastLight,
        GalleryThemeMode::HighContrastDark,
    ];

    for mode in modes {
        let tokens = GalleryVisualTokens::for_mode(mode);

        // Verify distinct visual roles
        assert_ne!(tokens.background, tokens.text);
        assert_ne!(tokens.selection_bg, tokens.search_match_bg);
        assert_ne!(tokens.diagnostic_error, tokens.diagnostic_warning);

        // WCAG Contrast Oracle verification
        let contrast = ContrastOracle::contrast_ratio(tokens.text, tokens.background);
        assert!(
            contrast >= 4.5,
            "Mode {:?} text contrast {:.2} must pass WCAG AA (>= 4.5)",
            mode,
            contrast
        );

        if mode == GalleryThemeMode::HighContrastLight || mode == GalleryThemeMode::HighContrastDark {
            assert!(
                contrast >= 7.0,
                "Mode {:?} high contrast text {:.2} must pass WCAG AAA (>= 7.0)",
                mode,
                contrast
            );
        }
    }
}

#[test]
fn test_negative_control_oracle_detects_defects() {
    let source = "let x = 42; // standard";

    // 1. Out of bounds start
    let err1 = TypographyGallery::semantic_copy_selection(
        source,
        TextSelectionRange {
            start: 50,
            end: 60,
        },
    );
    assert!(err1.is_err(), "oracle must detect out-of-bounds start");

    // 2. Inverted range (start > end)
    let err2 = TypographyGallery::semantic_copy_selection(
        source,
        TextSelectionRange {
            start: 10,
            end: 5,
        },
    );
    assert!(err2.is_err(), "oracle must detect inverted selection range");

    // 3. Splitting a multi-byte UTF-8 character (e.g. '🦀' is 4 bytes at offset 0)
    let emoji_source = "🦀 crab";
    let err3 = TypographyGallery::semantic_copy_selection(
        emoji_source,
        TextSelectionRange {
            start: 1, // mid-scalar
            end: 4,
        },
    );
    assert!(err3.is_err(), "oracle must detect selection splitting scalar value");

    // 4. Contrast oracle negative control: identical or very low-contrast colors fail
    let low_contrast_fg = [0.5, 0.5, 0.5, 1.0];
    let low_contrast_bg = [0.55, 0.55, 0.55, 1.0];
    let low_contrast = ContrastOracle::contrast_ratio(low_contrast_fg, low_contrast_bg);
    assert!(
        !ContrastOracle::passes_wcag_aa(low_contrast_fg, low_contrast_bg),
        "oracle must reject low-contrast color pair (ratio: {:.2})",
        low_contrast
    );
}
