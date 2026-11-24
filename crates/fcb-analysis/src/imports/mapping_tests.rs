#![forbid(unsafe_code)]

use super::*;
use fcb_core::{ArenaOwnerId, ByteOffset};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(897).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn resources(bytes: u64) -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap() }
fn build<'source>(source: &'source [u8], language: ImportLanguage, limit: usize,
    budget: &ResourceBudget, canceled: impl FnMut() -> bool)
    -> Result<CapturedImports<'source>, ImportError> {
    CapturedImports::build(source, request(), generation(), language, limit, budget,
        ResourceAllocationId::new(1).unwrap(), canceled)
}
fn encode(text: &str, little: Option<bool>, bom: bool) -> Vec<u8> {
    let Some(little) = little else {
        let mut raw = if bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
        raw.extend_from_slice(text.as_bytes()); return raw;
    };
    let mut raw = if !bom { Vec::new() } else if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        raw.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    raw
}

#[test]
fn scalar_dense_utf16_uses_language_admission_and_retains_only_fact_capacity() {
    let source_bytes = 128 * 1024;
    for (language, tail, destination, admitted) in [
        (ImportLanguage::Rust, "*/\nmod target;\n", "target", 16 * 1024 * 1024),
        (ImportLanguage::Jsx, "*/\nimport \"./target\";\n", "./target", 32 * 1024 * 1024),
        (ImportLanguage::Tsx, "*/\nimport \"./target\";\n", "./target", 32 * 1024 * 1024),
    ] {
        let text = format!("/*{}{}", "中".repeat((source_bytes - 2) / 2 - 2 - tail.len()), tail);
        assert!(text.len() > source_bytes);
        for little in [false, true] {
            let source = encode(&text, Some(little), true);
            assert_eq!(source.len(), source_bytes);
            let budget = resources(admitted);
            // The old full-capture map reserved over 96 MiB for these 128 KiB.
            let imports = build(&source, language, 4, &budget, || false).unwrap();
            assert_eq!(imports.links_seen(), 1);
            assert_eq!(imports.facts()[0].destination(), destination);
            assert!(imports.is_complete());
            let range = imports.facts()[0].specifier_range();
            let (start, end) = range.as_usize_bounds().unwrap();
            assert_eq!(&source[start..end], encode(destination, Some(little), false));
            let retained = size_of::<CapturedImports<'_>>()
                + imports.facts.capacity() * size_of::<ImportFact>()
                + imports.facts.iter().map(|fact| fact.destination.capacity()).sum::<usize>();
            assert_eq!(budget.accounting().managed().get(), retained as u64);
            assert!(budget.accounting().peak_reserved().get() <= admitted);
            assert!(budget.accounting().peak_reserved().get() > retained as u64 * 100);
            drop(imports);
            assert_eq!(budget.accounting().reserved().get(), 0);
            if matches!(language, ImportLanguage::Jsx | ImportLanguage::Tsx) {
                let smaller = resources(16 * 1024 * 1024);
                assert!(matches!(build(&source, language, 4, &smaller, || false), Err(ImportError::ResourceDenied)));
                assert_eq!(smaller.accounting().reserved().get(), 0);
            }
        }
    }
}

#[test]
fn late_import_specifiers_cross_decoder_boundaries_in_both_original_encodings() {
    for little in [None, Some(false), Some(true)] {
        // UTF-8 é crosses 4096; UTF-16's emoji high surrogate starts 4094.
        let prefix_bytes = if little.is_some() { 2035 } else { 4085 };
        let prefix = format!("/*{}*/\n", " ".repeat(prefix_bytes - 5));
        let statement = "import \"./é😀.js\";";
        let text = format!("{prefix}{statement}\n");
        let source = encode(&text, little, little.is_some());
        let budget = resources(16 * 1024 * 1024);
        let imports = build(&source, ImportLanguage::JavaScript, 4, &budget, || false).unwrap();
        let fact = &imports.facts()[0];
        assert_eq!(fact.destination(), "./é😀.js");
        assert_eq!(fact.original_range().start().get(), encode(&prefix, little, little.is_some()).len() as u64);
        assert_eq!(imports.source_bytes(1).unwrap(), encode(statement, little, false));
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], encode("./é😀.js", little, false));
        assert!(imports.is_complete());
    }
}

