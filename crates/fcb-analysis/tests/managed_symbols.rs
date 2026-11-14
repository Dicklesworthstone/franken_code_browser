#![forbid(unsafe_code)]

//! Public captured-symbol routes, with independent expected declarations and
//! byte slices. No replacement lexer, simulated compiler, or test-only parser.
use fcb_analysis::{OutlineItemKind, symbols::{CapturedSymbols, SymbolError, SymbolLanguage, MAX_SYMBOL_SOURCE_BYTES}};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(9531).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn build<'a>(source: &'a [u8], language: SymbolLanguage, limit: usize, budget: &ResourceBudget) -> CapturedSymbols<'a> {
    CapturedSymbols::build(source, request(), generation(), language, None, limit,
        budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
}
fn names<'a>(symbols: &'a CapturedSymbols<'_>) -> Vec<&'a str> {
    symbols.candidates().iter().map(|candidate| candidate.name()).collect()
}
fn refusal(source: &[u8], language: SymbolLanguage) -> SymbolError {
    let budget = budget();
    let error = CapturedSymbols::build(source, request(), generation(), language, None, 128,
        &budget, ResourceAllocationId::new(1).unwrap(), || false).err().expect("expected explicit refusal");
    assert_eq!(budget.accounting().reserved().get(), 0);
    error
}
fn check_utf8_evidence(symbols: &CapturedSymbols<'_>) {
    for candidate in symbols.candidates() {
        assert_eq!(symbols.name_bytes(candidate.id()).unwrap().unwrap(), candidate.name().as_bytes());
        assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        let range = candidate.original_range();
        let name = candidate.name_range().unwrap();
        assert!(range.start() <= name.start() && name.end() <= range.end());
        if let Some(parent) = candidate.parent_id() {
            let parent = symbols.candidate(parent).unwrap();
            assert_eq!(candidate.depth(), parent.depth() + 1);
            assert!(parent.original_range().start() <= range.start() && range.end() <= parent.original_range().end());
        }
    }
}

#[test]
fn language_names_aliases_and_raw_filename_suffixes_route_without_lossy_paths() {
    for (name, language) in [("java", SymbolLanguage::Java), ("csharp", SymbolLanguage::CSharp),
        ("cs", SymbolLanguage::CSharp), ("c#", SymbolLanguage::CSharp)] {
        assert_eq!(SymbolLanguage::from_name(name), Some(language));
    }
    assert_eq!(SymbolLanguage::Java.name(), "java");
    assert_eq!(SymbolLanguage::CSharp.name(), "csharp");
    assert_eq!(SymbolLanguage::from_path(b"src/\xffMain.JAVA"), Some(SymbolLanguage::Java));
    assert_eq!(SymbolLanguage::from_path(b"src/\xffMain.CS"), Some(SymbolLanguage::CSharp));
    for path in [b"Main.java.txt".as_slice(), b"Main.class", b"Main.jar", b"Main.csproj", b"Main.csx", b"Main.dll"] {
        assert_eq!(SymbolLanguage::from_path(path), None);
    }
}

#[test]
fn java_types_methods_constructors_records_and_annotation_members_preserve_hierarchy() {
    let source = br#"
package sample.browser;
import java.util.List;
@Deprecated
public sealed class Browser<T> extends Base implements Port permits Child {
    static { class InitializerLocal {} }
    public Browser(String name) { this.name = name; }
    @Override
    public <U extends Comparable<U>> List<U[]> run(U input) throws java.io.IOException {
        class MethodLocal { void hidden() {} }
        return null;
    }
    interface Port { String read(); default void reset() {} }
    record Point(int x, int y) {
        Point { if (x < 0) throw new IllegalArgumentException(); }
        int total() { return x + y; }
    }
    enum Mode { A { void constantLocal() {} }, B; public void clear() {} }
    @interface Flag { String value() default "class Fake {}"; int[] codes() default {1, 2}; }
}
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::Java, 128, &budget);
    assert_eq!(names(&symbols), ["Browser", "Browser", "run", "Port", "read", "reset", "Point", "Point",
        "total", "Mode", "clear", "Flag", "value", "codes"]);
    assert_eq!(symbols.candidates().iter().map(|c| c.kind().label()).collect::<Vec<_>>(),
        ["class", "initializer", "method", "interface", "method", "method", "class", "initializer",
         "method", "enum", "method", "interface", "method", "method"]);
    assert_eq!(symbols.candidates()[2].parent_id(), Some(1));
    assert_eq!(symbols.candidates()[4].parent_id(), Some(4));
    assert_eq!(symbols.candidates()[8].parent_id(), Some(7));
    assert_eq!(symbols.candidates()[10].parent_id(), Some(10));
    assert_eq!(symbols.candidates()[13].parent_id(), Some(12));
    assert!(!symbols.output_limited());
    check_utf8_evidence(&symbols);
}

