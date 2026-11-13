#![forbid(unsafe_code)]

//! Production extraction tests for bounded lexical Rust use-tree evidence.
//! Catalog navigation and compiler semantics are separate from this grammar.

use fcb_analysis::imports::{CapturedImports, ImportError, ImportKind, ImportLanguage,
    MAX_IMPORT_SPECIFIER_BYTES};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(80625).unwrap() }
fn build_with(source: &[u8], limit: usize, canceled: impl FnMut() -> bool) -> Result<CapturedImports<'_>, ImportError> {
    let request = CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap();
    let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    CapturedImports::build(source, request, QueryGeneration::new(owner(), 1).unwrap(), ImportLanguage::Rust,
        limit, &budget, ResourceAllocationId::new(1).unwrap(), canceled)
}
fn build(source: &[u8], limit: usize) -> CapturedImports<'_> { build_with(source, limit, || false).unwrap() }
fn paths<'a>(imports: &'a CapturedImports<'_>) -> Vec<&'a str> {
    imports.facts().iter().map(|fact| fact.destination()).collect()
}
fn specifiers<'a>(imports: &CapturedImports<'a>, source: &'a [u8]) -> Vec<&'a [u8]> {
    imports.facts().iter().map(|fact| {
        let (start, end) = fact.specifier_range().as_usize_bounds().unwrap();
        &source[start..end]
    }).collect()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for unit in text.encode_utf16() { bytes.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() }); }
    bytes
}

#[test]
fn grouped_aliases_self_globs_and_raw_names_keep_exact_leaf_evidence() {
    let source = b"pub use crate /* source */ :: net::{self as networking, tcp::{Client, r#type as Type}, udp::*,};";
    let imports = build(source, 20);
    assert_eq!(paths(&imports), ["crate::net", "crate::net::tcp::Client", "crate::net::tcp::r#type", "crate::net::udp::*"]);
    assert_eq!(specifiers(&imports, source), [b"self".as_slice(), b"Client", b"r#type", b"udp::*"]);
    assert_eq!(imports.links_seen(), 4);
    assert!(imports.is_complete());
    for fact in imports.facts() {
        assert_eq!(fact.kind(), ImportKind::RustUse);
        assert!(fact.resolution_allowed());
        assert_eq!(imports.source_bytes(fact.id()).unwrap(), &source[4..]);
        assert!(fact.original_range().start() <= fact.specifier_range().start());
        assert!(fact.specifier_range().end() <= fact.original_range().end());
    }
}

#[test]
fn root_groups_absolute_paths_and_aliases_preserve_order_and_distinct_occurrences() {
    let source = b"use {crate::one as _, self::two as r#type, super::super::three, self::super::four};\nuse ::std::{fmt::Debug, io};\nuse crate::{one, one};";
    let imports = build(source, 20);
    assert_eq!(paths(&imports), ["crate::one", "self::two", "super::super::three", "self::super::four",
        "::std::fmt::Debug", "::std::io", "crate::one", "crate::one"]);
    assert!(imports.is_complete());
    assert_eq!(imports.facts()[6].original_range(), imports.facts()[7].original_range());
    assert_ne!(imports.facts()[6].specifier_range(), imports.facts()[7].specifier_range());
    for (index, fact) in imports.facts().iter().enumerate() { assert_eq!(fact.id(), index as u64 + 1); }
}

#[test]
fn empty_groups_have_no_imports_and_sibling_prefixes_do_not_leak() {
    let source = b"use {}; use crate::{empty::{}, first::{A, B}, second::C, D}; use ::{external};";
    let imports = build(source, 20);
    assert_eq!(paths(&imports), ["crate::first::A", "crate::first::B", "crate::second::C", "crate::D", "::external"]);
    assert_eq!(imports.links_seen(), 5); assert!(imports.is_complete());
    let empty = build(b"use {}; use crate::nothing::{};", 0);
    assert!(empty.facts().is_empty()); assert!(empty.is_complete()); assert_eq!(empty.links_seen(), 0);
}

#[test]
fn final_self_and_raw_keywords_keep_the_module_path_and_leaf_spelling() {
    let source = b"use crate::net::self as network; use crate::{net::self, r#use, r#mod as r#fn};";
    let imports = build(source, 20);
    assert_eq!(paths(&imports), ["crate::net", "crate::net", "crate::r#use", "crate::r#mod"]);
    assert_eq!(specifiers(&imports, source), [b"crate::net::self".as_slice(), b"net::self", b"r#use", b"r#mod"]);
    assert!(imports.is_complete());
    let names = build(b"const r#use: bool = true; fn r#mod() {}", 20);
    assert!(names.facts().is_empty()); assert!(names.is_complete());
}

