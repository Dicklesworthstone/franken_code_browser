#![forbid(unsafe_code)]

use super::*;
use crate::imports::{CapturedImports, resolve_import};
use fcb_core::{ArenaOwnerId, FileId, QueryGeneration, SourceRevision};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(903).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
fn request(id: u64) -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), id).unwrap(), SourceRevision::new(owner(), 1).unwrap()).unwrap()
}
fn build(path: &[u8], text: &[u8], id: u64, budget: &ResourceBudget) -> Result<CapturedCargoManifest, ImportError> {
    CapturedCargoManifest::build(path, text, request(id), budget, ResourceAllocationId::new(id).unwrap(), || false)
}
fn fact(destination: &str) -> ImportFact {
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(1)).unwrap();
    ImportFact { id: 1, kind: ImportKind::RustUse, destination: destination.to_owned(), original_range: range,
        specifier_range: range, resolution_allowed: true, rust_module_context: Some(String::new()) }
}
fn resolve(source: &[u8], destination: &str, importing: Option<&CapturedCargoManifest>,
    target: Option<&CapturedCargoManifest>, paths: &[&[u8]]) -> ImportResolution<usize> {
    resolve_rust_dependency_import(source, &fact(destination), importing, target,
        |path| paths.iter().position(|candidate| *candidate == path))
}
const IMPORTER: &[u8] = b"[package]\nname='app'\nversion.workspace = true\n[dependencies]\nmy-core={path='../core'}\n";
const TARGET: &[u8] = b"[package]\nname='my-core'\n";
const SOURCE: &[u8] = b"app/src/main.rs";

#[test]
fn direct_path_dependency_opens_library_module_and_item_prefix() {
    let budget = budget();
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
    let paths: &[&[u8]] = &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs", b"core/src/model.rs"];
    for (destination, index, basis) in [
        ("my_core", 2, "rust-path-dependency-file-candidate"),
        ("my_core::model", 3, "rust-path-dependency-file-candidate"),
        ("my_core::model::*", 3, "rust-path-dependency-file-candidate"),
        ("my_core::model::Thing", 3, "rust-path-dependency-prefix-candidate"),
        ("::my_core::Thing", 2, "rust-path-dependency-prefix-candidate"),
    ] {
        let result = resolve(SOURCE, destination, Some(&importer), Some(&target), paths);
        assert_eq!(result.target, Some(index), "{destination}");
        assert_eq!(result.basis, basis);
    }
}

#[test]
fn quoted_alias_subtable_and_explicit_library_keep_manifest_provenance() {
    let budget = budget();
    let text = b"# [dependencies.fake]\r\n[package]\r\nname=\"app\"\r\n[dependencies.'engine-api']\r\npackage='engine-package'\r\npath='../core'\r\nfeatures=['one', 'two'] # ignored selection\r\n";
    let importer = build(b"app/Cargo.toml", text, 1, &budget).unwrap();
    let target = build(b"core/Cargo.toml",
        b"[package]\nname='engine-package'\n[lib]\nname='custom_library'\npath='code/root.rs'\ncrate-type=['rlib','cdylib']\n",
        2, &budget).unwrap();
    let paths: &[&[u8]] = &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/code/root.rs", b"core/code/model/mod.rs"];
    assert_eq!(resolve(SOURCE, "engine_api::model::Thing", Some(&importer), Some(&target), paths).target, Some(3));
    let dependency = importer.dependency(SOURCE, &fact("engine_api::Thing")).unwrap();
    assert_eq!(dependency.path(), b"core/Cargo.toml");
    let (start, end) = dependency.declaration_range().as_usize_bounds().unwrap();
    assert_eq!(&importer.source_bytes()[start..end],
        b"[dependencies.'engine-api']\r\npackage='engine-package'\r\npath='../core'\r\nfeatures=['one', 'two']");
    assert_eq!(importer.request(), request(1));
    assert_eq!(importer.source_len(), text.len());
}

