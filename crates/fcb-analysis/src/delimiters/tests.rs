#![forbid(unsafe_code)]

use super::*;
use fcb_core::{ArenaOwnerId, ByteOffset};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8841).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 7).unwrap(), SourceRevision::new(owner(), 11).unwrap()).unwrap()
}
fn range(start: usize, end: usize) -> ByteRange {
    ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64)).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn run(source: &[u8], language: SymbolLanguage, selected: ByteRange) -> Result<DelimiterMatch, DelimiterError> {
    let budget = budget();
    let result = matching_delimiter(source, request(), language, None, selected, &budget,
        ResourceAllocationId::new(1).unwrap(), || false);
    assert_eq!(budget.accounting().reserved().get(), 0, "all temporary analysis must be released");
    result
}

#[test]
fn nested_mixed_pairs_match_in_both_directions_with_original_identity() {
    // Expected counterparts come from fixture construction, not a second
    // implementation of the production stack algorithm.
    for depth in [1, 2, 3, 8, 63, 257] {
        let opens: String = (0..depth).map(|i| ['(', '[', '{'][i % 3]).collect();
        let closes: String = (0..depth).rev().map(|i| [')', ']', '}'][i % 3]).collect();
        let source = format!("{opens}x{closes}");
        for at in [0, depth / 2, depth - 1] {
            let expected = source.len() - 1 - at;
            let left = run(source.as_bytes(), SymbolLanguage::Rust, range(at, at + 1)).unwrap();
            let right = run(source.as_bytes(), SymbolLanguage::Rust, range(expected, expected + 1)).unwrap();
            assert_eq!(left.state(), DelimiterState::Matched);
            assert_eq!(right.state(), DelimiterState::Matched);
            assert_eq!(left.matching_range(), Some(range(expected, expected + 1)));
            assert_eq!(right.matching_range(), Some(range(at, at + 1)));
            assert_eq!(left.file(), request().file());
            assert_eq!(left.revision(), request().revision());
            assert_eq!(left.evidence_level(), "lexical-delimiter-candidate");
        }
    }
}

#[test]
fn malformed_nesting_never_returns_a_plausible_but_wrong_mate() {
    for at in 0..4 {
        let result = run(b"([)]", SymbolLanguage::Rust, range(at, at + 1)).unwrap();
        assert_eq!(result.state(), DelimiterState::Mismatched);
        assert_eq!(result.matching_range(), None);
    }
    for text in ["(", ")", "{([])", "([])}"] {
        let at = if text == "([])}" { 4 } else { 0 };
        let result = run(text.as_bytes(), SymbolLanguage::Rust, range(at, at + 1)).unwrap();
        assert_eq!(result.state(), DelimiterState::Unmatched);
        assert_eq!(result.matching_range(), None);
    }
    // Unrelated unmatched delimiters outside the selected region do not
    // prevent a local lexical pair; there is no whole-program validity claim.
    assert_eq!(run(b")()(", SymbolLanguage::Rust, range(1, 2)).unwrap().matching_range(), Some(range(2, 3)));
    assert_eq!(run(b"x<>y", SymbolLanguage::Rust, range(1, 2)).unwrap().state(), DelimiterState::NotDelimiter);
}

#[test]
fn upstream_literals_and_comments_are_opaque_across_supported_languages() {
    let fixtures = [
        (SymbolLanguage::Rust, "fn run() { let s = r#\" } ] ) \"#; /* outer /* } */ still ) */ (1); }"),
        (SymbolLanguage::Go, "func run() { s := ` } ] ) `; /* } */ _ = s; (1); }"),
        (SymbolLanguage::Cpp, "int run() { auto s = R\"tag( } ] ) )tag\"; /* } */ (1); }"),
        (SymbolLanguage::Swift, "func run() { let s = #\" } ] ) \"#; /* } */ (1); }"),
        (SymbolLanguage::Java, "class A { String s = \"\"\" } ] ) \"\"\"; void run() {} }"),
        (SymbolLanguage::CSharp, "class A { string s = @\" } ] ) \"; void Run() {} }"),
        (SymbolLanguage::JavaScript, "function run() { const s = ` } ] ) `; const r = /[{}]/; (1); }"),
        (SymbolLanguage::TypeScript, "function run(): void { const s = ` } ] ) `; (1); }"),
        (SymbolLanguage::Jsx, "function run() { const s = \" } ] ) \"; (1); }"),
        (SymbolLanguage::Tsx, "function run(): void { const s = \" } ] ) \"; (1); }"),
    ];
    for (language, source) in fixtures {
        let open = source.find('{').unwrap();
        let close = source.rfind('}').unwrap();
        let fake = source.find(" } ] ) ").unwrap() + 1;
        let result = run(source.as_bytes(), language, range(open, open + 1)).unwrap();
        assert_eq!(result.matching_range(), Some(range(close, close + 1)), "{}", language.name());
        assert_eq!(run(source.as_bytes(), language, range(fake, fake + 1)).unwrap().state(),
            DelimiterState::NotCode, "{}", language.name());
    }
    let python = "def run():\n    s = ''' } ] ) '''\n    # ) ] }\n    return ([1])\n";
    let open = python.rfind('(').unwrap();
    let close = python.rfind(')').unwrap();
    assert_eq!(run(python.as_bytes(), SymbolLanguage::Python, range(open, open + 1)).unwrap().matching_range(),
        Some(range(close, close + 1)));
    let fake = python.find(" } ] ) ").unwrap() + 1;
    assert_eq!(run(python.as_bytes(), SymbolLanguage::Python, range(fake, fake + 1)).unwrap().state(), DelimiterState::NotCode);
}