#[test]
fn csharp_file_namespace_records_generic_methods_and_properties_are_navigable() {
    let source = br#"
using System;
namespace Sample.Browser;
[Obsolete("class AttributeText {}")]
public partial class Store<T> : Base, IDisposable where T : class, new() {
    public Store() : base() {}
    public Store(int seed) : this() {}
    public T Current { get; private init; } = new T();
    public string Name => "class StringText {}";
    public int Count { get => 1; set { void AccessorLocal() {} } }
    public U Map<U>(T value) where U : class, new() => new U();
    int IRunner.Run() { return 1; }
    public (int X, int Y) Pair() => (1, 2);
    public void @event() { void BodyLocal() {} }
    public readonly record struct Point(int X, int Y) { public int Sum() => X + Y; }
    public interface Port { void Send(); }
}
public record Row<T>(T Value) where T : class;
namespace Nested { public struct Value { public bool Ready { get; set; } } }
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::CSharp, 128, &budget);
    assert_eq!(names(&symbols), ["Sample.Browser", "Store", "Store", "Store", "Current", "Name", "Count", "Map",
        "Run", "Pair", "event", "Point", "Sum", "Port", "Send", "Row", "Nested", "Value", "Ready"]);
    let kinds: Vec<_> = symbols.candidates().iter().map(|c| c.kind().label()).collect();
    assert_eq!(kinds, ["module", "class", "initializer", "initializer", "property", "property", "property", "method",
        "method", "method", "method", "struct", "method", "interface", "method", "class", "module", "struct", "property"]);
    assert_eq!(symbols.candidates()[1].parent_id(), Some(1));
    assert_eq!(symbols.candidates()[7].parent_id(), Some(2));
    assert_eq!(symbols.candidates()[12].parent_id(), Some(12));
    assert_eq!(symbols.candidates()[18].parent_id(), Some(18));
    assert_eq!(symbols.name_bytes(11).unwrap().unwrap(), b"event");
    check_utf8_evidence(&symbols);
}

#[test]
fn shared_lexer_and_member_context_exclude_calls_fields_strings_comments_and_local_types() {
    let java = br####"
// class Comment { void hidden() {} }
class Real {
    String multiline = """
        class TextBlock { void fake() {} }
        """;
    Runnable callback = () -> { class LambdaLocal {} };
    Object anonymous = new Object() { void AnonymousLocal() {} };
    Thing.call();
    void run() { class BodyLocal {} }
}
"####;
    let csharp = br####"
#region class DirectiveText { void fake() {} }
namespace Sample {
    // class Comment {}
    class Real {
        string verbatim = @"class VerbatimText {}";
        string raw = """class RawText { void fake() {} }""";
        string interpolated = $"class InterpolationText {{{Read("inner")}}}";
        Func<int> callback = () => { class LambdaLocal {} return 1; };
        Thing.Call();
        void Run() { class BodyLocal {} }
    }
}
#endregion
"####;
    for (language, source, expected) in [(SymbolLanguage::Java, java.as_slice(), vec!["Real", "run"]),
        (SymbolLanguage::CSharp, csharp.as_slice(), vec!["Sample", "Real", "Run"])] {
        let budget = budget();
        let symbols = build(source, language, 128, &budget);
        assert_eq!(names(&symbols), expected, "calls and opaque bodies must not become declaration candidates");
        check_utf8_evidence(&symbols);
    }
}

#[test]
fn csharp_conditional_candidates_remain_heuristic_and_do_not_evaluate_symbols() {
    let source = b"#if FEATURE\nclass Chosen { void Run() {} }\n#else\nclass Other { void Run() {} }\n#endif\n";
    let budget = budget();
    let symbols = build(source, SymbolLanguage::CSharp, 16, &budget);
    assert_eq!(names(&symbols), ["Chosen", "Run", "Other", "Run"]);
    assert_ne!(symbols.candidates()[1].id(), symbols.candidates()[3].id());
    assert_ne!(symbols.candidates()[1].parent_id(), symbols.candidates()[3].parent_id());
    assert!(symbols.candidates().iter().all(|c| c.evidence_level() == "heuristic-outline-candidate"));
}

