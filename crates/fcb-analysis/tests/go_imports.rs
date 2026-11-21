#![forbid(unsafe_code)]

use fcb_analysis::imports::{CapturedGoModule, CapturedImports, ImportError, ImportKind,
    ImportLanguage, ImportResolutionKind, MAX_GO_MODULE_SOURCE_BYTES, MAX_IMPORT_SOURCE_BYTES,
    find_go_module, go_package_source, resolve_go_import, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80632).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 3).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 7).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn build(source: &[u8], limit: usize) -> CapturedImports<'_> {
    CapturedImports::build(source, request(), generation(), ImportLanguage::Go, limit,
        &budget(), allocation(), || false).unwrap()
}
fn module<'a>(path: &[u8], source: &'a [u8]) -> CapturedGoModule<'a> {
    CapturedGoModule::build(path, source, request(), &budget(), allocation(), || false).unwrap()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() }); }
    bytes
}

#[test]
fn package_import_prefix_retains_grouped_alias_and_raw_string_evidence() {
    let source = br##"package main
// import "comment.example/fake"
import "fmt";
import (
    alias "example.com/project/core" // comment
    _ "example.com/project/plugin"
    . `example.com/project/dot`
)
import /* comments remain upstream-owned */ "os"
var text = `import "literal.example/fake"`
func nested() { import "invalid.example/body" }
"##;
    let imports = build(source, 32);
    assert_eq!(imports.facts().iter().map(|f| f.destination()).collect::<Vec<_>>(),
        ["fmt", "example.com/project/core", "example.com/project/plugin", "example.com/project/dot", "os"]);
    assert!(imports.is_complete());
    assert_eq!(imports.source_bytes(1).unwrap(), b"import \"fmt\";");
    assert_eq!(imports.source_bytes(2).unwrap(), b"alias \"example.com/project/core\"");
    for fact in imports.facts() {
        assert_eq!(fact.kind(), ImportKind::GoImport);
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], fact.destination().as_bytes());
        assert!(fact.resolution_allowed());
    }
    assert_eq!(ImportLanguage::from_path(b"pkg/file.go"), Some(ImportLanguage::Go));
}

#[test]
fn utf8_bom_utf16_surrogates_and_crlf_preserve_exact_original_import_ranges() {
    let text = "// 🦀\r\npackage main\r\nimport alias \"example.com/project/core\"\r\n";
    let mut utf8 = vec![0xef, 0xbb, 0xbf]; utf8.extend_from_slice(text.as_bytes());
    for (bytes, literal) in [(utf8, b"example.com/project/core".to_vec()),
        (utf16(text, false), utf16("example.com/project/core", false)[2..].to_vec()),
        (utf16(text, true), utf16("example.com/project/core", true)[2..].to_vec())] {
        let imports = build(&bytes, 8);
        assert_eq!(imports.facts().len(), 1);
        let (start, end) = imports.facts()[0].specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&bytes[start..end], literal);
        assert!(imports.validate_source(&bytes, request().file(), request().revision(), generation()).is_ok());
        assert_eq!(imports.validate_source(b"changed", request().file(), request().revision(), generation()), Err(ImportError::StaleSource));
    }
}

#[test]
fn malformed_imports_are_partial_and_opaque_text_never_becomes_a_link() {
    for source in [b"package main\nimport (\n\"one\"\n".as_slice(),
        b"package main\nimport alias\n\"not-attached\"\n", b"package main\nimport 'x'\n",
        b"package main\nimport (\"one\", \"two\")\n", b"package main import \"same-line\"\n"] {
        let imports = build(source, 16);
        assert!(imports.facts().is_empty(), "{source:?}");
        assert!(!imports.is_complete() && imports.diagnostics() > 0);
    }
    for source in [b"package main\n/* unclosed\nimport \"fake\"\n".as_slice(),
        b"package main\nimport \"broken\nimport \"fake\"\n", b"package main\nvar raw = `unterminated"] {
        assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Go, 16,
            &budget(), allocation(), || false), Err(ImportError::UnsupportedLexicalContext)));
    }
    let escaped = build(br#"package main
import "example.com/project/\x63ore"
"#, 8);
    assert_eq!(escaped.facts().len(), 1);
    assert!(!escaped.facts()[0].resolution_allowed());
    let unresolved: fcb_analysis::imports::ImportResolution<u64> = resolve_import(b"main.go", &escaped.facts()[0], |_| panic!("No path lookup for escaped Go imports"));
    assert_eq!(unresolved.kind, ImportResolutionKind::Unresolved);
}

