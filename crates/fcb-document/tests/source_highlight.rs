use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget};
use fcb_document::source_highlight::{source_highlight, SourceHighlightError, MAX_SOURCE_HIGHLIGHT_BYTES};
fn highlight(text: &str, lang: &str) -> fcb_document::source_highlight::SourceHighlight {
    let owner = ArenaOwnerId::new(123).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    source_highlight(text, lang, &budget, ResourceAllocationId::new(1).unwrap(), owner).unwrap()
}
fn classified(text: &str, lang: &str, needle: &str, role: &str) {
    let result = highlight(text, lang);
    let units: Vec<u16> = text.encode_utf16().collect();
    assert!(result.syntax_supported);
    let mut next = 0;
    let mut found = false;
    for run in result.runs {
        assert_eq!(run.start, next);
        next += run.length;
        let fragment = String::from_utf16(&units[run.start as usize..next as usize]).unwrap();
        if fragment.contains(needle) && run.role == role { found = true; }
    }
    assert_eq!(next as usize, units.len());
    assert!(found, "missing {role} classification for {needle}");
}
#[test]
fn upstream_rust_and_javascript_roles_are_real() {
    let rust = "// comment\nfn main() { let s = \"hello\"; let n = 42; }";
    classified(rust, "rs", "fn", "keyword");
    classified(rust, "rs", "comment", "comment");
    classified(rust, "rs", "hello", "string");
    classified(rust, "rs", "42", "number");
    classified("const x = \"hello\"; // comment", "js", "const", "keyword");
    classified("const x = \"hello\"; // comment", "js", "hello", "string");
}
#[test]
fn unicode_offsets_are_utf16_and_preserve_multiline_context() {
    let text = "/* 🦀 e\u{301}\ncomment */\nfn x() { \"😀\"; }";
    classified(text, "rust", "comment", "comment");
    classified(text, "rust", "😀", "string");
    let result = highlight(text, "rs");
    let start = text.find("fn").unwrap();
    let expected = text[..start].encode_utf16().count() as u64;
    assert!(result.runs.iter().any(|r| r.start == expected && r.role == "keyword"));
    assert_ne!(expected, start as u64);
}
#[test]
fn empty_unknown_and_limit_have_explicit_outcomes() {
    assert!(highlight("", "rust").runs.is_empty());
    let plain = highlight("🦀\n", "unknown");
    assert!(!plain.syntax_supported);
    assert_eq!(plain.runs.len(), 1);
    assert_eq!(plain.runs[0].length, 3);
    assert_eq!(plain.runs[0].role, "plain");
    let owner = ArenaOwnerId::new(124).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(1)).unwrap();
    assert!(matches!(source_highlight("fn x(){}", "rs", &budget, ResourceAllocationId::new(1).unwrap(), owner), Err(SourceHighlightError::Admission)));
    let oversized = "x".repeat(MAX_SOURCE_HIGHLIGHT_BYTES + 1);
    assert!(matches!(source_highlight(&oversized, "rs", &budget, ResourceAllocationId::new(1).unwrap(), owner), Err(SourceHighlightError::Limit)));
}
