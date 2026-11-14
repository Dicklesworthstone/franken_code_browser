#![forbid(unsafe_code)]

//! Real pinned-lexer extraction and closed-catalog navigation. These are
//! lexical candidates, not compiler preprocessing or include-path resolution.
use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    ImportResolutionKind, MAX_IMPORT_SOURCE_BYTES, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80631).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn build(source: &[u8], limit: usize) -> CapturedImports<'_> {
    CapturedImports::build(source, request(), generation(), ImportLanguage::Cpp, limit,
        &budget(), allocation(), || false).unwrap()
}
fn names<'a>(imports: &'a CapturedImports<'_>) -> Vec<&'a str> {
    imports.facts().iter().map(|fact| fact.destination()).collect()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() });
    }
    bytes
}

#[test]
fn quoted_angle_and_computed_includes_preserve_original_evidence() {
    let source = br####"# include "local.h" // preserved source
/*
#include "comment.h"
*/
const char *plain = "#include <string.h>";
const char *raw = R"tag(
#include "raw.h"
)tag";
#define EXPANSION(x) #include "macro-body.h"
#include <vector>
#include HEADER
#include MAKE_HEADER(name)
#if OPTIONAL_FEATURE
#include "../public/api.hpp"
#endif
"####;
    let imports = build(source, 32);
    assert_eq!(names(&imports), ["local.h", "vector", "HEADER", "MAKE_HEADER(name)", "../public/api.hpp"]);
    assert_eq!(imports.facts().iter().map(|fact| fact.kind()).collect::<Vec<_>>(),
        [ImportKind::CppQuotedInclude, ImportKind::CppAngleInclude, ImportKind::CppComputedInclude,
            ImportKind::CppComputedInclude, ImportKind::CppQuotedInclude]);
    assert_eq!(imports.source_bytes(1).unwrap(), b"# include \"local.h\"");
    assert!(!imports.facts()[2].resolution_allowed());
    assert!(imports.is_complete());
    for fact in imports.facts() {
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], fact.destination().as_bytes());
        assert!(fact.original_range().start() <= fact.specifier_range().start());
        assert!(fact.specifier_range().end() <= fact.original_range().end());
    }
}

#[test]
fn directive_word_reassembly_only_joins_adjacent_owner_terminals() {
    let source = br#"#include "one.h"
# include "two.h"
#         include "three.h"
#	include "four.h"
/* leading comment */ # /* separation */ include "five.h"
#inc lude "not-a-directive.h"
#incl/**/ude "also-not-a-directive.h"
value #include "not-line-leading.h"
const char *raw = R"(payload
)"; #include "after-a-string.h"
"#;
    assert_eq!(names(&build(source, 32)), ["one.h", "two.h", "three.h", "four.h", "five.h"]);
}

#[test]
fn utf16_and_utf8_bom_specifiers_map_to_original_units_without_unescaping() {
    let text = "// 😀\r\n# include \"../café.h\"\r\n";
    for big in [false, true] {
        let source = utf16(text, big);
        let imports = build(&source, 8);
        assert_eq!(names(&imports), ["../café.h"]);
        let fact = &imports.facts()[0];
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], &utf16("../café.h", big)[2..]);
        assert_eq!(start, 2 + text[..text.find("../").unwrap()].encode_utf16().count() * 2);
        assert_eq!(imports.source_bytes(1).unwrap(), &utf16("# include \"../café.h\"", big)[2..]);
    }
    let source = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
    let imports = build(&source, 8);
    assert_eq!(imports.facts()[0].specifier_range().start().get() as usize, 3 + text.find("../").unwrap());
}

#[test]
fn quoted_headers_use_only_exact_source_relative_case_sensitive_catalog_paths() {
    let source = b"#include \"./child.h\"\n#include \"../public/api.hpp\"\n#include \"child\"\n#include \"Child.h\"\n";
    let imports = build(source, 8);
    let paths: &[&[u8]] = &[b"src/\xff/child.h", b"src/public/api.hpp", b"child.h",
        b"src/\xff/child.hpp", b"src/\xff/child.hh"];
    for (fact, expected) in imports.facts().iter().zip([Some(0), Some(1), None, None]) {
        let mut consulted = Vec::new();
        let result = resolve_import(b"src/\xff/main.CPP", fact, |path| {
            consulted.push(path.to_vec()); paths.iter().position(|candidate| *candidate == path)
        });
        assert_eq!(result.target, expected);
        assert_eq!(consulted.len(), 1, "no guessed extension, basename or include-directory search");
        assert_eq!(result.kind, if expected.is_some() { ImportResolutionKind::Local } else { ImportResolutionKind::Missing });
        if expected.is_some() { assert_eq!(result.basis, "cpp-source-relative-include-candidate"); }
    }
    let literal = build(b"#include \"%2e%2e/header.h\"", 1);
    let result = resolve_import(b"src/main.c", &literal.facts()[0], |path|
        (path == b"src/%2e%2e/header.h").then_some(4));
    assert_eq!(result.target, Some(4), "percent text is a literal filename, never URL-decoded");
}

