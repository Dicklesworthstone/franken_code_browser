#![forbid(unsafe_code)]

//! Public captured-symbol routes with independent expected declarations and
//! original-byte slices. These fixtures do not substitute a lexer or parser.
use fcb_analysis::symbols::{CapturedSymbols, SymbolError, SymbolLanguage, SymbolNameMode, MAX_SYMBOL_SOURCE_BYTES};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(9532).unwrap() }
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
fn check_utf8_evidence(symbols: &CapturedSymbols<'_>, source: &[u8]) {
    for candidate in symbols.candidates() {
        assert_eq!(symbols.name_bytes(candidate.id()).unwrap().unwrap(), candidate.name().as_bytes());
        assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        let range = candidate.original_range();
        let name = candidate.name_range().unwrap();
        assert!(range.start() <= name.start() && name.end() <= range.end());
        let (start, end) = range.as_usize_bounds().unwrap();
        assert_eq!(symbols.source_bytes(candidate.id()).unwrap(), &source[start..end]);
        assert_eq!(candidate.line(), 1 + source[..start].iter().filter(|&&b| b == b'\n').count() as u64);
        if let Some(parent) = candidate.parent_id() {
            let parent = symbols.candidate(parent).unwrap();
            assert_eq!(candidate.depth(), parent.depth() + 1);
            assert!(parent.original_range().start() <= range.start() && range.end() <= parent.original_range().end());
        } else { assert_eq!(candidate.depth(), 0); }
    }
}

#[test]
fn javascript_and_markup_routes_keep_distinct_upstream_lexer_names() {
    for (name, language) in [("javascript", SymbolLanguage::JavaScript), ("js", SymbolLanguage::JavaScript),
        ("mjs", SymbolLanguage::JavaScript), ("cjs", SymbolLanguage::JavaScript), ("typescript", SymbolLanguage::TypeScript),
        ("ts", SymbolLanguage::TypeScript), ("mts", SymbolLanguage::TypeScript), ("cts", SymbolLanguage::TypeScript),
        ("jsx", SymbolLanguage::Jsx), ("tsx", SymbolLanguage::Tsx)] {
        assert_eq!(SymbolLanguage::from_name(name), Some(language));
    }
    for (path, language) in [(b"src/\xffView.JSX".as_slice(), SymbolLanguage::Jsx),
        (b"src/\xffView.TSX", SymbolLanguage::Tsx), (b"src/main.MTS", SymbolLanguage::TypeScript),
        (b"src/main.CTS", SymbolLanguage::TypeScript)] {
        assert_eq!(SymbolLanguage::from_path(path), Some(language));
    }
    assert_eq!(SymbolLanguage::Jsx.name(), "jsx");
    assert_eq!(SymbolLanguage::Tsx.name(), "tsx");
    for path in [b"source.ts.txt".as_slice(), b"source.jsx.map", b"tsconfig.json"] {
        assert_eq!(SymbolLanguage::from_path(path), None);
    }
}

#[test]
fn upstream_literals_comments_and_opaque_bodies_cannot_create_declarations() {
    let source = br####"
/*
function CommentFake() {}
class CommentClass {}
*/
const template = `text
function TemplateFake() {}
${`nested ${"class NestedFake {}"}`}
`;
const pattern = /function RegexFake\(\)\{\}/;
const quotient = 10 / 2;
const literal = "function StringFake() {}";
function real() { function LocalFake() {} class BodyFake {} }
const arrow = () => { function ArrowLocal() {} };
class Worker { static { function StaticLocal() {} } open() {} }
"####;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::JavaScript, 128, &budget);
    assert_eq!(names(&symbols), ["real", "arrow", "Worker", "open"]);
    assert_eq!(symbols.candidates()[3].parent_id(), Some(3));
    check_utf8_evidence(&symbols, source);
}

#[test]
fn multiline_named_generator_arrow_and_function_bindings_are_navigable() {
    let source = br#"
export async function
load(
    input
) { return input; }
function *entries() { yield 1; }
const mapped =
    (value) =>
        value + 1;
let asyncMap = async (
    value
) => { return value; };
var assigned = function internal(value) { return value; };
const short = value => value;
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::JavaScript, 128, &budget);
    assert_eq!(names(&symbols), ["load", "entries", "mapped", "asyncMap", "assigned", "short"]);
    assert!(symbols.candidates().iter().all(|c| c.kind().label() == "function" && c.parent_id().is_none()));
    assert_eq!(symbols.source_bytes(1).unwrap(), b"export async function\nload(\n    input\n) { return input; }");
    assert_eq!(symbols.source_bytes(3).unwrap(), b"const mapped =\n    (value) =>\n        value + 1;");
    assert!(!symbols.output_limited());
    check_utf8_evidence(&symbols, source);
}

