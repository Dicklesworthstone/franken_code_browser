#![forbid(unsafe_code)]

//! Production C/C++ navigation over pinned FMD tokens. These fixtures verify
//! declaration evidence and bounded refusal; they are not compiler qualification.
use fcb_analysis::{OutlineItemKind, symbols::{
    CapturedSymbols, SymbolError, SymbolLanguage, MAX_SYMBOL_SOURCE_BYTES,
}};
use fcb_core::{ArenaOwnerId, ByteLength, QueryGeneration, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(9531).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn build<'source>(source: &'source [u8], limit: usize, budget: &ResourceBudget) -> CapturedSymbols<'source> {
    CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Cpp, None, limit,
        budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
}
fn names<'outline>(outline: &'outline CapturedSymbols<'_>) -> Vec<&'outline str> {
    outline.candidates().iter().map(|candidate| candidate.name()).collect()
}
fn refusal(source: &[u8]) -> SymbolError {
    let budget = budget();
    let result = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Cpp,
        None, 128, &budget, ResourceAllocationId::new(1).unwrap(), || false);
    let error = result.err().expect("unsupported input must be refused");
    assert_eq!(budget.accounting().reserved().get(), 0);
    error
}

#[test]
fn c_and_cpp_file_routes_share_exact_declaration_navigation() {
    for name in ["c", "h", "cpp", "cc", "cxx", "hpp"] {
        assert_eq!(SymbolLanguage::from_name(name), Some(SymbolLanguage::Cpp));
    }
    assert_eq!(SymbolLanguage::Cpp.name(), "cpp");
    assert_eq!(SymbolLanguage::from_path(b"native/\xffview.CPP"), Some(SymbolLanguage::Cpp));
    let source = b"int open(int value) { return value; }\nextern int close(void);\n";
    let budget = budget();
    let outline = build(source, 16, &budget);
    assert_eq!(names(&outline), ["open", "close"]);
    assert_eq!(outline.source_bytes(1).unwrap(), b"int open(int value) { return value; }");
    assert_eq!(outline.source_bytes(2).unwrap(), b"extern int close(void);");
    assert_eq!(outline.name_bytes(1).unwrap().unwrap(), b"open");
    assert_eq!(outline.candidates()[1].line(), 2);
}

#[test]
fn named_scopes_methods_and_constructor_initializers_keep_exact_hierarchy() {
    let source = br#"
namespace kit {
class Widget {
public:
    Widget(int n) : value{n}, cache(make()) { struct Local {}; }
    ~Widget() noexcept = default;
    int open(int n) const { return n; }
protected:
    struct State { void reset(); };
private:
    int value;
};
int helper(void);
}
"#;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["kit", "Widget", "Widget", "~Widget", "open", "State", "reset", "helper"]);
    let kinds: Vec<_> = outline.candidates().iter().map(|c| c.kind().label()).collect();
    assert_eq!(kinds, ["module", "class", "initializer", "deinitializer", "method", "struct", "method", "function"]);
    let parents: Vec<_> = outline.candidates().iter().map(|c| c.parent_id()).collect();
    assert_eq!(parents, [None, Some(1), Some(2), Some(2), Some(2), Some(2), Some(6), Some(1)]);
    assert_eq!(outline.candidates()[6].depth(), 3);
    assert_eq!(outline.source_bytes(3).unwrap(),
        b"Widget(int n) : value{n}, cache(make()) { struct Local {}; }");
    assert_eq!(outline.source_bytes(5).unwrap(), b"int open(int n) const { return n; }");
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
fn forward_declarations_namespace_aliases_enums_and_simple_types_stop_at_their_own_boundaries() {
    let source = br#"
namespace outer::inner {
    struct Later;
    enum class Mode : unsigned { first, second };
    using Name = const char*;
    typedef unsigned long Count;
    typedef char Buffer[16];
    typedef struct Record { void run(); } RecordAlias;
}
namespace alias = outer::inner;
int after();
"#;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["outer::inner", "Later", "Mode", "Name", "Count", "Buffer",
        "Record", "run", "alias", "after"]);
    assert_eq!(outline.source_bytes(2).unwrap(), b"struct Later;");
    assert_eq!(outline.source_bytes(9).unwrap(), b"namespace alias = outer::inner;");
    assert_eq!(outline.candidates()[3].kind(), &OutlineItemKind::TypeAlias);
    assert_eq!(outline.candidates()[7].parent_id(), Some(7));
    assert_eq!(outline.candidates()[9].parent_id(), None);
}

