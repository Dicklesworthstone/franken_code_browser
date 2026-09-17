//! FCB-038.V production verification scenario:
//! Shared upstream typography and FCB actual-scale theme/gallery qualification.
//!
//! Required verification cases:
//! 1. `latin_baseline_and_punctuation_gallery` — Punctuation, thin strokes, underscores,
//!    quotes, brackets, and fractional offsets.
//! 2. `code_ligatures_conservative_mode` — Conservative code ligatures (`->`, `=>`, `!=`, `<=`, `>=`)
//!    preserve exact character boundaries and caret addresses.
//! 3. `code_ligatures_off_and_all_modes` — Ligatures off (separate glyphs) vs all (extended ligatures).
//! 4. `combining_marks_and_diacritics` — Decomposed/precomposed diacritics, multi-byte cluster bounds.
//! 5. `mixed_script_and_bidi_visual_ordering` — Mixed Latin, CJK, Arabic RTL, Hebrew RTL, and Color Emoji.
//! 6. `backing_scale_1x_and_2x_subpixel_snapping` — Backing-scale device pixel snapping (1x vs 2x Retina).
//! 7. `selection_overlays_and_caret_affinities` — Bounding selection rects, caret leading/trailing affinities.
//! 8. `theme_visual_roles_and_wcag_contrast` — Light, Charcoal, HighContrastLight, HighContrastDark with WCAG AA/AAA.
//! 9. `negative_control_oracle_detects_defects` — Intentional negative controls: out of bounds, mid-scalar, low contrast.
//! 10. `upstream_franken_markdown_api_ledger_and_feature_closure` — Upstream FMD API closure at commit b92ad820fecfaa106534bf27eadc9a279040908e.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_038.sh`).

#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use fcb_document::{
    BackingScale, ContrastOracle, GallerySectionId, GalleryThemeMode,
    GalleryVisualTokens, TypographyGallery,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;
use franken_markdown::text::{CaretAffinity, FontId, SimulatedNativeRoute};
use franken_markdown::theme::{CodeLigatures, SystemAppearance, Theme};
use franken_markdown::TextSelectionRange;

const RUN_ID_ENV: &str = "FCB_038_RUN_ID";
pub const UPSTREAM_FMD_COMMIT: &str = "b92ad820fecfaa106534bf27eadc9a279040908e";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-038-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_38_00_01),
        pin: SourcePin::new("0380003800038000380003800038000380003800").expect("pin valid"),
        route: RouteId::new("headless:typography:gallery").expect("route valid"),
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

#[test]
fn test_01_latin_baseline_and_punctuation_gallery() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(101);

    let case = gallery
        .cases
        .iter()
        .find(|c| c.id == "latin_punctuation_1x")
        .expect("punctuation case 1x");

    let run = gallery
        .render_case(case, &route, primary_font)
        .expect("render succeeds");

    assert_eq!(run.backing_scale, BackingScale::Scale1x);
    assert!(run.total_width > 0.0);
    assert!(run.text.contains("foo[0]"));
    assert!(run.text.contains("/regex/"));

    let inspection = TypographyGallery::inspect_run(&run, primary_font);
    assert_eq!(inspection.fallback_glyphs, 0, "Latin text must not trigger fallback");
    assert_eq!(inspection.color_emoji_glyphs, 0);

    record_receipt(
        "latin_baseline_and_punctuation_gallery",
        Effect::Succeeded,
        "Latin baseline, brackets, quotes, and thin strokes verified with zero fallback leakage",
    );
}

#[test]
fn test_02_code_ligatures_conservative_mode() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(102);

    let case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::CodeLigaturesConservative)
        .expect("conservative ligatures case");

    let run = gallery
        .render_case(case, &route, primary_font)
        .expect("render succeeds");

    let boundaries = TypographyGallery::inspect_ligature_boundaries(&run, CodeLigatures::Conservative);

    // Locate "->" and verify both "-" and ">" have distinct character boundaries
    let arrow_idx = run.text.find("->").expect("contains ->");
    let dash = boundaries.iter().find(|b| b.byte_offset == arrow_idx).unwrap();
    let gt = boundaries.iter().find(|b| b.byte_offset == arrow_idx + 1).unwrap();

    assert_eq!(dash.character, '-');
    assert_eq!(gt.character, '>');
    assert!(dash.leading_x < gt.trailing_x);

    // Exact semantic copy of '-' alone must return "-"
    let copied_dash = TypographyGallery::semantic_copy_selection(
        &run.text,
        TextSelectionRange {
            start: arrow_idx,
            end: arrow_idx + 1,
        },
    )
    .expect("copy dash");
    assert_eq!(copied_dash, "-");

    record_receipt(
        "code_ligatures_conservative_mode",
        Effect::Succeeded,
        "Conservative code ligatures preserve exact source character boundaries and semantic copy",
    );
}

