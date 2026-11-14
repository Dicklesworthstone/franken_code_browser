#![forbid(unsafe_code)]

//! Literal CommonJS and import() candidates use real upstream token spans and
//! exact captured bytes. Navigation is explicitly not Node/compiler binding.

use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    ImportResolutionKind, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80627).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn request() -> CaptureRequest { CaptureRequest::new(file(), revision()).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn build(source: &[u8], language: ImportLanguage, limit: usize) -> CapturedImports<'_> {
    CapturedImports::build(source, request(), generation(), language, limit, &budget(), allocation(), || false).unwrap()
}
fn destinations<'a>(imports: &'a CapturedImports<'_>) -> Vec<&'a str> {
    imports.facts().iter().map(|fact| fact.destination()).collect()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() {
        bytes.extend_from_slice(&if big { word.to_be_bytes() } else { word.to_le_bytes() });
    }
    bytes
}

#[test]
fn mixed_static_and_literal_loads_have_stable_distinct_kinds_and_exact_calls() {
    let source = b"import './static.js';\nconst first = require /* retain */ (\n './shared.cjs',\n);\nconst later = () => import('./shared.cjs');\nexport { x } from './reexport.js';\n";
    for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript, ImportLanguage::Jsx, ImportLanguage::Tsx] {
        let result = build(source, language, 20);
        assert!(result.is_complete());
        assert_eq!(destinations(&result), ["./static.js", "./shared.cjs", "./shared.cjs", "./reexport.js"]);
        let kinds: Vec<_> = result.facts().iter().map(|fact| fact.kind()).collect();
        assert_eq!(kinds, [ImportKind::JavaScriptImport, ImportKind::JavaScriptRequire,
            ImportKind::JavaScriptDynamicImport, ImportKind::JavaScriptExport]);
        assert_eq!(result.source_bytes(2).unwrap(), b"require /* retain */ (\n './shared.cjs',\n)");
        assert_eq!(result.source_bytes(3).unwrap(), b"import('./shared.cjs')");
        assert_ne!(result.facts()[1].original_range(), result.facts()[2].original_range());
        for (i, fact) in result.facts().iter().enumerate() {
            assert_eq!(fact.id(), i as u64 + 1);
            assert!(fact.resolution_allowed());
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert_eq!(&source[start..end], fact.destination().as_bytes());
        }
    }
}

#[test]
fn typescript_import_equals_and_type_queries_remain_lexical_load_expressions() {
    let source = b"import Tool = require('./tool');\nexport import More = require('./more');\ntype Value = import('./types').Value;\n";
    let result = build(source, ImportLanguage::TypeScript, 20);
    assert!(result.is_complete());
    assert_eq!(destinations(&result), ["./tool", "./more", "./types"]);
    assert_eq!(result.source_bytes(1).unwrap(), b"require('./tool')");
    assert_eq!(result.source_bytes(2).unwrap(), b"require('./more')");
    assert_eq!(result.facts()[2].kind(), ImportKind::JavaScriptDynamicImport);
}