#[test]
fn templates_attributes_default_parameters_and_linkage_keep_function_evidence() {
    let source = br#"
extern "C" {
    int c_api(int value);
}
namespace {
    [[nodiscard]] inline int hidden(int value = factory({1, 2})) noexcept { return value; }
}
template<typename T>
T echo(const T& value) { return value; }
auto transform(std::vector<std::pair<int, int>> values) -> int { return 0; }
"#;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["c_api", "hidden", "echo", "transform"]);
    assert!(outline.candidates().iter().all(|c| c.parent_id().is_none() && c.depth() == 0));
    assert!(outline.source_bytes(2).unwrap().starts_with(b"[[nodiscard]] inline"));
    assert!(outline.source_bytes(3).unwrap().starts_with(b"template<typename T>"));
    assert!(outline.source_bytes(4).unwrap().ends_with(b"{ return 0; }"));
}

#[test]
fn qualified_callables_do_not_invent_class_membership_or_parent_identity() {
    let source = br#"
int demo::open(int value) { return value; }
Widget::Widget(int value) : value{value} {}
Widget::~Widget() {}
int Widget::read() const { return value; }
"#;
    let budget = budget();
    let outline = build(source, 16, &budget);
    assert_eq!(names(&outline), ["open", "Widget", "~Widget", "read"]);
    assert_eq!(outline.candidates()[0].kind(), &OutlineItemKind::Function);
    assert_eq!(outline.candidates()[1].kind(), &OutlineItemKind::Initializer);
    assert_eq!(outline.candidates()[2].kind(), &OutlineItemKind::Deinitializer);
    assert_eq!(outline.candidates()[3].kind(), &OutlineItemKind::Function);
    assert!(outline.candidates().iter().all(|c| c.parent_id().is_none()));
}

#[test]
fn owner_tokens_hide_comment_string_raw_macro_and_function_body_decoys() {
    let source = br####"
/*
class Ghost { int fake(); };
int comment_fake() {}
*/
const char* quoted = "struct Quoted {}; int quoted_fake();";
const char* raw = u8R"tag(
class Raw { int raw_fake(); };
#define INNER struct Another {};
)tag";
/* leading comment */ #define MAKE struct MacroGhost { int macro_fake(); };
/* multiline comment
 */ # define STILL_FAKE int spaced_macro_fake() { struct Nested {}; }
# include <missing.h>
#if 0
int inactive_branch();
#else
int active_branch();
#endif
int real() { struct Local {}; auto local = [] { return 1; }; return local(); }
"####;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["inactive_branch", "active_branch", "real"]);
    assert!(outline.candidates().iter().all(|c| c.evidence_level() == "heuristic-outline-candidate"));
}

#[test]
fn initializers_lambdas_function_pointers_operators_and_unsupported_type_bodies_stay_opaque() {
    let source = br#"
typedef int (*Callback)(int);
int (*ptr)(int);
auto closure = []() { struct Hidden {}; return 1; };
int result = helper();
Widget value(42);
union Storage { struct HiddenUnion {}; int fake(); };
typedef struct { int anonymous_fake(); } Anonymous;
bool operator==(const Widget& left, const Widget& right) { return true; }
template<typename T>
int constrained(T value) requires Enabled<T> { return 1; }
int visible();
"#;
    let budget = budget();
    let outline = build(source, 128, &budget);
    assert_eq!(names(&outline), ["visible"]);
}

