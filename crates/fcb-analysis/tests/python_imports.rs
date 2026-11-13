#![forbid(unsafe_code)]

//! Python import navigation exercises the public exact-capture engine and
//! closed-catalog resolver. The oracle is original byte evidence and the finite
//! declared candidate set, never host Python execution or sys.path contents.

use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    ImportResolutionKind, IMPORT_EXTRACTOR, IMPORT_POLICY, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80623).unwrap() }
fn file() -> FileId { FileId::new(owner(), 1).unwrap() }
fn revision() -> SourceRevision { SourceRevision::new(owner(), 1).unwrap() }
fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
fn request() -> CaptureRequest { CaptureRequest::new(file(), revision()).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn build(source: &[u8], limit: usize) -> CapturedImports<'_> {
    CapturedImports::build(source, request(), generation(), ImportLanguage::Python,
        limit, &budget(), allocation(), || false).unwrap()
}
fn destinations<'a>(imports: &'a CapturedImports<'_>) -> Vec<&'a str> {
    imports.facts().iter().map(|fact| fact.destination()).collect()
}
fn specifier<'a>(source: &'a [u8], imports: &CapturedImports<'_>, index: usize) -> &'a [u8] {
    let (start, end) = imports.facts()[index].specifier_range().as_usize_bounds().unwrap();
    &source[start..end]
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() { bytes.extend_from_slice(&if big { word.to_be_bytes() } else { word.to_le_bytes() }); }
    bytes
}

#[test]
fn python_language_and_policy_are_explicit_for_sources_and_stubs() {
    for path in [b"pkg/main.py".as_slice(), b"pkg/api.pyi", b"dir\xff/API.PYI"] {
        assert_eq!(ImportLanguage::from_path(path), Some(ImportLanguage::Python));
    }
    assert_eq!(ImportLanguage::Python.name(), "python");
    assert_eq!(ImportKind::PythonImport.name(), "python-import");
    assert_eq!(ImportKind::PythonFrom.name(), "python-from");
    assert_eq!(IMPORT_POLICY, "rust-mod-use-js-static-and-python-import-v2");
    assert_eq!(IMPORT_EXTRACTOR, "franken-markdown-d6fbd1d5/import-statements-v2");
    assert_eq!(ImportLanguage::from_path(b"pkg.py/data.txt"), None);
}

#[test]
fn aliases_and_multiple_modules_keep_each_occurrence_and_the_whole_statement() {
    let source = b"import os, pkg.tools as tools;\nfrom .worker import Thing as Alias, Other\nfrom ..shared.tools import *\nimport os\n";
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), ["os", "pkg.tools", ".worker", "..shared.tools", "os"]);
    assert!(imports.is_complete());
    assert_eq!(imports.links_seen(), 5);
    assert_eq!(imports.source_bytes(1).unwrap(), b"import os, pkg.tools as tools;");
    assert_eq!(imports.source_bytes(1).unwrap(), imports.source_bytes(2).unwrap());
    assert_eq!(imports.source_bytes(3).unwrap(), b"from .worker import Thing as Alias, Other");
    assert_eq!(specifier(source, &imports, 0), b"os");
    assert_eq!(specifier(source, &imports, 1), b"pkg.tools");
    assert_eq!(specifier(source, &imports, 2), b".worker");
    assert_eq!(imports.facts()[0].kind(), ImportKind::PythonImport);
    assert_eq!(imports.facts()[2].kind(), ImportKind::PythonFrom);
    assert_ne!(imports.facts()[0].original_range(), imports.facts()[4].original_range());
}

#[test]
fn upstream_type_colors_and_soft_keywords_remain_valid_module_names() {
    let source = b"import type, match, case\nfrom . import list, print, open\n";
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), ["type", "match", "case", ".list", ".print", ".open"]);
    assert!(imports.is_complete());
    assert!(imports.facts().iter().all(|fact| fact.resolution_allowed()));
}

#[test]
fn bare_relative_imports_name_finite_candidates_without_fabricating_contiguous_source() {
    let source = b"from . import (\n    helper as alias, # exact comment\n    other,\n)\nfrom .. import sibling\nfrom .module import (Thing, Other as Alias,)\nfrom . import *\n";
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), [".helper", ".other", "..sibling", ".module", "."]);
    assert_eq!(specifier(source, &imports, 0), b"helper");
    assert_eq!(specifier(source, &imports, 1), b"other");
    assert_eq!(specifier(source, &imports, 2), b"sibling");
    assert_eq!(specifier(source, &imports, 3), b".module");
    assert_eq!(imports.source_bytes(1).unwrap(), b"from . import (\n    helper as alias, # exact comment\n    other,\n)");
    assert_eq!(imports.source_bytes(1).unwrap(), imports.source_bytes(2).unwrap());
    assert!(imports.facts()[0].resolution_allowed());
    assert!(!imports.facts()[4].resolution_allowed(), "a bare-relative star has no named module candidate");
    assert!(imports.is_complete(), "resolution limits do not erase lexical evidence");
}

