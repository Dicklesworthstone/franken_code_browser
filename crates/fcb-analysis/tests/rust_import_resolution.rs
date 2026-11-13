#![forbid(unsafe_code)]

//! Actual captured Rust statements feed the shared catalog-only resolver. The
//! fixtures distinguish conventional module candidates from compiler bindings
//! and preserve the source statement even when navigation cannot choose a file.

use fcb_analysis::imports::{CapturedImports, ImportKind, ImportLanguage,
    ImportResolution, ImportResolutionKind, resolve_import};
use fcb_core::{ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId,
    ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;

fn capture(source: &[u8]) -> CapturedImports<'_> {
    let owner = ArenaOwnerId::new(80624).unwrap();
    let request = CaptureRequest::new(FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap()).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    CapturedImports::build(source, request, QueryGeneration::new(owner, 1).unwrap(),
        ImportLanguage::Rust, 64, &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap()
}

fn resolution(source: &[u8], destination: &str, catalog: &[&[u8]]) -> ImportResolution<usize> {
    let statement = format!("use {destination};");
    let imports = capture(statement.as_bytes());
    assert_eq!(imports.facts().len(), 1, "{destination}");
    let fact = &imports.facts()[0];
    assert_eq!(fact.kind(), ImportKind::RustUse);
    resolve_import(source, fact, |path| catalog.iter().position(|candidate| *candidate == path))
}

fn local(result: ImportResolution<usize>, expected: usize, basis: &str) {
    assert_eq!(result.kind, ImportResolutionKind::Local);
    assert_eq!(result.target, Some(expected));
    assert_eq!(result.basis, basis);
}

#[test]
fn crate_paths_open_exact_modules_or_explicit_containing_module_candidates() {
    let paths: &[&[u8]] = &[b"src/lib.rs", b"src/api.rs", b"src/api/error/mod.rs"];
    local(resolution(b"src/lib.rs", "crate::api", paths), 1, "rust-use-module-file-candidate");
    local(resolution(b"src/lib.rs", "crate::api::Thing as Alias", paths), 1,
        "rust-use-module-prefix-candidate");
    local(resolution(b"src/lib.rs", "crate::api::error::Error", paths), 2,
        "rust-use-module-prefix-candidate");
    local(resolution(b"src/lib.rs", "crate::api::error", paths), 2,
        "rust-use-module-file-candidate");
    let missing = resolution(b"src/lib.rs", "crate::Unknown", paths);
    assert_eq!(missing.kind, ImportResolutionKind::Missing);
    assert_eq!(missing.target, None, "an unrecognized name must not become the root file");
    assert_eq!(missing.basis, "no-catalog-target");
}

#[test]
fn grouped_aliases_self_and_globs_keep_each_statement_and_its_module_candidate() {
    let source = b"use crate::api::{self as interface, Thing, error::{self, Error as Failure}, *};";
    let imports = capture(source);
    let paths: &[&[u8]] = &[b"src/lib.rs", b"src/api.rs", b"src/api/error.rs"];
    let expected = [
        ("crate::api", 1, "rust-use-module-file-candidate"),
        ("crate::api::Thing", 1, "rust-use-module-prefix-candidate"),
        ("crate::api::error", 2, "rust-use-module-file-candidate"),
        ("crate::api::error::Error", 2, "rust-use-module-prefix-candidate"),
        ("crate::api::*", 1, "rust-use-module-file-candidate"),
    ];
    assert_eq!(imports.facts().len(), expected.len());
    for (fact, &(name, target, basis)) in imports.facts().iter().zip(&expected) {
        assert_eq!(fact.destination(), name);
        assert!(fact.resolution_allowed());
        assert_eq!(imports.source_bytes(fact.id()).unwrap(), source);
        local(resolve_import(b"src/lib.rs", fact,
            |path| paths.iter().position(|candidate| *candidate == path)), target, basis);
    }
    local(resolution(b"src/lib.rs", "crate::*", paths), 0, "rust-use-module-file-candidate");
}

#[test]
fn self_uses_flat_and_mod_files_consistently_and_never_follows_an_alias_name() {
    for source in [b"src/net.rs".as_slice(), b"src/net/mod.rs"] {
        let paths: &[&[u8]] = &[b"src/lib.rs", source, b"src/net/io.rs", b"src/net/Alias.rs"];
        local(resolution(source, "self::io", paths), 2, "rust-use-module-file-candidate");
        local(resolution(source, "self::io::Stream as Alias", paths), 2,
            "rust-use-module-prefix-candidate");
        local(resolution(source, "self::Type", paths), 1, "rust-use-module-prefix-candidate");
        local(resolution(source, "self::*", paths), 1, "rust-use-module-file-candidate");
    }
    let paths: &[&[u8]] = &[b"src/main.rs", b"src/io.rs", b"src/main/io.rs"];
    local(resolution(b"src/main.rs", "self::io", paths), 1, "rust-use-module-file-candidate");
}

#[test]
fn super_and_self_super_ascend_logical_modules_and_stop_at_the_crate_candidate() {
    for parent in [b"src/net.rs".as_slice(), b"src/net/mod.rs"] {
        for source in [b"src/net/detail.rs".as_slice(), b"src/net/detail/mod.rs"] {
            let paths: &[&[u8]] = &[b"src/lib.rs", parent, source, b"src/net/io.rs", b"src/shared.rs"];
            local(resolution(source, "super::io", paths), 3, "rust-use-module-file-candidate");
            local(resolution(source, "self::super::io", paths), 3, "rust-use-module-file-candidate");
            local(resolution(source, "super::Thing", paths), 1, "rust-use-module-prefix-candidate");
            local(resolution(source, "super::super::shared", paths), 4, "rust-use-module-file-candidate");
            local(resolution(source, "super::super::RootType", paths), 0, "rust-use-module-prefix-candidate");
            assert_eq!(resolution(source, "super::super::super::outside", paths).kind,
                ImportResolutionKind::Unresolved);
        }
    }
    assert_eq!(resolution(b"src/lib.rs", "super::outside", &[b"src/lib.rs", b"outside.rs"]).kind,
        ImportResolutionKind::Unresolved);
}

#[test]
fn conventional_roots_are_nearest_catalog_evidence_and_preserve_raw_native_parents() {
    let paths: &[&[u8]] = &[b"lib.rs", b"crates/\xff/src/lib.rs", b"crates/\xff/src/net/client.rs",
        b"crates/\xff/src/api.rs", b"api.rs"];
    local(resolution(b"crates/\xff/src/net/client.rs", "crate::api", paths), 3,
        "rust-use-module-file-candidate");
    let roots: &[&[u8]] = &[b"src/lib.rs", b"src/main.rs", b"src/api.rs"];
    // An explicitly selected root names its own conventional crate, without
    // deciding which of these roots owns a separate non-root module file.
    for source in [b"src/lib.rs".as_slice(), b"src/main.rs"] {
        local(resolution(source, "crate::api", roots), 2, "rust-use-module-file-candidate");
    }
    let missing = resolution(b"src/other.rs", "crate::api", &[b"src/other.rs", b"src/api.rs"]);
    assert_eq!(missing.kind, ImportResolutionKind::Unresolved, "no root was established");
    assert_eq!(resolution(b"src/lib.rs", "crate::api", &[b"src/api.rs"]).kind,
        ImportResolutionKind::Unresolved, "an absent explicit root is not fabricated");
}

#[test]
fn two_crate_roots_and_two_module_forms_are_ambiguous_even_for_one_target_identity() {
    let imports = capture(b"use crate::api::Thing;");
    let fact = &imports.facts()[0];
    let roots: &[&[u8]] = &[b"src/lib.rs", b"src/main.rs", b"src/api.rs"];
    let result = resolve_import(b"src/consumer.rs", fact, |path| roots.contains(&path).then_some(1));
    assert_eq!(result.kind, ImportResolutionKind::Ambiguous);
    assert_eq!(result.target, None);
    assert_eq!(result.basis, "ambiguous-rust-crate-root");
    let modules: &[&[u8]] = &[b"src/lib.rs", b"src/api.rs", b"src/api/mod.rs", b"src/api/Thing.rs"];
    let result = resolve_import(b"src/lib.rs", fact, |path| modules.contains(&path).then_some(1));
    assert_eq!(result.kind, ImportResolutionKind::Ambiguous);
    assert_eq!(result.target, None, "a deeper file does not erase an ambiguous module prefix");
    assert_eq!(result.basis, "ambiguous-catalog-target");
    let parents: &[&[u8]] = &[b"src/lib.rs", b"src/net.rs", b"src/net/mod.rs", b"src/net/child.rs"];
    assert_eq!(resolution(b"src/net/child.rs", "super::Thing", parents).kind,
        ImportResolutionKind::Ambiguous);
}

#[test]
fn automatic_target_boundaries_never_inherit_an_outer_library_root() {
    for source in [b"src/bin/tool.rs".as_slice(), b"tests/integration.rs", b"examples/demo.rs",
        b"benches/benchmark.rs", b"src/bin/tool/helpers.rs"] {
        let paths: &[&[u8]] = &[b"lib.rs", b"src/lib.rs", source, b"src/api.rs", b"api.rs"];
        for name in ["crate::api", "super::api", "self::api"] {
            let result = resolution(source, name, paths);
            assert_eq!(result.kind, ImportResolutionKind::Unresolved, "{source:?} {name}");
            assert_eq!(result.target, None);
        }
    }
    let paths: &[&[u8]] = &[b"src/lib.rs", b"src/bin/tool/main.rs", b"src/bin/tool/helpers.rs", b"src/bin/tool/api.rs"];
    local(resolution(b"src/bin/tool/helpers.rs", "crate::api", paths), 3,
        "rust-use-module-file-candidate");
    local(resolution(b"src/bin/tool/main.rs", "crate::api", paths), 3,
        "rust-use-module-file-candidate");
}

#[test]
fn raw_ascii_names_are_lookup_components_but_unicode_and_unqualified_names_stay_evidence_only() {
    local(resolution(b"src/lib.rs", "crate::r#type", &[b"src/lib.rs", b"src/type.rs"]), 1,
        "rust-use-module-file-candidate");
    for name in ["serde::Deserialize", "std::io::Read", "api::Thing", "::external::Thing", "crate::caf\u{e9}"] {
        let statement = format!("use {name};");
        let imports = capture(statement.as_bytes());
        assert_eq!(imports.facts().len(), 1);
        let result = resolve_import::<usize>(b"src/lib.rs", &imports.facts()[0],
            |_| panic!("unmodeled name authorized catalog access: {name}"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        assert_eq!(imports.source_bytes(1).unwrap(), statement.as_bytes());
    }
}

#[test]
fn withheld_attribute_nested_and_macro_context_never_lends_path_authority() {
    for source in [
        "#[path = \"elsewhere.rs\"] mod custom; use crate::api;",
        "#[cfg_attr(feature = \"x\", path = \"elsewhere.rs\")] mod custom; use crate::api;",
        "mod nested { use super::api; }",
        "macro_rules! recipe { () => { use crate::api; } }",
    ] {
        let imports = capture(source.as_bytes());
        let uses = imports.facts().iter().filter(|fact| fact.kind() == ImportKind::RustUse).collect::<Vec<_>>();
        assert!(!uses.is_empty(), "{source}");
        for fact in uses {
            assert!(!fact.resolution_allowed(), "{source}");
            let result = resolve_import::<usize>(b"src/lib.rs", fact,
                |_| panic!("unqualified source context consulted a target"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        }
    }
}

#[test]
fn probe_depth_and_unprobeable_module_pairs_remain_unresolved() {
    let imports = capture(b"use crate::api;");
    let source = format!("{}consumer.rs", "deep/".repeat(65));
    let mut calls = 0usize;
    let result = resolve_import(source.as_bytes(), &imports.facts()[0], |path| {
        calls += 1;
        (path == b"lib.rs" || path == b"api.rs").then_some(1)
    });
    assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    assert_eq!(calls, 128, "ancestor probes have a fixed limit, not a path-depth-sized search");

    let source = [vec![b'a'; 16 * 1024 - 7], b"/lib.rs".to_vec()].concat();
    let imports = capture(b"use crate::x;");
    let mut calls = 0usize;
    let result = resolve_import(&source, &imports.facts()[0], |path| {
        calls += 1;
        assert_eq!(path, source.as_slice(), "the .rs candidate must not be queried if /mod.rs does not fit");
        Some(1)
    });
    assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    assert_eq!(calls, 1);
}

#[test]
fn utf16_statement_evidence_stays_original_while_candidate_paths_use_identifiers() {
    for big in [false, true] {
        let text = "// \u{1f600}\r\nuse crate::api::{Thing as Alias, self};";
        let mut source = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
        for unit in text.encode_utf16() {
            source.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() });
        }
        let imports = capture(&source);
        assert_eq!(imports.facts().len(), 2);
        for fact in imports.facts() {
            let result = resolve_import(b"src/lib.rs", fact, |path| match path {
                b"src/lib.rs" => Some(0), b"src/api.rs" => Some(1), _ => None,
            });
            assert_eq!(result.target, Some(1));
            assert_eq!(fact.original_range().start().get(), 16);
            assert_eq!(imports.source_bytes(fact.id()).unwrap(), &source[16..]);
        }
        assert_eq!(imports.facts()[0].destination(), "crate::api::Thing");
        assert_eq!(imports.facts()[1].destination(), "crate::api");
    }
}
