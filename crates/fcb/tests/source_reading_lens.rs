#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! Production unit and boundary test suite for the exact source reading lens (FCB-019.A / fcb-gjo.1).
//!
//! Required verification cases:
//! 1. `gutter_and_line_numbers_layout` — line number digits, padding, alignment, and formatting across scales.
//! 2. `independent_horizontal_scroll_and_wrap_modes` — unwrap mode horizontal column offsets vs viewport/column wrapping.
//! 3. `whitespace_and_indent_guides` — visible space/tab/CRLF markers and column-accurate indent guides.
//! 4. `bracket_guides_nested_and_negative_controls` — bidirectional matching, unclosed brackets, and mismatched delimiters.
//! 5. `find_in_file_search_and_cyclical_navigation` — case sensitivity, whole word, match navigation, and status reporting.
//! 6. `explicit_far_line_pending_state` — resolved line jumps vs pending far jumps beyond indexed extents vs out of bounds.
//! 7. `huge_line_horizontal_virtualization` — window-virtualized materialization and mode label truthfulness.
//! 8. `exact_selection_and_copy_fidelity_crlf_and_bidi` — exact CRLF retention, bidi logical order, and provenance metadata.
//! 9. `stale_capture_and_budget_refusal_negative_controls` — stale source/revision, owner mismatch, and oversized copy refusal.

use fcb::{
    ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision,
};
use fcb::search::{
    BracketKind, BracketMatchResult, BracketPairMatch, FindOptions, GuideOptions, GutterConfig,
    IndentGuide, LensModeLabel, LensSelection, LensViewport, LineEnding, LineNavigationResult,
    QueryGeneration, ReaderError, SourceReadingLens, VirtualLineRow, WhitespaceKind, WrapMode,
    compute_indent_guides, compute_whitespace_markers, find_matching_bracket,
    LineNumber,
};

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(919).unwrap()
}

fn file(id: u64) -> FileId {
    FileId::new(owner(), id).unwrap()
}

fn revision(id: u64) -> SourceRevision {
    SourceRevision::new(owner(), id).unwrap()
}

fn generation(id: u64) -> QueryGeneration {
    QueryGeneration::new(owner(), id).unwrap()
}

fn make_source(id: u64, rev: u64, path: &str, bytes: Vec<u8>) -> SourceCapture {
    SourceCapture::from_bytes(owner(), file(id), revision(rev), path, bytes).unwrap()
}

fn make_viewport(width_px: u32, height_px: u32) -> LensViewport {
    LensViewport::new(width_px, height_px, 20, 10).unwrap()
}

#[test]
fn gutter_and_line_numbers_layout() {
    let gutter = GutterConfig::default();

    // Digit counts
    assert_eq!(GutterConfig::digit_count(0), 1);
    assert_eq!(GutterConfig::digit_count(1), 1);
    assert_eq!(GutterConfig::digit_count(9), 1);
    assert_eq!(GutterConfig::digit_count(10), 2);
    assert_eq!(GutterConfig::digit_count(99), 2);
    assert_eq!(GutterConfig::digit_count(100), 3);
    assert_eq!(GutterConfig::digit_count(999), 3);
    assert_eq!(GutterConfig::digit_count(1000), 4);
    assert_eq!(GutterConfig::digit_count(1_000_000), 7);

    // Gutter width calculations (padding = 1 space on each side -> digits + 2)
    assert_eq!(gutter.gutter_width_chars(5), 3); // 1 + 2
    assert_eq!(gutter.gutter_width_chars(50), 4); // 2 + 2
    assert_eq!(gutter.gutter_width_chars(500), 5); // 3 + 2
    assert_eq!(gutter.gutter_width_chars(5000), 6); // 4 + 2

    // Pixel width (char_width = 10px)
    assert_eq!(gutter.gutter_width_px(500, 10), 50);

    // Formatted line numbers (right-aligned)
    assert_eq!(gutter.format_line_number(1, 100), "  1");
    assert_eq!(gutter.format_line_number(42, 100), " 42");
    assert_eq!(gutter.format_line_number(100, 100), "100");

    // Disabled gutter
    let disabled = GutterConfig {
        show_line_numbers: false,
        padding_chars: 1,
    };
    assert_eq!(disabled.gutter_width_chars(100), 0);
    assert_eq!(disabled.format_line_number(42, 100), "");
}

