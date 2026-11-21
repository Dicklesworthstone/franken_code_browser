#![forbid(unsafe_code)]

use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    MAX_IMPORT_ITEMS, MAX_IMPORT_SOURCE_BYTES};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80634).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 3).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 7).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn build(source: &[u8], limit: usize) -> CapturedImports<'_> {
    CapturedImports::build(source, request(), generation(), ImportLanguage::Java, limit,
        &budget(), allocation(), || false).unwrap()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() }); }
    bytes
}

#[test]
fn compilation_unit_imports_preserve_static_wildcard_and_multiline_evidence() {
    let source = br#"package example.app;
// import comment.Fake;
import example.core.Service;
import example /* keep original trivia */ . util
    . Helper ;
import example.other.*;
import static example.Constants.VALUE;
import static example.Constants.*;
class App {
    String text = "import literal.Fake;";
    char quote = '\'';
    String block = """
        import block.Fake;
        """;
    void method() { import nested.Fake; }
}
import after.Type;
"#;
    let imports = build(source, 32);
    assert_eq!(imports.facts().iter().map(|fact| (fact.kind(), fact.destination())).collect::<Vec<_>>(), [
        (ImportKind::JavaImport, "example.core.Service"),
        (ImportKind::JavaImport, "example.util.Helper"),
        (ImportKind::JavaImport, "example.other.*"),
        (ImportKind::JavaStaticImport, "example.Constants.VALUE"),
        (ImportKind::JavaStaticImport, "example.Constants.*"),
    ]);
    assert!(imports.is_complete());
    assert_eq!(imports.source_bytes(1).unwrap(), b"import example.core.Service;");
    assert_eq!(imports.source_bytes(2).unwrap(), b"import example /* keep original trivia */ . util\n    . Helper ;");
    let fact = &imports.facts()[1];
    let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
    assert_eq!(&source[start..end], b"example /* keep original trivia */ . util\n    . Helper");
    assert!(imports.facts()[0].resolution_allowed());
    assert!(!imports.facts()[2].resolution_allowed());
    assert!(imports.facts()[3..].iter().all(|fact| fact.resolution_allowed()));
}

#[test]
fn annotated_package_prefix_is_bounded_and_class_annotations_end_import_discovery() {
    let source = br#"@Deprecated
@example.Label(value = {"import fake.Type;", "})"}, nested = @Other("name"))
package example.app;
import example.Real;
@Marker class App { import nested.Fake; }
"#;
    let imports = build(source, 16);
    assert_eq!(imports.facts().len(), 1);
    assert_eq!(imports.facts()[0].destination(), "example.Real");
    assert!(imports.is_complete());
    for source in [b"@Deprecated public class App { import nested.Fake; }".as_slice(),
        b"@interface A { String value() default \"import fake.Type;\"; }",
        b"class App {} import late.Fake;", b"module app { import module.Fake; }"] {
        assert!(build(source, 16).facts().is_empty());
    }
    for source in [b"@A(] package app; import fake.Type;".as_slice(),
        b"@A(value = {\"x\"} package app; import fake.Type;"] {
        let imports = build(source, 16);
        assert!(imports.facts().is_empty());
        assert!(!imports.is_complete() && imports.diagnostics() > 0);
    }
}

#[test]
fn every_supported_separator_keeps_one_canonical_destination_and_exact_source() {
    for separator in [".", " . ", "/* left */. /* right */", "\n.\n", "\t.\u{c}", ".// comment\n"] {
        let specifier = ["example", "core", "Service"].join(separator);
        let statement = format!("import {specifier};");
        let imports = build(statement.as_bytes(), 4);
        assert!(imports.is_complete(), "{separator:?}");
        assert_eq!(imports.facts().len(), 1);
        assert_eq!(imports.facts()[0].destination(), "example.core.Service");
        assert_eq!(imports.source_bytes(1).unwrap(), statement.as_bytes());
        let (start, end) = imports.facts()[0].specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&statement.as_bytes()[start..end], specifier.as_bytes());
    }
}