#[test]
fn upstream_strings_comments_regex_and_jsx_text_cannot_fabricate_calls() {
    let source = br#"
// require('./comment'); import('./comment');
/* require('./block'); */
const text = "require('./quoted')";
const template = `import('./template')`;
const regex = /require\(['"]fake['"]\)/;
const value = require("./real");
"#;
    for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript] {
        let result = build(source, language, 20);
        assert!(result.is_complete()); assert_eq!(destinations(&result), ["./real"]);
    }
    let jsx = br#"const view = <div title="require('./attribute')">require('./text') import('./text') {require /* code */ ('./child')}</div>;"#;
    for language in [ImportLanguage::Jsx, ImportLanguage::Tsx] {
        let result = build(jsx, language, 20);
        assert!(result.is_complete()); assert_eq!(destinations(&result), ["./child"]);
    }
}

#[test]
fn qualified_and_constructor_calls_are_not_bare_loader_candidates() {
    let source = b"loader.require('./member'); loader?.require('./optional'); new require('./constructor'); require.resolve('./resolve'); import.meta.url;\nrequire('./real');";
    let result = build(source, ImportLanguage::JavaScript, 20);
    assert!(result.is_complete()); assert_eq!(destinations(&result), ["./real"]);
    // The bare spelling is useful navigational evidence even when its binding
    // is locally shadowed. It retains a distinct, explicitly lexical kind.
    let shadowed = build(b"function run(require) { return require('./candidate'); }", ImportLanguage::JavaScript, 20);
    assert_eq!(shadowed.facts()[0].kind(), ImportKind::JavaScriptRequire);
    assert_eq!(destinations(&shadowed), ["./candidate"]);
}

#[test]
fn computed_templates_extra_arguments_and_unclosed_calls_remain_partial() {
    for source in [
        b"require(name);".as_slice(), b"require('./base' + suffix);",
        b"require('./first', other);", b"require(`./template`);", b"require('./unfinished'",
        b"import(name);", b"import('./base' + suffix);",
        b"import('./first', { with: { type: 'json' } });", b"import(`./template`);",
    ] {
        let result = build(source, ImportLanguage::JavaScript, 20);
        assert!(result.facts().is_empty(), "{source:?}");
        assert!(!result.is_complete(), "{source:?}");
        assert!(result.diagnostics() > 0, "{source:?}");
    }
    // An unsupported outer expression must not swallow an independent, useful
    // literal candidate contained within it.
    let nested = build(b"require(transform(require('./inner'))); import('./valid');", ImportLanguage::JavaScript, 20);
    assert_eq!(destinations(&nested), ["./inner", "./valid"]);
    assert!(!nested.is_complete()); assert_eq!(nested.diagnostics(), 1);
}

#[test]
fn escaped_specifiers_retain_exact_evidence_without_guessed_unescaping() {
    let source = br#"require('./escaped\u002ejs'); import("./line\nbreak"); require('./real');"#;
    let result = build(source, ImportLanguage::JavaScript, 20);
    assert!(result.is_complete());
    assert_eq!(destinations(&result), [r"./escaped\u002ejs", r"./line\nbreak", "./real"]);
    for fact in &result.facts()[..2] {
        assert!(!fact.resolution_allowed());
        let resolution = resolve_import(b"src/main.cjs", fact, |_| -> Option<u64> {
            panic!("escaped path must not be looked up")
        });
        assert_eq!(resolution.kind, ImportResolutionKind::Unresolved);
    }
    assert!(result.facts()[2].resolution_allowed());
}

#[test]
fn literal_load_paths_use_the_closed_catalog_and_never_node_package_resolution() {
    let source = b"require('./tool'); import('./tool'); require('node:fs'); import('package');";
    let result = build(source, ImportLanguage::JavaScript, 20);
    for fact in &result.facts()[..2] {
        let resolved = resolve_import(b"src/\xff/main.cjs", fact, |path| (path == b"src/\xff/tool.cjs").then_some(8));
        assert_eq!(resolved.kind, ImportResolutionKind::Local);
        assert_eq!(resolved.target, Some(8)); assert_eq!(resolved.basis, "unique-relative-module-candidate");
        let ambiguous = resolve_import(b"src/main.mjs", fact, |path|
            [b"src/tool.cjs".as_slice(), b"src/tool.js"].contains(&path).then_some(8));
        assert_eq!(ambiguous.kind, ImportResolutionKind::Ambiguous); assert_eq!(ambiguous.target, None);
        let exact = resolve_import(b"src/main.mjs", fact, |path|
            (path == b"src/tool" || path == b"src/tool.js").then_some(9));
        assert_eq!(exact.kind, ImportResolutionKind::Local); assert_eq!(exact.basis, "exact-relative-path");
    }
    for fact in &result.facts()[2..] {
        let external = resolve_import(b"src/main.cjs", fact, |_| -> Option<u64> {
            panic!("external module names must not probe the catalog")
        });
        assert_eq!(external.kind, ImportResolutionKind::External); assert_eq!(external.target, None);
    }
}

#[test]
fn source_evidence_maps_to_original_utf16_and_bom_bytes() {
    let text = "// 😀\r\nconst value = require('./λ');\r\nimport('./next');";
    for big in [false, true] {
        let source = utf16(text, big); let result = build(&source, ImportLanguage::JavaScript, 20);
        assert_eq!(destinations(&result), ["./λ", "./next"]); assert!(result.is_complete());
        assert_eq!(result.source_bytes(1).unwrap(), &utf16("require('./λ')", big)[2..]);
        assert_eq!(result.source_bytes(2).unwrap(), &utf16("import('./next')", big)[2..]);
        for fact in result.facts() {
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert_eq!(&source[start..end], &utf16(fact.destination(), big)[2..]);
        }
    }
    let mut source = vec![0xef, 0xbb, 0xbf]; source.extend_from_slice(text.as_bytes());
    let result = build(&source, ImportLanguage::JavaScript, 20);
    let expected_start = 3 + text.find("require").unwrap();
    assert_eq!(result.facts()[0].original_range().start().get(), expected_start as u64);
    assert_eq!(result.source_bytes(1).unwrap(), "require('./λ')".as_bytes());
}

#[test]
fn bounded_output_preserves_duplicate_occurrence_counts_and_partial_coverage() {
    let source = b"require('./same'); import('./same'); require('./same');";
    for limit in 0..=3 {
        let result = build(source, ImportLanguage::JavaScript, limit);
        assert_eq!(result.links_seen(), 3); assert_eq!(result.facts().len(), limit);
        assert_eq!(result.is_complete(), limit == 3); assert_eq!(result.truncated(), limit < 3);
    }
    let long = format!("require('{}');", "x".repeat(1025));
    let result = build(long.as_bytes(), ImportLanguage::JavaScript, 20);
    assert!(result.facts().is_empty()); assert_eq!(result.diagnostics(), 1);
    let long_call = format!("require /*{}*/ ('./bounded');", "x".repeat(16 * 1024));
    let result = build(long_call.as_bytes(), ImportLanguage::JavaScript, 20);
    assert!(result.facts().is_empty()); assert_eq!(result.diagnostics(), 1);
}

#[test]
fn literal_loads_preserve_source_generation_cancellation_and_admission_guards() {
    let source = b"require('./one'); import('./two');";
    let result = build(source, ImportLanguage::JavaScript, 20);
    assert_eq!(result.validate_source(b"replacement", file(), revision(), generation()), Err(ImportError::StaleSource));
    assert_eq!(result.validate_delivery(file(), revision(), QueryGeneration::new(owner(), 2).unwrap()),
        Err(ImportError::StaleQuery));
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::JavaScript,
        20, &budget(), allocation(), || true), Err(ImportError::Canceled)));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::JavaScript,
        20, &tiny, allocation(), || false), Err(ImportError::ResourceDenied)));
}