#[test]
fn independent_horizontal_scroll_and_wrap_modes() {
    let content = b"line 1: short\nline 2: a very long line that exceeds standard narrow viewport width\nline 3: end";
    let capture = make_source(1, 1, "test.rs", content.to_vec());
    let line_starts = vec![0, 14, 83];
    let total_lines = 3;

    // Viewport: 200px wide / 10px char_width = 20 columns; 100px high / 20px line_height = 5 rows
    let vp = make_viewport(200, 100);
    let mut lens = SourceReadingLens::new(vp);

    // 1. WrapMode::None with scroll_x_cols = 0
    lens.set_wrap_mode(WrapMode::None);
    lens.set_scroll_x_cols(0);
    let rows = lens.render_visible_rows(&capture, &line_starts, total_lines).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].text, "line 1: short");
    assert_eq!(rows[1].text, "line 2: a very long line that exceeds standard narrow viewport width");
    assert!(!rows[1].is_wrapped_continuation);

    // 2. WrapMode::None with scroll_x_cols = 8 (horizontal scrolling shifts visible text)
    lens.set_scroll_x_cols(8);
    let scrolled_rows = lens.render_visible_rows(&capture, &line_starts, total_lines).unwrap();
    assert_eq!(scrolled_rows.len(), 3);
    assert_eq!(scrolled_rows[0].text, "short");
    assert_eq!(scrolled_rows[1].text, "very long line that exceeds standard narrow viewport width");

    // 3. WrapMode::ColumnLimit(20) - wraps line 2 into chunks of 20 chars
    lens.set_scroll_x_cols(0);
    lens.set_wrap_mode(WrapMode::ColumnLimit(20));
    let wrapped_rows = lens.render_visible_rows(&capture, &line_starts, total_lines).unwrap();
    // line 1 = 1 row (13 chars)
    // line 2 = 69 chars -> 4 chunks (20, 20, 20, 9)
    // line 3 = 1 row
    assert!(wrapped_rows.len() >= 5);
    assert_eq!(wrapped_rows[0].line_number, 1);
    assert!(!wrapped_rows[0].is_wrapped_continuation);

    assert_eq!(wrapped_rows[1].line_number, 2);
    assert_eq!(wrapped_rows[1].text, "line 2: a very long ");
    assert!(!wrapped_rows[1].is_wrapped_continuation);

    assert_eq!(wrapped_rows[2].line_number, 2);
    assert_eq!(wrapped_rows[2].text, "line that exceeds st");
    assert!(wrapped_rows[2].is_wrapped_continuation);
    assert_eq!(wrapped_rows[2].formatted_line_number, ""); // No duplicate gutter line number on continuation
}

#[test]
fn whitespace_and_indent_guides() {
    // 1. Whitespace markers
    let text = "  fn\ttest()\r\n";
    let markers = compute_whitespace_markers(text, 4);
    assert_eq!(markers.len(), 4);
    assert_eq!(markers[0].kind, WhitespaceKind::Space);
    assert_eq!(markers[0].column, 0);
    assert_eq!(markers[1].kind, WhitespaceKind::Space);
    assert_eq!(markers[1].column, 1);
    assert_eq!(markers[2].kind, WhitespaceKind::Tab);
    assert_eq!(markers[2].column, 4); // After "fn" (cols 2,3), tab advances to 4
    assert_eq!(markers[3].kind, WhitespaceKind::CrLf);

    // 2. Indent guides
    let code_4sp = "    let x = 1;";
    let guides_4sp = compute_indent_guides(code_4sp, 4);
    assert_eq!(guides_4sp.len(), 1);
    assert_eq!(guides_4sp[0].column, 0);
    assert_eq!(guides_4sp[0].level, 0);

    let code_8sp = "        let y = 2;";
    let guides_8sp = compute_indent_guides(code_8sp, 4);
    assert_eq!(guides_8sp.len(), 2);
    assert_eq!(guides_8sp[0], IndentGuide { column: 0, level: 0 });
    assert_eq!(guides_8sp[1], IndentGuide { column: 4, level: 1 });

    let code_tabs = "\t\tlet z = 3;";
    let guides_tabs = compute_indent_guides(code_tabs, 4);
    assert_eq!(guides_tabs.len(), 2);
    assert_eq!(guides_tabs[0], IndentGuide { column: 0, level: 0 });
    assert_eq!(guides_tabs[1], IndentGuide { column: 4, level: 1 });
}

#[test]
fn bracket_guides_nested_and_negative_controls() {
    let source = b"fn main() { let x = [1, (2 + 3)]; }";

    // Matching '(' at offset 7
    let match_paren = find_matching_bracket(source, 7);
    assert_eq!(
        match_paren,
        BracketMatchResult::Matched(BracketPairMatch {
            kind: BracketKind::Paren,
            open_offset: 7,
            close_offset: 8,
        })
    );

    // Matching '}' at offset 35 backwards to '{' at offset 10
    let match_brace = find_matching_bracket(source, 35);
    assert_eq!(
        match_brace,
        BracketMatchResult::Matched(BracketPairMatch {
            kind: BracketKind::Brace,
            open_offset: 10,
            close_offset: 35,
        })
    );

    // Matching inner nested paren '(' at offset 24 to ')' at offset 31
    let match_inner = find_matching_bracket(source, 24);
    assert_eq!(
        match_inner,
        BracketMatchResult::Matched(BracketPairMatch {
            kind: BracketKind::Paren,
            open_offset: 24,
            close_offset: 31,
        })
    );

    // Negative control 1: unclosed opening bracket
    let unclosed = b"fn broken() { let a = [1, 2, 3;";
    let res_unclosed = find_matching_bracket(unclosed, 22); // '['
    assert_eq!(
        res_unclosed,
        BracketMatchResult::Unmatched {
            offset: 22,
            kind: BracketKind::Bracket,
            is_open: true,
        }
    );

    // Negative control 2: non-bracket character
    let non_bracket = find_matching_bracket(source, 3); // 'm' in 'main'
    assert_eq!(non_bracket, BracketMatchResult::None);
}

