#![forbid(unsafe_code)]

//! Exact source evidence and declared-policy boundaries for lexical import
//! navigation. No project execution, filesystem resolver, or compiler claims.

use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    MAX_IMPORT_ITEMS, MAX_IMPORT_SOURCE_BYTES};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80621).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn request() -> CaptureRequest { CaptureRequest::new(file(), revision()).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn build(source: &[u8], language: ImportLanguage, limit: usize) -> CapturedImports<'_> {
    CapturedImports::build(source, request(), generation(), language, limit, &budget(), allocation(), || false).unwrap()
}
fn destinations<'a>(imports: &'a CapturedImports<'_>) -> Vec<&'a str> { imports.facts().iter().map(|fact| fact.destination()).collect() }
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() { bytes.extend_from_slice(&if big { word.to_be_bytes() } else { word.to_le_bytes() }); }
    bytes
}

#[test]
fn rust_modules_uses_aliases_and_duplicates_keep_exact_statement_evidence() {
    let source = b"pub mod child;\nuse crate::child::{Thing, Other as Alias};\nmod child;\n";
    let imports = build(source, ImportLanguage::Rust, 20);
    assert_eq!(destinations(&imports), ["child", "crate::child::Thing", "crate::child::Other", "child"]);
    assert_eq!(imports.links_seen(), 4); assert!(imports.is_complete());
    assert_eq!(imports.facts()[0].kind(), ImportKind::RustModule); assert!(imports.facts()[0].resolution_allowed());
    assert_eq!(imports.facts()[1].kind(), ImportKind::RustUse); assert!(imports.facts()[1].resolution_allowed());
    assert_eq!(imports.source_bytes(1).unwrap(), b"mod child;");
    assert_eq!(imports.source_bytes(2).unwrap(), b"use crate::child::{Thing, Other as Alias};");
    assert_eq!(imports.source_bytes(3).unwrap(), b"use crate::child::{Thing, Other as Alias};");
    assert_ne!(imports.facts()[0].original_range(), imports.facts()[3].original_range());
    for (index, fact) in imports.facts().iter().enumerate() { assert_eq!(fact.id(), index as u64 + 1); }
}

#[test]
fn upstream_lexical_spans_exclude_comments_strings_raw_strings_and_regex_source_text() {
    let rust = br##"
// mod fake_a;
const TEXT: &str = "mod fake_b; use crate::fake;";
const RAW: &str = r#"mod fake_c;"#;
/* outer /* mod fake_d; */ use fake; */
mod real;
"##;
    let imports = build(rust, ImportLanguage::Rust, 20);
    assert_eq!(destinations(&imports), ["real"]);
    let javascript = br#"
// import a from "./fake-a";
const text = "import b from './fake-b';";
/* export * from "./fake-c"; */
const pattern = /import\s+fake/;
import good from "./real.js";
"#;
    for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript] {
        let imports = build(javascript, language, 20);
        assert_eq!(destinations(&imports), ["./real.js"]);
        assert!(imports.is_complete());
    }
}

#[test]
fn static_javascript_and_typescript_import_forms_keep_specifier_and_statement_ranges() {
    let source = b"import \"side-effects\";\nimport /* keep */ thing,\n { A as B, type C } from './module.js';\nexport * as names from \"./all\";\nexport { named } from './other';\nexport { local };\n";
    for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript, ImportLanguage::Jsx, ImportLanguage::Tsx] {
        let imports = build(source, language, 20);
        assert_eq!(destinations(&imports), ["side-effects", "./module.js", "./all", "./other"]);
        assert_eq!(imports.facts()[2].kind(), ImportKind::JavaScriptExport);
        assert_eq!(imports.source_bytes(2).unwrap(), b"import /* keep */ thing,\n { A as B, type C } from './module.js';");
        assert!(imports.is_complete());
        for fact in imports.facts() {
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert_eq!(&source[start..end], fact.destination().as_bytes());
        }
    }
}

#[test]
fn dynamic_meta_and_escaped_imports_never_become_resolvable_static_dependencies() {
    let source = br#"import("./dynamic"); import.meta.url;
import value from "./escaped\u002ejs";
import direct from "./actual.js";
export { local };
"#;
    let imports = build(source, ImportLanguage::TypeScript, 20);
    assert_eq!(destinations(&imports), [r"./escaped\u002ejs", "./actual.js"]);
    assert!(!imports.facts()[0].resolution_allowed()); assert!(imports.facts()[1].resolution_allowed());
    assert_eq!(imports.diagnostics(), 1, "dynamic imports remain explicit unsupported inventory coverage");
    assert!(!imports.is_complete());
    let meta = build(b"const url = import.meta.url; export { local };", ImportLanguage::JavaScript, 20);
    assert!(meta.facts().is_empty()); assert!(meta.is_complete());
}

#[test]
fn rust_nested_macro_and_override_contexts_preserve_facts_but_withhold_path_resolution() {
    for source in [
        b"wrapper!(mod child;);".as_slice(),
        b"wrapper![mod child;];".as_slice(),
        b"wrapper!{mod child;}".as_slice(),
        b"mod inner { mod child; }".as_slice(),
        b"#[path = \"other.rs\"] mod child;".as_slice(),
        b"#[cfg_attr(feature = \"x\", path = \"other.rs\")] mod child;".as_slice(),
    ] {
        let imports = build(source, ImportLanguage::Rust, 20);
        assert_eq!(destinations(&imports), ["child"], "{source:?}");
        assert!(!imports.facts()[0].resolution_allowed());
    }
    let ordinary = build(b"#[cfg(test)] mod child;\nmod r#type;", ImportLanguage::Rust, 20);
    assert_eq!(destinations(&ordinary), ["child", "r#type"]);
    assert!(ordinary.facts().iter().all(|fact| fact.resolution_allowed()));
    assert_eq!(ordinary.source_bytes(2).unwrap(), b"mod r#type;");
}