#[test]
fn root_and_native_parent_paths_remain_exact() {
    let budget = budget();
    let importer = build(b"native/\xff/app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let target = build(b"native/\xff/core/Cargo.toml", TARGET, 2, &budget).unwrap();
    let paths: &[&[u8]] = &[b"native/\xff/app/Cargo.toml", b"native/\xff/core/Cargo.toml", b"native/\xff/core/src/lib.rs"];
    assert_eq!(resolve(b"native/\xff/app/src/main.rs", "my_core::Thing", Some(&importer), Some(&target), paths).target, Some(2));
    drop(importer); drop(target);
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let root = build(b"Cargo.toml", TARGET, 2, &budget).unwrap();
    let root_importer = build(b"app/Cargo.toml", b"[package]\nname='app'\n[dependencies]\nmy-core={path='..'}\n", 3, &budget).unwrap();
    let paths: &[&[u8]] = &[b"app/Cargo.toml", b"Cargo.toml", b"src/lib.rs"];
    assert_eq!(resolve(SOURCE, "my_core", Some(&root_importer), Some(&root), paths).target, Some(2));
    assert_eq!(resolve(SOURCE, "my_core", Some(&importer), Some(&root), paths).kind, ImportResolutionKind::Unresolved);
}

#[test]
fn captured_manifest_bytes_survive_mutation_and_release_managed_memory() {
    let budget = budget();
    let mut bytes = IMPORTER.to_vec();
    let importer = build(b"app/Cargo.toml", &bytes, 1, &budget).unwrap();
    bytes.fill(b'x');
    assert_eq!(importer.source_bytes(), IMPORTER);
    assert_eq!(importer.dependency(SOURCE, &fact("my_core")).unwrap().path(), b"core/Cargo.toml");
    assert!(budget.accounting().reserved().get() > 0);
    drop(importer);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn missing_metadata_and_non_library_targets_never_guess_a_source_file() {
    let budget = budget();
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let paths: &[&[u8]] = &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs", b"core/src/main.rs"];
    assert_eq!(resolve(SOURCE, "my_core", None, None, paths).basis, CONTEXT);
    assert_eq!(resolve(SOURCE, "my_core", Some(&importer), None, paths).basis, CONTEXT);
    for metadata in [
        b"[package]\nname='different'\n".as_slice(),
        b"[package]\nname='my-core'\nautolib=false\n",
        b"[package]\nname='my-core'\n[lib]\nname='different'\n",
        b"[package]\nname='my-core'\n[lib]\ncrate-type=['cdylib','staticlib']\n",
        b"[package]\nname='my-core'\n[lib]\ncrate-type=['bin']\n",
        b"[package]\nname='my-core'\n[lib]\npath='../outside.rs'\n",
    ] {
        let target = build(b"core/Cargo.toml", metadata, 2, &budget).unwrap();
        assert_eq!(resolve(SOURCE, "my_core::Thing", Some(&importer), Some(&target), paths).kind, ImportResolutionKind::Unresolved);
    }
    let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
    assert_eq!(resolve(SOURCE, "my_core", Some(&importer), Some(&target),
        &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/main.rs"]).kind, ImportResolutionKind::Missing);
}

#[test]
fn explicit_library_can_restore_autolib_false_without_guessing_its_path() {
    let budget = budget();
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let target = build(b"core/Cargo.toml",
        b"[package]\nname='my-core'\nautolib=false\n[lib]\npath='code.rs'\n", 2, &budget).unwrap();
    assert_eq!(resolve(SOURCE, "my_core::Thing", Some(&importer), Some(&target),
        &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/code.rs"]).target, Some(2));
}

#[test]
fn conflicting_aliases_and_module_files_remain_ambiguous() {
    let budget = budget();
    let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
    let paths: &[&[u8]] = &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs", b"core/src/model.rs", b"core/src/model/mod.rs"];
    for suffix in ["my_core={path='../elsewhere'}\n", "[target.'cfg(unix)'.dependencies]\nmy-core={path='../elsewhere'}\n"] {
        let text = [IMPORTER, suffix.as_bytes()].concat();
        let importer = build(b"app/Cargo.toml", &text, 1, &budget).unwrap();
        assert!(importer.dependency(SOURCE, &fact("my_core")).is_none());
        assert_eq!(resolve(SOURCE, "my_core", Some(&importer), Some(&target), paths).kind, ImportResolutionKind::Ambiguous);
    }
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let result = resolve_rust_dependency_import(SOURCE, &fact("my_core::model"), Some(&importer), Some(&target),
        |path| paths.contains(&path).then_some(7));
    assert_eq!(result.kind, ImportResolutionKind::Ambiguous, "two catalog paths remain distinct even when identities alias");
    assert_eq!(result.target, None);
}

#[test]
fn unavailable_nested_metadata_blocks_outer_package_fallback() {
    let budget = budget();
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
    let paths: &[&[u8]] = &[b"app/Cargo.toml", b"app/nested/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs"];
    let nearest = find_cargo_manifest(b"app/nested/src/lib.rs",
        |path| paths.iter().position(|candidate| *candidate == path)).unwrap();
    assert_eq!(nearest.path(), b"app/nested/Cargo.toml");
    assert_eq!(nearest.target, 1);
    assert_eq!(resolve(b"app/nested/src/lib.rs", "my_core", Some(&importer), Some(&target), paths).basis, CONTEXT);
    assert_eq!(resolve(SOURCE, "my_core", Some(&importer), Some(&target),
        &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/Cargo.toml", b"core/src/lib.rs"]).basis, CONTEXT);
    assert_eq!(resolve(SOURCE, "my_core::model", Some(&importer), Some(&target),
        &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs", b"core/src/model/Cargo.toml", b"core/src/model/mod.rs"]).basis, CONTEXT);
}

#[test]
fn unsupported_dependency_sources_and_escaping_paths_never_authorize_metadata_loads() {
    let budget = budget();
    for definition in [
        "'1.0'", "{workspace=true}", "{path='../core',git='https://example.invalid/repo'}",
        "{path='/absolute'}", "{path='../../escape'}", "{path='../core//child'}",
    ] {
        let text = format!("[package]\nname='app'\n[dependencies]\nmy-core={definition}\n");
        let importer = build(b"app/Cargo.toml", text.as_bytes(), 1, &budget).unwrap();
        assert!(importer.dependency(SOURCE, &fact("my_core")).is_none(), "{definition}");
    }
    for table in ["dev-dependencies", "build-dependencies", "target.'cfg(unix)'.dependencies", "workspace.dependencies"] {
        let text = format!("[package]\nname='app'\n[{table}]\nmy-core={{path='../core'}}\n");
        let importer = build(b"app/Cargo.toml", text.as_bytes(), 1, &budget).unwrap();
        assert!(importer.dependency(SOURCE, &fact("my_core")).is_none(), "{table}");
    }
}

#[test]
fn malformed_or_unqualified_metadata_is_not_partially_admitted() {
    let budget = budget();
    for metadata in [
        "[package]\nname='app'\nname='other'\n",
        "[package]\nname='app'\n[package]\nname='other'\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core',path='../elsewhere'}\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core'}\n[dependencies.core]\npath='../elsewhere'\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core'}\n[dependencies]\n",
        "[package]\nname='app'\n[dependencies]\ncore.path='../core'\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core',}\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core'\n}\n",
        "[package]\nname='app'\n[dependencies]\ncore={path=\"../co\\re\"}\n",
        "[package]\nname='app'\nnotes='''\n[dependencies]\ncore={path='../fake'}\n'''\n",
        "[package]\nname='app'\n[lib]\npath='a.rs'\npath='b.rs'\n",
        "[package]\nname='app'\n[lib]\ncrate-type=['rlib']\ncrate_type=['cdylib']\n",
        "[workspace]\nmembers=['app']\n",
    ] {
        assert!(build(b"app/Cargo.toml", metadata.as_bytes(), 1, &budget).is_err(), "{metadata}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}

#[test]
fn metadata_limits_cancellation_and_owner_mismatch_release_admission() {
    let budget = budget();
    assert!(matches!(build(b"app/Cargo.toml", &vec![b' '; MAX_CARGO_MANIFEST_SOURCE_BYTES + 1], 1, &budget),
        Err(ImportError::SourceLimit)));
    assert!(matches!(build(b"app/Cargo.toml", b"\xff", 1, &budget), Err(ImportError::UnsupportedEncoding)));
    let denied = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(build(b"app/Cargo.toml", IMPORTER, 1, &denied), Err(ImportError::ResourceDenied)));
    let mut polls = 0;
    let result = CapturedCargoManifest::build(b"app/Cargo.toml", IMPORTER, request(1), &budget,
        ResourceAllocationId::new(1).unwrap(), || { polls += 1; polls == 4 });
    assert!(matches!(result, Err(ImportError::Canceled)));
    assert_eq!(budget.accounting().reserved().get(), 0);
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let other = ArenaOwnerId::new(904).unwrap();
    let other_budget = ResourceBudget::new(other, ByteLength::new(16 * 1024 * 1024)).unwrap();
    let other_request = CaptureRequest::new(FileId::new(other, 1).unwrap(), SourceRevision::new(other, 1).unwrap()).unwrap();
    let target = CapturedCargoManifest::build(b"core/Cargo.toml", TARGET, other_request, &other_budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    assert_eq!(resolve(SOURCE, "my_core", Some(&importer), Some(&target),
        &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs"]).basis, CONTEXT);
}

#[test]
fn qualified_local_and_standard_paths_keep_existing_resolution_routes() {
    for destination in ["crate::module", "self::module", "super::module", "std::io", "core::fmt", "alloc::vec", "Self::Item"] {
        assert!(!is_rust_dependency_import(&fact(destination)), "{destination}");
    }
    let paths: &[&[u8]] = &[b"app/src/lib.rs", b"app/src/model.rs"];
    let local = fact("crate::model");
    assert_eq!(resolve_import(b"app/src/lib.rs", &local, |path| paths.iter().position(|p| *p == path)).target, Some(1));
    for destination in ["my_core::", "my_core::::Thing", "my_core::../other", "my_core::crate", "my_core::*::Thing"] {
        assert!(!is_rust_dependency_import(&fact(destination)), "{destination}");
    }
    let mut unqualified = fact("my_core::Thing");
    unqualified.rust_module_context = None;
    assert!(!is_rust_dependency_import(&unqualified));
}

#[test]
fn utf16_importer_retains_original_evidence_while_navigating_captured_metadata() {
    let budget = budget();
    let importer = build(b"app/Cargo.toml", IMPORTER, 1, &budget).unwrap();
    let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
    let statement = "use my_core::model::Thing;";
    for little in [false, true] {
        let encode = |text: &str| text.encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect::<Vec<_>>();
        let mut source = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        source.extend(encode("// 😀 captured source\n"));
        source.extend(encode(statement));
        let imports = CapturedImports::build(&source, request(3), QueryGeneration::new(owner(), 1).unwrap(),
            ImportLanguage::Rust, 4, &budget, ResourceAllocationId::new(3).unwrap(), || false).unwrap();
        assert_eq!(imports.facts().len(), 1);
        let fact = &imports.facts()[0];
        assert_eq!(fact.destination(), "my_core::model::Thing");
        assert_eq!(imports.source_bytes(fact.id()).unwrap(), encode(statement));
        let paths: &[&[u8]] = &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs", b"core/src/model.rs"];
        let resolution = resolve_rust_dependency_import(SOURCE, fact, Some(&importer), Some(&target),
            |path| paths.iter().position(|candidate| *candidate == path));
        assert_eq!(resolution.target, Some(3));
        assert_eq!(resolution.basis, "rust-path-dependency-prefix-candidate");
    }
}