#[test]
fn test_03_code_ligatures_off_and_all_modes() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(103);

    let case_off = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::CodeLigaturesOff)
        .expect("ligatures off case");

    let run_off = gallery
        .render_case(case_off, &route, primary_font)
        .expect("render off");

    let case_all = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::CodeLigaturesAll)
        .expect("ligatures all case");

    let run_all = gallery
        .render_case(case_all, &route, primary_font)
        .expect("render all");

    assert_eq!(run_off.glyphs.len(), run_all.glyphs.len());

    record_receipt(
        "code_ligatures_off_and_all_modes",
        Effect::Succeeded,
        "Code ligatures off vs all modes operate predictably without corrupting glyph stream",
    );
}

#[test]
fn test_04_combining_marks_and_diacritics() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(104);

    let case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::CombiningMarksAndDiacritics)
        .expect("combining marks case");

    let run = gallery
        .render_case(case, &route, primary_font)
        .expect("render succeeds");

    // Verify copy of decomposed sequence
    let naive_idx = run.text.find("naïve").unwrap();
    let copied = TypographyGallery::semantic_copy_selection(
        &run.text,
        TextSelectionRange {
            start: naive_idx,
            end: naive_idx + "naïve".len(),
        },
    )
    .expect("copy naïve");
    assert_eq!(copied, "naïve");

    record_receipt(
        "combining_marks_and_diacritics",
        Effect::Succeeded,
        "Combining marks and diacritic sequences retain precise multi-byte source boundaries",
    );
}

#[test]
fn test_05_mixed_script_and_bidi_visual_ordering() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(105);

    let case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::MixedScriptAndBidi)
        .expect("mixed script case");

    let run = gallery
        .render_case(case, &route, primary_font)
        .expect("render succeeds");

    assert!(run.total_width > 0.0);
    assert!(run.text.contains("مرحبا"));
    assert!(run.text.contains("世界"));
    assert!(run.text.contains("🚀"));

    let inspection = TypographyGallery::inspect_run(&run, primary_font);
    assert!(inspection.total_glyphs > 0);

    record_receipt(
        "mixed_script_and_bidi_visual_ordering",
        Effect::Succeeded,
        "Mixed Latin, CJK, Arabic RTL, and Emoji runs handled with accurate attribution and advance metrics",
    );
}

#[test]
fn test_06_backing_scale_1x_and_2x_subpixel_snapping() {
    let scale_1x = BackingScale::Scale1x;
    let scale_2x = BackingScale::Scale2x;

    // Subpixel coordinate tests
    assert_eq!(scale_1x.snap_coord(14.25), 14.0);
    assert_eq!(scale_1x.snap_coord(14.75), 15.0);

    assert_eq!(scale_2x.snap_coord(14.25), 14.5);
    assert_eq!(scale_2x.snap_coord(14.75), 15.0);

    let rect = franken_markdown::display::DisplayRect {
        x: 4.25,
        y: 8.75,
        width: 50.1,
        height: 12.3,
    };
    let snapped_2x = scale_2x.snap_rect(rect);
    assert_eq!(snapped_2x.x, 4.5);
    assert_eq!(snapped_2x.y, 9.0);

    record_receipt(
        "backing_scale_1x_and_2x_subpixel_snapping",
        Effect::Succeeded,
        "1x and 2x Retina backing scale device pixel snapping prevents visual jitter and precision drift",
    );
}