#[test]
fn control_condition_regex_ambiguity_refuses_false_facts_but_not_real_adjacent_calls() {
    for control in ["if (ready)", "while ((ready()))", "for (;;)","for await (item of items)"] {
        for load in ["require('fake-package')", "import('fake-package')"] {
            let source = format!("{control} /{load}/.test(text); require('./real');");
            for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript, ImportLanguage::Jsx, ImportLanguage::Tsx] {
                assert!(matches!(CapturedImports::build(source.as_bytes(), request(), generation(), language,
                    20, &budget(), allocation(), || false), Err(ImportError::UnsupportedLexicalContext)), "{source}");
            }
        }
        let permitted = format!("{control} require('./real');");
        let result = build(permitted.as_bytes(), ImportLanguage::JavaScript, 20);
        assert!(result.is_complete()); assert_eq!(destinations(&result), ["./real"]);
    }
    for source in [b"(ready) / require('./real');".as_slice(), b"ready() / require('./real');",
        b"object.if(ready) / require('./real');", b"if (ready) { require('./real'); }"] {
        let result = build(source, ImportLanguage::JavaScript, 20);
        assert!(result.is_complete()); assert_eq!(destinations(&result), ["./real"]);
    }
}

#[test]
fn labeled_jump_regex_ambiguity_respects_line_boundaries_and_comments() {
    for jump in ["break", "continue"] {
        for gap in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}", " /*\ncomment*/ ", " // comment\n"] {
            for load in ["require('fake-package')", "import('fake-package')"] {
                let source = format!("outer: while (true) {{ {jump} /* label */ outer{gap}/{load}/.test(text); }} require('./real');");
                for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript, ImportLanguage::Jsx, ImportLanguage::Tsx] {
                    assert!(matches!(CapturedImports::build(source.as_bytes(), request(), generation(), language,
                        20, &budget(), allocation(), || false), Err(ImportError::UnsupportedLexicalContext)), "{source}");
                }
            }
        }
        let source = format!("outer: while (true) {{ {jump} outer; /require('fake-package')/.test(text); }} require('./real');");
        let result = build(source.as_bytes(), ImportLanguage::JavaScript, 20);
        assert!(result.is_complete()); assert_eq!(destinations(&result), ["./real"]);
    }
    for source in [b"outer / require('./real');".as_slice(),
        b"object.break(outer) / require('./real');",
        b"if (ready) {} /require('fake-package')/.test(text); require('./real');"] {
        let result = build(source, ImportLanguage::JavaScript, 20);
        assert!(result.is_complete()); assert_eq!(destinations(&result), ["./real"]);
    }
}

#[test]
fn ambiguous_newline_slash_context_is_partial_instead_of_a_local_asi_parser() {
    for prefix in ["let x", "var x", "let first = 1, last", "var first = 1, last",
        "let x: Value", "type Name = Value", "outer", "object.break(outer)"] {
        for gap in ["\n", " /*\r\ncomment*/ ", "\u{2028}", "\u{2029}"] {
            for load in ["require('fake-package')", "import('fake-package')"] {
                let source = format!("{prefix}{gap}/{load}/.test(text); require('./real');");
                for language in [ImportLanguage::JavaScript, ImportLanguage::TypeScript, ImportLanguage::Jsx, ImportLanguage::Tsx] {
                    assert!(matches!(CapturedImports::build(source.as_bytes(), request(), generation(), language,
                        20, &budget(), allocation(), || false), Err(ImportError::UnsupportedLexicalContext)), "{source}");
                }
            }
        }
    }
    // The same conservative boundary also declines legal newline-leading
    // division; it never promotes ambiguous owner terminals into false facts.
    let source = b"value\n / require('./real');";
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::JavaScript,
        20, &budget(), allocation(), || false), Err(ImportError::UnsupportedLexicalContext)));
}