#[test]
fn limits_cancellation_and_independent_owners_remain_explicit() {
    let source = b"package main\nimport (\"one\"; \"two\"; \"three\")\n";
    let imports = build(source, 1);
    assert_eq!(imports.links_seen(), 3); assert_eq!(imports.facts().len(), 1); assert!(imports.truncated());
    let zero = build(source, 0);
    assert!(zero.facts().is_empty() && zero.truncated()); assert_eq!(zero.links_seen(), 3);
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Go, 8,
        &budget(), allocation(), || true), Err(ImportError::Canceled)));
    assert!(matches!(CapturedImports::build(&vec![b' '; MAX_IMPORT_SOURCE_BYTES + 1], request(), generation(),
        ImportLanguage::Go, 8, &budget(), allocation(), || false), Err(ImportError::SourceLimit)));
    let other = ArenaOwnerId::new(owner().get() + 1).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), QueryGeneration::new(other, 7).unwrap(),
        ImportLanguage::Go, 8, &budget(), allocation(), || false), Err(ImportError::OwnerMismatch)));
}

#[test]
fn captured_module_directives_are_bounded_and_nearest_boundaries_do_not_fall_through() {
    for source in [b"// module fake\nmodule example.com/project\ngo 1.23\n".as_slice(),
        b"module \"example.com/project\"\nrequire (\nother.example/lib v1.0.0\n)\n",
        b"module (\n`example.com/project`\n)\n"] {
        let captured = module(b"nested/go.mod", source);
        assert_eq!(captured.module_path(), "example.com/project");
        assert_eq!(captured.root(), b"nested"); assert_eq!(captured.request(), request());
    }
    for source in [b"module one\nmodule two\n".as_slice(), b"module ../escape\n",
        b"/* unsupported */\nmodule one\n", b"module \"escaped\\path\"\n", b"module (\none\ntwo\n)\n"] {
        assert!(CapturedGoModule::build(b"go.mod", source, request(), &budget(), allocation(), || false).is_err());
    }
    assert!(matches!(CapturedGoModule::build(b"go.mod", &vec![b' '; MAX_GO_MODULE_SOURCE_BYTES + 1],
        request(), &budget(), allocation(), || false), Err(ImportError::SourceLimit)));
    assert!(matches!(CapturedGoModule::build(b"go.mod", b"module one\n", request(), &budget(), allocation(), || true),
        Err(ImportError::Canceled)));
    let found = find_go_module(b"nested/pkg/file.go", |path| match path {
        b"nested/go.mod" => Some(2), b"go.mod" => panic!("Nearest module must win"), _ => None,
    }).unwrap();
    assert_eq!(found.target, 2); assert_eq!(found.path(), b"nested/go.mod");
}

#[test]
fn module_local_package_candidates_preserve_prefixes_boundaries_and_native_paths() {
    let source = b"package app\nimport (\"example.com/project/core\"; \"fmt\"; \"example.com/projectile/core\"; \"C\")\n";
    let imports = build(source, 8);
    let captured = module(b"nested/go.mod", b"module example.com/project\n");
    let files: [&[u8]; 5] = [b"nested/core/_ignored.go", b"nested/core/a_test.go", b"nested/core/b.go", b"nested/core/c.go", b"nested/core/sub/d.go"];
    let result = resolve_go_import(b"nested/cmd/main.go", &imports.facts()[0], Some(&captured), |_| false,
        |prefix| files.iter().position(|path| go_package_source(prefix, path)));
    assert_eq!(result.target, Some(2)); assert_eq!(result.basis, "go-module-package-file-candidate");
    for fact in &imports.facts()[1..] {
        let result: fcb_analysis::imports::ImportResolution<usize> = resolve_go_import(b"nested/cmd/main.go", fact,
            Some(&captured), |_| panic!("External import must not inspect local module boundaries"), |_| panic!("No guessed package"));
        assert_eq!(result.kind, ImportResolutionKind::External);
    }
    let nested: fcb_analysis::imports::ImportResolution<usize> = resolve_go_import(b"nested/cmd/main.go", &imports.facts()[0],
        Some(&captured), |path| path == b"nested/core/go.mod", |_| panic!("Cannot cross a nested module"));
    assert_eq!(nested.kind, ImportResolutionKind::Unresolved); assert_eq!(nested.basis, "go-nested-module-boundary");
    let missing: fcb_analysis::imports::ImportResolution<usize> = resolve_import(b"nested/cmd/main.go", &imports.facts()[0], |_| None);
    assert_eq!(missing.kind, ImportResolutionKind::Unresolved); assert_eq!(missing.basis, "go-module-context-unavailable");
    let raw = module(b"\xff/go.mod", b"module example.com/project\n");
    let result = resolve_go_import(b"\xff/main.go", &imports.facts()[0], Some(&raw), |_| false,
        |prefix| (prefix == b"\xff/core/").then_some(9));
    assert_eq!(result.target, Some(9));
}
