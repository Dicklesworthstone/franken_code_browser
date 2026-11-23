#![forbid(unsafe_code)]

//! Explicit Rust path attributes retain captured evidence and authorize only
//! one bounded source-relative catalog candidate, without compiler inference.

use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    ImportResolutionKind, MAX_IMPORT_SPECIFIER_BYTES, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80628).unwrap() }
fn capture_with(source: &[u8], limit: usize, canceled: impl FnMut() -> bool) -> Result<CapturedImports<'_>, ImportError> {
    let request = CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap();
    let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    CapturedImports::build(source, request, QueryGeneration::new(owner(), 1).unwrap(), ImportLanguage::Rust,
        limit, &budget, ResourceAllocationId::new(1).unwrap(), canceled)
}
fn capture(source: &[u8]) -> CapturedImports<'_> { capture_with(source, 20, || false).unwrap() }
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() }); }
    bytes
}

#[test]
fn explicit_attributes_preserve_the_full_statement_and_literal_specifier() {
    for visibility in ["", "pub ", "pub(crate) ", "pub(self) ", "pub(super) "] {
        let statement = format!("#[ /* before */ path /* key */ = \"platform/custom.rs\" /* after */ ]\n{visibility}mod r#type;");
        let source = format!("// source context\n{statement}\n");
        let imports = capture(source.as_bytes());
        assert!(imports.is_complete());
        assert_eq!(imports.facts().len(), 1, "{visibility}");
        let fact = &imports.facts()[0];
        assert_eq!(fact.kind(), ImportKind::RustPathModule);
        assert_eq!(fact.kind().name(), "rust-path-module");
        assert_eq!(fact.destination(), "platform/custom.rs");
        assert!(fact.resolution_allowed());
        assert_eq!(imports.source_bytes(1).unwrap(), statement.as_bytes());
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source.as_bytes()[start..end], b"platform/custom.rs");
    }
}

#[test]
fn exact_paths_are_relative_to_the_source_directory_even_in_flat_modules() {
    let imports = capture(b"#[path = \"platform/custom.rs\"] mod child;");
    for (source, expected) in [
        (b"lib.rs".as_slice(), b"platform/custom.rs".as_slice()),
        (b"src/lib.rs", b"src/platform/custom.rs"),
        (b"src/main.rs", b"src/platform/custom.rs"),
        (b"src/parent.rs", b"src/platform/custom.rs"),
        (b"src/nested/mod.rs", b"src/nested/platform/custom.rs"),
        (b"src/nested/parent.rs", b"src/nested/platform/custom.rs"),
    ] {
        let mut calls = 0;
        let result = resolve_import(source, &imports.facts()[0], |path| {
            calls += 1;
            assert_eq!(path, expected);
            Some(71usize)
        });
        assert_eq!(calls, 1);
        assert_eq!(result.kind, ImportResolutionKind::Local);
        assert_eq!(result.target, Some(71));
        assert_eq!(result.basis, "rust-path-attribute-candidate");
    }
}

#[test]
fn dot_segments_and_unicode_paths_keep_catalog_bytes_without_url_decoding() {
    let statement = "#[path = \"../shared/./caf\u{e9} #100%.inc\"] mod child;";
    let imports = capture(statement.as_bytes());
    let source = b"raw\xff/src/parent.rs";
    let expected = [b"raw\xff/shared/".as_slice(), "caf\u{e9} #100%.inc".as_bytes()].concat();
    let result = resolve_import(source, &imports.facts()[0], |path| {
        assert_eq!(path, expected);
        Some(9usize)
    });
    assert_eq!(result.kind, ImportResolutionKind::Local);
    assert_eq!(result.target, Some(9));
    assert_eq!(imports.source_bytes(1).unwrap(), statement.as_bytes());
}

#[test]
fn explicit_missing_paths_do_not_guess_extensions_or_module_directories() {
    let imports = capture(b"#[path = \"custom\"] mod child;");
    let mut queried = Vec::new();
    let result = resolve_import(b"src/parent.rs", &imports.facts()[0], |path| {
        queried.push(path.to_vec());
        matches!(path, b"src/custom.rs" | b"src/custom/mod.rs" | b"src/parent/custom").then_some(1usize)
    });
    assert_eq!(queried, [b"src/custom".to_vec()]);
    assert_eq!(result.kind, ImportResolutionKind::Missing);
    assert_eq!(result.basis, "no-catalog-target");
    assert_eq!(result.target, None);
}

