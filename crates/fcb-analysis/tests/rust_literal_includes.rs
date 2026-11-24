#![forbid(unsafe_code)]

use fcb_analysis::imports::{CapturedImports, ImportKind, ImportLanguage, ImportResolutionKind, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn build(source: &[u8], limit: usize) -> CapturedImports<'_> {
    let owner = ArenaOwnerId::new(81355).unwrap();
    let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    CapturedImports::build(source, request, QueryGeneration::new(owner, 1).unwrap(),
        ImportLanguage::Rust, limit, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
}

#[test]
fn source_docs_and_binary_paths_all_retain_their_own_evidence() {
    let text = concat!(
        "#![doc = include_str!(\"../docs/café.md\")]\n",
        "include! { \"generated.rs\" }\n",
        "fn data() { let _ = include_bytes![ /* bounded literal */ \"assets/logo.bin\", ]; }\n",
    );
    let kinds = [ImportKind::RustIncludeStr, ImportKind::RustInclude, ImportKind::RustIncludeBytes];
    let paths = ["../docs/café.md", "generated.rs", "assets/logo.bin"];
    let targets: &[&[u8]] = &["docs/café.md".as_bytes(), b"src/generated.rs", b"src/assets/logo.bin"];
    for utf16 in [false, true] {
        let encode = |s: &str| -> Vec<u8> {
            if utf16 { s.encode_utf16().flat_map(u16::to_le_bytes).collect() } else { s.as_bytes().to_vec() }
        };
        let mut source = if utf16 { vec![0xff, 0xfe] } else { vec![] };
        source.extend(encode(text));
        let imports = build(&source, 10);
        assert!(imports.is_complete());
        assert_eq!(imports.facts().len(), 3);
        for (n, fact) in imports.facts().iter().enumerate() {
            assert_eq!(fact.kind(), kinds[n]);
            assert_eq!(fact.destination(), paths[n]);
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert_eq!(&source[start..end], encode(paths[n]).as_slice());
            let result = resolve_import(b"src/lib.rs", fact, |path| targets.iter().position(|candidate| *candidate == path));
            assert_eq!(result.kind, ImportResolutionKind::Local);
            assert_eq!(result.target, Some(n));
            assert_eq!(result.basis, "rust-include-file-candidate");
        }
        let page = build(&source, 1);
        assert_eq!(page.links_seen(), 3);
        assert_eq!(page.facts().len(), 1);
        assert!(page.truncated());
    }
}

#[test]
fn opaque_text_dynamic_paths_and_macro_payloads_do_not_create_targets() {
    let source = br###"
// include!("comment.rs")
const TEXT: &str = r#"include_str!("string.md")"#;
macro_rules! example { () => { include_bytes!("template") } }
another_macro![demo(include_str!("tokens.md"))];
custom::include!("custom.rs");
include!(concat!(env!("OUT_DIR"), "/generated.rs"));
include_bytes!("one", "two");
include_str!("escaped\x2emd");
include_str!("actual.md");
"###;
    let imports = build(source, 32);
    assert_eq!(imports.facts().iter().map(|fact| fact.destination()).collect::<Vec<_>>(), ["actual.md"]);
    assert!(imports.diagnostics() > 0);
}

#[test]
fn root_bounds_and_literal_case_are_enforced_by_the_same_closed_catalog_lookup() {
    let source = br#"include_str!("../outside.md"); include!("/abs.rs"); include_bytes!("C:/path"); include_str!("missing.md");"#;
    let imports = build(source, 8);
    assert_eq!(imports.facts().len(), 4);
    for fact in &imports.facts()[..3] {
        let result = resolve_import::<usize>(b"lib.rs", fact, |_| panic!("inadmissible path probed"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }
    let fact = &imports.facts()[3];
    let mut probes = 0;
    let result = resolve_import::<usize>(b"\xff/main.rs", fact, |path| {
        assert_eq!(path, b"\xff/missing.md"); probes += 1; None
    });
    assert_eq!(probes, 1);
    assert_eq!(result.kind, ImportResolutionKind::Missing);
}

#[test]
fn unsupported_lexer_boundaries_refuse_apparent_includes_inside_opaque_source() {
    use fcb_analysis::imports::ImportError;
    let owner = ArenaOwnerId::new(81356).unwrap();
    let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(), SourceRevision::new(owner, 1).unwrap()).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    for source in [
        r##"const MESSAGE: &CStr = cr#"literal " include_str!("false.md") remainder"#;"##.to_owned(),
        format!("{}{}include_str!(\"false.md\");*/", "/*".repeat(65), "*/".repeat(64)),
    ] {
        let result = CapturedImports::build(source.as_bytes(), request, QueryGeneration::new(owner, 1).unwrap(),
            ImportLanguage::Rust, 8, &budget, ResourceAllocationId::new(1).unwrap(), || false);
        assert!(matches!(result, Err(ImportError::UnsupportedLexicalContext)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
