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
        "{path='../core',base='dev'}",
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
        "[package]\nname='app'\n[dependencies]\ncore.path.extra='../core'\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core',}\n",
        "[package]\nname='app'\n[dependencies]\ncore={path='../core'\n}\n",
        "[package]\nname='app'\n[dependencies]\ncore={path=\"../co\\re\"}\n",
        "[package]\nname='app'\nnotes='''\n[dependencies]\ncore={path='../fake'}\n'''\n",
        "[package]\nname='app'\n[lib]\npath='a.rs'\npath='b.rs'\n",
        "[package]\nname='app'\n[lib]\ncrate-type=['rlib']\ncrate_type=['cdylib']\n",
        "[package]\nversion='1.0'\n",
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


const WORKSPACE_SOURCE: &[u8] = b"crates/app/src/main.rs";
const WORKSPACE: &[u8] = b"[workspace]\nmembers=['crates/*']\n[workspace.dependencies]\nmy-core={path='crates/core'}\n";
const WORKSPACE_IMPORTER: &[u8] = b"[package]\nname='app'\n[dependencies]\nmy-core.workspace=true\n";
const WORKSPACE_PATHS: &[&[u8]] = &[
    b"Cargo.toml", b"crates/app/Cargo.toml", b"crates/core/Cargo.toml",
    b"crates/core/src/lib.rs", b"crates/core/src/model.rs",
];

fn resolve_workspace(source: &[u8], destination: &str, importing: &CapturedCargoManifest,
    workspace: Option<&CapturedCargoManifest>, target: &CapturedCargoManifest, paths: &[&[u8]])
    -> ImportResolution<usize> {
    resolve_rust_dependency_import_with_workspace(source, &fact(destination), Some(importing), workspace, Some(target),
        |path| paths.iter().position(|candidate| *candidate == path))
}

#[test]
fn inherited_dependency_spellings_open_workspace_relative_library_and_modules() {
    let budget = budget();
    let workspace = build(b"Cargo.toml", WORKSPACE, 2, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    assert!(workspace.is_workspace());
    for declaration in [
        "[dependencies]\nmy-core.workspace=true\n",
        "[dependencies]\nmy-core={workspace=true,optional=true,features=['one'],default-features=false}\n",
        "[dependencies.my-core]\nworkspace=true\nfeatures=['one']\n",
    ] {
        let text = format!("[package]\nname='app'\n{declaration}");
        let importer = build(b"crates/app/Cargo.toml", text.as_bytes(), 1, &budget).unwrap();
        assert!(importer.needs_workspace(&fact("my_core")));
        assert!(importer.dependency(WORKSPACE_SOURCE, &fact("my_core")).is_none());
        let nearest = find_cargo_workspace_manifest(&importer,
            |path| WORKSPACE_PATHS.iter().position(|candidate| *candidate == path)).unwrap();
        assert_eq!(nearest.path(), b"Cargo.toml");
        assert_eq!(nearest.target, 0);
        assert_eq!(importer.dependency_with_workspace(WORKSPACE_SOURCE, &fact("my_core"), Some(&workspace))
            .unwrap().path(), b"crates/core/Cargo.toml");
        for (destination, index, basis) in [
            ("my_core", 3, "rust-path-dependency-file-candidate"),
            ("my_core::model", 4, "rust-path-dependency-file-candidate"),
            ("my_core::model::Thing", 4, "rust-path-dependency-prefix-candidate"),
        ] {
            let result = resolve_workspace(WORKSPACE_SOURCE, destination, &importer, Some(&workspace), &target, WORKSPACE_PATHS);
            assert_eq!(result.target, Some(index), "{declaration}: {destination}");
            assert_eq!(result.basis, basis);
        }
        assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, None, &target, WORKSPACE_PATHS).basis, CONTEXT);
    }
}

#[test]
fn workspace_alias_and_quoted_subtable_preserve_both_exact_declarations() {
    let budget = budget();
    let workspace_text = b"[workspace]\nmembers=['crates/app']\n[workspace.dependencies.'engine-api']\npackage='engine-package'\npath='crates/core'\n";
    let member_text = b"[package]\nname='app'\n[dependencies]\n'engine-api'.workspace = true\n'engine-api'.features = ['one']\n";
    let workspace = build(b"Cargo.toml", workspace_text, 2, &budget).unwrap();
    let importer = build(b"crates/app/Cargo.toml", member_text, 1, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml",
        b"[package]\nname='engine-package'\n[lib]\nname='custom_library'\n", 3, &budget).unwrap();
    let location = importer.dependency_with_workspace(WORKSPACE_SOURCE, &fact("engine_api::Thing"), Some(&workspace)).unwrap();
    assert_eq!(location.declaration_request(), importer.request());
    let (start, end) = location.declaration_range().as_usize_bounds().unwrap();
    assert_eq!(&importer.source_bytes()[start..end],
        b"'engine-api'.workspace = true\n'engine-api'.features = ['one']");
    let (capture, range) = location.workspace_declaration().unwrap();
    assert_eq!(capture, workspace.request());
    let (start, end) = range.as_usize_bounds().unwrap();
    assert_eq!(&workspace.source_bytes()[start..end],
        b"[workspace.dependencies.'engine-api']\npackage='engine-package'\npath='crates/core'");
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "engine_api::Thing", &importer, Some(&workspace), &target, WORKSPACE_PATHS).target, Some(3));
}