#[test]
fn javascript_class_members_keep_hierarchy_and_skip_computed_names_and_static_bodies() {
    let source = br#"
class Worker {
    constructor(label) { this.label = label; }
    async *iterate() { yield this.label; }
    get value() { return this.label; }
    set value(next) { this.label = next; }
    #reset() { this.label = ""; }
    static build() { return new Worker(); }
    field = 42;
    handler = (
        value
    ) => { return value; };
    ["computed"]() { function ComputedLocal() {} }
    static { function StaticLocal() {} } after() {}
}
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::JavaScript, 128, &budget);
    assert_eq!(names(&symbols), ["Worker", "constructor", "iterate", "value", "value", "#reset",
        "build", "field", "handler", "after"]);
    assert_eq!(symbols.candidates().iter().map(|c| c.kind().label()).collect::<Vec<_>>(),
        ["class", "initializer", "method", "property", "property", "method", "method", "property", "method", "method"]);
    assert!(symbols.candidates()[1..].iter().all(|c| c.parent_id() == Some(1) && c.depth() == 1));
    check_utf8_evidence(&symbols, source);
}

#[test]
fn typescript_namespace_types_and_multiline_members_have_exact_names_and_parents() {
    let source = br#"
export namespace API {
    export interface Port<T> {
        read(
            input: T
        ): Promise<T>;
        readonly name: string;
        optional?(value: T): T;
    }
    export type Item<T> =
        { value: T; next?: Item<T> };
    export enum Mode { Fast, Safe }
    @sealed
    export abstract class Store<T extends { id: number }> implements Port<T> {
        @memo()
        public async read(
            input: T
        ): Promise<T> { function LocalFake() {} return input; }
        constructor(public name: string) {}
        get ready(): boolean { return true; }
        set ready(value: boolean) {}
        #reset() {}
        public handle =
            (value: T): T =>
                value;
    }
}
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["API", "Port", "read", "name", "optional", "Item", "Mode", "Store",
        "read", "constructor", "ready", "ready", "#reset", "handle"]);
    assert_eq!(symbols.candidates().iter().map(|c| c.kind().label()).collect::<Vec<_>>(),
        ["module", "interface", "method", "property", "method", "type", "enum", "class",
         "method", "initializer", "property", "property", "method", "method"]);
    assert_eq!(symbols.candidates()[2].parent_id(), Some(2));
    assert_eq!(symbols.candidates()[5].parent_id(), Some(1));
    assert_eq!(symbols.candidates()[8].parent_id(), Some(8));
    assert_eq!(symbols.candidates()[13].parent_id(), Some(8));
    assert_eq!(symbols.candidates().iter().filter(|c| c.matches_name("read", SymbolNameMode::Exact)).count(), 2);
    check_utf8_evidence(&symbols, source);
}

#[test]
fn typescript_generic_arrows_and_object_function_return_types_do_not_consume_implementations() {
    let source = br#"
export function identity<T extends { id: number }>(
    value: T
): T { return value; }
const map: (value: number) => number =
    <T extends number>(value: T): T => value;
class Shapes {
    shape(): { value: string } { return { value: "ok" }; }
    factory(): (value: string) => string { return value => value; }
    callback: (value: string) => string;
    count: number
    readonly label: string
    next(): string { return ""; }
}
interface Port {
    read(): string
    write(value: string): void
}
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["identity", "map", "Shapes", "shape", "factory", "callback",
        "count", "label", "next", "Port", "read", "write"]);
    assert_eq!(symbols.source_bytes(4).unwrap(), b"shape(): { value: string } { return { value: \"ok\" }; }");
    assert_eq!(symbols.source_bytes(5).unwrap(), b"factory(): (value: string) => string { return value => value; }");
    assert_eq!(symbols.candidates()[8].parent_id(), Some(3));
    assert_eq!(symbols.candidates()[10].parent_id(), Some(10));
    check_utf8_evidence(&symbols, source);
}