#[test]
fn original_utf16_and_bom_coordinates_are_not_decoded_utf8_offsets() {
    let text = "😀\r\nmod child;\r\nuse crate::thing;";
    for big in [false, true] {
        let source = utf16(text, big); let imports = build(&source, ImportLanguage::Rust, 20);
        assert_eq!(destinations(&imports), ["child", "crate::thing"]);
        let fact = &imports.facts()[0];
        assert_eq!(fact.original_range().start().get(), 10);
        assert_eq!(fact.original_range().end().get(), 30);
        assert_eq!(fact.specifier_range().start().get(), 18);
        assert_eq!(fact.specifier_range().end().get(), 28);
        let encoded = utf16("mod child;", big);
        assert_eq!(imports.source_bytes(1).unwrap(), &encoded[2..]);
    }
    let mut source = vec![0xef, 0xbb, 0xbf]; source.extend_from_slice(text.as_bytes());
    let imports = build(&source, ImportLanguage::Rust, 20);
    assert_eq!(imports.facts()[0].original_range().start().get(), 9);
    assert_eq!(imports.facts()[0].original_range().end().get(), 19);
    assert_eq!(imports.source_bytes(1).unwrap(), b"mod child;");
}

#[test]
fn output_limits_count_omitted_occurrences_and_do_not_fabricate_complete_empty_inventory() {
    let source = b"mod a; mod b; mod a;";
    for limit in [0, 1, 2, 3] {
        let imports = build(source, ImportLanguage::Rust, limit);
        assert_eq!(imports.links_seen(), 3); assert_eq!(imports.facts().len(), limit);
        assert_eq!(imports.truncated(), limit < 3); assert_eq!(imports.is_complete(), limit == 3);
    }
    let empty = build(b"fn nothing() {}", ImportLanguage::Rust, 0);
    assert!(empty.is_complete()); assert_eq!(empty.links_seen(), 0);
}

#[test]
fn malformed_decoding_unterminated_statements_and_long_specifiers_stay_explicit() {
    assert!(matches!(CapturedImports::build(&[0xff, 0, 1], request(), generation(), ImportLanguage::Rust,
        10, &budget(), allocation(), || false), Err(ImportError::UnsupportedEncoding)));
    let rust = build(b"use crate::unfinished", ImportLanguage::Rust, 20);
    assert!(!rust.is_complete()); assert_eq!(rust.diagnostics(), 1); assert!(rust.facts().is_empty());
    let js = build(b"import x from 'unterminated", ImportLanguage::JavaScript, 20);
    assert!(!js.is_complete()); assert!(js.facts().is_empty());
    let reexport = build(b"export * from", ImportLanguage::JavaScript, 20);
    assert!(!reexport.is_complete()); assert!(reexport.facts().is_empty());
    let source = format!("import '{}';", "x".repeat(1025));
    let long = build(source.as_bytes(), ImportLanguage::JavaScript, 20);
    assert!(!long.is_complete()); assert_eq!(long.diagnostics(), 1);
}

#[test]
fn capture_owner_generation_cancel_and_resource_bounds_are_enforced() {
    let source = b"mod child;";
    let imports = build(source, ImportLanguage::Rust, 20);
    assert!(imports.validate_source(source, file(), revision(), generation()).is_ok());
    assert_eq!(imports.validate_source(b"mod other;", file(), revision(), generation()), Err(ImportError::StaleSource));
    assert_eq!(imports.validate_delivery(file(), revision(), QueryGeneration::new(owner(), 2).unwrap()), Err(ImportError::StaleQuery));
    let foreign = ArenaOwnerId::new(80622).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), QueryGeneration::new(foreign, 1).unwrap(),
        ImportLanguage::Rust, 1, &budget(), allocation(), || false), Err(ImportError::OwnerMismatch)));
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Rust,
        MAX_IMPORT_ITEMS + 1, &budget(), allocation(), || false), Err(ImportError::InvalidLimits)));
    assert!(matches!(CapturedImports::build(&vec![b'x'; MAX_IMPORT_SOURCE_BYTES + 1], request(), generation(),
        ImportLanguage::Rust, 1, &budget(), allocation(), || false), Err(ImportError::SourceLimit)));
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Rust,
        1, &budget(), allocation(), || true), Err(ImportError::Canceled)));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Rust,
        1, &tiny, allocation(), || false), Err(ImportError::ResourceDenied)));
    let mut polls = 0;
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Rust,
        1, &budget(), allocation(), || { polls += 1; polls > 3 }), Err(ImportError::Canceled)));
}

#[test]
fn native_path_language_selection_is_bounded_and_does_not_decode_parent_bytes() {
    assert_eq!(ImportLanguage::from_path(b"dir\xff/thing.TSX"), Some(ImportLanguage::Tsx));
    assert_eq!(ImportLanguage::from_path(b"dir/file.mjs"), Some(ImportLanguage::JavaScript));
    assert_eq!(ImportLanguage::from_path(b"dir/file.cts"), Some(ImportLanguage::TypeScript));
    assert_eq!(ImportLanguage::from_path(b"dir.rs/file"), None);
    assert_eq!(ImportLanguage::from_path(b"file.swift"), None);
}
