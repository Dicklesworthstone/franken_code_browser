#![forbid(unsafe_code)]
#![cfg(unix)]

//! Scoped Project Files must keep the progressive catalog's bounds and reader
//! authority while proving coverage only for the selected raw directory.

mod support;
use support::{parse, Json};
use std::{collections::BTreeSet, ffi::OsString, fs,
    os::unix::{ffi::OsStringExt, fs::symlink}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, FileId};
use fcb::source::SourceError;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, catalog_session::{CatalogDiscoveryPolicy,
    CatalogDocumentLinkSelection, CatalogSession, CatalogSessionError,
    MAX_CATALOG_DIRECTORY_BYTES, MAX_CATALOG_PAGE_ENTRIES, MAX_CATALOG_PAGE_PATH_BYTES,
    MAX_CATALOG_RETAINED_PAGES, MAX_CATALOG_RETAINED_FILES,
    MAX_CATALOG_RETAINED_PATH_BYTES, MAX_CATALOG_RESPONSE_BYTES},
    reader::ReaderDocumentOptions};

const ORIGINAL: &[u8] = b"fn selected_directory_capture() {}\n";
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn file(owner: ArenaOwnerId, id: u64) -> FileId { FileId::new(owner, id).unwrap() }
fn json(response: &HostResponse) -> Json {
    assert!(response.as_str().len() <= MAX_CATALOG_RESPONSE_BYTES);
    parse(response.as_str().as_bytes()).unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn raw(payload: &Json) -> Vec<u8> {
    let text = payload.get("hex").text();
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}
fn new_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-directory-catalog-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Retain fixtures for diagnosis.
}
fn write(root: &Path, relative: impl AsRef<Path>, bytes: &[u8]) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
}
fn linked_files(directory: &Path, count: usize) {
    fs::create_dir_all(directory).unwrap();
    let seed = directory.join("file-0000.rs"); fs::write(&seed, ORIGINAL).unwrap();
    for ordinal in 1..count {
        fs::hard_link(&seed, directory.join(format!("file-{ordinal:04}.rs"))).unwrap();
    }
}
fn scope(value: &Json, directory: Option<&[u8]>) {
    assert_eq!(value.get("discovery_scope").text(), if directory.is_some() { "directory" } else { "workspace" });
    match directory {
        Some(directory) => assert_eq!(raw(value.get("discovery_directory")), directory),
        None => assert_eq!(value.get("discovery_directory"), &Json::Null),
    }
    assert_eq!(value.get("path_basis").text(), "project-root");
    assert_eq!(value.get("discovery_source_bytes_read").number(), 0);
    assert_eq!(value.get("discovery_source_read_calls").number(), 0);
}
fn bounds(page: &Json) {
    assert!(page.get("retained_pages").number() <= MAX_CATALOG_RETAINED_PAGES as u64);
    assert!(page.get("retained_files").number() <= MAX_CATALOG_RETAINED_FILES as u64);
    assert!(page.get("retained_path_bytes").number() <= MAX_CATALOG_RETAINED_PATH_BYTES as u64);
    assert!(page.get("files").array().len() <= MAX_CATALOG_PAGE_ENTRIES);
    let bytes: usize = page.get("files").array().iter().map(|row| raw(row.get("path")).len()).sum();
    assert!(bytes <= MAX_CATALOG_PAGE_PATH_BYTES);
}
struct Row { page: u64, file: u64, path: Vec<u8> }
fn drain(session: &mut CatalogSession, directory: &[u8]) -> (Vec<Row>, Json) {
    let mut page = json(&session.page(1, || false).unwrap());
    let mut rows = Vec::new();
    for _ in 0..128 {
        scope(&page, Some(directory)); bounds(&page);
        rows.extend(page.get("files").array().iter().map(|row| Row {
            page: page.get("page_id").number(), file: row.get("file_id").number(),
            path: raw(row.get("path")),
        }));
        if !page.get("more").flag() { return (rows, page); }
        page = json(&session.advance(page.get("page_id").number(), || false).unwrap());
    }
    panic!("small scoped fixture did not finish within bounded pages");
}

