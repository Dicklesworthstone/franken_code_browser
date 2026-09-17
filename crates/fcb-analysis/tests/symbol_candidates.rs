#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;
use fcb_analysis::symbols::{CapturedSymbols, SymbolLanguage, SymbolNameMode};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(851).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }

#[test]
fn every_exposed_route_retains_actual_source_evidence_for_recognized_candidates() {
    let cases = [
        (SymbolLanguage::Rust, b"pub struct Thing {}\nfn hello() {}\n".as_slice(), "hello", "function"),
        (SymbolLanguage::Python, b"class Thing:\n    def hello(self):\n        pass\n", "hello", "method"),
        (SymbolLanguage::JavaScript, b"export function hello() {}\n", "hello", "function"),
        (SymbolLanguage::TypeScript, b"export interface Thing {}\n", "Thing", "interface"),
        (SymbolLanguage::Go, b"package example\nfunc (t *Thing) Hello() {}\n", "Hello", "method"),
        (SymbolLanguage::Cpp, b"namespace example {\nclass Thing {};\n}\n", "Thing", "class"),
    ];
    for (language, source, name, kind) in cases {
        let budget = budget();
        let symbols = CapturedSymbols::build(source, request(), generation(), language, None, 64,
            &budget, allocation(), || false).unwrap();
        let candidate = symbols.candidates().iter().find(|item| item.matches_name(name, SymbolNameMode::Exact)).unwrap();
        assert_eq!(candidate.kind().label(), kind);
        assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        let evidence = symbols.source_bytes(candidate.id()).unwrap();
        assert!(evidence.windows(name.len()).any(|window| window == name.as_bytes()));
        let (start, end) = candidate.original_range().as_usize_bounds().unwrap();
        assert_eq!(evidence, &source[start..end]);
        assert_eq!(candidate.line(), 1 + source[..start].iter().filter(|&&byte| byte == b'\n').count() as u64);
        drop(symbols);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn unsupported_forms_and_tiny_display_admission_cannot_imply_a_complete_definition_inventory() {
    let budget = budget();
    // The current C/C++ extractor recognizes classes/structs/namespaces only.
    // Keep this limitation visible; a C function is not a proven missing symbol.
    let source = b"int ordinary_function(void) { return 1; }\n";
    let symbols = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Cpp, None, 64,
        &budget, allocation(), || false).unwrap();
    assert!(symbols.candidates().is_empty());
    assert!(symbols.no_recognized_declarations());
    drop(symbols);
    let source = b"def first():\n    pass\ndef second():\n    pass\n";
    let symbols = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Python, None, 1,
        &budget, allocation(), || false).unwrap();
    assert!(symbols.output_limited()); assert_eq!(symbols.candidates()[0].name(), "first");
    assert!(symbols.candidate(2).is_none());
}