#[test]
fn inheritance_uses_exact_manifest_key_before_rust_alias_normalization() {
    let budget = budget();
    let importer = build(b"crates/app/Cargo.toml", WORKSPACE_IMPORTER, 1, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    let workspace = build(b"Cargo.toml",
        b"[workspace]\nmembers=['crates/app']\n[workspace.dependencies]\nmy_core={path='crates/core',package='my-core'}\n",
        2, &budget).unwrap();
    assert!(importer.dependency_with_workspace(WORKSPACE_SOURCE, &fact("my_core"), Some(&workspace)).is_none());
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS).basis, CONTEXT);
    drop(workspace);
    let workspace = build(b"Cargo.toml",
        b"[workspace]\nmembers=['crates/app']\n[workspace.dependencies]\nmy-core={path='crates/core'}\nmy_core={path='elsewhere'}\n",
        2, &budget).unwrap();
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS).target, Some(3));
}

#[test]
fn root_package_inherits_from_its_own_capture_without_a_second_load() {
    let budget = budget();
    for workspace_declaration in [
        "[workspace]\nmembers=['core']\n[workspace.dependencies]\nmy-core={path='core'}\n",
        "[workspace.dependencies]\nmy-core.path='core'\n",
    ] {
        let text = format!("[package]\nname='app'\n[dependencies]\nmy-core.workspace=true\n{workspace_declaration}");
        let importer = build(b"Cargo.toml", text.as_bytes(), 1, &budget).unwrap();
        let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
        assert!(importer.is_workspace());
        assert!(!importer.needs_workspace(&fact("my_core")));
        assert!(find_cargo_workspace_manifest(&importer, |_| -> Option<usize> {
            panic!("a root package must not load its own metadata twice");
        }).is_none());
        let location = importer.dependency(b"src/main.rs", &fact("my_core")).unwrap();
        assert_eq!(location.path(), b"core/Cargo.toml");
        assert_eq!(location.workspace_declaration().unwrap().0, importer.request());
        assert_eq!(resolve(b"src/main.rs", "my_core::Thing", Some(&importer), Some(&target),
            &[b"Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs"]).target, Some(2));
    }
    let virtual_root = build(b"Cargo.toml", b"[workspace.dependencies]\nmy-core={path='core'}\n", 4, &budget).unwrap();
    assert!(virtual_root.is_workspace());
    assert!(virtual_root.package.is_none());
}

#[test]
fn workspace_member_scope_and_exclusions_never_expand_to_unknown_packages() {
    let budget = budget();
    let importer = build(b"crates/app/Cargo.toml", WORKSPACE_IMPORTER, 1, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    for (membership, allowed) in [
        ("members=['crates/app']", true),
        ("members=['./crates/*']", true),
        ("members=['crates/app']\nexclude=['crates/other']", true),
        ("members=['crates/*']\nexclude=['crates/app']", false),
        ("members=['crates/*']\nexclude=['crates/*']", false),
        ("members=['crates/app']\nexclude=['crates/a*']", false),
        ("members=['crates/**']", false),
        ("members=['crates/ap?']", false),
        ("members=['crates/[a-z]*']", false),
        ("members=['elsewhere/*']", false),
        ("members=['crates']", false),
        ("members=[]", false),
        ("", false),
    ] {
        let text = format!("[workspace]\n{membership}\n[workspace.dependencies]\nmy-core={{path='crates/core'}}\n");
        let workspace = build(b"Cargo.toml", text.as_bytes(), 2, &budget).unwrap();
        let result = resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS);
        assert_eq!(result.target.is_some(), allowed, "{membership}");
    }
    let workspace = build(b"Cargo.toml", WORKSPACE, 2, &budget).unwrap();
    let nested = build(b"crates/app/nested/Cargo.toml", WORKSPACE_IMPORTER, 4, &budget).unwrap();
    assert!(nested.dependency_with_workspace(b"crates/app/nested/src/lib.rs", &fact("my_core"), Some(&workspace)).is_none(),
        "one component wildcard must not match an unlisted nested package");
}