#[test]
fn full_decode_validation_survives_zero_or_one_retained_fact_limits() {
    let text = format!("mod first;\n/*{}*/\nmod last;", " ".repeat(8192));
    for limit in [0, 1] {
        for little in [None, Some(false), Some(true)] {
            let mut source = encode(&text, little, little.is_some());
            source.extend_from_slice(&match little {
                None => vec![0xf0],
                Some(true) => vec![0x00, 0xd8],
                Some(false) => vec![0xd8, 0x00],
            });
            let budget = resources(16 * 1024 * 1024);
            assert!(matches!(build(&source, ImportLanguage::Rust, limit, &budget, || false),
                Err(ImportError::UnsupportedEncoding)));
            assert_eq!(budget.accounting().reserved().get(), 0);
            let retry = build(b"mod retry;", ImportLanguage::Rust, 1, &budget, || false).unwrap();
            assert_eq!(retry.facts()[0].destination(), "retry");
        }
    }
}

#[test]
fn bounded_fact_output_keeps_complete_counts_and_canonical_path_amplification() {
    let prefix = format!("crate::{}", "p".repeat(900));
    let source = format!("use {prefix}::{{Alpha, Beta, Gamma, Delta}};");
    let budget = resources(4 * 1024 * 1024);
    for limit in [0, 1, 4] {
        let imports = build(source.as_bytes(), ImportLanguage::Rust, limit, &budget, || false).unwrap();
        assert_eq!(imports.links_seen(), 4);
        assert_eq!(imports.facts().len(), limit);
        assert_eq!(imports.truncated(), limit != 4);
        for (index, fact) in imports.facts().iter().enumerate() {
            let leaf = ["Alpha", "Beta", "Gamma", "Delta"][index];
            assert_eq!(fact.id(), index as u64 + 1);
            assert_eq!(fact.destination(), format!("{prefix}::{leaf}"));
            assert_eq!(imports.source_bytes(fact.id()).unwrap(), source.as_bytes());
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert_eq!(&source.as_bytes()[start..end], leaf.as_bytes());
        }
        if limit == 4 {
            assert!(imports.facts().iter().map(|fact| fact.destination().len()).sum::<usize>() > source.len() * 2);
        }
        drop(imports);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn denied_or_canceled_imports_release_admission_before_an_exact_retry() {
    let budget = resources(2 * 1024 * 1024);
    assert!(matches!(build(&b" ".repeat(128 * 1024), ImportLanguage::Rust, 1, &budget, || false),
        Err(ImportError::ResourceDenied)));
    assert_eq!(budget.accounting().reserved().get(), 0);
    let mut polls = 0;
    assert!(matches!(build(b"mod old;", ImportLanguage::Rust, 1, &budget, || {
        polls += 1; polls == 4
    }), Err(ImportError::Canceled)));
    assert_eq!(polls, 4);
    assert_eq!(budget.accounting().reserved().get(), 0);
    let imports = build(b"mod old;", ImportLanguage::Rust, 1, &budget, || false).unwrap();
    assert!(imports.validate_source(b"mod old;", request().file(), request().revision(), generation()).is_ok());
    assert_eq!(imports.validate_source(b"mod new;", request().file(), request().revision(), generation()),
        Err(ImportError::StaleSource));
    assert_eq!(imports.validate_delivery(request().file(), request().revision(),
        QueryGeneration::new(owner(), 2).unwrap()), Err(ImportError::StaleQuery));
    drop(imports);
    assert_eq!(budget.accounting().reserved().get(), 0);
    let extent = request().with_range(ByteRange::new(ByteOffset::new(100), ByteOffset::new(108)).unwrap()).unwrap();
    assert!(matches!(CapturedImports::build(b"mod old;", extent, generation(), ImportLanguage::Rust, 1,
        &budget, ResourceAllocationId::new(1).unwrap(), || false), Err(ImportError::InvalidEvidence)));
    assert_eq!(budget.accounting().reserved().get(), 0);
}


#[test]
fn all_import_languages_keep_late_relationships_in_larger_utf8_and_utf16_sources() {
    let cases = [
        (ImportLanguage::Rust, "//", "mod child;", "child", ImportKind::RustModule),
        (ImportLanguage::JavaScript, "//", "import \"./child.js\";", "./child.js", ImportKind::JavaScriptImport),
        (ImportLanguage::TypeScript, "//", "import \"./child.js\";", "./child.js", ImportKind::JavaScriptImport),
        (ImportLanguage::Jsx, "//", "import \"./child.js\";", "./child.js", ImportKind::JavaScriptImport),
        (ImportLanguage::Tsx, "//", "import \"./child.js\";", "./child.js", ImportKind::JavaScriptImport),
        (ImportLanguage::Python, "#", "import package.helper", "package.helper", ImportKind::PythonImport),
        (ImportLanguage::Cpp, "//", "#include \"child.hpp\"", "child.hpp", ImportKind::CppQuotedInclude),
        (ImportLanguage::Go, "//", "import \"example/pkg\"", "example/pkg", ImportKind::GoImport),
        (ImportLanguage::Java, "//", "import example.library.Helper;", "example.library.Helper", ImportKind::JavaImport),
    ];
    for (language, comment, statement, destination, kind) in cases {
        let prefix = format!("{}{}", if language == ImportLanguage::Go { "package example\n" } else { "" },
            format!("{comment} held source\n").repeat(10_000));
        let text = format!("{prefix}{statement}\n");
        assert!(text.len() > 128 * 1024);
        for little in [None, Some(false), Some(true)] {
            let source = encode(&text, little, little.is_some());
            let budget = resources(128 * 1024 * 1024);
            let imports = build(&source, language, 8, &budget, || false).unwrap();
            assert!(imports.is_complete(), "incomplete {}", language.name());
            assert_eq!(imports.links_seen(), 1);
            let fact = &imports.facts()[0];
            assert_eq!(fact.kind(), kind);
            assert_eq!(fact.destination(), destination);
            assert_eq!(fact.original_range().start().get(),
                encode(&prefix, little, little.is_some()).len() as u64);
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert_eq!(&source[start..end], encode(destination, little, false));
            assert_eq!(imports.validate_delivery(request().file(), request().revision(), generation()), Ok(()));
            drop(imports);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }
}

#[test]
fn maximum_original_capture_keeps_utf16_decoded_expansion_and_exact_final_evidence() {
    for (language, tail, destination, admitted) in [
        (ImportLanguage::Rust, "*/\nmod edge;", "edge", 128 * 1024 * 1024),
        (ImportLanguage::Jsx, "*/\nimport \"./edge\";", "./edge", 224 * 1024 * 1024),
        (ImportLanguage::Tsx, "*/\nimport \"./edge\";", "./edge", 224 * 1024 * 1024),
    ] {
        for little in [None, Some(false), Some(true)] {
            let original_units = if little.is_some() { (MAX_IMPORT_SOURCE_BYTES - 2) / 2 }
                else { MAX_IMPORT_SOURCE_BYTES };
            let fill = if little.is_some() { "中" } else { "x" };
            let text = format!("/*{}{}", fill.repeat(original_units - 2 - tail.len()), tail);
            if little.is_some() {
                assert!(text.len() > MAX_IMPORT_SOURCE_BYTES);
                assert!(text.len() <= MAX_IMPORT_SOURCE_BYTES * 3 / 2);
            }
            let source = encode(&text, little, little.is_some());
            assert_eq!(source.len(), MAX_IMPORT_SOURCE_BYTES);
            let budget = resources(admitted);
            let imports = build(&source, language, 4, &budget, || false).unwrap();
            assert!(imports.is_complete());
            assert_eq!(imports.links_seen(), 1);
            let fact = &imports.facts()[0];
            assert_eq!(fact.destination(), destination);
            let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
            assert!(start > 128 * 1024);
            assert_eq!(&source[start..end], encode(destination, little, false));
            assert!(budget.accounting().peak_reserved().get() <= admitted);
            drop(imports);
            assert_eq!(budget.accounting().reserved().get(), 0);
            let oversized = b" ".repeat(MAX_IMPORT_SOURCE_BYTES + 1);
            assert!(matches!(build(&oversized, language, 4, &budget, || false), Err(ImportError::SourceLimit)));
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }
}

#[test]
fn shared_mapping_does_not_impose_symbol_line_limits_on_imports() {
    let text = format!("{}/*{}*/\nmod later;", "\n".repeat(70_000), "中".repeat(24_000));
    assert!(text.len() > 128 * 1024);
    for little in [None, Some(false), Some(true)] {
        let source = encode(&text, little, little.is_some());
        let budget = resources(64 * 1024 * 1024);
        let imports = build(&source, ImportLanguage::Rust, 4, &budget, || false).unwrap();
        assert!(imports.is_complete());
        assert_eq!(imports.links_seen(), 1);
        assert_eq!(imports.facts()[0].destination(), "later");
        let (start, end) = imports.facts()[0].specifier_range().as_usize_bounds().unwrap();
        assert_eq!(&source[start..end], encode("later", little, false));
    }
}
