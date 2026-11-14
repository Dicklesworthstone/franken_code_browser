#![forbid(unsafe_code)]

//! Actual Swift declaration recognition over the pinned shared lexer. These
//! tests do not provide a mock parser or claim compiler-semantic qualification.
use fcb_analysis::{OutlineItemKind, symbols::{
    CapturedSymbols, SymbolError, SymbolLanguage, MAX_SYMBOL_SOURCE_BYTES,
}};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(9521).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn build<'source>(source: &'source [u8], limit: usize, budget: &ResourceBudget) -> CapturedSymbols<'source> {
    CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Swift, None, limit,
        budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
}
fn names<'outline>(outline: &'outline CapturedSymbols<'_>) -> Vec<&'outline str> {
    outline.candidates().iter().map(|candidate| candidate.name()).collect()
}
fn refusal(source: &[u8]) -> SymbolError {
    let budget = budget();
    let result = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Swift,
        None, 128, &budget, ResourceAllocationId::new(1).unwrap(), || false);
    let error = result.err().expect("unsupported input must be refused");
    assert_eq!(budget.accounting().reserved().get(), 0);
    error
}

#[test]
fn swift_filename_route_is_exact_and_does_not_claim_unrelated_extensions() {
    assert_eq!(SymbolLanguage::from_name("swift"), Some(SymbolLanguage::Swift));
    assert_eq!(SymbolLanguage::Swift.name(), "swift");
    assert_eq!(SymbolLanguage::from_path(b"native/\xffview.SWIFT"), Some(SymbolLanguage::Swift));
    for path in [b"swift".as_slice(), b"file.swif", b"file.swift.txt", b"file.swiftmodule"] {
        assert_eq!(SymbolLanguage::from_path(path), None);
    }
}

#[test]
fn swift_types_members_and_nested_hierarchy_keep_exact_names_and_kinds() {
    let source = br#"
struct Panel {
    let title: String
    var body: some View { Text(title) }
    struct State {
        var count = 0
        mutating func update() { count += 1 }
    }
    init(title: String) { self.title = title }
    subscript(index: Int) -> String { title }
}
actor Loader { func load() async -> String { "ready" } }
class Owner { deinit {} }
enum Mode { case compact, expanded }
extension Panel.State { func reset() {} }
typealias Handler = () -> Void
var running = false
let version = 1
"#;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["Panel", "title", "body", "State", "count", "update", "init", "subscript",
        "Loader", "load", "Owner", "deinit", "Mode", "Panel.State", "reset", "Handler", "running", "version"]);
    let kinds: Vec<_> = outline.candidates().iter().map(|c| c.kind().label()).collect();
    assert_eq!(kinds, ["struct", "property", "property", "struct", "property", "method", "initializer", "subscript",
        "actor", "method", "class", "deinitializer", "enum", "extension", "method", "type", "variable", "const"]);
    let state = &outline.candidates()[3];
    let update = &outline.candidates()[5];
    assert_eq!(state.parent_id(), Some(1));
    assert_eq!(update.parent_id(), Some(state.id()));
    assert_eq!(update.depth(), 2);
    for candidate in outline.candidates() {
        assert_eq!(outline.name_bytes(candidate.id()).unwrap().unwrap(), candidate.name().as_bytes());
        assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        let name = candidate.name_range().unwrap();
        let evidence = candidate.original_range();
        assert!(evidence.start() <= name.start() && name.end() <= evidence.end());
        if let Some(parent) = candidate.parent_id() {
            let parent = outline.candidate(parent).unwrap().original_range();
            assert!(parent.start() <= evidence.start() && evidence.end() <= parent.end());
        }
    }
    assert!(!outline.output_limited());
}