#[test]
fn unavailable_or_nonworkspace_nearest_boundary_blocks_outer_inheritance() {
    let budget = budget();
    let importer = build(b"crates/app/Cargo.toml", WORKSPACE_IMPORTER, 1, &budget).unwrap();
    let workspace = build(b"Cargo.toml", WORKSPACE, 2, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    let mut paths = WORKSPACE_PATHS.to_vec();
    paths.push(b"crates/Cargo.toml");
    let boundary = find_cargo_workspace_manifest(&importer,
        |path| paths.iter().position(|candidate| *candidate == path)).unwrap();
    assert_eq!(boundary.path(), b"crates/Cargo.toml");
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, &paths).basis, CONTEXT);
    let nonworkspace = build(b"crates/Cargo.toml", b"[package]\nname='outer'\n", 4, &budget).unwrap();
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&nonworkspace), &target, &paths).basis, CONTEXT);
    drop(nonworkspace);
    let nested_workspace = build(b"crates/Cargo.toml",
        b"[workspace]\nmembers=['app']\n[workspace.dependencies]\nmy-core={path='core'}\n", 4, &budget).unwrap();
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&nested_workspace), &target, &paths).target, Some(3));
    paths.push(b"crates/app/src/Cargo.toml");
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&nested_workspace), &target, &paths).basis, CONTEXT);
}

#[test]
fn explicit_package_workspace_must_match_the_nearest_captured_root() {
    let budget = budget();
    let workspace = build(b"Cargo.toml", WORKSPACE, 2, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    for (explicit, allowed) in [("../..", true), ("..", false), ("../../../escape", false), ("/absolute", false)] {
        let text = format!("[package]\nname='app'\nworkspace='{explicit}'\n[dependencies]\nmy-core.workspace=true\n");
        let importer = build(b"crates/app/Cargo.toml", text.as_bytes(), 1, &budget).unwrap();
        let result = resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS);
        assert_eq!(result.target.is_some(), allowed, "{explicit}");
    }
    assert!(build(b"Cargo.toml", b"[package]\nname='app'\nworkspace='.'\n[workspace]\n", 1, &budget).is_err());
}

#[test]
fn inherited_overrides_external_sources_and_unstable_path_bases_remain_unavailable() {
    let budget = budget();
    let workspace = build(b"Cargo.toml", WORKSPACE, 2, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    for field in [
        "path='../core'", "package='my-core'", "version='1'", "git='https://example.invalid/repo'",
        "registry='private'", "base='dev'",
    ] {
        let text = format!("[package]\nname='app'\n[dependencies]\nmy-core={{workspace=true,{field}}}\n");
        let importer = build(b"crates/app/Cargo.toml", text.as_bytes(), 1, &budget).unwrap();
        assert!(!importer.needs_workspace(&fact("my_core")), "{field}");
        assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS).basis, CONTEXT);
    }
    drop(workspace);
    let importer = build(b"crates/app/Cargo.toml", WORKSPACE_IMPORTER, 1, &budget).unwrap();
    for definition in [
        "'1.0'", "{git='https://example.invalid/repo'}", "{path='crates/core',registry='private'}",
        "{path='crates/core',base='dev'}", "{path='crates/core',optional=true}",
        "{path='crates/core',optional=false}", "{path='crates/core',workspace=true}",
        "{path='../outside'}", "{path='/absolute'}",
    ] {
        let text = format!("[workspace]\nmembers=['crates/app']\n[workspace.dependencies]\nmy-core={definition}\n");
        let workspace = build(b"Cargo.toml", text.as_bytes(), 2, &budget).unwrap();
        assert!(importer.dependency_with_workspace(WORKSPACE_SOURCE, &fact("my_core"), Some(&workspace)).is_none(), "{definition}");
        assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS).basis, CONTEXT);
    }
}

#[test]
fn dotted_direct_dependencies_preserve_existing_navigation_without_workspace_io() {
    let budget = budget();
    let importer = build(b"app/Cargo.toml",
        b"[package]\nname='app'\n[dependencies]\nmy-core.path='../core'\nmy-core.features=['one']\n",
        1, &budget).unwrap();
    let target = build(b"core/Cargo.toml", TARGET, 2, &budget).unwrap();
    assert!(!importer.needs_workspace(&fact("my_core")));
    assert!(find_cargo_workspace_manifest(&importer, |_| -> Option<usize> {
        panic!("direct dependencies must not load workspace metadata");
    }).is_none());
    let location = importer.dependency(SOURCE, &fact("my_core")).unwrap();
    assert_eq!(location.declaration_request(), importer.request());
    assert!(location.workspace_declaration().is_none());
    assert_eq!(resolve(SOURCE, "my_core", Some(&importer), Some(&target),
        &[b"app/Cargo.toml", b"core/Cargo.toml", b"core/src/lib.rs"]).target, Some(2));
}