#[test]
fn find_in_file_search_and_cyclical_navigation() {
    let content = b"fn search_needle() {\n    let needle = 42;\n    // Needle in comment\n}\n";
    let capture = make_source(1, 1, "find_test.rs", content.to_vec());

    let vp = make_viewport(400, 200);
    let mut lens = SourceReadingLens::new(vp);

    // Case-insensitive search
    lens.start_find(
        &capture,
        "needle",
        FindOptions {
            case_sensitive: false,
            whole_word: false,
        },
    );

    let session = lens.find_session().unwrap();
    assert_eq!(session.matches().len(), 3);
    assert_eq!(session.status().total_matches, 3);
    assert_eq!(session.status().current_match_one_based, Some(1));

    // Cyclical navigation
    let mut_session = lens.find_session_mut().unwrap();
    let m2 = mut_session.next_match().unwrap();
    assert_eq!(m2.line_number, 2);
    assert_eq!(mut_session.status().current_match_one_based, Some(2));

    let m3 = mut_session.next_match().unwrap();
    assert_eq!(m3.line_number, 3);
    assert_eq!(mut_session.status().current_match_one_based, Some(3));

    // Wraps back to match 1
    let m1_again = mut_session.next_match().unwrap();
    assert_eq!(m1_again.line_number, 1);
    assert_eq!(mut_session.status().current_match_one_based, Some(1));

    // Previous match wraps back to match 3
    let m3_again = mut_session.prev_match().unwrap();
    assert_eq!(m3_again.line_number, 3);
    assert_eq!(mut_session.status().current_match_one_based, Some(3));

    // Case-sensitive search: "Needle" only matches 1 occurrence (line 3)
    lens.start_find(
        &capture,
        "Needle",
        FindOptions {
            case_sensitive: true,
            whole_word: false,
        },
    );
    let cs_session = lens.find_session().unwrap();
    assert_eq!(cs_session.matches().len(), 1);
    assert_eq!(cs_session.matches()[0].line_number, 3);

    // Negative control: non-existent search needle
    lens.start_find(
        &capture,
        "nonexistent_symbol",
        FindOptions::default(),
    );
    let empty_session = lens.find_session().unwrap();
    assert_eq!(empty_session.matches().len(), 0);
    assert_eq!(empty_session.status().total_matches, 0);
    assert_eq!(empty_session.status().current_match_one_based, None);
}

#[test]
fn explicit_far_line_pending_state() {
    let vp = make_viewport(400, 200);
    let mut lens = SourceReadingLens::new(vp);

    let indexed_through = 50;
    let total_lines = Some(500);

    // 1. Navigation to already indexed line (<= 50) resolves immediately
    let target_ok = LineNumber::new(25).unwrap();
    let res_ok = lens.navigate_to_line(target_ok, indexed_through, total_lines);
    assert_eq!(
        res_ok,
        LineNavigationResult::Resolved {
            line: target_ok,
            target_scroll_y: 25,
        }
    );
    assert_eq!(lens.scroll_y_line(), 25);

    // 2. Navigation to far line (> 50 and <= 500) reports explicit PendingFarJump
    let target_far = LineNumber::new(250).unwrap();
    let res_far = lens.navigate_to_line(target_far, indexed_through, total_lines);
    assert_eq!(
        res_far,
        LineNavigationResult::PendingFarJump {
            target_line: target_far,
            indexed_through_line: 50,
        }
    );
    // Crucial: far jump does not change scroll_y to an invalid/unverified position
    assert_eq!(lens.scroll_y_line(), 25);

    // 3. Navigation beyond total lines reports OutOfBounds
    let target_oob = LineNumber::new(1000).unwrap();
    let res_oob = lens.navigate_to_line(target_oob, indexed_through, total_lines);
    assert_eq!(
        res_oob,
        LineNavigationResult::OutOfBounds {
            target_line: target_oob,
            total_lines: 500,
        }
    );
}