#[test]
fn original_utf16_and_utf8_bom_ranges_survive_unicode_declarations() {
    let text = "// 😀\r\nnamespace café {\r\n    int ouvrir(int value) { return value; }\r\n}\n";
    let start = text.find("ouvrir").unwrap();
    for encoding in 0..3 {
        let raw = if encoding == 2 { [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat() }
            else {
                let mut raw = if encoding == 0 { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
                for unit in text.encode_utf16() {
                    raw.extend_from_slice(&if encoding == 0 { unit.to_le_bytes() } else { unit.to_be_bytes() });
                }
                raw
            };
        let expected: Vec<u8> = if encoding == 2 { b"ouvrir".to_vec() }
            else { "ouvrir".encode_utf16().flat_map(|unit|
                if encoding == 0 { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect() };
        let budget = budget();
        let outline = build(&raw, 16, &budget);
        assert_eq!(names(&outline), ["café", "ouvrir"]);
        let candidate = &outline.candidates()[1];
        let expected_start = if encoding == 2 { 3 + start }
            else { 2 + 2 * text[..start].encode_utf16().count() };
        assert_eq!(candidate.line(), 3);
        assert_eq!(candidate.name_range().unwrap().start().get(), expected_start as u64);
        assert_eq!(outline.name_bytes(candidate.id()).unwrap().unwrap(), expected);
    }
}

#[test]
fn output_limits_report_actual_omission_and_keep_valid_parent_ids() {
    let budget = budget();
    let outline = build(b"namespace N { struct S { int open(); }; }", 2, &budget);
    assert_eq!(names(&outline), ["N", "S"]);
    assert_eq!(outline.candidates()[1].parent_id(), Some(1));
    assert!(outline.output_limited());
    drop(outline);
    let exact = build(b"namespace N { int only(); }", 2, &budget);
    assert_eq!(names(&exact), ["N", "only"]);
    assert!(!exact.output_limited());
}

#[test]
fn active_nesting_is_bounded_but_sequential_bodies_and_opaque_braces_are_not_depth() {
    let deep = format!("{}{}", "struct Layer {\n".repeat(129), "};\n".repeat(129));
    assert_eq!(refusal(deep.as_bytes()), SymbolError::ComplexityLimit);
    let source = format!("/* {} */\nconst char* text = \"{}\";\n{}",
        "{".repeat(300), "}".repeat(300), "int next() { return 1; }\n".repeat(180));
    let budget = budget();
    let outline = build(source.as_bytes(), 256, &budget);
    assert_eq!(outline.candidates().len(), 180);
    assert!(outline.candidates().iter().all(|c| c.name() == "next"));
    assert!(!outline.output_limited());
    assert_eq!(refusal(&vec![b'x'; MAX_SYMBOL_SOURCE_BYTES + 1]), SymbolError::SourceLimit);
    assert_eq!(refusal(format!("{}int outside();", "\n".repeat(65_536)).as_bytes()), SymbolError::ComplexityLimit);
}

#[test]
fn unsupported_translation_phases_raw_delimiters_and_digraphs_refuse_atomically() {
    for source in [
        b"// continued \\\nint fake();\nint after();".as_slice(),
        b"// continued \\ \t\r\nint fake();",
        b"// before\rint fake();",
        b"// trigraph ??/\nint fake();",
        b"struct Hidden <% int fake(); %>; int after();",
        b"int array<:4:>; int after();",
        b"int \\u0066ake(); int after();",
        b"const char* raw = R\"punct!(\nclass Fake {};\n)punct!\"; int after();",
        b"#define VALUE R\"tag(\nopaque\n)tag\"; int macro_tail();\nint after();",
        b"#          \nint swallowed();",
    ] {
        assert_eq!(refusal(source), SymbolError::UnsupportedLexicalContext);
    }
    let budget = budget();
    let permitted = build(br#"const char* raw = R"tag(<% %> \u0066 ??x)tag"; int real();"#, 16, &budget);
    assert_eq!(names(&permitted), ["real"]);
}

#[test]
fn malformed_delimiters_and_encoding_publish_no_partial_inventory() {
    for source in [
        b"int valid(); struct Missing {".as_slice(),
        b"int valid(); struct Wrong { int f(]; };",
        b"int valid(); }",
        b"int valid(); int broken() { return values[0; }",
    ] {
        assert_eq!(refusal(source), SymbolError::InvalidEvidence);
    }
    assert_eq!(refusal(b"int valid();\xff"), SymbolError::UnsupportedEncoding);
    let budget = budget();
    let empty = build(b"", 16, &budget);
    assert!(empty.candidates().is_empty() && !empty.no_recognized_declarations());
    drop(empty);
    let comment = build(b"/* int hidden();", 16, &budget);
    assert!(comment.candidates().is_empty() && comment.no_recognized_declarations());
    drop(comment);
    let trailing = build(b"int valid(); /* unfinished class Hidden {", 16, &budget);
    assert_eq!(names(&trailing), ["valid"]);
}

#[test]
fn malformed_callable_headers_cannot_borrow_a_later_declarations_body() {
    let source = b"int broken() int stolen() {}\nint honest();\n";
    let budget = budget();
    let outline = build(source, 16, &budget);
    assert_eq!(names(&outline), ["honest"]);
}

#[test]
fn every_observed_cancellation_checkpoint_releases_reserved_inventory() {
    let source = b"namespace N { struct Item { Item(int x):x{x}{} int open(int n){return n;} }; }\nint top();";
    let budget = budget();
    let mut checks = 0;
    let probe = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Cpp,
        None, 32, &budget, ResourceAllocationId::new(1).unwrap(), || { checks += 1; false }).unwrap();
    drop(probe);
    assert!(checks > 10);
    for stop in 1..=checks {
        let mut visited = 0;
        let result = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Cpp,
            None, 32, &budget, ResourceAllocationId::new(1).unwrap(), || {
                visited += 1; visited == stop
            });
        assert!(matches!(result, Err(SymbolError::Canceled)), "checkpoint {stop}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