#[test]
fn duplicate_or_malformed_workspace_fields_never_authorize_partial_metadata() {
    let budget = budget();
    for metadata in [
        "[workspace]\nmembers=['app']\nmembers=['other']\n",
        "[workspace]\nexclude=[]\nexclude=[]\n",
        "[workspace]\nmembers=[1]\n",
        "[workspace]\nmembers='app'\n",
        "[workspace]\n[workspace]\n",
        "[workspace.dependencies]\nmy-core.path='core'\nmy-core.path='other'\n",
        "[workspace.dependencies]\nmy-core={path='core'}\nmy-core.features=[]\n",
        "[workspace.dependencies]\nmy-core.path='core'\n[workspace.dependencies.my-core]\nfeatures=[]\n",
        "[package]\nname='app'\n[dependencies]\nmy-core.workspace=true\nmy-core.workspace=true\n",
        "[package]\nname='app'\n[dependencies]\nmy-core={workspace=true,workspace=true}\n",
        "[package]\nname='app'\n[dependencies]\nmy-core.workspace='true'\n",
        "[package]\nname='app'\n[dependencies]\nmy-core.workspace=true\nmy-core.default_features=false\nmy-core.default-features=true\n",
    ] {
        assert!(build(b"Cargo.toml", metadata.as_bytes(), 1, &budget).is_err(), "{metadata}");
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
    let members = (0..=MAX_WORKSPACE_PATTERNS).map(|index| format!("'member{index}'")).collect::<Vec<_>>().join(",");
    let text = format!("[workspace]\nmembers=[{members}]\n");
    assert!(matches!(build(b"Cargo.toml", text.as_bytes(), 1, &budget), Err(ImportError::SourceLimit)));
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn workspace_capture_owner_and_retained_bytes_are_checked_without_filesystem_access() {
    let budget = budget();
    let importer = build(b"crates/app/Cargo.toml", WORKSPACE_IMPORTER, 1, &budget).unwrap();
    let target = build(b"crates/core/Cargo.toml", TARGET, 3, &budget).unwrap();
    let mut bytes = WORKSPACE.to_vec();
    let workspace = build(b"Cargo.toml", &bytes, 2, &budget).unwrap();
    bytes.fill(b'x');
    assert_eq!(workspace.source_bytes(), WORKSPACE);
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&workspace), &target, WORKSPACE_PATHS).target, Some(3));
    let other = ArenaOwnerId::new(905).unwrap();
    let other_budget = ResourceBudget::new(other, ByteLength::new(16 * 1024 * 1024)).unwrap();
    let other_request = CaptureRequest::new(FileId::new(other, 2).unwrap(), SourceRevision::new(other, 1).unwrap()).unwrap();
    let foreign = CapturedCargoManifest::build(b"Cargo.toml", WORKSPACE, other_request, &other_budget,
        ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    assert_eq!(resolve_workspace(WORKSPACE_SOURCE, "my_core", &importer, Some(&foreign), &target, WORKSPACE_PATHS).basis, CONTEXT);
    drop(foreign);
    assert_eq!(other_budget.accounting().reserved().get(), 0);
    drop(workspace); drop(importer); drop(target);
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn raw_parent_paths_and_workspace_ambiguity_keep_catalog_identity() {
    let budget = budget();
    let root = b"native/\xff/Cargo.toml";
    let importer_path = b"native/\xff/crates/app/Cargo.toml";
    let target_path = b"native/\xff/crates/core/Cargo.toml";
    let source = b"native/\xff/crates/app/src/main.rs";
    let workspace = build(root, WORKSPACE, 2, &budget).unwrap();
    let importer = build(importer_path, WORKSPACE_IMPORTER, 1, &budget).unwrap();
    let target = build(target_path, TARGET, 3, &budget).unwrap();
    let paths: &[&[u8]] = &[root, importer_path, target_path, b"native/\xff/crates/core/src/lib.rs"];
    assert_eq!(resolve_workspace(source, "my_core", &importer, Some(&workspace), &target, paths).target, Some(3));
    drop(importer);
    let competing = [WORKSPACE_IMPORTER, b"my_core.workspace=true\n"].concat();
    let importer = build(importer_path, &competing, 1, &budget).unwrap();
    assert_eq!(resolve_workspace(source, "my_core", &importer, Some(&workspace), &target, paths).kind, ImportResolutionKind::Ambiguous);
    assert!(importer.dependency_with_workspace(source, &fact("my_core"), Some(&workspace)).is_none());
}