#[test]
fn legal_continuations_and_inter_token_spaces_keep_exact_specifier_spans() {
    let source = b"import pkg . \\\r\n child as alias, \\\r\n other\nfrom . \\\n util import (\n thing as name,\n)\nfrom . import (a, \\\n\n b)\n";
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), ["pkg.child", "other", ".util", ".a", ".b"]);
    assert_eq!(specifier(source, &imports, 0), b"pkg . \\\r\n child");
    assert_eq!(specifier(source, &imports, 2), b". \\\n util");
    assert_eq!(imports.source_bytes(3).unwrap(), b"from . \\\n util import (\n thing as name,\n)");
    assert!(imports.is_complete());
}

#[test]
fn upstream_python_strings_comments_and_dynamic_calls_do_not_create_static_facts() {
    let source = br####"
# import fake_comment
ordinary = "from . import fake_string"
raw = r"import fake_raw"
bytes_literal = b"import fake_bytes"
doc = """
from .fake_doc import Thing
import fake_multiline
"""
formatted = f"""from . import fake_format {value}"""
single = f"from . import fake_single {value}"
__import__("dynamic")
importlib.import_module(".dynamic", "pkg")
from .real import Thing
"####;
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), [".real"]);
    assert!(imports.is_complete());
    let unterminated = build(b"doc = '''\nfrom . import hidden\n", 20);
    assert!(unterminated.facts().is_empty(), "the upstream trailing string owns all apparent statements");
}

#[test]
fn malformed_imports_publish_no_partial_facts_and_recover_at_the_next_logical_line() {
    for malformed in [
        "import good,", "import good, other as", "import .bad", "import (good)",
        "import good as if", "from .module import Good,", "from . import (good,, bad)",
        "from .module import (Good as, Other)", "from .module import pkg.name",
        "from .module import ()", "from . import (*)", "from .module import Good extra",
        "from ..module. import Good", "from import Good", "from . import good as",
    ] {
        let source = format!("{malformed}\nfrom .real import Value\n");
        let imports = build(source.as_bytes(), 20);
        assert_eq!(destinations(&imports), [".real"], "{malformed}");
        assert_eq!(imports.diagnostics(), 1, "{malformed}");
        assert!(!imports.is_complete(), "{malformed}");
    }
    for source in [b"from . import (good".as_slice(), b"from .module import (Good,"] {
        let imports = build(source, 20);
        assert!(imports.facts().is_empty());
        assert_eq!(imports.diagnostics(), 1);
    }
}

#[test]
fn physical_newlines_and_comments_cannot_silently_join_separate_statements() {
    let source = b"from .module\nimport actual\nfrom . import a, # no continuation\nfrom .next import B\nfrom . import \\\n# ends the logical line\nfrom .real import Value\n";
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), ["actual", ".next", ".real"]);
    assert_eq!(imports.diagnostics(), 3);
    let invalid_escape = build(b"from . import \\ \nfrom .real import Value\n", 20);
    assert_eq!(destinations(&invalid_escape), [".real"]);
    assert_eq!(invalid_escape.diagnostics(), 1);
}

#[test]
fn nested_and_inline_suites_work_without_treating_expression_keywords_as_statements() {
    let source = b"if enabled: from . import helper; import external\n\
def function():\n    from .inner import Thing\n\
field: import false_annotation\n\
value = lambda: import false_lambda\n\
value = (from .false_expression import Thing)\n\
yield from values\n";
    let imports = build(source, 20);
    assert_eq!(destinations(&imports), [".helper", "external", ".inner"]);
    assert!(imports.is_complete());
}

#[test]
fn utf16_both_endians_and_utf8_bom_preserve_original_statement_and_imported_name_bytes() {
    let text = "# \u{1f600}\r\nfrom . import helper as chosen\r\n";
    for big in [false, true] {
        let source = utf16(text, big);
        let imports = build(&source, 20);
        assert_eq!(destinations(&imports), [".helper"]);
        let fact = &imports.facts()[0];
        assert_eq!(fact.original_range().start().get(), 14);
        assert_eq!(fact.original_range().end().get(), 74);
        assert_eq!(fact.specifier_range().start().get(), 42);
        assert_eq!(fact.specifier_range().end().get(), 54);
        assert_eq!(imports.source_bytes(1).unwrap(), &utf16("from . import helper as chosen", big)[2..]);
        assert_eq!(specifier(&source, &imports, 0), &utf16("helper", big)[2..]);
        assert!(imports.validate_source(&source, file(), revision(), generation()).is_ok());
        assert_eq!(imports.validate_source(b"changed", file(), revision(), generation()), Err(ImportError::StaleSource));
    }
    let mut source = vec![0xef, 0xbb, 0xbf];
    source.extend_from_slice(text.as_bytes());
    let imports = build(&source, 20);
    assert_eq!(imports.facts()[0].original_range().start().get(), 11);
    assert_eq!(specifier(&source, &imports, 0), b"helper");
}