#[test]
fn scoped_paging_reaches_late_files_without_sibling_discovery_or_growing_retention() {
    let root = new_root(); let directory = b"parent/selected";
    linked_files(&root.join("parent/selected"), 1300);
    write(&root, "parent/selectedish/outside.rs", b"not this directory");
    for ordinal in 0..16 {
        write(&root, format!("sibling-{ordinal}/.gitignore"), b"[broken");
        write(&root, format!("sibling-{ordinal}/outside.rs"), b"never visited");
    }
    let catalog_owner = owner(80101);
    let mut session = CatalogSession::open_directory_with_policy(catalog_owner, &root,
        directory, CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    assert_eq!(session.directory().unwrap().as_bytes(), directory);
    assert_eq!(session.page(1, || false).unwrap().exit_code(), EXIT_PARTIAL);
    let (rows, complete) = drain(&mut session, directory);
    assert_eq!(rows.len(), 1300);
    let mut unique = BTreeSet::new();
    for (ordinal, row) in rows.iter().enumerate() {
        assert_eq!(row.file, ordinal as u64 + 1);
        assert!(row.path.starts_with(b"parent/selected/"));
        assert!(unique.insert(row.path.clone()));
    }
    assert_eq!(complete.get("state").text(), "complete");
    assert!(complete.get("discovery_complete").flag());
    assert_eq!(complete.get("catalogued_files").number(), 1300);
    assert_eq!(complete.get("rule_policy").get("files_loaded").number(), 0);
    assert!(complete.get("first_retained_page").number() > 1);
    assert!(matches!(session.page(1, || false), Err(CatalogSessionError::StalePage)));
    scope(&json(&session.info(|| false).unwrap()), Some(directory));
    scope(&json(&session.stop(|| false).unwrap()), Some(directory));
    let selected = rows.last().unwrap();
    scope(&json(&session.page(selected.page, || false).unwrap()), Some(directory));
    assert!(matches!(session.open_reader(owner(80102), selected.page,
        file(owner(80199), selected.file), 4096, || false), Err(CatalogSessionError::ForeignOwner)));
    let (mut reader, receipt) = session.open_reader(owner(80102), selected.page,
        file(catalog_owner, selected.file), 4096, || false).unwrap();
    let receipt = json(&receipt); scope(&receipt, Some(directory));
    assert_eq!(receipt.get("source_bytes_read").number(), ORIGINAL.len() as u64);
    fs::write(root.join(OsString::from_vec(selected.path.clone())), b"changed after capture").unwrap();
    session.grant().revoke(); drop(session);
    assert_eq!(json(&reader.copy_range(0, ORIGINAL.len() as u64, || false).unwrap())
        .get("original_hex").text(), hex(ORIGINAL));
}

#[test]
fn raw_directory_and_file_names_survive_completed_scope_receipts_and_reader_capture() {
    let root = new_root(); let directory = b"C:\\scope\xff";
    let target = root.join(OsString::from_vec(directory.to_vec())); fs::create_dir(&target).unwrap();
    let original: Vec<u8> = "\u{feff}αβ\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let names = [b"raw-\xfe.md".to_vec(), b"C:\\leaf.rs".to_vec(), b"literal%2f.rs".to_vec()];
    for name in &names { fs::write(target.join(OsString::from_vec(name.clone())), &original).unwrap(); }
    let catalog_owner = owner(80201);
    let mut session = CatalogSession::open_directory_with_policy(catalog_owner, &root,
        directory, CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let (rows, complete) = drain(&mut session, directory);
    assert_eq!(rows.len(), names.len()); assert!(complete.get("discovery_complete").flag());
    for name in names {
        let mut expected = directory.to_vec(); expected.push(b'/'); expected.extend(name);
        assert!(rows.iter().any(|row| row.path == expected));
    }
    let selected = rows.iter().find(|row| row.path.ends_with(b"raw-\xfe.md")).unwrap();
    let (mut reader, receipt) = session.open_reader(owner(80202), selected.page,
        file(catalog_owner, selected.file), 4096, || false).unwrap();
    scope(&json(&receipt), Some(directory));
    assert_eq!(json(&receipt).get("reader").get("encoding").text(), "utf16le");
    assert_eq!(json(&reader.copy_range(0, original.len() as u64, || false).unwrap())
        .get("original_hex").text(), hex(&original));
    assert_eq!(session.directory().unwrap().as_bytes(), directory);
}

#[test]
fn scoped_repository_rules_inherit_all_ancestors_without_reading_sibling_policy() {
    let root = new_root(); let directory = b"parent/selected";
    write(&root, ".gitignore", b"*.tmp\n");
    write(&root, "parent/.gitignore", b"*.rs\n!kept.rs\n");
    write(&root, "parent/selected/.fcbignore", b"!restored.rs\n");
    for name in ["kept.rs", "restored.rs", "dropped.rs", "dropped.tmp", "notes.txt"] {
        write(&root, format!("parent/selected/{name}"), ORIGINAL);
    }
    write(&root, "parent/sibling/.gitignore", b"[broken");
    let mut session = CatalogSession::open_directory_with_policy(owner(80301), &root,
        directory, CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let (rows, page) = drain(&mut session, directory);
    assert!(page.get("discovery_complete").flag());
    assert_eq!(page.get("rule_policy").get("files_loaded").number(), 3);
    assert!(rows.iter().all(|row| row.path.starts_with(b"parent/selected/")));
    for expected in ["kept.rs", "restored.rs", "notes.txt"] {
        assert!(rows.iter().any(|row| row.path == format!("parent/selected/{expected}").as_bytes()));
    }
    for excluded in ["dropped.rs", "dropped.tmp"] {
        assert!(!rows.iter().any(|row| row.path == format!("parent/selected/{excluded}").as_bytes()));
    }
}

#[test]
fn excluded_or_unavailable_ancestors_remain_partial_and_include_excluded_reads_no_rules() {
    let root = new_root(); let directory = b"ignored/selected";
    write(&root, ".gitignore", b"ignored/\n");
    write(&root, "ignored/selected/.gitignore", b"!kept.rs\n");
    write(&root, "ignored/selected/kept.rs", ORIGINAL);
    let mut withheld = CatalogSession::open_directory_with_policy(owner(80401), &root,
        directory, CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let (rows, page) = drain(&mut withheld, directory);
    assert!(rows.is_empty()); assert_eq!(page.get("state").text(), "partial");
    assert!(!page.get("discovery_complete").flag());
    assert_eq!(page.get("rule_policy").get("files_loaded").number(), 1);
    assert_eq!(withheld.info(|| false).unwrap().exit_code(), EXIT_PARTIAL);

    fs::write(root.join(".gitignore"), b"[broken").unwrap();
    let mut unavailable = CatalogSession::open_directory_with_policy(owner(80402), &root,
        directory, CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let (rows, page) = drain(&mut unavailable, directory);
    assert!(rows.is_empty()); assert_eq!(page.get("state").text(), "partial");
    assert_eq!(page.get("rule_policy").get("files_read").number(), 1);
    assert_eq!(page.get("rule_policy").get("failed_files").number(), 1);

    let mut included = CatalogSession::open_directory_with_policy(owner(80403), &root,
        directory, CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let (rows, page) = drain(&mut included, directory);
    assert!(rows.iter().any(|row| row.path == b"ignored/selected/kept.rs"));
    assert!(page.get("discovery_complete").flag());
    assert!(!page.get("rule_files_enabled").flag());
    assert_eq!(page.get("rule_policy"), &Json::Null);
    write(&root, ".fcb-cache/selected/private.rs", ORIGINAL);
    let mut owned = CatalogSession::open_directory_with_policy(owner(80404), &root,
        b".fcb-cache/selected", CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let (rows, page) = drain(&mut owned, b".fcb-cache/selected");
    assert!(rows.is_empty()); assert_eq!(page.get("state").text(), "partial");
    assert!(!page.get("rule_files_enabled").flag());
    assert_eq!(page.get("rule_policy"), &Json::Null);
}

#[test]
fn empty_scopes_complete_but_missing_files_symlinks_and_malformed_targets_refuse() {
    let root = new_root(); fs::create_dir(root.join("empty")).unwrap();
    let mut empty = CatalogSession::open_directory_with_policy(owner(80501), &root,
        b"empty", CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let (rows, page) = drain(&mut empty, b"empty");
    assert!(rows.is_empty()); assert!(page.get("discovery_complete").flag());
    assert_eq!(empty.info(|| false).unwrap().exit_code(), EXIT_OK);
    write(&root, "regular.rs", ORIGINAL);
    symlink(root.join("empty"), root.join("alias")).unwrap();
    for target in [b"alias".as_slice(), b"alias/nested"] {
        assert!(matches!(CatalogSession::open_directory_with_policy(owner(80502), &root,
            target, CatalogDiscoveryPolicy::IncludeExcluded, || false),
            Err(CatalogSessionError::Source(SourceError::SymlinkForbidden))));
    }
    for target in [b"missing".as_slice(), b"regular.rs"] {
        assert!(CatalogSession::open_directory_with_policy(owner(80503), &root,
            target, CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err());
    }
    let mut invalid: Vec<Vec<u8>> = [b"".as_slice(), b"/empty", b"empty/", b"empty//child",
        b".", b"..", b"empty/../other", b"empty/./child", b"empty\0child"]
        .into_iter().map(<[u8]>::to_vec).collect();
    invalid.push(vec![b'x'; MAX_CATALOG_DIRECTORY_BYTES + 1]);
    invalid.push(vec!["a"; 32].join("/").into_bytes());
    for target in invalid {
        // Invalid raw input is rejected before attempting even root metadata.
        assert!(matches!(CatalogSession::open_directory_with_policy(owner(80504),
            &root.join("absent-root"), &target, CatalogDiscoveryPolicy::IncludeExcluded, || false),
            Err(CatalogSessionError::InvalidLimits)), "{target:?}");
    }
    let mut workspace = CatalogSession::open(owner(80505), &root, || false).unwrap();
    assert_eq!(workspace.directory(), None);
    scope(&json(&workspace.page(1, || false).unwrap()), None);
    scope(&json(&workspace.info(|| false).unwrap()), None);
}

#[test]
fn directory_admission_accepts_exact_byte_bound_and_keeps_depth_relative_to_project_root() {
    let root = new_root();
    let mut components = vec!["p".repeat(240); 8]; components.push("q".repeat(120));
    let directory = components.join("/"); assert_eq!(directory.len(), MAX_CATALOG_DIRECTORY_BYTES);
    fs::create_dir_all(root.join(&directory)).unwrap();
    let mut exact = CatalogSession::open_directory_with_policy(owner(80601), &root,
        directory.as_bytes(), CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let (rows, complete) = drain(&mut exact, directory.as_bytes());
    assert!(rows.is_empty()); assert!(complete.get("discovery_complete").flag());

    let directory = vec!["d"; 31].join("/");
    write(&root, format!("{directory}/direct.rs"), ORIGINAL);
    write(&root, format!("{directory}/too-deep/withheld.rs"), ORIGINAL);
    let mut deep = CatalogSession::open_directory_with_policy(owner(80602), &root,
        directory.as_bytes(), CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let (rows, partial) = drain(&mut deep, directory.as_bytes());
    assert_eq!(rows.len(), 1); assert!(rows[0].path.ends_with(b"/direct.rs"));
    assert_eq!(partial.get("state").text(), "partial");
    assert_eq!(partial.get("discovery").get("depth_limited").number(), 1);
    assert!(!partial.get("discovery_complete").flag());
}

#[test]
fn canceled_scoped_advance_replays_the_same_page_and_stop_keeps_the_fixed_target() {
    let root = new_root(); let directory = b"selected"; linked_files(&root.join("selected"), 800);
    let mut session = CatalogSession::open_directory_with_policy(owner(80701), &root,
        directory, CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    let first = json(&session.page(1, || false).unwrap());
    assert!(matches!(session.advance(1, || true), Err(CatalogSessionError::Canceled)));
    assert_eq!(json(&session.info(|| false).unwrap()).get("latest_page").number(), 1);
    let mut polls = 0;
    assert!(matches!(session.advance(1, || { polls += 1; polls == 3 }), Err(CatalogSessionError::Canceled)));
    let info = json(&session.info(|| false).unwrap()); scope(&info, Some(directory));
    assert_eq!(info.get("latest_page").number(), 2);
    let recovered = json(&session.advance(1, || false).unwrap()); scope(&recovered, Some(directory));
    assert_eq!(recovered.get("page_id").number(), 2);
    assert_eq!(recovered, json(&session.advance(1, || false).unwrap()));
    let paused = json(&session.stop(|| false).unwrap()); scope(&paused, Some(directory));
    assert!(paused.get("paused").flag()); assert!(paused.get("more").flag());
    let replay = json(&session.page(1, || false).unwrap()); scope(&replay, Some(directory));
    assert_eq!(replay.get("files"), first.get("files"));
    let resumed = json(&session.advance(2, || false).unwrap()); scope(&resumed, Some(directory));
    assert_eq!(resumed.get("page_id").number(), 3); assert!(!resumed.get("paused").flag());
}

#[test]
fn explicit_document_link_can_leave_the_discovery_folder_within_the_original_project_grant() {
    let root = new_root(); let directory = b"docs";
    let original = b"# Selected\n\n[outside this folder](../src/target.md)\n";
    let target = b"# Still inside the project\n";
    write(&root, "docs/README.md", original); write(&root, "src/target.md", target);
    let catalog_owner = owner(80801);
    let mut session = CatalogSession::open_directory_with_policy(catalog_owner, &root,
        directory, CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let (rows, page) = drain(&mut session, directory);
    assert_eq!(rows.len(), 1); assert_eq!(rows[0].path, b"docs/README.md");
    let (mut origin, receipt) = session.open_reader(owner(80802), rows[0].page,
        file(catalog_owner, rows[0].file), 4096, || false).unwrap();
    scope(&json(&receipt), Some(directory));
    origin.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap();
    origin.prepare_document_links(1, 16, || false).unwrap();
    let selection = CatalogDocumentLinkSelection { source_page: rows[0].page,
        source_file: file(catalog_owner, rows[0].file), document_generation: 1, link_id: 1 };
    let (mut linked, receipt) = session.open_document_link(&origin, owner(80803),
        selection, 4096, || false).unwrap();
    let receipt = json(&receipt); scope(&receipt, Some(directory));
    assert_eq!(raw(receipt.get("target_file").get("path")), b"src/target.md");
    assert_eq!(receipt.get("catalogued_files"), page.get("catalogued_files"));
    assert_eq!(origin.capture().bytes(), original);
    drop(session);
    assert_eq!(json(&linked.copy_range(0, target.len() as u64, || false).unwrap())
        .get("original_hex").text(), hex(target));
}
