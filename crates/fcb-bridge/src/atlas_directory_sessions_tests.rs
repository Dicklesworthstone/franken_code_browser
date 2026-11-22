#![forbid(unsafe_code)]

//! Directory discovery uses the real shared catalog before search/index caps.
//! Tests retain their filesystem inputs for independent verifier diagnosis.
use super::*;
use std::{ffi::OsString, fs, os::unix::{ffi::{OsStrExt, OsStringExt}, fs::symlink}, path::PathBuf};
use fcb_app::host::{atlas_session::directory_path,
    atlas_search::{AtlasIndexOptions, AtlasSearchPathKind}};
use crate::reader_sessions::Command as ReaderCommand;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-directory-search-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn file(root: &Path, path: impl AsRef<Path>, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn receipt(text: &str, directory: Option<&[u8]>) {
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert_eq!(text.matches("\"discovery_scope\":").count(), 1, "{text}");
    assert_eq!(text.matches("\"discovery_directory\":").count(), 1, "{text}");
    assert_eq!(text.matches("\"path_basis\":\"project-root\"").count(), 1, "{text}");
    match directory {
        Some(directory) => {
            assert!(text.contains("\"discovery_scope\":\"directory\""), "{text}");
            assert!(text.contains(&format!("\"discovery_directory\":{{\"encoding\":\"unix-bytes\",\"hex\":\"{}\"", hex(directory))), "{text}");
        }
        None => assert!(text.contains("\"discovery_scope\":\"workspace\",\"discovery_directory\":null"), "{text}"),
    }
}
fn open(table: &AtlasSessions, root: &Path, directory: &[u8], max_files: usize,
    policy: CatalogDiscoveryPolicy) -> u64 {
    let handle = table.create().unwrap();
    let response = table.open_directory_with_policy(handle, root, directory_path(directory).unwrap(),
        AtlasSessionOptions { max_files, ..Default::default() }, policy, || false).unwrap();
    receipt(response.as_str(), Some(directory));
    let canonical = fs::canonicalize(root).unwrap();
    assert!(response.as_str().contains(&format!("\"root_path\":{{\"encoding\":\"unix-bytes\",\"hex\":\"{}\"",
        hex(canonical.as_os_str().as_bytes()))), "the original grant must not become the directory root");
    handle
}
fn search(table: &AtlasSessions, handle: u64, generation: u64, needle: &str) -> HostResponse {
    table.execute(handle, Command::Search { generation, needle,
        options: AtlasSearchOptions { max_files: 32, max_matches: 32,
            max_file_bytes: 4096, max_source_bytes: 65536 } }, || false).unwrap()
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 32, max_file_bytes: 4096,
        max_source_bytes: 65536, max_index_grams: 65536 }
}
fn number(text: &str, field: &str) -> u64 {
    let marker = format!("\"{field}\":\"");
    text.split_once(&marker).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn directory_search_reaches_a_folder_omitted_by_the_root_metadata_cap() {
    let root = root(); let table = AtlasSessions::new();
    let folders = ["first", "second"];
    for folder in folders { file(&root, format!("{folder}/source.rs"), b"original needle"); }
    let workspace = table.create().unwrap();
    let initial = table.open(workspace, &root,
        AtlasSessionOptions { max_files: 1, ..Default::default() }, || false).unwrap();
    receipt(initial.as_str(), None);
    assert!(initial.as_str().contains("\"discovery_complete\":false"));
    let initial_hits = search(&table, workspace, 1, "needle");
    assert!(initial_hits.as_str().contains("\"retained_hits\":\"1\""));
    // No platform-specific read_dir order is assumed: select the missing folder.
    let selected = folders.into_iter().find(|folder|
        !initial_hits.as_str().contains(&format!("\"display\":\"{folder}/source.rs\""))).unwrap();
    let focused = open(&table, &root, selected.as_bytes(), 1, CatalogDiscoveryPolicy::RepositoryRules);
    let found = search(&table, focused, 1, "needle");
    receipt(found.as_str(), Some(selected.as_bytes()));
    assert!(found.as_str().contains("\"catalogued_files\":\"1\""));
    assert!(found.as_str().contains("\"discovery_complete\":true"));
    assert!(found.as_str().contains("\"search_complete\":true"));
    assert!(found.as_str().contains(&format!("\"display\":\"{selected}/source.rs\"")));
    assert!(found.as_str().contains("\"retained_hits\":\"1\""));
    let previous = table.execute(workspace, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    receipt(previous.as_str(), None);
    assert!(previous.as_str().contains("\"discovery_complete\":false"));
    table.close(focused).unwrap(); table.close(workspace).unwrap();
}

#[test]
fn directory_receipt_survives_live_index_progress_pages_and_independent_source_transfer() {
    let root = root(); let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    file(&root, "selected/source.rs", b"original needle\n");
    file(&root, "sibling/source.rs", b"outside needle\n");
    let handle = open(&table, &root, b"selected", 4, CatalogDiscoveryPolicy::RepositoryRules);
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "selected").unwrap();
    let begun = table.execute(handle, Command::SearchBeginScoped { generation: 1, needle: "needle",
        scope: &scope, options: Default::default() }, || false).unwrap();
    receipt(begun.as_str(), Some(b"selected"));
    let done = table.execute(handle, Command::SearchStep { generation: 1 }, || false).unwrap();
    receipt(done.as_str(), Some(b"selected"));
    assert!(done.as_str().contains("\"search_complete\":true"));
    assert!(done.as_str().contains("\"retained_hits\":\"1\""));
    for command in [Command::SearchPage { generation: 1, start: 0, limit: 10 },
        Command::SearchOverlay { generation: 1 }] {
        receipt(table.execute(handle, command, || false).unwrap().as_str(), Some(b"selected"));
    }
    let live_reader = readers.create().unwrap();
    receipt(table.open_search_reader(handle, &readers, live_reader, 1, 1, || false).unwrap().as_str(), Some(b"selected"));

    let begun = table.execute_index(handle, IndexCommand::PrepareBeginScoped { generation: 10,
        scope: &scope, options: index_options() }, || false).unwrap();
    receipt(begun.as_str(), Some(b"selected"));
    assert!(begun.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    receipt(table.execute_index(handle, IndexCommand::PrepareProgress { generation: 10 }, || false)
        .unwrap().as_str(), Some(b"selected"));
    let built = table.execute_index(handle, IndexCommand::PrepareStep { generation: 10 }, || false).unwrap();
    receipt(built.as_str(), Some(b"selected"));
    assert!(built.as_str().contains("\"index_build_in_progress\":false"));
    assert!(built.as_str().contains("\"capture_complete\":true"));
    receipt(table.execute_index(handle, IndexCommand::Info, || false).unwrap().as_str(), Some(b"selected"));
    fs::write(root.join("selected/source.rs"), b"replacement current bytes\n").unwrap();
    let begun = table.execute_index(handle, IndexCommand::BeginScoped { generation: 11,
        index_generation: 10, needle: "needle", scope: &scope, max_matches: 10, max_scan_bytes: 65536 }, || false).unwrap();
    receipt(begun.as_str(), Some(b"selected"));
    let done = table.execute(handle, Command::SearchStep { generation: 11 }, || false).unwrap();
    receipt(done.as_str(), Some(b"selected"));
    assert!(done.as_str().contains("\"search_complete\":true"));
    let page = table.execute(handle, Command::SearchPage { generation: 11, start: 0, limit: 10 }, || false).unwrap();
    receipt(page.as_str(), Some(b"selected"));
    assert!(page.as_str().contains("\"display\":\"selected/source.rs\""));
    assert!(!page.as_str().contains("\"display\":\"sibling/source.rs\""));
    let file = number(page.as_str(), "file_id"); let revision = number(page.as_str(), "source_revision");
    let source_reader = readers.create().unwrap();
    let source = table.open_index_reader(handle, &readers, source_reader, 10, file, revision, || false).unwrap();
    receipt(source.as_str(), Some(b"selected"));
    assert!(source.as_str().contains("\"source_reopened\":false"));
    receipt(table.execute_index(handle, IndexCommand::Clear { generation: 12 }, || false)
        .unwrap().as_str(), Some(b"selected"));
    table.close(handle).unwrap();
    for reader in [live_reader, source_reader] {
        let copy = readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 16 }, || false).unwrap();
        assert!(copy.as_str().contains("\"original_hex\":\"6f726967696e616c206e6565646c650a\""));
        readers.close(reader).unwrap();
    }
}

