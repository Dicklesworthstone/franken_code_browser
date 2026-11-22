#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Public Desk discovery composition: directory admission precedes metadata caps.
//! Code-first fixtures for fcb-llov.2/fcb-qtb.1; independent execution is pending.
mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, os::unix::{ffi::{OsStrExt, OsStringExt}, fs::symlink},
    path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::host::{HostResponse,
    atlas_session::{AtlasSessionOptions, directory_path, file_path},
    atlas_search::{AtlasSearchOptions, AtlasSearchScope, AtlasIndexOptions},
    atlas_paths::{AtlasPathOptions, PathCase, PathMatchMode},
    catalog_session::CatalogDiscoveryPolicy,
    desk::{DeskSession, DeskLimits, repository::DeskRepository}};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-desk-discovery-{}-{}-{}",
        std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn file(root: &Path, path: impl AsRef<Path>, source: &[u8]) {
    let path = root.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}
fn owner(id: u64) -> ArenaOwnerId { ArenaOwnerId::new(id).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn scope(record: &Json, root: &Path, directory: Option<&[u8]>) {
    assert_eq!(record.get("path_basis").text(), "project-root");
    assert_eq!(record.get("discovery_file"), &Json::Null);
    match directory {
        Some(path) => {
            assert_eq!(record.get("discovery_scope").text(), "directory");
            assert_eq!(record.get("discovery_directory").get("hex").text(), hex(path));
        }
        None => {
            assert_eq!(record.get("discovery_scope").text(), "workspace");
            assert_eq!(record.get("discovery_directory"), &Json::Null);
        }
    }
    // Only atlas info advertises root_path; query/path receipts retain raw
    // project-relative member identities and the same discovery-scope receipt.
    if let Json::Object(fields) = record {
        if let Some(path) = fields.get("root_path") {
            assert_eq!(path.get("hex").text(), hex(fs::canonicalize(root).unwrap().as_os_str().as_bytes()));
        }
    }
}

#[test]
fn directory_discovery_reaches_the_folder_missing_from_a_capped_workspace_and_keeps_old_bytes() {
    let root = fixture(); let folders = ["first", "second"];
    let original = b"original needle\n";
    for folder in folders { file(&root, format!("{folder}/source.rs"), original); }
    let options = AtlasSessionOptions { max_files: 1, ..Default::default() };
    let mut whole = DeskRepository::open(owner(901), &root, options, || false).unwrap();
    let info = json(&whole.info(|| false).unwrap()); scope(&info, &root, None);
    assert!(!info.get("discovery_complete").flag());
    let initial = json(&whole.search(1, "needle", Default::default(), || false).unwrap());
    assert_eq!(initial.get("retained_hits").number(), 1);
    let initial_path = initial.get("hits").array()[0].get("path").get("hex").text();
    let selected = folders.into_iter()
        .find(|folder| hex(format!("{folder}/source.rs").as_bytes()) != initial_path).unwrap();
    let mut focused = DeskRepository::open_directory_with_policy(owner(902), &root,
        directory_path(selected.as_bytes()).unwrap(), options,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let info = json(&focused.info(|| false).unwrap()); scope(&info, &root, Some(selected.as_bytes()));
    assert!(info.get("discovery_complete").flag());
    assert_eq!(info.get("catalogued_files").number(), 1);
    let begun = json(&focused.begin_query_scoped(1, "needle", AtlasSearchScope::all(),
        AtlasSearchOptions::default(), || false).unwrap());
    scope(&begun, &root, Some(selected.as_bytes()));
    assert_eq!(begun.get("source_bytes_read").number(), 0);
    let found = json(&focused.step_query(1, 0, || false).unwrap());
    scope(&found, &root, Some(selected.as_bytes()));
    assert!(found.get("search_complete").flag());
    assert_eq!(found.get("retained_hits").number(), 1);
    assert_eq!(found.get("hits").array()[0].get("path").get("hex").text(),
        hex(format!("{selected}/source.rs").as_bytes()));
    let old = json(&whole.page(1, 0, 10, || false).unwrap());
    assert!(!old.get("search_complete").flag());
    fs::write(root.join(selected).join("source.rs"), b"changed current bytes\n").unwrap();
    let mut desk = DeskSession::new(owner(903), DeskLimits::default()).unwrap();
    let opened = focused.open_hit(&mut desk, 0, 1, 1, 1, || false).unwrap();
    let pane = opened.change.active.unwrap();
    assert_eq!(desk.model().source(pane, 1).unwrap().bytes(), original);
    drop(focused); drop(whole);
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(desk.model().source(pane, 1).unwrap().bytes(), original);
}

#[test]
fn explicit_workspace_policy_preserves_legacy_defaults_and_directory_rules_can_be_bypassed() {
    let root = fixture();
    file(&root, ".gitignore", b"ignored/\n");
    file(&root, "tracked.rs", b"visible needle");
    file(&root, "ignored/selected/source.rs", b"private needle");
    file(&root, "ignored/selected/.gitignore", b"!source.rs\n");
    let mut legacy = DeskRepository::open(owner(911), &root, Default::default(), || false).unwrap();
    let mut explicit = DeskRepository::open_with_policy(owner(912), &root, Default::default(),
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let mut all = DeskRepository::open_with_policy(owner(913), &root, Default::default(),
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let legacy_info = json(&legacy.info(|| false).unwrap());
    let explicit_info = json(&explicit.info(|| false).unwrap());
    scope(&legacy_info, &root, None); scope(&explicit_info, &root, None);
    assert_eq!(legacy_info.get("discovery_policy"), explicit_info.get("discovery_policy"));
    assert_eq!(legacy_info.get("catalogued_files"), explicit_info.get("catalogued_files"));
    for repo in [&mut legacy, &mut explicit] {
        let page = json(&repo.search(1, "needle", Default::default(), || false).unwrap());
        assert!(page.get("search_complete").flag());
        assert_eq!(page.get("retained_hits").number(), 1);
    }
    assert_eq!(json(&all.search(1, "needle", Default::default(), || false).unwrap())
        .get("retained_hits").number(), 2);
    for rule in [b"ignored/\n".as_slice(), b"[broken\n"] {
        fs::write(root.join(".gitignore"), rule).unwrap();
        for (id, policy, hits, complete) in [
            (914, CatalogDiscoveryPolicy::RepositoryRules, 0, false),
            (915, CatalogDiscoveryPolicy::IncludeExcluded, 1, true)] {
            let mut repo = DeskRepository::open_directory_with_policy(owner(id), &root,
                directory_path(b"ignored/selected").unwrap(), Default::default(), policy, || false).unwrap();
            let page = json(&repo.search(1, "private needle", Default::default(), || false).unwrap());
            scope(&page, &root, Some(b"ignored/selected"));
            assert_eq!(page.get("retained_hits").number(), hits);
            assert_eq!(page.get("discovery_complete").flag(), complete);
            assert_eq!(page.get("search_complete").flag(), complete);
            if policy == CatalogDiscoveryPolicy::IncludeExcluded {
                assert!(!page.get("rule_files_enabled").flag());
                assert_eq!(page.get("rule_policy"), &Json::Null);
            }
        }
    }
}

#[test]
fn raw_directory_paths_remain_project_relative_in_metadata_and_exact_reader_captures() {
    let root = fixture(); let outside = fixture();
    let directory = b"C:\\raw\xff"; let native = OsString::from_vec(directory.to_vec());
    let original = b"raw original needle\n";
    file(&root, Path::new(&native).join("source.rs"), original);
    file(&outside, "escape.rs", b"outside sentinel");
    symlink(&outside, root.join(&native).join("escape")).unwrap();
    let mut repo = DeskRepository::open_directory_with_policy(owner(921), &root,
        directory_path(directory).unwrap(), Default::default(),
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    scope(&json(&repo.info(|| false).unwrap()), &root, Some(directory));
    let paths = json(&repo.find_paths(1, b"source.rs", AtlasPathOptions {
        max_results: 10, case: PathCase::Sensitive, mode: PathMatchMode::Exact }, || false).unwrap());
    scope(&paths, &root, Some(directory));
    assert!(!paths.get("source_payload_read").flag());
    let mut expected = directory.to_vec(); expected.extend_from_slice(b"/source.rs");
    assert_eq!(paths.get("hits").array()[0].get("path").get("hex").text(), hex(&expected));
    assert_eq!(json(&repo.search(1, "outside sentinel", Default::default(), || false).unwrap())
        .get("retained_hits").number(), 0);
    let found = json(&repo.search(2, "needle", Default::default(), || false).unwrap());
    assert_eq!(found.get("hits").array()[0].get("path").get("hex").text(), hex(&expected));
    fs::write(root.join(&native).join("source.rs"), b"rewritten").unwrap();
    let mut desk = DeskSession::new(owner(922), DeskLimits::default()).unwrap();
    let opened = repo.open_hit(&mut desk, 0, 1, 2, 1, || false).unwrap();
    drop(repo);
    assert_eq!(desk.model().source(opened.change.active.unwrap(), 1).unwrap().bytes(), original);
    assert_eq!(desk.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"needle");
}

#[test]
fn rejected_and_canceled_candidates_leave_the_existing_repository_and_reader_usable() {
    let root = fixture(); file(&root, "selected/source.rs", b"original needle\n");
    symlink(root.join("selected"), root.join("alias")).unwrap();
    let mut old = DeskRepository::open(owner(931), &root, Default::default(), || false).unwrap();
    old.search(1, "needle", Default::default(), || false).unwrap();
    let mut desk = DeskSession::new(owner(932), DeskLimits::default()).unwrap();
    let opened = old.open_hit(&mut desk, 0, 1, 1, 1, || false).unwrap();
    for path in [b"missing".as_slice(), b"alias", b"selected/source.rs"] {
        assert!(DeskRepository::open_directory_with_policy(owner(933), &root,
            directory_path(path).unwrap(), Default::default(),
            CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err());
    }
    for max_files in [0, 20_001] {
        assert!(DeskRepository::open_with_policy(owner(934), &root,
            AtlasSessionOptions { max_files, ..Default::default() },
            CatalogDiscoveryPolicy::RepositoryRules, || false).is_err());
    }
    for path in [b"".as_slice(), b"/selected", b"selected/..", b"selected//child", b"selected/\0child"] {
        assert!(directory_path(path).is_err());
    }
    let mut polls = 0;
    let probe = DeskRepository::open_directory_with_policy(owner(935), &root,
        directory_path(b"selected").unwrap(), Default::default(),
        CatalogDiscoveryPolicy::IncludeExcluded, || { polls += 1; false }).unwrap();
    drop(probe); assert!(polls > 2);
    for stop_at in [1, polls] {
        let mut calls = 0;
        let error = DeskRepository::open_directory_with_policy(owner(936), &root,
            directory_path(b"selected").unwrap(), Default::default(),
            CatalogDiscoveryPolicy::IncludeExcluded, || { calls += 1; calls >= stop_at }).err().unwrap();
        assert!(error.is_canceled());
        assert_eq!(calls, stop_at);
    }
    assert_eq!(old.accepted_generation(), Some(1));
    assert_eq!(json(&old.page(1, 0, 10, || false).unwrap()).get("retained_hits").number(), 1);
    assert_eq!(desk.model().revision(), 1);
    assert_eq!(desk.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"needle");
    assert_eq!(desk.model().source(opened.change.active.unwrap(), 1).unwrap().bytes(), b"original needle\n");
}


fn file_scope(record: &Json, root: &Path, file: &[u8]) {
    assert_eq!(record.get("discovery_scope").text(), "file");
    assert_eq!(record.get("discovery_directory"), &Json::Null);
    assert_eq!(record.get("discovery_file").get("encoding").text(), "unix-bytes");
    assert_eq!(record.get("discovery_file").get("hex").text(), hex(file));
    assert_eq!(record.get("path_basis").text(), "project-root");
    if let Json::Object(fields) = record {
        if let Some(path) = fields.get("root_path") {
            assert_eq!(path.get("hex").text(), hex(fs::canonicalize(root).unwrap().as_os_str().as_bytes()));
        }
    }
}

#[test]
fn exact_file_discovery_reaches_beyond_both_root_and_parent_caps_and_retains_indexed_bytes() {
    let root = fixture(); let names = ["first.rs", "second.rs", "third.rs"];
    let original = b"original needle\n";
    for name in names { file(&root, format!("selected/{name}"), original); }
    let options = AtlasSessionOptions { max_files: 1, ..Default::default() };
    let mut whole = DeskRepository::open(owner(941), &root, options, || false).unwrap();
    let mut folder = DeskRepository::open_directory_with_policy(owner(942), &root,
        directory_path(b"selected").unwrap(), options,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let whole_hits = json(&whole.search(1, "needle", Default::default(), || false).unwrap());
    let folder_hits = json(&folder.search(1, "needle", Default::default(), || false).unwrap());
    assert!(!whole_hits.get("discovery_complete").flag());
    assert!(!folder_hits.get("discovery_complete").flag());
    assert_eq!(whole_hits.get("retained_hits").number(), 1);
    assert_eq!(folder_hits.get("retained_hits").number(), 1);
    // read_dir order is unspecified. Select a member missing from BOTH actual
    // observations; merely reopening its parent would fail this regression.
    let selected = names.into_iter().map(|name| format!("selected/{name}")).find(|path| {
        [&whole_hits, &folder_hits].iter().all(|page|
            page.get("hits").array().iter().all(|hit|
                hit.get("path").get("hex").text() != hex(path.as_bytes())))
    }).unwrap();
    let mut selected_repo = DeskRepository::open_file_with_policy(owner(943), &root,
        file_path(selected.as_bytes()).unwrap(), options,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let info = json(&selected_repo.info(|| false).unwrap());
    file_scope(&info, &root, selected.as_bytes());
    assert_eq!(info.get("catalogued_files").number(), 1);
    assert!(info.get("discovery_complete").flag());
    assert_eq!(info.get("discovery_excluded_entries").number(), 0);
    let live = json(&selected_repo.search(1, "needle", Default::default(), || false).unwrap());
    file_scope(&live, &root, selected.as_bytes());
    assert!(live.get("search_complete").flag());
    assert_eq!(live.get("eligible_files").number(), 1);
    assert_eq!(live.get("retained_hits").number(), 1);
    assert_eq!(live.get("hits").array()[0].get("path").get("hex").text(), hex(selected.as_bytes()));

    let begun = json(&selected_repo.begin_index(2, AtlasIndexOptions {
        max_files: 1, max_file_bytes: 1024, max_source_bytes: 4096, max_index_grams: 0 }, || false).unwrap());
    file_scope(&begun, &root, selected.as_bytes());
    assert_eq!(begun.get("initial_source_bytes_read").number(), 0);
    let built = json(&selected_repo.step_index(2, 0, || false).unwrap());
    file_scope(&built, &root, selected.as_bytes());
    assert!(!selected_repo.index_progress(2).unwrap().in_progress);
    fs::write(root.join(&selected), b"changed live source\n").unwrap();
    let indexed = json(&selected_repo.search_indexed(3, 2, "needle", 10, 4096, || false).unwrap());
    file_scope(&indexed, &root, selected.as_bytes());
    assert!(indexed.get("search_complete").flag());
    assert_eq!(indexed.get("source_bytes_read").number(), 0);
    assert_eq!(indexed.get("retained_hits").number(), 1);
    assert_eq!(indexed.get("hits").array()[0].get("path").get("hex").text(), hex(selected.as_bytes()));
    assert_eq!(indexed.get("hits").array()[0].get("source_revision").number(), 2);
    let mut desk = DeskSession::new(owner(944), DeskLimits::default()).unwrap();
    let opened = selected_repo.open_hit(&mut desk, 0, 1, 3, 1, || false).unwrap();
    selected_repo.clear_index(4, || false).unwrap();
    drop(selected_repo); drop(folder);
    assert!(!json(&whole.page(1, 0, 10, || false).unwrap()).get("search_complete").flag());
    drop(whole);
    let pane = opened.change.active.unwrap();
    assert_eq!(desk.model().source(pane, 1).unwrap().bytes(), original);
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), b"needle");
}

#[test]
fn exact_file_discovery_reports_member_exclusion_separately_from_unresolved_ancestor_coverage() {
    let root = fixture();
    file(&root, ".gitignore", b"ignored.rs\nblocked/\n");
    file(&root, "src/ignored.rs", b"private needle\n");
    file(&root, "blocked/source.rs", b"private needle\n");
    for (offset, (path, excluded_complete)) in [
        (b"src/ignored.rs".as_slice(), true), (b"blocked/source.rs".as_slice(), false),
    ].into_iter().enumerate() {
        let mut excluded = DeskRepository::open_file_with_policy(owner(951 + offset as u64), &root,
            file_path(path).unwrap(), AtlasSessionOptions { max_files: 1, ..Default::default() },
            CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
        let info = json(&excluded.info(|| false).unwrap());
        file_scope(&info, &root, path);
        assert_eq!(info.get("catalogued_files").number(), 0);
        assert_eq!(info.get("discovery_complete").flag(), excluded_complete);
        assert_eq!(info.get("discovery_excluded_entries").number(), 1);
        let hidden = json(&excluded.search(1, "needle", Default::default(), || false).unwrap());
        file_scope(&hidden, &root, path);
        assert_eq!(hidden.get("retained_hits").number(), 0);
        assert_eq!(hidden.get("search_complete").flag(), excluded_complete);
        assert_eq!(hidden.get("source_bytes_read").number(), 0);
        assert_eq!(hidden.get("discovery_excluded_entries").number(), 1);
        // Query filtering did not remove a catalogued member. Discovery's
        // deliberate exclusion is a different receipt and must not be hidden.
        assert_eq!(hidden.get("excluded_files").number(), 0);

        let mut included = DeskRepository::open_file_with_policy(owner(953 + offset as u64), &root,
            file_path(path).unwrap(), AtlasSessionOptions { max_files: 1, ..Default::default() },
            CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
        let visible = json(&included.search(1, "needle", Default::default(), || false).unwrap());
        file_scope(&visible, &root, path);
        assert!(visible.get("search_complete").flag());
        assert_eq!(visible.get("retained_hits").number(), 1);
        assert_eq!(visible.get("discovery_excluded_entries").number(), 0);
        assert!(!visible.get("rule_files_enabled").flag());
        assert_eq!(visible.get("rule_policy"), &Json::Null);
    }
}

#[test]
fn raw_exact_file_member_keeps_project_relative_identity_through_paths_and_source_activation() {
    let root = fixture();
    let member = b"C:\\raw\xff/leaf\t\xfe.rs";
    let native = OsString::from_vec(member.to_vec());
    let original = b"raw original needle\n";
    file(&root, &native, original);
    file(&root, "sibling.rs", b"sibling sentinel\n");
    let mut repo = DeskRepository::open_file_with_policy(owner(961), &root,
        file_path(member).unwrap(), AtlasSessionOptions { max_files: 1, ..Default::default() },
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    file_scope(&json(&repo.info(|| false).unwrap()), &root, member);
    let paths = json(&repo.find_paths(1, b"leaf\t\xfe.rs", AtlasPathOptions {
        max_results: 10, case: PathCase::Sensitive, mode: PathMatchMode::Exact }, || false).unwrap());
    file_scope(&paths, &root, member);
    assert!(!paths.get("source_payload_read").flag());
    assert_eq!(paths.get("hits").array().len(), 1);
    assert_eq!(paths.get("hits").array()[0].get("path").get("hex").text(), hex(member));
    let sibling = json(&repo.search(1, "sibling sentinel", Default::default(), || false).unwrap());
    assert!(sibling.get("search_complete").flag());
    assert_eq!(sibling.get("retained_hits").number(), 0);
    let found = json(&repo.search(2, "needle", Default::default(), || false).unwrap());
    file_scope(&found, &root, member);
    assert_eq!(found.get("hits").array()[0].get("path").get("hex").text(), hex(member));
    fs::write(root.join(&native), b"rewritten").unwrap();
    let mut desk = DeskSession::new(owner(962), DeskLimits::default()).unwrap();
    let opened = repo.open_hit(&mut desk, 0, 1, 2, 1, || false).unwrap();
    drop(repo);
    assert_eq!(desk.model().source(opened.change.active.unwrap(), 1).unwrap().bytes(), original);
    assert_eq!(desk.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"needle");
}

#[test]
fn failed_and_canceled_exact_file_candidates_keep_an_existing_repository_and_reader() {
    let root = fixture(); let outside = fixture();
    file(&root, "selected/source.rs", b"original needle\n");
    file(&outside, "outside.rs", b"outside sentinel\n");
    symlink(root.join("selected"), root.join("alias")).unwrap();
    symlink(outside.join("outside.rs"), root.join("selected/link.rs")).unwrap();
    let mut old = DeskRepository::open_file_with_policy(owner(971), &root,
        file_path(b"selected/source.rs").unwrap(), Default::default(),
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    old.search(1, "needle", Default::default(), || false).unwrap();
    let mut desk = DeskSession::new(owner(972), DeskLimits::default()).unwrap();
    let opened = old.open_hit(&mut desk, 0, 1, 1, 1, || false).unwrap();
    for path in [b"missing.rs".as_slice(), b"selected", b"alias/source.rs", b"selected/link.rs"] {
        assert!(DeskRepository::open_file_with_policy(owner(973), &root,
            file_path(path).unwrap(), Default::default(),
            CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err());
    }
    for path in [b"".as_slice(), b"/selected/source.rs", b"selected/../source.rs",
        b"selected//source.rs", b"selected/\0source.rs", b"selected/source.rs/"] {
        assert!(file_path(path).is_err());
    }
    for max_files in [0, 20_001] {
        assert!(DeskRepository::open_file_with_policy(owner(974), &root,
            file_path(b"selected/source.rs").unwrap(),
            AtlasSessionOptions { max_files, ..Default::default() },
            CatalogDiscoveryPolicy::RepositoryRules, || false).is_err());
    }
    let mut polls = 0;
    let probe = DeskRepository::open_file_with_policy(owner(975), &root,
        file_path(b"selected/source.rs").unwrap(), Default::default(),
        CatalogDiscoveryPolicy::IncludeExcluded, || { polls += 1; false }).unwrap();
    drop(probe); assert!(polls > 2);
    for stop_at in [1, polls] {
        let mut calls = 0;
        let error = DeskRepository::open_file_with_policy(owner(976), &root,
            file_path(b"selected/source.rs").unwrap(), Default::default(),
            CatalogDiscoveryPolicy::IncludeExcluded, || { calls += 1; calls >= stop_at }).err().unwrap();
        assert!(error.is_canceled());
        assert_eq!(calls, stop_at);
    }
    assert_eq!(old.accepted_generation(), Some(1));
    let old_page = json(&old.page(1, 0, 10, || false).unwrap());
    file_scope(&old_page, &root, b"selected/source.rs");
    assert!(old_page.get("search_complete").flag());
    assert_eq!(old_page.get("retained_hits").number(), 1);
    assert_eq!(desk.model().revision(), 1);
    assert_eq!(desk.model().selected_bytes(opened.change.active.unwrap(), 1).unwrap(), b"needle");
    assert_eq!(desk.model().source(opened.change.active.unwrap(), 1).unwrap().bytes(), b"original needle\n");
}