#[test]
fn external_computed_escaped_and_escaping_headers_never_probe_the_catalog() {
    let source = br#"#include <vector>
#include HEADER
#include "../outside.h"
#include "/absolute.h"
#include "C:/drive.h"
#include "dir\header.h"
#include "one.h" "two.h"
"#;
    let imports = build(source, 16);
    assert_eq!(imports.facts().len(), 7);
    for (i, fact) in imports.facts().iter().enumerate() {
        let result = resolve_import(b"main.cpp", fact, |_| -> Option<u64> { panic!("unqualified catalog lookup") });
        assert_eq!(result.kind, if i == 0 { ImportResolutionKind::External } else { ImportResolutionKind::Unresolved });
        assert!(result.target.is_none());
    }
    let quoted = build(b"#include \"child.h\"", 1);
    assert_eq!(resolve_import(b"main.rs", &quoted.facts()[0], |_| -> Option<u64> { panic!("foreign language path") }).kind,
        ImportResolutionKind::Unresolved);
}

#[test]
fn unsupported_translation_phases_are_refused_instead_of_complete_empty_results() {
    for source in [
        b"// hidden \\\n#include \"fake.h\"\n".as_slice(),
        b"// hidden \\\r\n#include \"fake.h\"\r\n",
        b"#define MANY(x) x \\ \t\n + x\n#include \"otherwise-valid.h\"",
        b"// old Mac\r#include \"fake.h\"\r",
        b"??=include \"trigraph.h\"\n",
        b"// hidden ??/\n#include \"fake.h\"\n",
        br###"const char *raw = R"!(payload "
#include "fake.h"
)!";
"###,
    ] {
        let budget = budget();
        let result = CapturedImports::build(source, request(), generation(), ImportLanguage::Cpp,
            8, &budget, allocation(), || false);
        assert!(matches!(result, Err(ImportError::UnsupportedLexicalContext)), "{source:?}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    for prefix in ["R", "u8R", "uR", "UR", "LR"] {
        let source = format!("const char *s = {prefix}\"!(payload \"\n#include \"fake.h\"\n)!\";");
        assert!(matches!(CapturedImports::build(source.as_bytes(), request(), generation(), ImportLanguage::Cpp,
            8, &budget(), allocation(), || false), Err(ImportError::UnsupportedLexicalContext)));
    }
    let permitted = b"// ordinary comment\n#define SINGLE(x) (x)\n#include \"real.h\"\n";
    assert_eq!(names(&build(permitted, 8)), ["real.h"]);
}

#[test]
fn output_limits_duplicates_and_malformed_directives_remain_visible() {
    let source = b"#include \"a.h\"\n#include \"a.h\"\n#include HEADER\n";
    for limit in 0..=3 {
        let imports = build(source, limit);
        assert_eq!(imports.links_seen(), 3); assert_eq!(imports.facts().len(), limit);
        assert_eq!(imports.truncated(), limit < 3); assert_eq!(imports.is_complete(), limit == 3);
    }
    let malformed = format!("#include\n#include \"\"\n#include \"{}\"\n#include \"valid.h\"\n", "x".repeat(1025));
    let imports = build(malformed.as_bytes(), 8);
    assert_eq!(names(&imports), ["valid.h"]);
    assert_eq!(imports.diagnostics(), 3); assert!(!imports.is_complete());
}

#[test]
fn cancellation_and_admission_release_all_candidate_resources() {
    let source = b"# include \"header.h\"\n#include <vector>\n";
    let budget = budget(); let mut checks = 0;
    let probe = CapturedImports::build(source, request(), generation(), ImportLanguage::Cpp, 8,
        &budget, allocation(), || { checks += 1; false }).unwrap();
    drop(probe);
    for stop in 1..=checks {
        let mut visited = 0;
        let result = CapturedImports::build(source, request(), generation(), ImportLanguage::Cpp, 8,
            &budget, allocation(), || { visited += 1; visited == stop });
        assert!(matches!(result, Err(ImportError::Canceled)), "checkpoint {stop}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    assert!(matches!(CapturedImports::build(&vec![b'x'; MAX_IMPORT_SOURCE_BYTES + 1], request(), generation(),
        ImportLanguage::Cpp, 8, &budget, allocation(), || false), Err(ImportError::SourceLimit)));
    for extension in ["c", "h", "cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "ipp", "tpp", "inl", "ixx"] {
        assert_eq!(ImportLanguage::from_path(format!("dir/file.{}", extension.to_uppercase()).as_bytes()), Some(ImportLanguage::Cpp));
    }
    assert_eq!(ImportLanguage::from_path(b"file.cpp.txt"), None);
    assert_eq!(ImportLanguage::from_path(b"raw/\xff.HPP"), Some(ImportLanguage::Cpp));
}