#[test]
fn shared_lexer_and_declaration_context_exclude_literals_imports_and_local_bodies() {
    let source = br####"
import class Foundation.NSObject
import func Foundation.NSLog
/* func comment() {}
   /* struct NestedComment {} */
*/
class Example {
    class func make() -> Example { Example() }
    class var shared: Example { Example() }
    @available(*, message: "func attributeText() { }")
    func `class`() {
        func localFunction() {}
        let text = "func localString() {}"
    }
    let closure = {
        func closureLocal() {}
        return "class Hidden {}"
    }()
}
let raw = ##"func rawString() {} // still a string"##
let multiline = #"""
struct MultilineString {
    func nestedString() {}
}
"""#
let interpolated = "prefix \(String("func interpolationString() {}")) suffix"
NSObject.init()
#if DEBUG
func visible() {}
#endif
"####;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["Example", "make", "shared", "class", "closure", "raw",
        "multiline", "interpolated", "visible"]);
    let escaped = &outline.candidates()[3];
    assert_eq!(outline.name_bytes(escaped.id()).unwrap().unwrap(), b"class");
    assert!(outline.source_bytes(escaped.id()).unwrap().starts_with(b"func `class`"));
    assert_eq!(escaped.kind(), &OutlineItemKind::Method);
}

#[test]
fn protocol_requirements_and_multiline_generic_functions_are_navigable() {
    let source = br#"
protocol Store {
    associatedtype Value
    var current: Value { get }
    init(seed: Value)
    func load(
        _ key: String
    ) async throws -> Value
    subscript(key: String) -> Value { get }
    func reset()
}
extension Store {
    typealias ID = String
    func cached<T: Collection>(_ key: T)
        -> Value
        where T.Element: Equatable
    { fatalError() }
}
func ==(left: Int, right: Int) -> Bool { false }
"#;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["Store", "Value", "current", "init", "load", "subscript", "reset",
        "Store", "ID", "cached", "=="]);
    for candidate in &outline.candidates()[1..7] { assert_eq!(candidate.parent_id(), Some(1)); }
    let load = &outline.candidates()[4];
    assert_eq!(outline.source_bytes(load.id()).unwrap().last(), Some(&b'e'));
    assert!(!outline.source_bytes(load.id()).unwrap().windows(9).any(|bytes| bytes == b"subscript"));
    assert_eq!(outline.candidates()[9].kind(), &OutlineItemKind::Method);
    assert_eq!(outline.candidates()[10].kind(), &OutlineItemKind::Function);
}

#[test]
fn original_utf16_and_bom_offsets_survive_escaped_unicode_names() {
    let text = "// 😀\r\nstruct Café {\r\n    func `repeat`() {}\r\n}\n";
    let start = text.find("repeat").unwrap();
    for encoding in 0..3 {
        let raw = if encoding == 2 { [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat() }
        else {
            let mut raw = if encoding == 0 { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                raw.extend_from_slice(&if encoding == 0 { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            raw
        };
        let expected: Vec<u8> = if encoding == 2 { b"repeat".to_vec() }
            else { "repeat".encode_utf16().flat_map(|unit|
                if encoding == 0 { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect() };
        let budget = budget();
        let outline = build(&raw, 16, &budget);
        assert_eq!(names(&outline), ["Café", "repeat"]);
        let candidate = &outline.candidates()[1];
        assert_eq!(candidate.line(), 3);
        let expected_start = if encoding == 2 { 3 + start } else { 2 + 2 * text[..start].encode_utf16().count() };
        assert_eq!(candidate.name_range().unwrap().start().get(), expected_start as u64);
        assert_eq!(outline.name_bytes(candidate.id()).unwrap().unwrap(), expected);
    }
}

#[test]
fn output_limit_preserves_parent_ids_and_only_reports_actual_omission() {
    let budget = budget();
    let outline = build(b"struct Parent { func child() {} }", 1, &budget);
    assert_eq!(names(&outline), ["Parent"]);
    assert!(outline.output_limited());
    assert_eq!(outline.candidates()[0].parent_id(), None);
    drop(outline);
    let exact = build(b"func only() {}", 1, &budget);
    assert_eq!(names(&exact), ["only"]);
    assert!(!exact.output_limited());
}

#[test]
fn nesting_limits_ignore_opaque_literal_and_comment_braces() {
    let deep = format!("{}{}", "struct Layer {\n".repeat(129), "}\n".repeat(129));
    assert_eq!(refusal(deep.as_bytes()), SymbolError::ComplexityLimit);
    let source = format!("/* {} */\nlet text = \"{}\"\nfunc visible() {{}}\n",
        "{".repeat(300), "}".repeat(300));
    let budget = budget();
    let outline = build(source.as_bytes(), 16, &budget);
    assert_eq!(names(&outline), ["text", "visible"]);
    assert_eq!(refusal(&vec![b'x'; MAX_SYMBOL_SOURCE_BYTES + 1]), SymbolError::SourceLimit);
    let too_many_lines = "\n".repeat(4096);
    assert_eq!(refusal(too_many_lines.as_bytes()), SymbolError::ComplexityLimit);
}

#[test]
fn malformed_delimiters_and_encoding_never_publish_fabricated_ranges() {
    for source in [b"struct Missing {".as_slice(), b"struct Wrong { func f(] {} }",
        b"}\nfunc after() {}", b"func broken() { return [1, 2 }"] {
        assert_eq!(refusal(source), SymbolError::InvalidEvidence);
    }
    assert_eq!(refusal(b"func valid() {}\xff"), SymbolError::UnsupportedEncoding);
    let budget = budget();
    let empty = build(b"", 16, &budget);
    assert!(empty.candidates().is_empty() && !empty.no_recognized_declarations());
    drop(empty);
    let comment = build(b"// func ignored() {}\n", 16, &budget);
    assert!(comment.candidates().is_empty() && comment.no_recognized_declarations());
}

#[test]
fn every_observed_cancellation_checkpoint_releases_the_new_inventory() {
    let source = b"struct Item { var count = 0\nfunc change() {} }\nfunc top() {}";
    let budget = budget();
    let mut checks = 0;
    let probe = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Swift,
        None, 32, &budget, ResourceAllocationId::new(1).unwrap(), || { checks += 1; false }).unwrap();
    drop(probe);
    assert!(checks > 10);
    for stop in 1..=checks {
        let mut visited = 0;
        let result = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Swift,
            None, 32, &budget, ResourceAllocationId::new(1).unwrap(), || {
                visited += 1; visited == stop
            });
        assert!(matches!(result, Err(SymbolError::Canceled)), "checkpoint {stop}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn malformed_header_cannot_borrow_a_later_declarations_body() {
    let source = b"func broken() func stolen() {}\nfunc honest() {}\n";
    let budget = budget();
    let outline = build(source, 16, &budget);
    assert_eq!(names(&outline), ["honest"]);
}
