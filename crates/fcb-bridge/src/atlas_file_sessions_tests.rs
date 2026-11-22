#![forbid(unsafe_code)]

//! Direct member catalogs exercise real retained search and captured readers.
//! Inputs remain on disk for the independent verifier's failure diagnosis.
use super::*;
use std::{ffi::OsString, fs, os::unix::{ffi::{OsStrExt, OsStringExt}, fs::symlink}, path::PathBuf};
use fcb_app::host::{atlas_session::{file_path, directory_path},
    atlas_search::{AtlasIndexOptions, AtlasSearchPathKind}};
use crate::reader_sessions::Command as ReaderCommand;

fn fixture_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-file-search-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn put(root: &Path, path: impl AsRef<Path>, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn hex(raw: &[u8]) -> String { raw.iter().map(|byte| format!("{byte:02x}")).collect() }
fn receipt(text: &str, file: &[u8]) {
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    for field in ["discovery_scope", "discovery_directory", "discovery_file", "path_basis"] {
        assert_eq!(text.matches(&format!("\"{field}\":")).count(), 1, "{text}");
    }
    assert!(text.contains("\"discovery_scope\":\"file\",\"discovery_directory\":null"), "{text}");
    assert!(text.contains(&format!("\"discovery_file\":{{\"encoding\":\"unix-bytes\",\"hex\":\"{}\"", hex(file))), "{text}");
    assert!(text.contains("\"path_basis\":\"project-root\""), "{text}");
}
fn open(table: &AtlasSessions, root: &Path, file: &[u8], policy: CatalogDiscoveryPolicy) -> u64 {
    let handle = table.create().unwrap();
    let response = table.open_file_with_policy(handle, root, file_path(file).unwrap(),
        AtlasSessionOptions { max_files: 1, ..Default::default() }, policy, || false).unwrap();
    receipt(response.as_str(), file);
    assert!(response.as_str().contains(&format!("\"root_path\":{{\"encoding\":\"unix-bytes\",\"hex\":\"{}\"",
        hex(fs::canonicalize(root).unwrap().as_os_str().as_bytes()))));
    handle
}
fn search(table: &AtlasSessions, handle: u64, generation: u64, needle: &str) -> HostResponse {
    table.execute(handle, Command::Search { generation, needle,
        options: AtlasSearchOptions { max_files: 32, max_matches: 32,
            max_file_bytes: 4096, max_source_bytes: 65536 } }, || false).unwrap()
}
fn number(text: &str, field: &str) -> u64 {
    text.split_once(&format!("\"{field}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn direct_file_search_reaches_a_member_missing_from_both_root_and_parent_catalogs() {
    let root = fixture_root(); let table = AtlasSessions::new();
    let files: Vec<_> = (0..8).map(|n| format!("crowded/source-{n}.rs")).collect();
    for file in &files { put(&root, file, b"shared needle"); }
    let workspace = table.create().unwrap();
    let workspace_info = table.open(workspace, &root,
        AtlasSessionOptions { max_files: 1, ..Default::default() }, || false).unwrap();
    assert!(workspace_info.as_str().contains("\"discovery_complete\":false"));
    let folder = table.create().unwrap();
    let folder_info = table.open_directory_with_policy(folder, &root, directory_path(b"crowded").unwrap(),
        AtlasSessionOptions { max_files: 1, ..Default::default() },
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    assert!(folder_info.as_str().contains("\"discovery_complete\":false"));
    let root_hits = search(&table, workspace, 1, "needle");
    let folder_hits = search(&table, folder, 1, "needle");
    // Choose an actually omitted member; no read_dir ordering is assumed.
    let selected = files.iter().find(|file| {
        let marker = format!("\"display\":\"{file}\"");
        !root_hits.as_str().contains(&marker) && !folder_hits.as_str().contains(&marker)
    }).unwrap();
    let handle = open(&table, &root, selected.as_bytes(), CatalogDiscoveryPolicy::RepositoryRules);
    let found = search(&table, handle, 1, "needle");
    receipt(found.as_str(), selected.as_bytes());
    for expected in ["\"catalogued_files\":\"1\"", "\"discovery_complete\":true",
        "\"search_complete\":true", "\"retained_hits\":\"1\""] {
        assert!(found.as_str().contains(expected), "{expected}: {}", found.as_str());
    }
    assert!(found.as_str().contains(&format!("\"display\":\"{selected}\"")));
    assert!(table.execute(workspace, Command::SearchPage { generation: 1, start: 0, limit: 10 },
        || false).unwrap().as_str().contains("\"discovery_scope\":\"workspace\""));
    assert!(table.execute(folder, Command::SearchPage { generation: 1, start: 0, limit: 10 },
        || false).unwrap().as_str().contains("\"discovery_scope\":\"directory\""));
    table.close(handle).unwrap(); table.close(folder).unwrap(); table.close(workspace).unwrap();
}

#[test]
fn file_scope_survives_index_pages_source_transfer_and_changed_live_bytes() {
    let root = fixture_root(); let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    put(&root, "selected/source.rs", b"original needle\n");
    put(&root, "selected/sibling.rs", b"outside needle\n");
    let file = b"selected/source.rs";
    let handle = open(&table, &root, file, CatalogDiscoveryPolicy::RepositoryRules);
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "selected/source.rs").unwrap();
    receipt(table.execute(handle, Command::SearchBeginScoped { generation: 1, needle: "needle",
        scope: &scope, options: Default::default() }, || false).unwrap().as_str(), file);
    let live = table.execute(handle, Command::SearchStep { generation: 1 }, || false).unwrap();
    receipt(live.as_str(), file);
    assert!(live.as_str().contains("\"search_complete\":true"));
    let live_reader = readers.create().unwrap();
    receipt(table.open_search_reader(handle, &readers, live_reader, 1, 1, || false).unwrap().as_str(), file);
    for command in [Command::SearchPage { generation: 1, start: 0, limit: 10 },
        Command::SearchOverlay { generation: 1 }] {
        receipt(table.execute(handle, command, || false).unwrap().as_str(), file);
    }
    let options = AtlasIndexOptions { max_files: 32, max_file_bytes: 4096,
        max_source_bytes: 65536, max_index_grams: 65536 };
    let begun = table.execute_index(handle, IndexCommand::PrepareBeginScoped { generation: 10,
        scope: &scope, options }, || false).unwrap();
    receipt(begun.as_str(), file);
    assert!(begun.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    receipt(table.execute_index(handle, IndexCommand::PrepareProgress { generation: 10 }, || false)
        .unwrap().as_str(), file);
    let built = table.execute_index(handle, IndexCommand::PrepareStep { generation: 10 }, || false).unwrap();
    receipt(built.as_str(), file);
    assert!(built.as_str().contains("\"index_build_in_progress\":false"));
    assert!(built.as_str().contains("\"capture_complete\":true"));
    receipt(table.execute_index(handle, IndexCommand::Info, || false).unwrap().as_str(), file);
    fs::write(root.join("selected/source.rs"), b"replacement without old text\n").unwrap();
    receipt(table.execute_index(handle, IndexCommand::BeginScoped { generation: 11,
        index_generation: 10, needle: "needle", scope: &scope, max_matches: 10, max_scan_bytes: 65536 },
        || false).unwrap().as_str(), file);
    let done = table.execute(handle, Command::SearchStep { generation: 11 }, || false).unwrap();
    receipt(done.as_str(), file);
    assert!(done.as_str().contains("\"search_complete\":true"));
    let page = table.execute(handle, Command::SearchPage { generation: 11, start: 0, limit: 10 }, || false).unwrap();
    receipt(page.as_str(), file);
    assert!(page.as_str().contains("\"retained_hits\":\"1\""));
    assert!(page.as_str().contains("\"display\":\"selected/source.rs\""));
    assert!(!page.as_str().contains("\"display\":\"selected/sibling.rs\""));
    let source_reader = readers.create().unwrap();
    let source = table.open_index_reader(handle, &readers, source_reader, 10,
        number(page.as_str(), "file_id"), number(page.as_str(), "source_revision"), || false).unwrap();
    receipt(source.as_str(), file);
    assert!(source.as_str().contains("\"source_reopened\":false"));
    receipt(table.execute_index(handle, IndexCommand::Clear { generation: 12 }, || false)
        .unwrap().as_str(), file);
    table.close(handle).unwrap();
    for reader in [live_reader, source_reader] {
        assert!(readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 16 }, || false)
            .unwrap().as_str().contains("\"original_hex\":\"6f726967696e616c206e6565646c650a\""));
        readers.close(reader).unwrap();
    }
}

#[test]
fn workspace_and_visible_query_filters_cannot_widen_a_file_catalog() {
    let root = fixture_root(); let table = AtlasSessions::new();
    put(&root, "selected.rs", b"inside needle"); put(&root, "sibling.rs", b"outside needle");
    let handle = open(&table, &root, b"selected.rs", CatalogDiscoveryPolicy::RepositoryRules);
    let outside = search(&table, handle, 1, "outside");
    receipt(outside.as_str(), b"selected.rs");
    assert!(outside.as_str().contains("\"retained_hits\":\"0\""));
    assert!(outside.as_str().contains("\"search_complete\":true"));
    let members = [hex(b"selected.rs"), hex(b"sibling.rs")].join(",");
    let visible = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, &members).unwrap();
    let partial = table.execute(handle, Command::SearchScoped { generation: 2, needle: "needle",
        scope: &visible, options: Default::default() }, || false).unwrap();
    receipt(partial.as_str(), b"selected.rs");
    assert!(partial.as_str().contains("\"retained_hits\":\"1\""));
    assert!(partial.as_str().contains("\"scope_region_missing_files\":\"1\""));
    assert!(partial.as_str().contains("\"search_complete\":false"));
    table.close(handle).unwrap();
}

#[test]
fn explicit_file_exclusion_and_unavailable_ancestor_policy_remain_distinct() {
    let root = fixture_root(); let table = AtlasSessions::new();
    put(&root, ".gitignore", b"*.rs\n"); put(&root, "selected.rs", b"private needle");
    let excluded = open(&table, &root, b"selected.rs", CatalogDiscoveryPolicy::RepositoryRules);
    let broad = open(&table, &root, b"selected.rs", CatalogDiscoveryPolicy::IncludeExcluded);
    let blocked = search(&table, excluded, 1, "private");
    receipt(blocked.as_str(), b"selected.rs");
    assert!(blocked.as_str().contains("\"catalogued_files\":\"0\""));
    assert!(blocked.as_str().contains("\"discovery_complete\":true"));
    assert!(blocked.as_str().contains("\"discovery_excluded_entries\":\"1\""));
    assert!(blocked.as_str().contains("\"discovery_unavailable_entries\":\"0\""));
    let admitted = search(&table, broad, 1, "private");
    assert!(admitted.as_str().contains("\"search_complete\":true"));
    assert!(admitted.as_str().contains("\"retained_hits\":\"1\""));
    assert!(admitted.as_str().contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    table.close(broad).unwrap(); table.close(excluded).unwrap();
    for rule in [b"ignored/\n".as_slice(), b"[unsupported\n"] {
        let root = fixture_root();
        put(&root, ".gitignore", rule);
        put(&root, "ignored/selected.rs", b"private needle");
        let handle = open(&table, &root, b"ignored/selected.rs", CatalogDiscoveryPolicy::RepositoryRules);
        let blocked = search(&table, handle, 1, "private");
        receipt(blocked.as_str(), b"ignored/selected.rs");
        assert!(blocked.as_str().contains("\"discovery_complete\":false"));
        assert!(blocked.as_str().contains("\"search_complete\":false"));
        assert!(blocked.as_str().contains("\"retained_hits\":\"0\""));
        table.close(handle).unwrap();
    }
}

#[test]
fn raw_file_identity_is_reversible_and_source_admission_never_follows_symlinks() {
    let outside = fixture_root(); let root = fixture_root(); let table = AtlasSessions::new();
    let file = b"C:\\raw\xff\n.rs";
    put(&root, Path::new(&OsString::from_vec(file.to_vec())), b"original needle\n");
    put(&outside, "escape.rs", b"outside");
    symlink(outside.join("escape.rs"), root.join("alias.rs")).unwrap();
    symlink(&outside, root.join("alias-dir")).unwrap();
    let handle = open(&table, &root, file, CatalogDiscoveryPolicy::IncludeExcluded);
    let found = search(&table, handle, 1, "needle");
    receipt(found.as_str(), file);
    assert!(found.as_str().contains(&format!("\"hex\":\"{}\"", hex(file))));
    assert!(found.as_str().contains("\"retained_hits\":\"1\""));
    let empty = table.create().unwrap();
    for raw in [b"missing.rs".as_slice(), b"alias.rs", b"alias-dir/escape.rs"] {
        assert!(table.open_file_with_policy(empty, &root, file_path(raw).unwrap(),
            Default::default(), CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err());
        assert_eq!(table.execute(empty, Command::Info, || false).err(), Some(AccessError::NotOpen));
    }
    table.open_file_with_policy(empty, &root, file_path(file).unwrap(),
        Default::default(), CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    table.close(empty).unwrap(); table.close(handle).unwrap();
}

#[test]
fn raw_file_parser_preserves_identity_and_admits_only_the_bounded_final_component() {
    for raw in [b"a.rs".as_slice(), b"a/b.rs", b"C:\\name\xff.rs"] {
        assert_eq!(file_path(raw).unwrap().as_bytes(), raw);
    }
    for raw in [b"".as_slice(), b"/a", b"a/", b".", b"..", b"a/../b",
        b"a//b", b"a/\0b"] {
        assert_eq!(file_path(raw).err(), Some(AtlasSessionError::InvalidLimits), "{raw:?}");
    }
    let limit = std::iter::repeat_n("a", 32).collect::<Vec<_>>().join("/");
    assert!(file_path(limit.as_bytes()).is_ok());
    assert_eq!(directory_path(limit.as_bytes()).err(), Some(AtlasSessionError::InvalidLimits));
    assert_eq!(file_path(format!("{limit}/a").as_bytes()).err(), Some(AtlasSessionError::InvalidLimits));
    assert_eq!(file_path(&[b'a'; 2049]).err(), Some(AtlasSessionError::InvalidLimits));
}