#[test]
fn invalid_paths_and_unmodeled_importer_names_never_probe_the_catalog() {
    for destination in ["", "/outside.rs", "../../outside.rs", "nested//child.rs", "C:/outside.rs",
        "nested/", ".", "..", "nested/.", "nested/..", "nested\\child.rs"] {
        let source = format!("#[path = \"{destination}\"] mod child;");
        let imports = capture(source.as_bytes());
        assert_eq!(imports.facts().len(), 1, "{destination}");
        let result = resolve_import::<usize>(b"src/lib.rs", &imports.facts()[0],
            |_| panic!("unsupported path probed the catalog: {destination}"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved, "{destination}");
    }
    let imports = capture(b"#[path = \"child.rs\"] mod child;");
    for source in [b"src/source.py".as_slice(), b"src/.rs", b"../src/lib.rs", b"/src/lib.rs"] {
        let result = resolve_import::<usize>(source, &imports.facts()[0],
            |_| panic!("unsupported source path probed the catalog: {source:?}"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }
}

#[test]
fn conditional_duplicate_computed_and_nested_attributes_keep_targets_withheld() {
    for source in [
        "#[cfg(test)] #[path = \"other.rs\"] mod child;",
        "#[path = \"other.rs\"] #[cfg(test)] mod child;",
        "#[path = \"one.rs\"] #[path = \"two.rs\"] mod child;",
        "#[cfg_attr(test, path = \"other.rs\")] mod child;",
        "#[path = concat!(\"other\", \".rs\")] mod child;",
        "#[path = r\"other.rs\"] mod child;",
        "#[path = \"other.rs\"] pub(in crate) mod child;",
        "mod parent { #[path = \"other.rs\"] mod child; }",
        "mod parent { fn ready() {} #[path = \"other.rs\"] mod child; }",
        "wrapper! { #[path = \"other.rs\"] mod child; }",
        "macro_rules! recipe { () => { #[path = \"other.rs\"] mod child; } }",
        "#![cfg(test)] const READY: bool = true; #[path = \"other.rs\"] mod child;",
        "#![cfg_attr(test, allow(unused))] const READY: bool = true; #[path = \"other.rs\"] mod child;",
        "#[path = \"other\\u{2e}rs\"] mod child;",
        "#[path = \"other.rs\"] mod r#self;",
        "#[path = \"other.rs\"] mod _;",
    ] {
        let imports = capture(source.as_bytes());
        assert!(!imports.facts().is_empty(), "{source}");
        for fact in imports.facts() {
            assert!(!fact.resolution_allowed(), "{source}");
            let result = resolve_import::<usize>(b"src/lib.rs", fact,
                |_| panic!("unsupported context probed a catalog target: {source}"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved, "{source}");
        }
    }
}

#[test]
fn an_explicit_candidate_does_not_authorize_conventional_module_or_use_guesses() {
    let imports = capture(b"#[path = \"custom.rs\"] mod custom; mod conventional; use crate::api;");
    assert_eq!(imports.facts().len(), 3);
    assert_eq!(imports.facts()[0].kind(), ImportKind::RustPathModule);
    assert!(imports.facts()[0].resolution_allowed());
    for fact in &imports.facts()[1..] {
        assert!(!fact.resolution_allowed());
        let result = resolve_import::<usize>(b"src/lib.rs", fact,
            |_| panic!("explicit path lent authority to a conventional guess"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }
}

#[test]
fn comments_strings_and_inline_modules_cannot_manufacture_external_path_facts() {
    let source = br##"// #[path = "fake.rs"] mod fake;
const TEXT: &str = "#[path = \"fake.rs\"] mod fake;";
#[path = "inline.rs"] mod inline { fn run() {} }
#[path = "real.rs"] mod real;
"##;
    let imports = capture(source);
    assert_eq!(imports.facts().len(), 1);
    assert_eq!(imports.facts()[0].kind(), ImportKind::RustPathModule);
    assert_eq!(imports.facts()[0].destination(), "real.rs");
    assert!(imports.facts()[0].resolution_allowed());
    assert_eq!(imports.source_bytes(1).unwrap(), b"#[path = \"real.rs\"] mod real;");
}

#[test]
fn bom_and_utf16_ranges_recover_original_attribute_and_literal_bytes() {
    let statement = "#[path = \"caf\u{e9}/\u{1f600}.rs\"]\r\npub(crate) mod child;";
    let text = format!("// \u{1f600}\r\n{statement}\r\n");
    let utf8 = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
    for (source, original, specifier) in [
        (utf8, statement.as_bytes().to_vec(), "caf\u{e9}/\u{1f600}.rs".as_bytes().to_vec()),
        (utf16(&text, false), utf16(statement, false)[2..].to_vec(), utf16("caf\u{e9}/\u{1f600}.rs", false)[2..].to_vec()),
        (utf16(&text, true), utf16(statement, true)[2..].to_vec(), utf16("caf\u{e9}/\u{1f600}.rs", true)[2..].to_vec()),
    ] {
        let imports = capture(&source);
        assert_eq!(imports.facts().len(), 1);
        let fact = &imports.facts()[0];
        assert_eq!(fact.kind(), ImportKind::RustPathModule);
        assert_eq!(fact.destination(), "caf\u{e9}/\u{1f600}.rs");
        assert!(fact.resolution_allowed());
        assert_eq!(imports.source_bytes(1).unwrap(), original);
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], specifier);
        assert!(fact.original_range().start() <= fact.specifier_range().start());
        assert!(fact.specifier_range().end() <= fact.original_range().end());
    }
}

#[test]
fn path_attribute_limits_and_cancellation_remain_bounded() {
    let source = b"#[path = \"one.rs\"] mod one; #[path = \"two.rs\"] mod two;";
    let imports = capture_with(source, 1, || false).unwrap();
    assert_eq!(imports.facts().len(), 1);
    assert_eq!(imports.links_seen(), 2);
    assert!(imports.truncated());
    assert!(matches!(capture_with(source, 20, || true), Err(ImportError::Canceled)));

    let source = format!("#[path = \"{}\"] mod child;", "x".repeat(MAX_IMPORT_SPECIFIER_BYTES + 1));
    let imports = capture(source.as_bytes());
    assert!(imports.facts().is_empty());
    assert_eq!(imports.diagnostics(), 1);

    let source = format!("#[path = \"{}child.rs\"] mod child;", "x/".repeat(128));
    let imports = capture(source.as_bytes());
    let result = resolve_import::<usize>(b"src/lib.rs", &imports.facts()[0],
        |_| panic!("excessive component count probed a target"));
    assert_eq!(result.kind, ImportResolutionKind::Unresolved);

    let source = b"#[path = \"child.rs\"] mod child;";
    let imports = capture(source);
    let oversized_source = [vec![b'x'; 16 * 1024 - 7], b"/lib.rs".to_vec()].concat();
    let result = resolve_import::<usize>(&oversized_source, &imports.facts()[0],
        |_| panic!("an oversized resolved path probed a target"));
    assert_eq!(result.kind, ImportResolutionKind::Unresolved);
}