#[test]
fn semicolonless_type_alias_generics_keep_commas_inside_the_evidence_and_preserve_later_functions() {
    let source = b"type Pair = Map<string, Array<number>>\nfunction real() {}\n";
    let budget = budget();
    let symbols = build(source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["Pair", "real"]);
    assert_eq!(symbols.source_bytes(1).unwrap(), b"type Pair = Map<string, Array<number>>");
    assert_eq!(symbols.source_bytes(2).unwrap(), b"function real() {}");
    check_utf8_evidence(&symbols, source);
}

#[test]
fn semicolonless_type_alias_does_not_absorb_an_ordinary_statement() {
    let source = b"type A = string\nconsole.log(1)\nfunction real() {}\n";
    let budget = budget();
    let symbols = build(source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["A", "real"]);
    assert_eq!(symbols.source_bytes(1).unwrap(), b"type A = string");
    assert_eq!(symbols.source_bytes(2).unwrap(), b"function real() {}");
    check_utf8_evidence(&symbols, source);
}

#[test]
fn generic_calls_instantiations_and_fields_preserve_whole_evidence_and_later_declarations() {
    let source = br#"
const make = () => factory<Foo, Array<Bar>>()
function real() {}
const specialize = () => factory<Foo>
function after() {}
class Store {
    field = factory<Foo, Bar>()
    next() {}
}
"#;
    let budget = budget();
    let symbols = build(source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["make", "real", "specialize", "after", "Store", "field", "next"]);
    assert_eq!(symbols.source_bytes(1).unwrap(), b"const make = () => factory<Foo, Array<Bar>>()");
    assert_eq!(symbols.source_bytes(3).unwrap(), b"const specialize = () => factory<Foo>");
    assert_eq!(symbols.source_bytes(6).unwrap(), b"field = factory<Foo, Bar>()");
    check_utf8_evidence(&symbols, source);
}

#[test]
fn contextual_values_end_at_newlines_but_comparison_operators_keep_their_rhs_opaque() {
    for language in [SymbolLanguage::JavaScript, SymbolLanguage::TypeScript, SymbolLanguage::Jsx, SymbolLanguage::Tsx] {
        let source = b"const value = async\nfunction real() {}\nconst compare = left >\nfunction hidden() {}\nfunction after() {}\n";
        let budget = budget();
        let symbols = build(source, language, 128, &budget);
        assert_eq!(names(&symbols), ["real", "after"]);
        check_utf8_evidence(&symbols, source);
    }
}

#[test]
fn generic_lookahead_cannot_cross_comparisons_into_unrelated_declarations() {
    let source = b"const less = () => left < right\nfunction middle() {}\nconst upper = top >\nfunction hidden() {}\nfunction after() {}";
    let budget = budget();
    let symbols = build(source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["less", "middle", "after"]);
    assert_eq!(symbols.source_bytes(1).unwrap(), b"const less = () => left < right");
    check_utf8_evidence(&symbols, source);
}

#[test]
fn unary_statements_do_not_extend_the_preceding_arrow_expression() {
    for operator in ["!", "~"] {
        let source = format!("const f = () => x\n{operator}sideEffect()\nfunction g() {{}}");
        let budget = budget();
        let symbols = build(source.as_bytes(), SymbolLanguage::JavaScript, 128, &budget);
        assert_eq!(names(&symbols), ["f", "g"]);
        assert_eq!(symbols.source_bytes(1).unwrap(), b"const f = () => x");
        check_utf8_evidence(&symbols, source.as_bytes());
    }
}