#[test]
fn nested_and_path_override_contexts_keep_evidence_without_module_authority() {
    for source in [
        b"mod inner { use crate::{one, two}; }".as_slice(),
        b"fn run() { use crate::{one, two}; }".as_slice(),
        b"wrapper! { use crate::{one, two}; }".as_slice(),
        b"#[path = \"alternate.rs\"] mod child; use crate::{one, two};".as_slice(),
        b"#[cfg_attr(feature = \"x\", path = \"alternate.rs\")] mod child; use crate::{one, two};".as_slice(),
    ] {
        let imports = build(source, 20);
        let uses: Vec<_> = imports.facts().iter().filter(|fact| fact.kind() == ImportKind::RustUse).collect();
        assert_eq!(uses.len(), 2, "{source:?}");
        assert!(uses.iter().all(|fact| !fact.resolution_allowed()), "{source:?}");
        assert!(imports.is_complete());
    }
    let imports = build(b"fn run() { use crate::inner; } use crate::outer;", 20);
    assert!(!imports.facts()[0].resolution_allowed()); assert!(imports.facts()[1].resolution_allowed());
    let configured = build(b"#[cfg(test)] use crate::tests;", 20);
    assert!(configured.facts()[0].resolution_allowed(), "a lexical candidate does not claim cfg evaluation");
}

#[test]
fn bad_late_branches_never_publish_earlier_normalized_targets() {
    for source in [
        b"use crate::{Good, Bad as };".as_slice(),
        b"use crate::{Good, Bad::};".as_slice(),
        b"use crate::{Good,,Bad};".as_slice(),
        b"use crate::{Good, Bad + Other};".as_slice(),
        b"use crate::{Good, * as Alias};".as_slice(),
        b"use crate : : item;".as_slice(),
        b"use crate::r #type;".as_slice(),
        b"use crate::{Good, r#crate};".as_slice(),
        b"use crate::{Good, r#self};".as_slice(),
        b"use crate::{Good, r#super};".as_slice(),
        b"use crate::{Good, r#Self};".as_slice(),
        b"use crate::{Good, r#_};".as_slice(),
        b"use crate::{Good, $crate::Bad};".as_slice(),
        b"use crate::{Good, self::Bad};".as_slice(),
        b"use crate::{Good, foo::super::Bad};".as_slice(),
        b"use crate::{Good, foo::crate::Bad};".as_slice(),
    ] {
        let imports = build(source, 20);
        assert!(!imports.is_complete(), "{source:?}");
        assert_eq!(imports.diagnostics(), 1, "{source:?}");
        assert_eq!(imports.facts().len(), 1, "{source:?}");
        let fact = &imports.facts()[0];
        assert!(!fact.resolution_allowed());
        assert_eq!(fact.destination().as_bytes(), &source[4..source.len() - 1]);
        assert_eq!(imports.source_bytes(1).unwrap(), source);
    }
}

#[test]
fn original_utf16_and_bom_leaf_ranges_remain_exact() {
    let text = "// 😀\r\nuse crate::module::{Thing as T, Other};";
    for big in [false, true] {
        let source = utf16(text, big); let imports = build(&source, 20);
        assert_eq!(paths(&imports), ["crate::module::Thing", "crate::module::Other"]);
        let slices = specifiers(&imports, &source);
        assert_eq!(slices[0], &utf16("Thing", big)[2..]);
        assert_eq!(slices[1], &utf16("Other", big)[2..]);
        assert_eq!(imports.source_bytes(1).unwrap(), &utf16("use crate::module::{Thing as T, Other};", big)[2..]);
        assert!(imports.is_complete());
    }
    let mut source = vec![0xef, 0xbb, 0xbf]; source.extend_from_slice(text.as_bytes());
    let imports = build(&source, 20);
    assert_eq!(specifiers(&imports, &source), [b"Thing".as_slice(), b"Other"]);
    assert_eq!(imports.facts()[0].specifier_range().start().get(), 3 + text.find("Thing").unwrap() as u64);
}

#[test]
fn output_depth_statement_and_path_budgets_are_explicit() {
    let source = b"use crate::{A, B::{self, C}, *};";
    for limit in [0, 1, 2, 3, 4] {
        let imports = build(source, limit);
        assert_eq!(imports.links_seen(), 4); assert_eq!(imports.facts().len(), limit);
        assert_eq!(imports.truncated(), limit < 4); assert_eq!(imports.is_complete(), limit == 4);
    }
    let too_deep = format!("use crate::{}thing{};", "{".repeat(33), "}".repeat(33));
    let imports = build(too_deep.as_bytes(), 20);
    assert_eq!(imports.facts().len(), 1); assert!(!imports.facts()[0].resolution_allowed());
    assert_eq!(imports.diagnostics(), 1); assert!(!imports.is_complete());
    let too_many = format!("use crate::{{{}}};", "A,".repeat(600));
    let imports = build(too_many.as_bytes(), 20);
    assert!(imports.facts().is_empty()); assert!(!imports.is_complete());
    let too_long = format!("use crate::{};", "a".repeat(MAX_IMPORT_SPECIFIER_BYTES));
    let imports = build(too_long.as_bytes(), 20);
    assert!(imports.facts().is_empty()); assert!(!imports.is_complete());
}

#[test]
fn large_group_cancellation_discards_all_candidate_output() {
    let source = format!("use crate::{{{}}};", (0..200).map(|n| format!("item{n},")).collect::<String>());
    let mut complete_polls = 0;
    let imports = build_with(source.as_bytes(), 200, || { complete_polls += 1; false }).unwrap();
    assert_eq!(imports.facts().len(), 200); assert!(imports.is_complete());
    assert!(complete_polls > 200);
    let mut polls = 0;
    assert!(matches!(build_with(source.as_bytes(), 200, || {
        polls += 1; polls >= complete_polls / 2
    }), Err(ImportError::Canceled)));
}