#[test]
fn finite_python_resolution_preserves_native_parent_bytes_and_does_not_infer_imported_symbols() {
    let source = b"from .worker import Tool\nfrom . import helper\nfrom ..shared.tools import Thing\n";
    let imports = build(source, 20);
    let paths: &[&[u8]] = &[b"pkg/\xff/worker.py", b"pkg/\xff/helper/__init__.py", b"pkg/shared/tools.py"];
    for (index, fact) in imports.facts().iter().enumerate() {
        let mut calls = 0;
        let result = resolve_import(b"pkg/\xff/main.pyi", fact, |path| {
            calls += 1;
            paths.iter().position(|candidate| *candidate == path)
        });
        assert_eq!(result.target, Some(index));
        assert_eq!(result.kind, ImportResolutionKind::Local);
        assert_eq!(result.basis, "python-relative-module-candidate");
        assert_eq!(calls, 2, "the complete module/package pair must be checked");
    }
    let result = resolve_import(b"pkg/main.py", &imports.facts()[0],
        |path| (path == b"pkg/worker/Tool.py").then_some(1));
    assert_eq!(result.kind, ImportResolutionKind::Missing, "an imported member is not another module edge");
    let result = resolve_import(b"pkg/__init__.py", &imports.facts()[1],
        |path| (path == b"pkg/helper.py").then_some(1));
    assert_eq!(result.target, Some(1));
}

#[test]
fn absolute_ambiguous_missing_and_root_escape_outcomes_never_guess_a_target() {
    let imports = build(b"import os.path\nfrom pkg.tools import Thing\nfrom .helper import Thing\nfrom ...outside import Thing\n", 20);
    for fact in &imports.facts()[..2] {
        let result = resolve_import::<usize>(b"pkg/main.py", fact, |_| panic!("absolute imports cannot inspect sys.path or local lookalikes"));
        assert_eq!(result.kind, ImportResolutionKind::External);
        assert_eq!(result.basis, "external-specifier");
    }
    let relative = &imports.facts()[2];
    let result = resolve_import(b"pkg/main.py", relative,
        |path| [b"pkg/helper.py".as_slice(), b"pkg/helper/__init__.py"].contains(&path).then_some(7));
    assert_eq!(result.kind, ImportResolutionKind::Ambiguous, "distinct candidate paths remain ambiguous even for one identity");
    assert_eq!(result.target, None);
    let result = resolve_import(b"pkg/main.py", relative, |path| (path == b"pkg/helper.pyi").then_some(7));
    assert_eq!(result.kind, ImportResolutionKind::Missing, "stub discovery is not runtime module resolution");
    let result = resolve_import::<usize>(b"pkg/main.py", &imports.facts()[3], |_| panic!("root escape looked up"));
    assert_eq!(result.kind, ImportResolutionKind::Unresolved);
}

#[test]
fn unicode_module_spelling_remains_exact_evidence_without_assuming_identifier_normalization() {
    let source = "from .caf\u{e9} import Thing\n";
    let imports = build(source.as_bytes(), 20);
    assert_eq!(destinations(&imports), [".caf\u{e9}"]);
    assert_eq!(specifier(source.as_bytes(), &imports, 0), ".caf\u{e9}".as_bytes());
    assert!(!imports.facts()[0].resolution_allowed());
    let result = resolve_import::<usize>(b"pkg/main.py", &imports.facts()[0],
        |_| panic!("an unmodeled normalized identifier cannot authorize lookup"));
    assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    assert!(imports.is_complete());
}

#[test]
fn output_limits_count_all_valid_occurrences_and_statement_limits_are_explicit() {
    let source = b"import a, b, c\nfrom . import one, two\n";
    for limit in [0, 2, 5] {
        let imports = build(source, limit);
        assert_eq!(imports.links_seen(), 5);
        assert_eq!(imports.facts().len(), limit);
        assert_eq!(imports.truncated(), limit < 5);
        assert_eq!(imports.is_complete(), limit == 5);
    }
    for statement in [
        format!("import {}", "a".repeat(1025)),
        format!("from . import ({}last)", "item,".repeat(1024)),
        format!("from . import ({}name)", " ".repeat(16 * 1024)),
    ] {
        let source = format!("{statement}\nfrom .next import Value\n");
        let imports = build(source.as_bytes(), 20);
        assert_eq!(destinations(&imports), [".next"]);
        assert_eq!(imports.diagnostics(), 1);
        assert!(!imports.is_complete());
    }
}

#[test]
fn python_capture_cancellation_and_resource_admission_fail_before_publishing_rows() {
    let source = b"from . import (one, two, three)\n";
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Python,
        20, &tiny, allocation(), || false), Err(ImportError::ResourceDenied)));
    let mut polls = 0;
    assert!(matches!(CapturedImports::build(source, request(), generation(), ImportLanguage::Python,
        20, &budget(), allocation(), || { polls += 1; polls > 10 }), Err(ImportError::Canceled)));
    let imports = build(source, 20);
    assert_eq!(imports.validate_delivery(file(), revision(), QueryGeneration::new(owner(), 2).unwrap()),
        Err(ImportError::StaleQuery));
}