#[test]
fn folder_catalog_never_widens_for_workspace_or_frozen_visible_queries() {
    let root = root(); let table = AtlasSessions::new();
    file(&root, "selected/source.rs", b"inside needle");
    file(&root, "sibling/source.rs", b"outside needle");
    let handle = open(&table, &root, b"selected", 8, CatalogDiscoveryPolicy::RepositoryRules);
    let whole_catalog = search(&table, handle, 1, "outside");
    receipt(whole_catalog.as_str(), Some(b"selected"));
    assert!(whole_catalog.as_str().contains("\"retained_hits\":\"0\""));
    let paths = [hex(b"selected/source.rs"), hex(b"sibling/source.rs")].join(",");
    let visible = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, &paths).unwrap();
    let partial = table.execute(handle, Command::SearchScoped { generation: 2, needle: "needle",
        scope: &visible, options: Default::default() }, || false).unwrap();
    receipt(partial.as_str(), Some(b"selected"));
    assert!(partial.as_str().contains("\"retained_hits\":\"1\""));
    assert!(partial.as_str().contains("\"scope_region_missing_files\":\"1\""));
    assert!(partial.as_str().contains("\"search_complete\":false"));
    table.close(handle).unwrap();
}

#[test]
fn ancestor_exclusions_and_failed_rules_do_not_become_successful_empty_folder_search() {
    for rule in [b"ignored/\n".as_slice(), b"[broken\n"] {
        let root = root(); let table = AtlasSessions::new();
        file(&root, ".gitignore", rule);
        file(&root, "ignored/selected/.gitignore", b"!source.rs\n");
        file(&root, "ignored/selected/source.rs", b"private needle");
        let rules = open(&table, &root, b"ignored/selected", 8, CatalogDiscoveryPolicy::RepositoryRules);
        let included = open(&table, &root, b"ignored/selected", 8, CatalogDiscoveryPolicy::IncludeExcluded);
        let blocked = search(&table, rules, 1, "private needle");
        receipt(blocked.as_str(), Some(b"ignored/selected"));
        assert!(blocked.as_str().contains("\"discovery_complete\":false"));
        assert!(blocked.as_str().contains("\"search_complete\":false"));
        assert!(blocked.as_str().contains("\"retained_hits\":\"0\""));
        let found = search(&table, included, 1, "private needle");
        receipt(found.as_str(), Some(b"ignored/selected"));
        assert!(found.as_str().contains("\"search_complete\":true"));
        assert!(found.as_str().contains("\"retained_hits\":\"1\""));
        assert!(found.as_str().contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
        table.close(included).unwrap();
        receipt(table.execute(rules, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false)
            .unwrap().as_str(), Some(b"ignored/selected"));
        table.close(rules).unwrap();
    }
}

#[test]
fn raw_directory_identity_survives_paths_streaming_and_reader_capture_without_symlink_escape() {
    let outside = root(); let root = root(); let table = AtlasSessions::new(); let readers = ReaderSessions::new();
    let directory = b"C:\\raw\xff";
    let native = OsString::from_vec(directory.to_vec());
    file(&root, Path::new(&native).join("source.rs"), b"original needle\n");
    file(&outside, "escape.rs", b"escaping sentinel");
    symlink(&outside, root.join(&native).join("escape")).unwrap();
    let handle = open(&table, &root, directory, 8, CatalogDiscoveryPolicy::IncludeExcluded);
    let found = search(&table, handle, 1, "needle");
    receipt(found.as_str(), Some(directory));
    assert!(found.as_str().contains("\"hex\":\"433a5c726177ff2f736f757263652e7273\""));
    assert!(search(&table, handle, 2, "escaping").as_str().contains("\"retained_hits\":\"0\""));
    let paths = table.execute_paths(handle, PathCommand::Find { generation: 1,
        needle: b"source.rs", options: Default::default() }, || false).unwrap();
    receipt(paths.as_str(), Some(directory));
    assert!(paths.as_str().contains("\"retained_hits\":\"1\""));
    let stream = table.execute_stream(handle, StreamCommand::Search { needle: "needle",
        options: Default::default() }, || false).unwrap();
    receipt(stream.as_str(), Some(directory));
    assert!(stream.as_str().contains("\"retained_hits\":\"1\""));
    let generation = number(stream.as_str(), "query_generation");
    receipt(table.execute_stream(handle, StreamCommand::Hit { generation, position: 0 }, || false)
        .unwrap().as_str(), Some(directory));
    let capture = search(&table, handle, 3, "needle");
    receipt(capture.as_str(), Some(directory));
    fs::write(root.join(&native).join("source.rs"), b"new source").unwrap();
    let reader = readers.create().unwrap();
    receipt(table.open_search_reader(handle, &readers, reader, 3, 1, || false).unwrap().as_str(), Some(directory));
    table.close(handle).unwrap();
    assert!(readers.execute(reader, ReaderCommand::CopyRange { start: 0, end: 16 }, || false)
        .unwrap().as_str().contains("6f726967696e616c206e6565646c650a"));
    readers.close(reader).unwrap();
}

#[test]
fn canceling_directory_initialization_keeps_other_handles_alive_and_the_empty_handle_retryable() {
    let root = root(); let table = AtlasSessions::new();
    file(&root, "selected/source.rs", b"needle");
    file(&root, "other/source.rs", b"independent");
    let independent = open(&table, &root, b"other", 8, CatalogDiscoveryPolicy::RepositoryRules);
    for policy in [CatalogDiscoveryPolicy::RepositoryRules, CatalogDiscoveryPolicy::IncludeExcluded] {
        let handle = table.create().unwrap(); let mut fired = false;
        let result = table.open_directory_with_policy(handle, &root, directory_path(b"selected").unwrap(),
            Default::default(), policy, || {
                if !fired {
                    fired = true; table.cancel(handle).unwrap();
                    receipt(table.execute(independent, Command::Info, || false).unwrap().as_str(), Some(b"other"));
                }
                false
            });
        assert!(fired);
        assert_eq!(result.err(), Some(AccessError::Canceled));
        assert_eq!(table.execute(handle, Command::Info, || false).err(), Some(AccessError::NotOpen));
        let retried = table.open_directory_with_policy(handle, &root, directory_path(b"selected").unwrap(),
            Default::default(), policy, || false).unwrap();
        receipt(retried.as_str(), Some(b"selected"));
        assert_eq!(table.open_directory_with_policy(handle, &root, directory_path(b"other").unwrap(),
            Default::default(), policy, || false).err(), Some(AccessError::AlreadyOpen));
        receipt(table.execute(handle, Command::Info, || false).unwrap().as_str(), Some(b"selected"));
        table.close(handle).unwrap();
        assert!(search(&table, independent, if policy == CatalogDiscoveryPolicy::RepositoryRules { 1 } else { 2 },
            "independent").as_str().contains("\"retained_hits\":\"1\""));
    }
    table.close(independent).unwrap();
}
