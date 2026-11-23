#![forbid(unsafe_code)]

use fcb_analysis::symbols::{CapturedSymbols, SymbolError, SymbolLanguage};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(859).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap()
}
fn build<'source>(source: &'source [u8], language: SymbolLanguage,
    budget: &ResourceBudget, canceled: impl FnMut() -> bool)
    -> Result<CapturedSymbols<'source>, SymbolError> {
    CapturedSymbols::build(source, request(), QueryGeneration::new(owner(), 1).unwrap(),
        language, None, 32, budget, ResourceAllocationId::new(1).unwrap(), canceled)
}

#[derive(Clone, Copy, Debug)]
enum Encoding { Utf8, Utf8Bom, Utf16Le, Utf16Be }
impl Encoding {
    fn bytes(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf8Bom => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
            Self::Utf16Le | Self::Utf16Be => {
                let little = matches!(self, Self::Utf16Le);
                let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
                }
                bytes
            }
        }
    }
    fn original_offset(self, text: &str, decoded_offset: usize) -> u64 {
        match self {
            Self::Utf8 => decoded_offset as u64,
            Self::Utf8Bom => decoded_offset as u64 + 3,
            Self::Utf16Le | Self::Utf16Be => 2 + 2 * text[..decoded_offset].encode_utf16().count() as u64,
        }
    }
}

#[test]
fn mixed_line_endings_preserve_symbol_lines_and_original_evidence_in_every_encoding() {
    // Block comments let the upstream lexer keep its own language semantics:
    // physical source lines still count inside an opaque comment or literal.
    let text = "/* 🧭\rsecond */\rfn first() {}\r\n\rfn second() {}\nfn third() {}\r";
    for encoding in [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be] {
        let source = encoding.bytes(text);
        let budget = budget();
        let outline = build(&source, SymbolLanguage::Rust, &budget, || false).unwrap();
        assert_eq!(outline.candidates().len(), 3, "{encoding:?}");
        for (candidate, (name, line)) in outline.candidates().iter()
            .zip([("first", 3), ("second", 5), ("third", 6)]) {
            assert_eq!(candidate.name(), name);
            assert_eq!(candidate.line(), line, "{encoding:?}, {name}");
            let declaration = format!("fn {name}() {{}}");
            let start = text.find(&declaration).unwrap();
            let end = start + declaration.len();
            assert_eq!(candidate.original_range().start().get(), encoding.original_offset(text, start));
            assert_eq!(candidate.original_range().end().get(), encoding.original_offset(text, end));
            let name_range = candidate.name_range().unwrap();
            assert_eq!(name_range.start().get(), encoding.original_offset(text, start + 3));
            assert_eq!(name_range.end().get(), encoding.original_offset(text, start + 3 + name.len()));
            let (start, end) = candidate.original_range().as_usize_bounds().unwrap();
            assert_eq!(outline.source_bytes(candidate.id()).unwrap(), &source[start..end]);
            assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        }
        // Negative control: the former LF-only count must fail this fixture.
        let second = text.find("fn second").unwrap();
        let lf_only = text[..second].bytes().filter(|&byte| byte == b'\n').count() as u64 + 1;
        assert_ne!(outline.candidates()[1].line(), lf_only);
        drop(outline);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn exactly_4096_physical_lines_are_admitted_with_or_without_a_terminal_newline() {
    for separator in ["\n", "\r", "\r\n"] {
        for terminal in ["", separator] {
            let source = format!("{}fn edge() {{}}{terminal}", separator.repeat(4095));
            let budget = budget();
            let outline = build(source.as_bytes(), SymbolLanguage::Rust, &budget, || false).unwrap();
            assert_eq!(outline.candidates().len(), 1);
            assert_eq!(outline.candidates()[0].name(), "edge");
            assert_eq!(outline.candidates()[0].line(), 4096);
            assert_eq!(outline.candidates()[0].original_range().start().get(), (4095 * separator.len()) as u64);
            assert!(!outline.output_limited());
            drop(outline);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }
}

#[test]
fn bare_cr_cannot_bypass_the_same_line_limit_as_lf_or_crlf() {
    for separator in ["\n", "\r", "\r\n"] {
        let source = format!("{}fn outside() {{}}", separator.repeat(4096));
        let budget = budget();
        assert!(matches!(build(source.as_bytes(), SymbolLanguage::Rust, &budget, || false),
            Err(SymbolError::ComplexityLimit)), "separator {separator:?}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn shared_physical_line_mapping_does_not_relax_language_specific_lexical_refusals() {
    for (source, language) in [
        (b"def example():\r    pass\r".as_slice(), SymbolLanguage::Python),
        (b"int example() {}\r".as_slice(), SymbolLanguage::Cpp),
    ] {
        let budget = budget();
        assert!(matches!(build(source, language, &budget, || false),
            Err(SymbolError::UnsupportedLexicalContext)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn empty_and_terminal_only_sources_do_not_fabricate_declarations() {
    for source in [b"".as_slice(), b"\n", b"\r", b"\r\n", b"\r\n\r\n", b"\n\r"] {
        let budget = budget();
        let outline = build(source, SymbolLanguage::Rust, &budget, || false).unwrap();
        assert!(outline.candidates().is_empty());
        assert!(!outline.output_limited());
        drop(outline);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn cancellation_during_line_mapping_releases_the_admitted_scratch() {
    let source = format!("{}fn later() {{}}", "\r\n".repeat(2048));
    let budget = budget();
    let mut polls = 0;
    let result = build(source.as_bytes(), SymbolLanguage::Rust, &budget, || {
        polls += 1;
        polls == 3
    });
    assert!(matches!(result, Err(SymbolError::Canceled)));
    assert_eq!(polls, 3);
    assert_eq!(budget.accounting().reserved().get(), 0);
}