#[test]
fn utf16_endian_and_utf8_bom_ranges_select_original_units_for_unicode_and_escaped_names() {
    for (language, text, expected_name) in [
        (SymbolLanguage::Java, "// 😀\r\nclass Café {\r\n    void ouvrir() {}\r\n}\n", "ouvrir"),
        (SymbolLanguage::CSharp, "// 😀\r\nclass Café {\r\n    void @class() {}\r\n}\n", "class"),
    ] {
        let start = text.rfind(expected_name).unwrap();
        for encoding in 0..3 {
            let raw = if encoding == 2 { [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat() }
                else {
                    let mut raw = if encoding == 0 { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
                    for unit in text.encode_utf16() {
                        raw.extend_from_slice(&if encoding == 0 { unit.to_le_bytes() } else { unit.to_be_bytes() });
                    }
                    raw
                };
            let expected: Vec<u8> = if encoding == 2 { expected_name.as_bytes().to_vec() }
                else { expected_name.encode_utf16().flat_map(|unit|
                    if encoding == 0 { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect() };
            let budget = budget();
            let symbols = build(&raw, language, 16, &budget);
            assert_eq!(names(&symbols), ["Café", expected_name]);
            let candidate = &symbols.candidates()[1];
            assert_eq!(candidate.line(), 3);
            let expected_start = if encoding == 2 { 3 + start } else { 2 + 2 * text[..start].encode_utf16().count() };
            assert_eq!(candidate.name_range().unwrap().start().get(), expected_start as u64);
            assert_eq!(symbols.name_bytes(candidate.id()).unwrap().unwrap(), expected);
        }
    }
}

#[test]
fn unsupported_upstream_lexical_context_is_a_named_refusal_not_an_empty_inventory() {
    for source in [br"class Esc\u0061ped {}".as_slice(), br"// \u000a class Hidden {}", br#"class A { String s = "\u0022"; }"#] {
        assert_eq!(refusal(source, SymbolLanguage::Java), SymbolError::UnsupportedLexicalContext);
    }
    assert_eq!(refusal(br"class Esc\u0061ped {}", SymbolLanguage::CSharp), SymbolError::UnsupportedLexicalContext);
    for language in [SymbolLanguage::Java, SymbolLanguage::CSharp] {
        assert_eq!(refusal(b"// comment\rclass Hidden {}", language), SymbolError::UnsupportedLexicalContext);
    }
    assert_eq!(refusal("// comment\u{2028}class Hidden {}".as_bytes(), SymbolLanguage::CSharp), SymbolError::UnsupportedLexicalContext);
    assert_eq!(SymbolError::UnsupportedLexicalContext.code(), "SYMBOL_UNSUPPORTED_LEXICAL_CONTEXT");
    let budget = budget();
    let symbols = build(br#"class A { string text = "\u0041"; void Run() {} }"#, SymbolLanguage::CSharp, 16, &budget);
    assert_eq!(names(&symbols), ["A", "Run"]);
}

#[test]
fn incomplete_headers_do_not_borrow_later_method_bodies_and_delimiters_fail_closed() {
    for language in [SymbolLanguage::Java, SymbolLanguage::CSharp] {
        let budget = budget();
        let symbols = build(b"class A { void broken() void later() {} void valid() {} }", language, 16, &budget);
        assert_eq!(names(&symbols), ["A", "valid"]);
        for source in [b"class Broken { void run() {".as_slice(), b"class Wrong { void run(] {} }", b"}"] {
            assert_eq!(refusal(source, language), SymbolError::InvalidEvidence);
        }
    }
}

#[test]
fn source_nesting_output_and_cancel_limits_preserve_truth_and_release_reservations() {
    for language in [SymbolLanguage::Java, SymbolLanguage::CSharp] {
        let deep = format!("{}{}", "class Layer {".repeat(129), "}".repeat(129));
        assert_eq!(refusal(deep.as_bytes(), language), SymbolError::ComplexityLimit);
        assert_eq!(refusal(&vec![b' '; MAX_SYMBOL_SOURCE_BYTES + 1], language), SymbolError::SourceLimit);
        let source = format!("/* {} */ class A {{ void Run() {{}} }}", "{".repeat(256));
        let budget = budget();
        let limited = build(source.as_bytes(), language, 1, &budget);
        assert_eq!(names(&limited), ["A"]);
        assert!(limited.output_limited());
        drop(limited);
        let exact = build(b"class A {}", language, 1, &budget);
        assert!(!exact.output_limited());
        assert_eq!(exact.candidates()[0].kind(), &OutlineItemKind::Class);
        drop(exact);
        let mut checkpoints = 0;
        let result = CapturedSymbols::build(source.as_bytes(), request(), generation(), language, None, 16,
            &budget, ResourceAllocationId::new(1).unwrap(), || { checkpoints += 1; checkpoints > 10 });
        assert_eq!(result.err(), Some(SymbolError::Canceled));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn activation_cannot_relabel_replaced_bytes_or_a_new_query() {
    for language in [SymbolLanguage::Java, SymbolLanguage::CSharp] {
        let budget = budget();
        let symbols = build(b"class A { void Run() {} }", language, 16, &budget);
        assert_eq!(symbols.validate_source(b"class B { void Run() {} }", request().file(), request().revision(), generation()),
            Err(SymbolError::StaleSource));
        assert_eq!(symbols.validate_delivery(request().file(), request().revision(), QueryGeneration::new(owner(), 2).unwrap()),
            Err(SymbolError::StaleQuery));
        assert_eq!(symbols.name_bytes(2).unwrap().unwrap(), b"Run");
    }
}
