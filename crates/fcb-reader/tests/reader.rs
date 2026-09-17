//! Focused tests for the source reading lens: line numbering, CRLF/BOM
//! handling, find-in-file, selection/copy, bracket matching, indent guides,
//! bounded viewport with pending far-line state.

#![forbid(unsafe_code)]

use fcb_reader::{
    find_matching_bracket, indent_guide, BoundedViewport, SourceReader,
};

const SAMPLE: &str = "fn main() {\n    let x = 42;\n    println!(\"{}\", x);\n}\n";

#[test]
fn line_count_and_numbering() {
    let reader = SourceReader::new(SAMPLE.as_bytes());
    assert_eq!(reader.line_count(), 4);
    let line1 = reader.line(1).expect("line 1");
    assert_eq!(line1.content, "fn main() {");
    let line4 = reader.line(4).expect("line 4");
    assert_eq!(line4.content, "}");
    assert!(reader.line(5).is_err(), "no line 5");
    assert!(reader.line(0).is_err(), "no line 0");
}

#[test]
fn crlf_endings_are_detected_and_stripped() {
    let crlf_source = "line1\r\nline2\r\nline3\r\n";
    let reader = SourceReader::new(crlf_source.as_bytes());
    assert!(reader.had_crlf());
    assert_eq!(reader.line_count(), 3);
    let line1 = reader.line(1).expect("line 1");
    assert_eq!(line1.content, "line1", "CRLF must be stripped from content");
}

#[test]
fn bom_is_stripped_and_offsets_adjusted() {
    let bom_source = b"\xEF\xBB\xBFkey = value\n";
    let reader = SourceReader::new(bom_source);
    assert_eq!(reader.content_start(), 3);
    let line = reader.line(1).expect("line 1");
    assert_eq!(line.content, "key = value");
}

#[test]
fn find_finds_all_occurrences_with_char_columns() {
    let source = "let x = 1;\nlet y = x + 1;\n// let z;\n";
    let reader = SourceReader::new(source.as_bytes());
    let results = reader.find("let");
    assert_eq!(results.len(), 3, "three occurrences of 'let'");
    assert_eq!(results[0], (1, 0), "line 1, column 0");
    assert_eq!(results[1], (2, 0), "line 2, column 0");
    assert_eq!(results[2], (3, 3), "line 3, column 3 (after comment marker)");
}

#[test]
fn selection_range_and_text_are_byte_exact() {
    let source = "hello world\nsecond line\n";
    let reader = SourceReader::new(source.as_bytes());
    // Select "world" on line 1, column 6, length 5.
    let range = reader
        .selection_range(1, 6, 5)
        .expect("valid selection range");
    let text = reader.selection_text(range).expect("selection text");
    assert_eq!(text, "world");
}

#[test]
fn selection_across_multibyte_uses_char_columns() {
    let source = "héllo wörld\n";
    let reader = SourceReader::new(source.as_bytes());
    // "wörld" starts at char column 6 (after "héllo " = 6 chars).
    let range = reader.selection_range(1, 6, 5).expect("valid selection");
    let text = reader.selection_text(range).expect("text");
    assert_eq!(text, "wörld");
}

#[test]
fn bracket_matching_finds_pairs_and_skips_strings() {
    let source = "fn main() { let x = [1, 2]; }";
    // Match the '(' at position 3.
    let close_paren = find_matching_bracket(source, 3);
    assert_eq!(close_paren, Some(12), "matching ) for ( at pos 3");
    // Match the '{' at position 13.
    let close_brace = find_matching_bracket(source, 13);
    assert_eq!(close_brace, Some(27), "matching close brace at pos 13");
    // Match the '[' at position 22.
    let close_bracket = find_matching_bracket(source, 22);
    assert_eq!(close_bracket, Some(26), "matching ] for [ at pos 22");
    // A quote-embedded bracket is skipped.
    let with_string = "fn f() { let s = \"{\"; }";
    let brace = find_matching_bracket(with_string, 7);
    assert_eq!(brace, Some(21), "string-embedded brace is skipped");
}

#[test]
fn indent_guides_track_depth() {
    assert_eq!(indent_guide("    code", 4).depth, 1);
    assert_eq!(indent_guide("        code", 4).depth, 2);
    assert_eq!(indent_guide("\tcode", 4).depth, 1);
    assert_eq!(indent_guide("\t\tcode", 4).depth, 2);
    assert!(indent_guide("\tcode", 4).uses_tabs);
    assert!(!indent_guide("    code", 4).uses_tabs);
    assert_eq!(indent_guide("no indent", 4).indent_width, 0);
}

#[test]
fn viewport_tracks_pending_far_lines() {
    let total = 100;
    let viewport = BoundedViewport::around(total, 50, 20);
    assert_eq!(viewport.first_line, 40);
    assert_eq!(viewport.last_line, 60);
    assert_eq!(viewport.pending_before(), 39);
    assert_eq!(viewport.pending_after(), 40);
    assert!(viewport.contains(50));
    assert!(!viewport.contains(20));
    assert!(!viewport.contains(80));

    // Near the start: first_line clamps to 1.
    let near_start = BoundedViewport::around(total, 5, 20);
    assert_eq!(near_start.first_line, 1);
    assert_eq!(near_start.pending_before(), 0);
}

#[test]
fn empty_source_produces_one_empty_line() {
    let reader = SourceReader::new(b"");
    assert_eq!(reader.line_count(), 1);
    let line = reader.line(1).expect("empty file has line 1");
    assert_eq!(line.content, "");
}

#[test]
fn lines_range_returns_inclusive_bounds() {
    let reader = SourceReader::new(SAMPLE.as_bytes());
    let lines = reader.lines_range(2, 3).expect("valid range");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].number, 2);
    assert_eq!(lines[1].number, 3);
    assert!(reader.lines_range(0, 2).is_err(), "line 0 is invalid");
    assert!(reader.lines_range(3, 2).is_err(), "end < start");
}
