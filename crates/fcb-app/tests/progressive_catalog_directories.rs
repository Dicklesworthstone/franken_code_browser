#![forbid(unsafe_code)]
#![cfg(unix)]

//! Clickable folder observations share the progressive file catalog's retained
//! pages and raw-path bounds. Navigation uses a newly validated scoped session;
//! a directory row is metadata, never file-reader or historical-path authority.
mod support;
use fcb::{ArenaOwnerId, FileId};
use fcb_app::host::{
    HostResponse,
    catalog_session::{
        CatalogDiscoveryPolicy, CatalogSession, CatalogSessionError, MAX_CATALOG_PAGE_ENTRIES,
        MAX_CATALOG_PAGE_PATH_BYTES, MAX_CATALOG_RESPONSE_BYTES, MAX_CATALOG_RETAINED_FILES,
        MAX_CATALOG_RETAINED_PAGES, MAX_CATALOG_RETAINED_PATH_BYTES,
    },
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{FileTypeExt, symlink},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use support::{Json, parse};

const SOURCE: &[u8] = b"fn child_folder_source() {}\n";
fn owner(value: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(value).unwrap()
}
fn json(response: &HostResponse) -> Json {
    assert!(response.as_str().len() <= MAX_CATALOG_RESPONSE_BYTES);
    parse(response.as_str().as_bytes()).unwrap()
}
fn new_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "fcb-folder-rows-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path // Retain source fixtures for diagnosis.
}
fn native(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}
fn raw(row: &Json) -> Vec<u8> {
    let text = row.get("path").get("hex").text();
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn write(root: &Path, path: &[u8], bytes: &[u8]) {
    let path = root.join(native(path));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn open(
    root: &Path,
    id: u64,
    directory: Option<&[u8]>,
    policy: CatalogDiscoveryPolicy,
) -> CatalogSession {
    match directory {
        Some(directory) => {
            CatalogSession::open_directory_with_policy(owner(id), root, directory, policy, || false)
        }
        None => CatalogSession::open_with_policy(owner(id), root, policy, || false),
    }
    .unwrap()
}
fn bounds(page: &Json, parent: &[u8]) {
    assert_eq!(
        page.get("directory_rows_policy").text(),
        "direct-child-directories-v1"
    );
    assert_eq!(page.get("path_basis").text(), "project-root");
    assert_eq!(
        page.get("discovery_scope").text(),
        if parent.is_empty() {
            "workspace"
        } else {
            "directory"
        }
    );
    let files = page.get("files").array();
    let directories = page.get("directories").array();
    assert!(files.len() + directories.len() <= MAX_CATALOG_PAGE_ENTRIES);
    assert!(page.get("retained_pages").number() <= MAX_CATALOG_RETAINED_PAGES as u64);
    assert!(
        page.get("retained_files").number() + page.get("retained_directories").number()
            <= MAX_CATALOG_RETAINED_FILES as u64
    );
    assert!(
        page.get("retained_directories").number() <= page.get("catalogued_directories").number()
    );
    assert!(
        page.get("catalogued_directories").number()
            <= page.get("discovery").get("directories").number()
    );
    let path_bytes: usize = files
        .iter()
        .chain(directories)
        .map(|row| raw(row).len())
        .sum();
    assert!(path_bytes <= MAX_CATALOG_PAGE_PATH_BYTES);
    assert!(path_bytes as u64 <= page.get("retained_path_bytes").number());
    assert!(page.get("retained_path_bytes").number() <= MAX_CATALOG_RETAINED_PATH_BYTES as u64);
    let mut previous = 0;
    for row in directories {
        let id = row.get("directory_id").number();
        assert!(id > previous && id <= page.get("catalogued_directories").number());
        previous = id;
        let path = raw(row);
        let actual_parent = path
            .iter()
            .rposition(|byte| *byte == b'/')
            .map_or(&[][..], |at| &path[..at]);
        assert_eq!(
            actual_parent, parent,
            "only exact direct children belong in the folder list"
        );
        assert_eq!(
            row.get("within_browse_limits").flag(),
            path.split(|byte| *byte == b'/').count() <= 31
        );
    }
    assert_eq!(page.get("discovery_source_bytes_read").number(), 0);
    assert_eq!(page.get("discovery_source_read_calls").number(), 0);
}
fn drain(
    session: &mut CatalogSession,
    parent: &[u8],
) -> (Vec<(u64, Json)>, Vec<(u64, Json)>, Json) {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut page = json(&session.page(1, || false).unwrap());
    loop {
        bounds(&page, parent);
        let id = page.get("page_id").number();
        files.extend(
            page.get("files")
                .array()
                .iter()
                .cloned()
                .map(|row| (id, row)),
        );
        directories.extend(
            page.get("directories")
                .array()
                .iter()
                .cloned()
                .map(|row| (id, row)),
        );
        if !page.get("more").flag() {
            return (files, directories, page);
        }
        page = json(&session.advance(id, || false).unwrap());
    }
}

#[test]
fn complete_folder_only_page_exposes_a_real_empty_child() {
    let root = new_root();
    fs::create_dir(root.join("child")).unwrap();
    let mut session = open(&root, 80900, None, CatalogDiscoveryPolicy::IncludeExcluded);
    let page = json(&session.page(1, || false).unwrap());
    bounds(&page, b"");
    assert!(page.get("files").array().is_empty());
    assert!(page.get("discovery_complete").flag());
    assert_eq!(page.get("directories").array().len(), 1);
    let child = &page.get("directories").array()[0];
    assert_eq!(raw(child), b"child");
    assert!(child.get("within_browse_limits").flag());
    let mut child = open(
        &root,
        80899,
        Some(&raw(child)),
        CatalogDiscoveryPolicy::IncludeExcluded,
    );
    let child_page = json(&child.page(1, || false).unwrap());
    bounds(&child_page, b"child");
    assert!(
        child_page.get("files").array().is_empty()
            && child_page.get("directories").array().is_empty()
    );
    assert!(child_page.get("discovery_complete").flag());
}

#[test]
fn direct_child_rows_open_empty_nonempty_and_raw_folders_without_changing_file_identity() {
    let root = new_root();
    fs::create_dir(root.join("empty")).unwrap();
    write(&root, b"root.rs", SOURCE);
    write(&root, b"code/item.rs", SOURCE);
    write(&root, b"code/nested/late.rs", SOURCE);
    write(&root, b"code-other/outside.rs", SOURCE);
    write(&root, b"raw-\xff\\:/exact.rs", SOURCE);
    let mut workspace = open(&root, 80901, None, CatalogDiscoveryPolicy::IncludeExcluded);
    let (files, directories, terminal) = drain(&mut workspace, b"");
    assert!(terminal.get("discovery_complete").flag());
    let names: BTreeSet<_> = directories.iter().map(|(_, row)| raw(row)).collect();
    assert_eq!(
        names,
        [
            b"code".to_vec(),
            b"code-other".to_vec(),
            b"empty".to_vec(),
            b"raw-\xff\\:".to_vec()
        ]
        .into_iter()
        .collect()
    );
    for (index, (_, row)) in files.iter().enumerate() {
        assert_eq!(
            row.get("file_id").number(),
            index as u64 + 1,
            "folder IDs never consume FileIds"
        );
    }
    for (index, (_, row)) in directories.iter().enumerate() {
        assert_eq!(row.get("directory_id").number(), index as u64 + 1);
    }
    for (index, directory) in [b"empty".as_slice(), b"code", b"raw-\xff\\:"]
        .into_iter()
        .enumerate()
    {
        let mut scoped = open(
            &root,
            80910 + index as u64,
            Some(directory),
            CatalogDiscoveryPolicy::IncludeExcluded,
        );
        let (files, folders, terminal) = drain(&mut scoped, directory);
        assert!(terminal.get("discovery_complete").flag());
        if directory == b"empty" {
            assert!(files.is_empty() && folders.is_empty());
            continue;
        }
        if directory == b"code" {
            assert_eq!(folders.len(), 1);
            assert_eq!(raw(&folders[0].1), b"code/nested");
            assert_eq!(files.len(), 2);
            assert!(
                !files
                    .iter()
                    .any(|(_, row)| raw(row).starts_with(b"code-other/"))
            );
        } else {
            assert!(folders.is_empty());
            assert_eq!(files.len(), 1);
        }
        let (page, row) = &files[0];
        let (reader, receipt) = scoped
            .open_reader(
                owner(80920 + index as u64),
                *page,
                FileId::new(scoped.owner(), row.get("file_id").number()).unwrap(),
                4096,
                || false,
            )
            .unwrap();
        assert_eq!(json(&receipt).get("path"), row.get("path"));
        assert_eq!(reader.capture().bytes(), SOURCE);
    }
}

#[test]
fn folder_only_pages_replay_canceled_advances_and_evict_without_file_reader_authority() {
    let root = new_root();
    for id in 0..1300 {
        fs::create_dir(root.join(format!("folder-{id:04}"))).unwrap();
    }
    let mut session = open(&root, 80940, None, CatalogDiscoveryPolicy::IncludeExcluded);
    let first = json(&session.page(1, || false).unwrap());
    bounds(&first, b"");
    assert!(first.get("files").array().is_empty());
    assert!(!first.get("directories").array().is_empty());
    assert!(first.get("more").flag());
    assert!(matches!(
        session.open_reader(
            owner(80941),
            1,
            FileId::new(session.owner(), 1).unwrap(),
            4096,
            || false
        ),
        Err(CatalogSessionError::MissingFile)
    ));
    assert!(matches!(
        session.advance(1, || true),
        Err(CatalogSessionError::Canceled)
    ));
    assert_eq!(
        json(&session.info(|| false).unwrap())
            .get("latest_page")
            .number(),
        1
    );
    let mut polls = 0;
    assert!(matches!(
        session.advance(1, || {
            polls += 1;
            polls == 3
        }),
        Err(CatalogSessionError::Canceled)
    ));
    let mut page = json(&session.advance(1, || false).unwrap());
    assert_eq!(page.get("page_id").number(), 2);
    assert_eq!(
        page.get("directories"),
        json(&session.advance(1, || false).unwrap()).get("directories")
    );
    assert_eq!(
        first.get("directories"),
        json(&session.page(1, || false).unwrap()).get("directories")
    );
    assert!(json(&session.stop(|| false).unwrap()).get("paused").flag());
    let mut seen = first.get("directories").array().len() as u64;
    loop {
        bounds(&page, b"");
        seen += page.get("directories").array().len() as u64;
        if !page.get("more").flag() {
            break;
        }
        page = json(
            &session
                .advance(page.get("page_id").number(), || false)
                .unwrap(),
        );
    }
    assert_eq!(seen, 1300);
    assert_eq!(page.get("catalogued_directories").number(), 1300);
    assert_eq!(page.get("catalogued_files").number(), 0);
    assert!(page.get("discovery_complete").flag());
    assert!(page.get("first_retained_page").number() > 1);
    assert!(matches!(
        session.page(1, || false),
        Err(CatalogSessionError::StalePage)
    ));
    assert!(matches!(
        session.advance(1, || false),
        Err(CatalogSessionError::StalePage)
    ));
    session.grant().revoke();
    assert!(session.info(|| false).is_err());
    assert!(
        session
            .page(page.get("page_id").number(), || false)
            .is_err()
    );
}

#[test]
fn mixed_discovery_beyond_twenty_thousand_keeps_one_shared_retained_metadata_window() {
    let root = new_root();
    let seed = root.join("file-00000.rs");
    fs::write(&seed, SOURCE).unwrap();
    for id in 1..20_001 {
        fs::hard_link(&seed, root.join(format!("file-{id:05}.rs"))).unwrap();
    }
    for id in 0..513 {
        fs::create_dir(root.join(format!("folder-{id:03}"))).unwrap();
    }
    write(&root, b"folder-512/late.rs", SOURCE);
    let mut session = open(&root, 80950, None, CatalogDiscoveryPolicy::IncludeExcluded);
    let mut page = json(&session.page(1, || false).unwrap());
    let (mut next_file, mut next_directory) = (1, 1);
    let mut late = None;
    loop {
        bounds(&page, b"");
        for row in page.get("files").array() {
            assert_eq!(row.get("file_id").number(), next_file);
            next_file += 1;
            if raw(row) == b"folder-512/late.rs" {
                assert!(row.get("file_id").number() > 20_000);
                let (reader, _) = session
                    .open_reader(
                        owner(80951),
                        page.get("page_id").number(),
                        FileId::new(session.owner(), row.get("file_id").number()).unwrap(),
                        4096,
                        || false,
                    )
                    .unwrap();
                late = Some(reader);
            }
        }
        for row in page.get("directories").array() {
            assert_eq!(row.get("directory_id").number(), next_directory);
            next_directory += 1;
        }
        if !page.get("more").flag() {
            break;
        }
        page = json(
            &session
                .advance(page.get("page_id").number(), || false)
                .unwrap(),
        );
    }
    assert_eq!(next_file - 1, 20_002);
    assert_eq!(next_directory - 1, 513);
    assert_eq!(page.get("catalogued_files").number(), 20_002);
    assert_eq!(page.get("catalogued_directories").number(), 513);
    assert!(page.get("discovery_complete").flag());
    session.grant().revoke();
    drop(session);
    assert_eq!(late.unwrap().capture().bytes(), SOURCE);
}

#[test]
fn files_and_folders_share_path_byte_limits_before_page_entry_limits() {
    let root = new_root();
    let parent = vec!["p".repeat(200); 3].join("/");
    let directory = root.join(&parent);
    fs::create_dir_all(&directory).unwrap();
    for id in 0..130 {
        fs::create_dir(directory.join(format!("{}-{id:03}", "d".repeat(190)))).unwrap();
        fs::write(
            directory.join(format!("{}-{id:03}.rs", "f".repeat(190))),
            SOURCE,
        )
        .unwrap();
    }
    let mut session = open(
        &root,
        80960,
        Some(parent.as_bytes()),
        CatalogDiscoveryPolicy::IncludeExcluded,
    );
    let mut page = json(&session.page(1, || false).unwrap());
    let (mut files, mut folders, mut bounded_by_paths) = (0, 0, false);
    loop {
        bounds(&page, parent.as_bytes());
        let count = page.get("files").array().len() + page.get("directories").array().len();
        if page.get("more").flag() && count > 0 && count < MAX_CATALOG_PAGE_ENTRIES {
            bounded_by_paths = true;
        }
        files += page.get("files").array().len();
        folders += page.get("directories").array().len();
        if !page.get("more").flag() {
            break;
        }
        page = json(
            &session
                .advance(page.get("page_id").number(), || false)
                .unwrap(),
        );
    }
    assert_eq!((files, folders), (130, 130));
    assert!(bounded_by_paths);
    assert!(page.get("discovery_complete").flag());
}

#[test]
fn directory_rows_respect_rules_and_never_promote_symlinks_or_special_objects() {
    let root = new_root();
    fs::create_dir(root.join("kept")).unwrap();
    fs::create_dir(root.join("ignored")).unwrap();
    fs::write(root.join(".gitignore"), b"ignored/\n").unwrap();
    symlink(root.join("kept"), root.join("alias")).unwrap();
    let fifo = root.join("fifo");
    let created = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .output()
        .expect("mkfifo is required to create the special-object fixture");
    assert!(
        created.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo());
    for (id, policy, expected) in [
        (
            80970,
            CatalogDiscoveryPolicy::RepositoryRules,
            vec![b"kept".to_vec()],
        ),
        (
            80971,
            CatalogDiscoveryPolicy::IncludeExcluded,
            vec![b"ignored".to_vec(), b"kept".to_vec()],
        ),
    ] {
        let mut session = open(&root, id, None, policy);
        let (_, directories, terminal) = drain(&mut session, b"");
        let names: BTreeSet<_> = directories.iter().map(|(_, row)| raw(row)).collect();
        assert_eq!(names, expected.into_iter().collect());
        assert_eq!(
            terminal
                .get("discovery")
                .get("symlinks_not_followed")
                .number(),
            1
        );
        assert_eq!(terminal.get("discovery").get("special_objects").number(), 1);
        assert!(!names.contains(b"alias".as_slice()) && !names.contains(b"fifo".as_slice()));
    }
}

#[test]
fn depth_thirty_two_folder_is_visible_but_cannot_invalidate_usable_sibling_files() {
    let root = new_root();
    let parent = vec!["d"; 31].join("/");
    let deepest = format!("{parent}/child");
    fs::create_dir_all(root.join(&deepest)).unwrap();
    write(&root, format!("{parent}/usable.rs").as_bytes(), SOURCE);
    let mut session = open(
        &root,
        80980,
        Some(parent.as_bytes()),
        CatalogDiscoveryPolicy::IncludeExcluded,
    );
    let (files, directories, terminal) = drain(&mut session, parent.as_bytes());
    assert_eq!(directories.len(), 1);
    assert_eq!(raw(&directories[0].1), deepest.as_bytes());
    assert!(!directories[0].1.get("within_browse_limits").flag());
    assert_eq!(files.len(), 1);
    assert_eq!(raw(&files[0].1), format!("{parent}/usable.rs").as_bytes());
    assert_eq!(terminal.get("discovery").get("depth_limited").number(), 1);
    assert!(!terminal.get("discovery_complete").flag());
    assert!(matches!(
        CatalogSession::open_directory_with_policy(
            owner(80981),
            &root,
            deepest.as_bytes(),
            CatalogDiscoveryPolicy::IncludeExcluded,
            || false
        ),
        Err(CatalogSessionError::InvalidLimits)
    ));
    let (page, row) = &files[0];
    let (reader, receipt) = session
        .open_reader(
            owner(80982),
            *page,
            FileId::new(session.owner(), row.get("file_id").number()).unwrap(),
            4096,
            || false,
        )
        .unwrap();
    assert_eq!(reader.capture().bytes(), SOURCE);
    assert_eq!(
        json(&receipt).get("directory_rows_policy").text(),
        "direct-child-directories-v1"
    );
    assert_eq!(
        json(&receipt).get("path").get("hex").text(),
        hex(format!("{parent}/usable.rs").as_bytes())
    );
}