#[test]
fn unfinished_upstream_literals_cannot_reopen_the_compilation_unit_prefix() {
    for source in ["@A(\"unterminated\n) package app; import fake.Type;",
        "@A('\n) package app; import fake.Type;", "@A(\"unterminated\\\"",
        "@A(\"\"\"\nunfinished text block", "/*/", "/* unfinished comment"] {
        assert!(matches!(CapturedImports::build(source.as_bytes(), request(), generation(),
            ImportLanguage::Java, 8, &budget(), allocation(), || false),
            Err(ImportError::UnsupportedLexicalContext)), "{source:?}");
    }
    let source = br#"@A(
    value = "quoted \" annotation )",
    block = """
        import opaque.Fake;
        """)
package app;
import real.Type;
"#;
    let imports = build(source, 8);
    assert!(imports.is_complete());
    assert_eq!(imports.facts().len(), 1);
    assert_eq!(imports.facts()[0].destination(), "real.Type");
}

#[test]
fn original_byte_ranges_survive_utf8_bom_utf16_endianness_and_surrogates() {
    let statement = "import /* evidence 🦀 */ example.core.Service;";
    let text = format!("// 🦀 prelude\r\npackage app;\r\n{statement}\r\nclass App {{}}\r\n");
    let mut utf8 = vec![0xef, 0xbb, 0xbf]; utf8.extend_from_slice(text.as_bytes());
    for (source, expected_statement, expected_name) in [
        (utf8, statement.as_bytes().to_vec(), b"example.core.Service".to_vec()),
        (utf16(&text, false), utf16(statement, false)[2..].to_vec(), utf16("example.core.Service", false)[2..].to_vec()),
        (utf16(&text, true), utf16(statement, true)[2..].to_vec(), utf16("example.core.Service", true)[2..].to_vec()),
    ] {
        let imports = build(&source, 8);
        assert!(imports.is_complete());
        assert_eq!(imports.facts().len(), 1);
        assert_eq!(imports.source_bytes(1).unwrap(), expected_statement);
        let (start, end) = imports.facts()[0].specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], expected_name);
        assert!(imports.validate_source(&source, request().file(), request().revision(), generation()).is_ok());
    }
}

#[test]
fn literal_unicode_names_are_evidence_and_dollar_names_remain_navigable_candidates() {
    let source = "package café;\nimport café.服务;\nimport example.$Outer._Inner$2;\nimport record.var.Type;\nimport module.open.requires.exports.uses.provides.to.with.opens.transitive.Type;";
    let imports = build(source.as_bytes(), 16);
    assert!(imports.is_complete());
    assert_eq!(imports.facts().iter().map(|fact| fact.destination()).collect::<Vec<_>>(),
        ["café.服务", "example.$Outer._Inner$2", "record.var.Type",
            "module.open.requires.exports.uses.provides.to.with.opens.transitive.Type"]);
    assert!(!imports.facts()[0].resolution_allowed());
    assert!(imports.facts()[1..].iter().all(|fact| fact.resolution_allowed()));
    assert_eq!(imports.source_bytes(1).unwrap(), "import café.服务;".as_bytes());
    for source in ["import example.e\u{301};", "import example._;", "import example.2Type;", "import example.class;"] {
        let imports = build(source.as_bytes(), 8);
        assert!(imports.facts().is_empty());
        assert!(!imports.is_complete() && imports.diagnostics() > 0);
    }
}

#[test]
fn malformed_statement_never_publishes_its_plausible_prefix_or_untrusted_suffix() {
    for malformed in ["import example.MissingSemicolon", "import static Single;",
        "import example..Type;", "import example.*.Type;", "import example.Type as Alias;",
        "import static;", "import \"example.Type\";", "import example.Type, other.Type;",
        "package late;", "import example.Type { import nested.Fake; }"] {
        let source = format!("import good.Type;\n{malformed}\nimport after.Type;");
        let imports = build(source.as_bytes(), 16);
        assert_eq!(imports.facts().len(), 1, "{malformed}");
        assert_eq!(imports.facts()[0].destination(), "good.Type");
        assert!(!imports.is_complete() && imports.diagnostics() > 0, "{malformed}");
    }
    for source in [b"package broken..name; import after.Type;".as_slice(),
        b"package no_semicolon\nimport after.Type;", b"package first; package second; import after.Type;"] {
        let imports = build(source, 16);
        assert!(imports.facts().is_empty());
        assert!(!imports.is_complete());
    }
}