#[test]
fn utf16_both_orders_and_bom_use_exact_original_scalar_ranges() {
    let text = "// 😀 é\r\nfn run() { [1, (2)] }\n";
    let opening = text.find('{').unwrap();
    let closing = text.rfind('}').unwrap();
    for little in [true, false] {
        let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() { bytes.extend(if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
        let open = 2 + text[..opening].encode_utf16().count() * 2;
        let close = 2 + text[..closing].encode_utf16().count() * 2;
        let result = run(&bytes, SymbolLanguage::Rust, range(open, open + 2)).unwrap();
        assert_eq!(result.selected_range(), range(open, open + 2));
        assert_eq!(result.matching_range(), Some(range(close, close + 2)));
        assert_eq!(run(&bytes, SymbolLanguage::Rust, range(close, close + 2)).unwrap().matching_range(), Some(range(open, open + 2)));
        assert_eq!(run(&bytes, SymbolLanguage::Rust, range(open + 1, open + 2)), Err(DelimiterError::InvalidSelection));
        assert_eq!(run(&bytes, SymbolLanguage::Rust, range(0, 2)), Err(DelimiterError::InvalidSelection));
        let emoji = 2 + text[..text.find('😀').unwrap()].encode_utf16().count() * 2;
        assert_eq!(run(&bytes, SymbolLanguage::Rust, range(emoji, emoji + 2)), Err(DelimiterError::InvalidSelection));
    }
    let utf8 = "\u{feff}é😀([])";
    let open = utf8.find('(').unwrap();
    assert_eq!(run(utf8.as_bytes(), SymbolLanguage::Rust, range(open, open + 1)).unwrap().matching_range(),
        Some(range(utf8.len() - 1, utf8.len())));
    assert_eq!(run(utf8.as_bytes(), SymbolLanguage::Rust, range(4, 5)), Err(DelimiterError::InvalidSelection));
    assert_eq!(run(b"()", SymbolLanguage::Rust, range(0, 2)), Err(DelimiterError::InvalidSelection));
}

#[test]
fn malformed_encoding_and_unqualified_lexical_boundaries_fail_conservatively() {
    assert_eq!(run(b"()\xff", SymbolLanguage::Rust, range(0, 1)), Err(DelimiterError::UnsupportedEncoding));
    assert_eq!(run(&[0xff, 0xfe, b'(', 0, b')', 0, 0, 0xd8], SymbolLanguage::Rust, range(2, 4)),
        Err(DelimiterError::UnsupportedEncoding));
    for (language, source) in [
        (SymbolLanguage::Rust, "() /* never closed"),
        (SymbolLanguage::Rust, "() \"never closed"),
        (SymbolLanguage::Rust, "() cr#\"unsupported\"#"),
        (SymbolLanguage::Python, "() 'broken\nnext = (1)"),
        (SymbolLanguage::Java, "() // \\u000a }"),
        (SymbolLanguage::Cpp, "() // continued \\\n }"),
        (SymbolLanguage::Cpp, "() ; int main() <% { %> }"),
        (SymbolLanguage::Cpp, "() ; int values<:1:> = { 1 };"),
        (SymbolLanguage::Cpp, "() ; int \\u0061 = 1;"),
        (SymbolLanguage::JavaScript, "() // comment\r }"),
        (SymbolLanguage::JavaScript, "() `value ${/}/.test(x)}`"),
        (SymbolLanguage::Swift, "() ; let pattern = /[a-z]/"),
        (SymbolLanguage::Swift, "() ; let pattern = #/[a-z]/#"),
    ] {
        assert_eq!(run(source.as_bytes(), language, range(0, 1)), Err(DelimiterError::UnsupportedLexicalContext), "{}", language.name());
    }
}

#[test]
fn large_source_depth_admission_extents_and_cancellation_are_explicit() {
    let mut large = vec![b' '; 256 * 1024];
    large.extend_from_slice(b"([])");
    let open = large.len() - 4;
    assert_eq!(run(&large, SymbolLanguage::Rust, range(open, open + 1)).unwrap().matching_range(),
        Some(range(large.len() - 1, large.len())));
    assert_eq!(run(&vec![b'('; MAX_DELIMITER_SOURCE_BYTES + 1], SymbolLanguage::Rust, range(0, 1)), Err(DelimiterError::SourceLimit));
    assert_eq!(run(&vec![b'('; MAX_DELIMITER_DEPTH + 1], SymbolLanguage::Rust, range(0, 1)), Err(DelimiterError::ComplexityLimit));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(matching_delimiter(b"()", request(), SymbolLanguage::Rust, None, range(0, 1), &tiny,
        ResourceAllocationId::new(1).unwrap(), || false), Err(DelimiterError::ResourceDenied));
    let budget = budget();
    assert_eq!(matching_delimiter(b"()", request().with_range(range(0, 2)).unwrap(), SymbolLanguage::Rust,
        None, range(0, 1), &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(DelimiterError::InvalidEvidence));
    for stop in 1..=8 {
        let mut polls = 0;
        assert_eq!(matching_delimiter(b"fn run() { ([]) }", request(), SymbolLanguage::Rust, None, range(9, 10),
            &budget, ResourceAllocationId::new(stop).unwrap(), || { polls += 1; polls == stop }), Err(DelimiterError::Canceled));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
