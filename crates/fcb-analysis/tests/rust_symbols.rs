#![forbid(unsafe_code)]

use fcb_analysis::{OutlineItemKind, symbols::{CapturedSymbols, SymbolError, SymbolLanguage}};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(854).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn build<'source>(source: &'source [u8], limit: usize, budget: &ResourceBudget) -> CapturedSymbols<'source> {
    CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Rust, None, limit,
        budget, allocation(), || false).unwrap()
}
fn names<'outline>(outline: &'outline CapturedSymbols<'_>) -> Vec<&'outline str> {
    outline.candidates().iter().map(|candidate| candidate.name()).collect()
}
fn assert_refused(source: &[u8], expected: SymbolError) {
    let budget = budget();
    let result = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Rust, None, 1024,
        &budget, allocation(), || false);
    assert!(matches!(result, Err(error) if error == expected));
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn hundreds_of_sequential_items_and_methods_do_not_consume_a_nesting_budget() {
    let mut source = String::from("struct Worker;\n");
    for index in 0..256 { source.push_str(&format!("fn top_{index}() {{}}\n")); }
    source.push_str("impl Worker {\n");
    for index in 0..256 { source.push_str(&format!("fn method_{index}(&self) {{}}\n")); }
    source.push_str("}\n");
    let budget = budget();
    let outline = build(source.as_bytes(), 1024, &budget);
    assert_eq!(outline.candidates().len(), 514);
    assert!(!outline.output_limited());
    let implementation = outline.candidates().iter().find(|candidate| matches!(candidate.kind(), OutlineItemKind::Impl)).unwrap();
    assert!(outline.candidates().iter().filter(|candidate| candidate.name().starts_with("top_"))
        .all(|candidate| candidate.parent_id().is_none() && candidate.depth() == 0));
    assert!(outline.candidates().iter().filter(|candidate| candidate.name().starts_with("method_"))
        .all(|candidate| candidate.parent_id() == Some(implementation.id()) && candidate.depth() == 1
            && matches!(candidate.kind(), OutlineItemKind::Method)));
    for candidate in outline.candidates() {
        assert_eq!(candidate.evidence_level(), "heuristic-outline-candidate");
        if let Some(name) = outline.name_bytes(candidate.id()).unwrap() { assert_eq!(name, candidate.name().as_bytes()); }
        let (start, end) = candidate.original_range().as_usize_bounds().unwrap();
        assert_eq!(outline.source_bytes(candidate.id()).unwrap(), &source.as_bytes()[start..end]);
    }
    drop(outline);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn braces_in_literals_comments_and_sequential_body_blocks_are_opaque() {
    let source = format!("// {} fn line_fake() {{}}\n/* {} fn block_fake() {{}} */\n\
        const TEXT: &str = r##\"{} fn string_fake() {{}}\"##;\n\
        fn actual() {{ {} let byte = b'{{'; let string = \"fn ordinary_fake() {{}}\"; }}\n",
        "{".repeat(512), "{}".repeat(512), "{".repeat(512), "{};".repeat(256));
    let budget = budget();
    let outline = build(source.as_bytes(), 16, &budget);
    assert_eq!(names(&outline), ["TEXT", "actual"]);
    assert!(!outline.output_limited());
}

#[test]
fn modules_traits_implementations_and_foreign_items_keep_precise_parentage() {
    let source = br##"#![allow(dead_code)]
#[cfg_attr(any(), doc = "fn attribute_fake() {}")]
mod nested {
    macro_rules! generated { () => { fn macro_definition_fake() {} } }
    generated! { fn macro_invocation_fake() {} }
    pub trait Task<T> {
        const N: usize = { fn const_fake() {} 1 };
        type Output;
        fn run(&self) -> T;
        fn provided() {}
    }
    pub struct Worker<T>(T);
    impl<T: Fn() -> u8> Task<T> for Worker<T> where T: Clone {
        #[inline] pub unsafe extern "C" fn r#type(&self) -> fn() {}
        const VALUE: usize = { fn initializer_fake() {} 1 };
        type Output = u8;
    }
    unsafe extern "C" { pub safe fn foreign(); static mut SLOT: u8; }
    pub(crate) async fn outside<const N: usize>() where [(); N]: Sized {}
}
mod separate;
"##;
    let budget = budget();
    let outline = build(source, 64, &budget);
    let find = |name: &str| outline.candidates().iter().find(|candidate| candidate.name() == name).unwrap();
    let module = find("nested");
    let task = find("Task");
    let implementation = outline.candidates().iter().find(|candidate| matches!(candidate.kind(), OutlineItemKind::Impl)).unwrap();
    assert_eq!(task.parent_id(), Some(module.id()));
    assert_eq!(implementation.parent_id(), Some(module.id()));
    assert_eq!(find("Worker").parent_id(), Some(module.id()));
    for name in ["run", "provided"] {
        assert_eq!(find(name).parent_id(), Some(task.id()));
        assert!(matches!(find(name).kind(), OutlineItemKind::Method));
    }
    assert_eq!(find("r#type").parent_id(), Some(implementation.id()));
    assert_eq!(outline.name_bytes(find("r#type").id()).unwrap().unwrap(), b"r#type");
    assert!(outline.source_bytes(find("r#type").id()).unwrap().starts_with(b"fn r#type"));
    assert!(outline.name_bytes(implementation.id()).unwrap().is_none());
    for name in ["foreign", "SLOT", "outside"] {
        assert_eq!(find(name).parent_id(), Some(module.id()));
        assert_eq!(find(name).depth(), 1);
    }
    assert_eq!(find("separate").parent_id(), None);
    assert!(outline.candidates().iter().all(|candidate| !candidate.name().contains("fake")));
    assert_eq!(outline.candidates().iter().filter(|candidate| candidate.name() == "Output").count(), 2);
}

#[test]
fn original_utf16_bytes_and_crlf_lines_survive_nested_raw_identifiers() {
    let text = "// 🧭\r\nmod inner { fn r#type() {} }\r\n";
    for little_endian in [true, false] {
        let mut source = if little_endian { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() {
            source.extend_from_slice(&if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        let budget = budget();
        let outline = build(&source, 16, &budget);
        assert_eq!(names(&outline), ["inner", "r#type"]);
        let function = &outline.candidates()[1];
        assert_eq!(function.line(), 2);
        assert_eq!(function.parent_id(), Some(outline.candidates()[0].id()));
        let expected: Vec<u8> = "r#type".encode_utf16().flat_map(|unit|
            if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        assert_eq!(outline.name_bytes(function.id()).unwrap().unwrap(), expected);
        let (start, end) = function.original_range().as_usize_bounds().unwrap();
        assert_eq!(outline.source_bytes(function.id()).unwrap(), &source[start..end]);
    }
}

#[test]
fn unsupported_split_identifiers_cannot_publish_a_shortened_name() {
    let source = "struct Cafe\u{301}; mod Cafe\u{301} {} const Cafe\u{301}: u8 = 1; \
        struct r#é; mod r#é {} const r#é: u8 = 1; fn r#café() {} \
        struct Café; fn r#type() {}";
    let budget = budget();
    let outline = build(source.as_bytes(), 32, &budget);
    assert_eq!(names(&outline), ["Café", "r#type"]);
}

#[test]
fn actual_delimiter_and_generic_nesting_still_have_hard_bounds() {
    let nested = |count: usize| {
        let mut source = String::new();
        for index in 0..count { source.push_str(&format!("mod m{index} {{")); }
        source.push_str("fn leaf() {}");
        source.push_str(&"}".repeat(count));
        source
    };
    let accepted = nested(127);
    let budget = budget();
    let outline = build(accepted.as_bytes(), 1024, &budget);
    assert_eq!(outline.candidates().len(), 128);
    assert_eq!(outline.candidates().last().unwrap().depth(), 127);
    drop(outline);
    assert_refused(nested(128).as_bytes(), SymbolError::ComplexityLimit);
    assert_refused(format!("fn deep() {{ {}0{} }}", "[".repeat(128), "]".repeat(128)).as_bytes(), SymbolError::ComplexityLimit);
    assert_refused(format!("struct Deep{}T{};", "<".repeat(129), ">".repeat(129)).as_bytes(), SymbolError::ComplexityLimit);
}

#[test]
fn unsupported_deep_comments_and_malformed_groups_never_publish_apparent_items() {
    let accepted = format!("{} fn hidden() {{}} {} fn visible() {{}}", "/*".repeat(63), "*/".repeat(63));
    let budget = budget();
    let outline = build(accepted.as_bytes(), 16, &budget);
    assert_eq!(names(&outline), ["visible"]);
    drop(outline);
    assert_refused(format!("{} fn hidden() {{}} {} fn visible() {{}}", "/*".repeat(65), "*/".repeat(65)).as_bytes(),
        SymbolError::UnsupportedLexicalContext);
    assert_refused(br##"const RAW: &CStr = cr#"quoted " ; fn hidden() {} "#;"##,
        SymbolError::UnsupportedLexicalContext);
    for source in [b"fn broken() {".as_slice(), b"fn broken([)] {}", b"fn broken() {} }"] {
        assert_refused(source, SymbolError::InvalidEvidence);
    }
}

#[test]
fn display_limit_counts_only_recognized_candidates_and_cannot_hide_more_results() {
    let budget = budget();
    let outline = build(b"fn only() { fn local() {} } generated! { fn hidden() {} }", 1, &budget);
    assert_eq!(names(&outline), ["only"]);
    assert!(!outline.output_limited());
    drop(outline);
    let outline = build(b"fn only() {} generated! { fn hidden() {} } fn second() {}", 1, &budget);
    assert_eq!(names(&outline), ["only"]);
    assert!(outline.output_limited());
    drop(outline);
    let outline = build(b"mod outer { fn nested() {} }", 1, &budget);
    assert_eq!(names(&outline), ["outer"]);
    assert!(outline.output_limited());
}

#[test]
fn malformed_signature_cannot_borrow_the_next_declarations_body() {
    let budget = budget();
    let outline = build(b"fn broken() fn stolen() {}\nfn honest() {}\n", 16, &budget);
    assert_eq!(names(&outline), ["honest"]);
}

#[test]
fn every_cancellation_checkpoint_releases_the_inventory_reservation() {
    let source = b"mod outer { struct Item; impl Item { fn first() {} fn second<T>() {} } }";
    let budget = budget();
    let mut checks = 0;
    let probe = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Rust, None, 32,
        &budget, allocation(), || { checks += 1; false }).unwrap();
    drop(probe);
    assert!(checks > 10);
    for stop in 1..=checks {
        let mut visited = 0;
        let result = CapturedSymbols::build(source, request(), generation(), SymbolLanguage::Rust, None, 32,
            &budget, allocation(), || { visited += 1; visited == stop });
        assert!(matches!(result, Err(SymbolError::Canceled)), "checkpoint {stop}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