#[test]
fn unqualified_prelexical_escapes_and_whitespace_refuse_explicitly() {
    for source in ["import example.\\u0054ype;", "// hidden \\u000a import fake.Type;\nimport real.Type;",
        "// bare CR\rimport fake.Type;", "import example.\u{2028}Type;", "import example.\u{b}Type;"] {
        assert!(matches!(CapturedImports::build(source.as_bytes(), request(), generation(),
            ImportLanguage::Java, 8, &budget(), allocation(), || false),
            Err(ImportError::UnsupportedLexicalContext)), "{source:?}");
    }
    let permitted = "// Unicode comment whitespace \u{2028}\nimport example.Type;\nclass A { String text = \"\u{2028}\"; }";
    assert!(build(permitted.as_bytes(), 8).is_complete());
    assert!(build(b"// CRLF\r\nimport example.Type;", 8).is_complete());
}

#[test]
fn source_statement_identifier_and_annotation_limits_have_no_partial_fact() {
    let long_name = format!("import example.{};", "T".repeat(1024));
    let long_statement = format!("import example /* {} */ . Type;", " ".repeat(16 * 1024));
    let many_terms = format!("import {}Type;", "a.".repeat(512));
    let deep_annotation = format!("@A({}0{}) package app; import example.Type;", "(".repeat(64), ")".repeat(64));
    for source in [long_name, long_statement, many_terms, deep_annotation] {
        let imports = build(source.as_bytes(), 8);
        assert!(imports.facts().is_empty());
        assert!(!imports.is_complete() && imports.diagnostics() > 0);
    }
    assert!(matches!(CapturedImports::build(&vec![b' '; MAX_IMPORT_SOURCE_BYTES + 1], request(), generation(),
        ImportLanguage::Java, 8, &budget(), allocation(), || false), Err(ImportError::SourceLimit)));
    assert!(matches!(CapturedImports::build(b"", request(), generation(), ImportLanguage::Java,
        MAX_IMPORT_ITEMS + 1, &budget(), allocation(), || false), Err(ImportError::InvalidLimits)));
}

#[test]
fn retained_limit_and_seen_count_are_independent_including_zero_rows() {
    let source = b"import one.Type; import two.Type; import static three.Type.MEMBER; import four.*;";
    for limit in [0, 1, 3] {
        let imports = build(source, limit);
        assert_eq!(imports.facts().len(), limit);
        assert_eq!(imports.links_seen(), 4);
        assert!(imports.truncated() && !imports.is_complete());
    }
    let imports = build(source, 4);
    assert_eq!(imports.links_seen(), 4);
    assert!(imports.is_complete());
}

#[test]
fn cancellation_admission_and_capture_identity_remain_explicit() {
    let source = b"import example.Type;";
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Java, 8,
        &budget(), allocation(), || true), Err(ImportError::Canceled)));
    let mut checkpoints = 0;
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Java, 8,
        &budget(), allocation(), || { checkpoints += 1; checkpoints > 7 }), Err(ImportError::Canceled)));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Java, 8,
        &tiny, allocation(), || false), Err(ImportError::ResourceDenied)));
    let other = ArenaOwnerId::new(owner().get() + 1).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), QueryGeneration::new(other, 7).unwrap(),
        ImportLanguage::Java, 8, &budget(), allocation(), || false), Err(ImportError::OwnerMismatch)));
    let imports = build(source, 8);
    assert_eq!(imports.validate_source(b"import replaced.Type;", request().file(), request().revision(), generation()),
        Err(ImportError::StaleSource));
    assert_eq!(imports.validate_delivery(request().file(), request().revision(), QueryGeneration::new(owner(), 8).unwrap()),
        Err(ImportError::StaleQuery));
    assert_eq!(imports.source_bytes(0), Err(ImportError::NotFound));
    assert_eq!(imports.source_bytes(2), Err(ImportError::NotFound));
}

#[test]
fn path_selection_and_public_kind_names_are_explicit() {
    for path in [b"src/App.java".as_slice(), b"raw\xff/App.JAVA", b"App.Java"] {
        assert_eq!(ImportLanguage::from_path(path), Some(ImportLanguage::Java));
    }
    assert_eq!(ImportLanguage::from_path(b"folder.java/file.txt"), None);
    assert_eq!(ImportLanguage::Java.name(), "java");
    assert_eq!(ImportKind::JavaImport.name(), "java-import");
    assert_eq!(ImportKind::JavaStaticImport.name(), "java-static-import");
}