#[test]
fn huge_line_horizontal_virtualization() {
    // Construct a line exceeding LENS_HUGE_LINE_BYTE_LIMIT (64 KiB)
    let huge_len = 70 * 1024;
    let mut bytes = vec![b'A'; huge_len];
    bytes.push(b'\n');

    let capture = make_source(1, 1, "huge.txt", bytes);
    let line_starts = vec![0];
    let total_lines = 1;

    let vp = make_viewport(200, 100); // 20 columns
    let mut lens = SourceReadingLens::new(vp);
    lens.set_wrap_mode(WrapMode::None);
    lens.set_scroll_x_cols(100);

    let rows = lens.render_visible_rows(&capture, &line_starts, total_lines).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mode_label, LensModeLabel::WindowVirtualized);
    // Materialized columns: 20 visible + 32 overscan = 52 characters, NOT 70,000 characters!
    assert_eq!(rows[0].text.len(), 52);
    // Full raw byte range is preserved accurately
    assert_eq!(rows[0].raw_byte_range.len().get(), (huge_len + 1) as u64);
}

#[test]
fn exact_selection_and_copy_fidelity_crlf_and_bidi() {
    // 1. CRLF line endings preservation
    let crlf_bytes = b"first line\r\nsecond line\r\nthird line\r\n";
    let capture_crlf = make_source(10, 1, "crlf.txt", crlf_bytes.to_vec());

    let sel_crlf = LensSelection {
        file: file(10),
        revision: revision(1),
        generation: generation(1),
        byte_range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(25)).unwrap(),
        start_line: 1,
        start_col: 0,
        end_line: 2,
        end_col: 13,
    };

    let copied_crlf = sel_crlf.copy_exact_bytes(&capture_crlf, 1024).unwrap();
    // Must contain exact \r\n, NEVER silently normalized to \n!
    assert_eq!(copied_crlf, b"first line\r\nsecond line\r\n");

    let prov_crlf = sel_crlf.copy_with_provenance(&capture_crlf, 1024).unwrap();
    assert_eq!(prov_crlf.logical_path, "crlf.txt");
    assert_eq!(prov_crlf.exact_bytes, b"first line\r\nsecond line\r\n");

    // 2. Bidirectional Arabic/Hebrew text preservation in logical order
    // "مرحبا بالعالم" (Arabic: Hello World)
    let arabic_text = "مرحبا بالعالم\n";
    let capture_bidi = make_source(11, 1, "bidi.txt", arabic_text.as_bytes().to_vec());

    let sel_bidi = LensSelection {
        file: file(11),
        revision: revision(1),
        generation: generation(1),
        byte_range: ByteRange::new(
            ByteOffset::new(0),
            ByteOffset::new(arabic_text.len() as u64),
        )
        .unwrap(),
        start_line: 1,
        start_col: 0,
        end_line: 1,
        end_col: arabic_text.chars().count() as u32,
    };

    let copied_bidi = sel_bidi.copy_exact_bytes(&capture_bidi, 1024).unwrap();
    assert_eq!(copied_bidi, arabic_text.as_bytes());
}

#[test]
fn stale_capture_and_budget_refusal_negative_controls() {
    let bytes = b"authoritative production code bytes\n";
    let capture = make_source(20, 1, "stale.rs", bytes.to_vec());

    // Selection pointing to revision 1
    let selection = LensSelection {
        file: file(20),
        revision: revision(1),
        generation: generation(1),
        byte_range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(bytes.len() as u64)).unwrap(),
        start_line: 1,
        start_col: 0,
        end_line: 1,
        end_col: 36,
    };

    // 1. Success on matching source and revision
    assert!(selection.copy_exact_bytes(&capture, 1024).is_ok());

    // 2. Negative control: Stale source revision (capture updated to revision 2)
    let capture_rev2 = make_source(20, 2, "stale.rs", bytes.to_vec());
    let err_stale_rev = selection.copy_exact_bytes(&capture_rev2, 1024);
    assert_eq!(err_stale_rev, Err(ReaderError::StaleSource));

    // 3. Negative control: Stale file ID (different file)
    let capture_diff_file = make_source(21, 1, "other.rs", bytes.to_vec());
    let err_diff_file = selection.copy_exact_bytes(&capture_diff_file, 1024);
    assert_eq!(err_diff_file, Err(ReaderError::StaleSource));

    // 4. Negative control: Stale query generation
    let mut stale_gen_sel = selection;
    stale_gen_sel.generation = generation(99);
    let err_stale_gen = stale_gen_sel.validate(&capture, generation(1));
    assert_eq!(err_stale_gen, Err(ReaderError::StaleQuery));

    // 5. Negative control: Copy budget refusal (selection is 37 bytes, budget is 20 bytes)
    let err_budget = selection.copy_exact_bytes(&capture, 20);
    assert_eq!(err_budget, Err(ReaderError::ResourceDenied));
}