#[test]
fn modifier_lookahead_is_bounded_even_inside_a_multiline_type() {
    let source = format!("class Store {{ field: Value\n{} next(): Value {{ return value; }} }}", "public\n".repeat(129));
    assert_eq!(refusal(source.as_bytes(), SymbolLanguage::TypeScript), SymbolError::ComplexityLimit);
    let budget = budget();
    let permitted = b"class Store { field: Value\npublic next(): Value { return value; } }";
    let symbols = build(permitted, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(names(&symbols), ["Store", "field", "next"]);
    check_utf8_evidence(&symbols, permitted);
}

#[test]
fn jsx_and_tsx_markup_is_opaque_while_surrounding_declarations_remain_available() {
    for language in [SymbolLanguage::Jsx, SymbolLanguage::Tsx] {
        let source = br#"
const View = () => <section title="function AttributeFake() {}">
    <span>function ChildFake()</span>
    class ChildClass
    {(() => { function EmbeddedLocal() {} return "text"; })()}
    <>
        <br />
        class FragmentFake
    </>
</section>
function after() { return <><span>class NestedFake</span></>; }
class Component { render() { return <div>function BodyFake()</div>; } }
"#;
        let budget = budget();
        let symbols = build(source, language, 128, &budget);
        assert_eq!(names(&symbols), ["View", "after", "Component", "render"], "{language:?}");
        assert_eq!(symbols.source_bytes(1).unwrap(), br#"const View = () => <section title="function AttributeFake() {}">
    <span>function ChildFake()</span>
    class ChildClass
    {(() => { function EmbeddedLocal() {} return "text"; })()}
    <>
        <br />
        class FragmentFake
    </>
</section>"#);
        assert_eq!(symbols.candidates()[3].parent_id(), Some(3));
        check_utf8_evidence(&symbols, source);
    }
}

#[test]
fn utf16_and_utf8_bom_offsets_select_original_identifier_bytes_after_crlf_and_non_bmp_text() {
    let text = "// 😀\r\nexport class Café {\r\n    open(): string { return \"ok\"; }\r\n}\r\n";
    for little in [false, true] {
        let encode = |s: &str| -> Vec<u8> {
            s.encode_utf16().flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect()
        };
        let mut source = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        source.extend_from_slice(&encode(text));
        let budget = budget();
        let symbols = build(&source, SymbolLanguage::TypeScript, 128, &budget);
        assert_eq!(names(&symbols), ["Café", "open"]);
        assert_eq!(symbols.name_bytes(1).unwrap().unwrap(), encode("Café"));
        assert_eq!(symbols.name_bytes(2).unwrap().unwrap(), encode("open"));
        let before = text.split("open").next().unwrap();
        assert_eq!(symbols.candidates()[1].name_range().unwrap().start().get(), 2 + before.encode_utf16().count() as u64 * 2);
        assert_eq!(symbols.candidates()[0].line(), 2);
        assert_eq!(symbols.candidates()[1].line(), 3);
        assert_eq!(symbols.source_bytes(2).unwrap(), encode("open(): string { return \"ok\"; }"));
    }
    let source = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
    let budget = budget();
    let symbols = build(&source, SymbolLanguage::TypeScript, 128, &budget);
    assert_eq!(symbols.name_bytes(1).unwrap().unwrap(), "Café".as_bytes());
    assert_eq!(symbols.candidates()[0].name_range().unwrap().start().get(), 3 + text.find("Café").unwrap() as u64);
}

#[test]
fn regex_inside_template_interpolation_cannot_expose_apparent_outer_declarations() {
    let source = br##"const value = `${ /}`+{/.test("x") }
function phantom() {}
`;
function real() {}"##;
    assert_eq!(refusal(source, SymbolLanguage::JavaScript), SymbolError::UnsupportedLexicalContext);
    let permitted = br##"const value = `${ `nested ${"function Phantom() {}"}` }
function TemplateText() {}
`;
function real() {}"##;
    let budget = budget();
    let symbols = build(permitted, SymbolLanguage::JavaScript, 128, &budget);
    assert_eq!(names(&symbols), ["real"]);
    check_utf8_evidence(&symbols, permitted);
}

#[test]
fn ambiguous_owner_contexts_are_refused_but_nearby_supported_sources_succeed() {
    for source in [
        b"if (ok) /function phantom() {}/.test(text);\nfunction real() {}".as_slice(),
        b"value\n/function phantom() {}/.test(text);\nfunction real() {}",
        b"function \\u0066() {}\nfunction real() {}",
        b"const ratio = {} / /;function phantom() {}/.test(value);\nfunction real() {}",
        b"if (ok) {} /x;function phantom() {}/.test(value);\nfunction real() {}",
        b"// hidden\rfunction real() {}",
        b"#!/usr/bin/env node\nfunction real() {}",
        "// hidden\u{2028}function real() {}".as_bytes(),
    ] {
        assert_eq!(refusal(source, SymbolLanguage::JavaScript), SymbolError::UnsupportedLexicalContext);
    }
    for source in [
        b"if (ok) { /function phantom() {}/.test(text); }\nfunction real() {}".as_slice(),
        b"const value = 12 / 3;\nfunction real() {}",
        b"if (ok) {}; /x;function phantom() {}/.test(value);\nfunction real() {}",
        b"// hidden\r\nfunction real() {}",
        br#"const escaped = "\u0066"; function real() {}"#,
    ] {
        let budget = budget();
        let symbols = build(source, SymbolLanguage::JavaScript, 128, &budget);
        assert_eq!(names(&symbols), ["real"]);
    }
}

#[test]
fn output_admission_is_separate_from_no_recognized_declarations_and_sibling_count() {
    let budget = budget();
    let source = b"class One { first() {} second() {} } class Two {}";
    let limited = build(source, SymbolLanguage::JavaScript, 2, &budget);
    assert_eq!(names(&limited), ["One", "first"]);
    assert!(limited.output_limited());
    assert!(limited.candidate(3).is_none());
    drop(limited);
    let exact = build(b"function one() {} function two() {}", SymbolLanguage::JavaScript, 2, &budget);
    assert!(!exact.output_limited());
    drop(exact);
    let source: String = (0..256).map(|i| format!("function sibling_{i}() {{}}\n")).collect();
    let siblings = build(source.as_bytes(), SymbolLanguage::JavaScript, 512, &budget);
    assert_eq!(siblings.candidates().len(), 256);
    assert!(!siblings.output_limited());
    drop(siblings);
    let absent = build(b"const object = { callback: () => 1 }; consume(function nested() {});",
        SymbolLanguage::JavaScript, 128, &budget);
    assert!(absent.candidates().is_empty());
    assert!(absent.no_recognized_declarations());
    drop(absent);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn malformed_delimiters_source_nesting_cancellation_and_budget_limits_release_admission() {
    for source in [b"class Broken { run() {}".as_slice(), b"function broken(] {}", b"class Broken {"] {
        assert_eq!(refusal(source, SymbolLanguage::JavaScript), SymbolError::InvalidEvidence);
    }
    let markup = format!("const View = () => {}{};", "<div>".repeat(129), "</div>".repeat(129));
    assert_eq!(refusal(markup.as_bytes(), SymbolLanguage::Jsx), SymbolError::ComplexityLimit);
    let nested = format!("const value = {}0{};", "(".repeat(129), ")".repeat(129));
    assert_eq!(refusal(nested.as_bytes(), SymbolLanguage::JavaScript), SymbolError::ComplexityLimit);
    assert_eq!(refusal(&vec![b' '; MAX_SYMBOL_SOURCE_BYTES + 1], SymbolLanguage::JavaScript), SymbolError::SourceLimit);
    let too_many_lines = "\n".repeat(4096);
    assert_eq!(refusal(too_many_lines.as_bytes(), SymbolLanguage::TypeScript), SymbolError::ComplexityLimit);
    let budget = budget();
    let source = b"class Worker { run() {} }\n".repeat(32);
    let mut calls = 0;
    let result = CapturedSymbols::build(&source, request(), generation(), SymbolLanguage::JavaScript, None, 128,
        &budget, ResourceAllocationId::new(1).unwrap(), || { calls += 1; calls > 32 });
    assert!(matches!(result, Err(SymbolError::Canceled)));
    assert!(calls > 32);
    assert_eq!(budget.accounting().reserved().get(), 0);
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(CapturedSymbols::build(b"function real() {}", request(), generation(), SymbolLanguage::JavaScript,
        None, 128, &tiny, ResourceAllocationId::new(1).unwrap(), || false), Err(SymbolError::ResourceDenied)));
    assert_eq!(tiny.accounting().reserved().get(), 0);
}

#[test]
fn exact_names_do_not_weaken_capture_or_generation_validation() {
    let budget = budget();
    let source = b"export const work = (value) => value;";
    let symbols = build(source, SymbolLanguage::JavaScript, 128, &budget);
    assert_eq!(symbols.name_bytes(1).unwrap().unwrap(), b"work");
    assert_eq!(symbols.validate_source(b"export const next = (value) => value;", request().file(),
        request().revision(), generation()), Err(SymbolError::StaleSource));
    assert_eq!(symbols.validate_delivery(request().file(), request().revision(),
        QueryGeneration::new(owner(), 2).unwrap()), Err(SymbolError::StaleQuery));
    assert_eq!(symbols.validate_delivery(request().file(), SourceRevision::new(owner(), 2).unwrap(),
        generation()), Err(SymbolError::StaleSource));
    assert_eq!(symbols.name_bytes(2), Err(SymbolError::NotFound));
}