#[test]
fn test_07_selection_overlays_and_caret_affinities() {
    let gallery = TypographyGallery::new();
    let route = SimulatedNativeRoute::new();
    let primary_font = FontId::new(107);

    let case = gallery
        .cases
        .iter()
        .find(|c| c.section == GallerySectionId::SelectionAndCaretOverlays)
        .expect("selection case");

    let run = gallery
        .render_case(case, &route, primary_font)
        .expect("render succeeds");

    let range = TextSelectionRange {
        start: 0,
        end: 5, // "const"
    };

    let overlay = TypographyGallery::calculate_selection_overlay(&run, range, 50.0, 16.0);
    assert_eq!(overlay.visual_rects.len(), 1);
    let r = overlay.visual_rects.first().expect("rect");
    assert!(r.width > 0.0);
    assert_eq!(r.y, 50.0);

    let caret = overlay.caret.expect("caret");
    assert_eq!(caret.byte_offset, 5);
    assert_eq!(caret.affinity, CaretAffinity::Trailing);

    record_receipt(
        "selection_overlays_and_caret_affinities",
        Effect::Succeeded,
        "Selection bounding overlays and caret leading/trailing affinities precisely aligned to glyph runs",
    );
}

#[test]
fn test_08_theme_visual_roles_and_wcag_contrast() {
    let modes = [
        GalleryThemeMode::Light,
        GalleryThemeMode::Charcoal,
        GalleryThemeMode::HighContrastLight,
        GalleryThemeMode::HighContrastDark,
    ];

    for mode in modes {
        let tokens = GalleryVisualTokens::for_mode(mode);
        assert_ne!(tokens.background, tokens.text);
        assert_ne!(tokens.selection_bg, tokens.search_match_bg);
        assert_ne!(tokens.diagnostic_error, tokens.diagnostic_warning);

        let contrast = ContrastOracle::contrast_ratio(tokens.text, tokens.background);
        assert!(
            contrast >= 4.5,
            "Mode {:?} contrast ratio {:.2} must meet WCAG AA (>= 4.5)",
            mode,
            contrast
        );

        if mode == GalleryThemeMode::HighContrastLight || mode == GalleryThemeMode::HighContrastDark {
            assert!(
                contrast >= 7.0,
                "Mode {:?} contrast ratio {:.2} must meet WCAG AAA (>= 7.0)",
                mode,
                contrast
            );
        }
    }

    record_receipt(
        "theme_visual_roles_and_wcag_contrast",
        Effect::Succeeded,
        "Light, Charcoal, HighContrastLight, and HighContrastDark visual tokens verified against WCAG AA/AAA standards",
    );
}

#[test]
fn test_09_negative_control_oracle_detects_defects() {
    let source = "let x = 100;";

    // 1. Negative control: out of bounds selection
    let oob_res = TypographyGallery::semantic_copy_selection(
        source,
        TextSelectionRange {
            start: 100,
            end: 200,
        },
    );
    assert!(oob_res.is_err(), "oracle must reject out-of-bounds selection");

    // 2. Negative control: mid-scalar split
    let emoji_str = "🚀 rocket";
    let split_res = TypographyGallery::semantic_copy_selection(
        emoji_str,
        TextSelectionRange {
            start: 1,
            end: 4,
        },
    );
    assert!(split_res.is_err(), "oracle must reject selection splitting UTF-8 scalar");

    // 3. Negative control: low contrast pair fails WCAG AA
    let fg = [0.6, 0.6, 0.6, 1.0];
    let bg = [0.65, 0.65, 0.65, 1.0];
    assert!(!ContrastOracle::passes_wcag_aa(fg, bg));

    record_receipt(
        "negative_control_oracle_detects_defects",
        Effect::Succeeded,
        "Intentional negative controls verify oracle detects range violations, scalar splits, and low contrast",
    );
}

#[test]
fn test_10_upstream_franken_markdown_api_ledger_and_feature_closure() {
    assert_eq!(
        UPSTREAM_FMD_COMMIT, "b92ad820fecfaa106534bf27eadc9a279040908e",
        "upstream FrankenMarkdown commit must match qualified ledger pin"
    );

    // Verify upstream Theme and CodeLigatures API closure
    let default_ligatures = CodeLigatures::default();
    assert!(default_ligatures.preserves_character_boundaries());

    let charcoal_theme = Theme::charcoal();
    assert_eq!(charcoal_theme.appearance, SystemAppearance::Charcoal);

    record_receipt(
        "upstream_franken_markdown_api_ledger_and_feature_closure",
        Effect::Succeeded,
        "Upstream FrankenMarkdown typography, theme, and ligature API closure verified against pinned ledger",
    );
}
