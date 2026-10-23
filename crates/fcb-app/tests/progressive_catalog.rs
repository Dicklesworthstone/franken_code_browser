#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
use support::{parse, Json};
use std::{ffi::OsString, fs, os::unix::{ffi::OsStringExt, fs::symlink}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, FileId};
use fcb::source::SourceError;
use fcb_app::{AppError, EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, catalog_session::{CatalogSession, CatalogSessionError,
    MAX_CATALOG_PAGE_ENTRIES, MAX_CATALOG_PAGE_PATH_BYTES, MAX_CATALOG_RETAINED_PAGES,
    MAX_CATALOG_RETAINED_FILES, MAX_CATALOG_RETAINED_PATH_BYTES, MAX_CATALOG_RESPONSE_BYTES}};

const ORIGINAL: &[u8] = b"fn beyond_twenty_thousand() { /* exact capture */ }\n";
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn file(owner: ArenaOwnerId, id: u64) -> FileId { FileId::new(owner, id).unwrap() }
fn json(response: &HostResponse) -> Json {
    assert!(response.as_str().len() <= MAX_CATALOG_RESPONSE_BYTES);
    parse(response.as_str().as_bytes()).unwrap()
}
fn new_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-progressive-catalog-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    root // Retain fixtures for diagnosis; no destructive cleanup.
}
fn linked_files(root: &PathBuf, count: usize) {
    // Real nonempty regular files with distinct namespace identities. Hard
    // links avoid spending 80 MiB of filesystem blocks on identical fixture
    // bytes; the production walker observes every actual directory entry.
    let seed = root.join("file-00000.rs"); fs::write(&seed, ORIGINAL).unwrap();
    for i in 1..count { fs::hard_link(&seed, root.join(format!("file-{i:05}.rs"))).unwrap(); }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn raw_path(row: &Json) -> PathBuf {
    let encoded = row.get("path").get("hex").text();
    let bytes = (0..encoded.len()).step_by(2).map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).unwrap()).collect();
    PathBuf::from(OsString::from_vec(bytes))
}
fn limits(page: &Json) {
    assert!(page.get("retained_pages").number() <= MAX_CATALOG_RETAINED_PAGES as u64);
    assert!(page.get("retained_files").number() <= MAX_CATALOG_RETAINED_FILES as u64);
    assert!(page.get("retained_path_bytes").number() <= MAX_CATALOG_RETAINED_PATH_BYTES as u64);
    assert!(page.get("files").array().len() <= MAX_CATALOG_PAGE_ENTRIES);
    let paths: usize = page.get("files").array().iter().map(|row| row.get("path").get("hex").text().len() / 2).sum();
    assert!(paths <= MAX_CATALOG_PAGE_PATH_BYTES);
    assert_eq!(page.get("discovery_source_bytes_read").number(), 0);
    assert_eq!(page.get("discovery_source_read_calls").number(), 0);
    assert_eq!(page.get("ordering").text(), "provisional-discovery-order");
    assert_eq!(page.get("paused").flag(), page.get("state").text() == "paused");
    assert_eq!(page.get("more").flag(), matches!(page.get("state").text(), "discovering" | "paused"));
    assert_eq!(page.get("discovery_complete").flag(), page.get("state").text() == "complete");
}

#[test]
fn real_nonempty_tree_exceeds_twenty_thousand_without_growing_retained_metadata() {
    let root = new_root(); let count = 20_513; linked_files(&root, count);
    let catalog_owner = owner(71101);
    let mut session = CatalogSession::open(catalog_owner, &root, || false).unwrap();
    let first = session.page(1, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_PARTIAL);
    let mut page = json(&first);
    assert!(page.get("catalogued_files").number() <= MAX_CATALOG_PAGE_ENTRIES as u64);
    assert!(page.get("more").flag());
    let mut next_file = 1;
    let mut selected = None;
    loop {
        limits(&page);
        let page_id = page.get("page_id").number();
        for row in page.get("files").array() {
            assert_eq!(row.get("file_id").number(), next_file);
            if next_file > 20_000 { selected = Some((page_id, next_file, raw_path(row))); }
            next_file += 1;
        }
        assert_eq!(page.get("catalogued_files").number(), next_file - 1);
        if !page.get("more").flag() { break; }
        page = json(&session.advance(page_id, || false).unwrap());
    }
    assert_eq!(next_file - 1, count as u64);
    assert_eq!(page.get("state").text(), "complete");
    assert!(page.get("first_retained_page").number() > 1);
    assert!(matches!(session.page(1, || false), Err(CatalogSessionError::StalePage)));
    let first_retained = page.get("first_retained_page").number();
    assert!(matches!(session.advance(first_retained - 1, || false), Err(CatalogSessionError::StalePage)));
    let latest = page.get("latest_page").number();
    assert!(matches!(session.advance(latest, || false), Err(CatalogSessionError::NoMorePages)));
    assert_eq!(json(&session.stop(|| false).unwrap()).get("state").text(), "complete");
    let (page_id, file_id, path) = selected.unwrap();
    let (mut reader, receipt) = session.open_reader(owner(71102), page_id, file(catalog_owner, file_id), 4096, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("file_id").number(), file_id);
    assert_eq!(receipt.get("source_bytes_read").number(), ORIGINAL.len() as u64);
    assert!(receipt.get("source_read_calls").number() > 0);
    assert_eq!(receipt.get("source_observation").text(), "new-capture-after-metadata-selection");
    fs::write(root.join(path), b"changed live source, not the retained observation").unwrap();
    session.grant().revoke(); drop(session);
    assert_eq!(json(&reader.copy_range(0, ORIGINAL.len() as u64, || false).unwrap()).get("original_hex").text(), hex(ORIGINAL));
}

#[test]
fn preadvance_and_postadvance_cancellation_recover_exactly_once_and_stop_resumes() {
    let root = new_root(); linked_files(&root, 800);
    let mut session = CatalogSession::open(owner(71201), &root, || false).unwrap();
    let first = json(&session.page(1, || false).unwrap());
    assert!(matches!(session.advance(1, || true), Err(CatalogSessionError::Canceled)));
    assert_eq!(json(&session.info(|| false).unwrap()).get("latest_page").number(), 1);
    let mut polls = 0;
    assert!(matches!(session.advance(1, || { polls += 1; polls == 3 }), Err(CatalogSessionError::Canceled)));
    // Poll 3 occurs when page() starts, after the walker consumed and retained
    // its batch. The lost reply cannot cause a second cursor advance on retry.
    assert_eq!(json(&session.info(|| false).unwrap()).get("latest_page").number(), 2);
    let recovered = json(&session.advance(1, || false).unwrap());
    assert_eq!(recovered.get("page_id").number(), 2);
    assert_eq!(recovered.get("files"), json(&session.advance(1, || false).unwrap()).get("files"));
    assert_eq!(json(&session.info(|| false).unwrap()).get("latest_page").number(), 2);
    assert_eq!(first.get("files"), json(&session.page(1, || false).unwrap()).get("files"));
    let paused = json(&session.stop(|| false).unwrap());
    assert!(paused.get("paused").flag()); assert!(paused.get("more").flag());
    let replay = json(&session.page(1, || false).unwrap());
    assert_eq!(replay.get("state").text(), "paused");
    assert_eq!(replay.get("latest_page").number(), 2);
    let continued = json(&session.advance(2, || false).unwrap());
    assert_eq!(continued.get("page_id").number(), 3); assert!(!continued.get("paused").flag());
    assert!(matches!(session.advance(0, || false), Err(CatalogSessionError::StalePage)));
    assert!(matches!(session.advance(10, || false), Err(CatalogSessionError::StalePage)));
}

#[test]
fn every_advance_delivery_checkpoint_preserves_a_replayable_successor() {
    let root = new_root(); linked_files(&root, 600);
    let mut probe = CatalogSession::open(owner(71250), &root, || false).unwrap();
    let mut checkpoints = 0;
    probe.advance(1, || { checkpoints += 1; false }).unwrap();
    assert!(checkpoints > MAX_CATALOG_PAGE_ENTRIES);
    drop(probe);
    for cancel_at in 1..=checkpoints {
        let mut session = CatalogSession::open(owner(71251), &root, || false).unwrap();
        let mut polls = 0;
        assert!(matches!(session.advance(1, || { polls += 1; polls == cancel_at }), Err(CatalogSessionError::Canceled)));
        let recovered = json(&session.advance(1, || false).unwrap());
        assert_eq!(recovered.get("page_id").number(), 2);
        assert_eq!(recovered.get("latest_page").number(), 2);
        assert_eq!(recovered.get("files"), json(&session.advance(1, || false).unwrap()).get("files"));
        let mut page = recovered;
        while page.get("more").flag() {
            page = json(&session.advance(page.get("page_id").number(), || false).unwrap());
        }
        assert_eq!(page.get("catalogued_files").number(), 600, "checkpoint {cancel_at} lost consumed metadata");
        assert!(page.get("discovery_complete").flag());
    }
}

#[test]
fn empty_progress_page_is_not_a_completed_empty_workspace() {
    let root = new_root();
    for i in 0..300 { fs::create_dir(root.join(format!("dir-{i:03}"))).unwrap(); }
    fs::write(root.join("dir-299/visible.rs"), ORIGINAL).unwrap();
    let mut session = CatalogSession::open(owner(71301), &root, || false).unwrap();
    let mut page = json(&session.page(1, || false).unwrap());
    assert!(page.get("files").array().is_empty()); assert!(page.get("more").flag());
    assert!(!page.get("discovery_complete").flag());
    while page.get("more").flag() {
        page = json(&session.advance(page.get("page_id").number(), || false).unwrap());
    }
    assert_eq!(page.get("catalogued_files").number(), 1);
    assert_eq!(page.get("state").text(), "complete");
    let empty = new_root();
    let mut empty = CatalogSession::open(owner(71302), &empty, || false).unwrap();
    let reply = empty.page(1, || false).unwrap(); assert_eq!(reply.exit_code(), EXIT_OK);
    assert!(json(&reply).get("files").array().is_empty());
    assert!(json(&reply).get("discovery_complete").flag());
}

#[test]
fn raw_names_utf16_and_changed_observed_length_open_exact_independent_readers() {
    let root = new_root();
    let names: [&[u8]; 4] = [b"raw-\xff\\:.rs", b"line\n.rs", "caf\u{e9}.rs".as_bytes(), "cafe\u{301}.rs".as_bytes()];
    for name in names { fs::write(root.join(OsString::from_vec(name.to_vec())), b"x").unwrap(); }
    let catalog_owner = owner(71401);
    let mut session = CatalogSession::open(catalog_owner, &root, || false).unwrap();
    let page = json(&session.page(1, || false).unwrap());
    assert_eq!(page.get("files").array().len(), names.len());
    for (i, row) in page.get("files").array().iter().enumerate() {
        let big_endian = i % 2 != 0;
        let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
        for unit in "\u{1f980} exact\r\n\u{feff}capture\0".encode_utf16() {
            bytes.extend_from_slice(&if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
        }
        let path = raw_path(row); fs::write(root.join(&path), &bytes).unwrap();
        assert_eq!(row.get("observed_bytes").number(), 1);
        let (mut reader, response) = session.open_reader(owner(71410 + i as u64), 1,
            file(catalog_owner, row.get("file_id").number()), 4096, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("path"), row.get("path"));
        let info = receipt.get("reader");
        assert_eq!(info.get("path"), row.get("path"));
        assert_eq!(info.get("capture_origin").text(), "host-supplied");
        assert_eq!(info.get("captured_bytes").number(), bytes.len() as u64);
        assert_eq!(info.get("initial_source_bytes_read").number(), 0);
        assert_eq!(info.get("initial_read_calls").number(), 0);
        for key in ["accepted_query_generation", "accepted_outline_generation", "accepted_document_generation"] {
            assert_eq!(info.get(key), &Json::Null);
        }
        assert_eq!(receipt.get("window").get("source_revision"), info.get("source_revision"));
        assert_eq!(receipt.get("window").get("owner"), info.get("owner"));
        assert_eq!(info.get("capture_sha256").text(), fcb::store::Sha256::digest(&bytes).to_hex());
        assert_eq!(json(&reader.copy_range(0, bytes.len() as u64, || false).unwrap()).get("original_hex").text(), hex(&bytes));
    }
}

#[test]
fn long_raw_paths_obey_the_walker_overshoot_and_retained_page_budgets() {
    let root = new_root();
    let mut nested = root.clone();
    for _ in 0..3 { nested.push("d".repeat(200)); fs::create_dir(&nested).unwrap(); }
    let seed = nested.join(format!("{}-000.rs", "f".repeat(190)));
    fs::write(&seed, ORIGINAL).unwrap();
    for i in 1..400 { fs::hard_link(&seed, nested.join(format!("{}-{i:03}.rs", "f".repeat(190)))).unwrap(); }
    let mut session = CatalogSession::open(owner(71450), &root, || false).unwrap();
    let mut page = json(&session.page(1, || false).unwrap());
    let mut saw_threshold_overshoot = false;
    loop {
        limits(&page);
        let paths: usize = page.get("files").array().iter().map(|row| row.get("path").get("hex").text().len() / 2).sum();
        saw_threshold_overshoot |= paths > 64 * 1024;
        if !page.get("more").flag() { break; }
        page = json(&session.advance(page.get("page_id").number(), || false).unwrap());
    }
    assert!(saw_threshold_overshoot);
    assert_eq!(page.get("catalogued_files").number(), 400);
    assert!(page.get("discovery_complete").flag());
}

#[test]
fn stale_foreign_missing_and_aliased_rows_refuse_before_even_root_metadata_access() {
    let root = new_root(); linked_files(&root, 1400);
    let catalog_owner = owner(71501);
    let mut session = CatalogSession::open(catalog_owner, &root, || false).unwrap();
    let first = json(&session.page(1, || false).unwrap());
    let old = first.get("files").array()[0].get("file_id").number();
    let mut current = first;
    while current.get("latest_page").number() < 5 {
        current = json(&session.advance(current.get("latest_page").number(), || false).unwrap());
    }
    let current_page = current.get("page_id").number();
    let valid = current.get("files").array()[0].get("file_id").number();
    let moved = root.with_extension("retired"); fs::rename(&root, &moved).unwrap();
    assert!(matches!(session.open_reader(owner(71502), 1, file(catalog_owner, old), 4096, || false), Err(CatalogSessionError::StalePage)));
    assert!(matches!(session.open_reader(owner(71502), current_page, file(owner(71503), valid), 4096, || false), Err(CatalogSessionError::ForeignOwner)));
    assert!(matches!(session.open_reader(catalog_owner, current_page, file(catalog_owner, valid), 4096, || false), Err(CatalogSessionError::ForeignOwner)));
    assert!(matches!(session.open_reader(owner(71502), current_page, file(catalog_owner, old), 4096, || false), Err(CatalogSessionError::MissingFile)));
    assert!(matches!(session.open_reader(owner(71502), current_page, file(catalog_owner, valid), 4096, || false), Err(CatalogSessionError::App(AppError::SourceChanged))));
}

#[test]
fn replaced_root_and_root_symlink_cannot_rebind_a_live_catalog() {
    for as_symlink in [false, true] {
        let root = new_root(); fs::write(root.join("file.rs"), ORIGINAL).unwrap();
        let mut session = CatalogSession::open(owner(71601), &root, || false).unwrap();
        let old = root.with_extension("original"); fs::rename(&root, &old).unwrap();
        if as_symlink { symlink(&old, &root).unwrap(); }
        else { fs::create_dir(&root).unwrap(); fs::write(root.join("file.rs"), b"imposter").unwrap(); }
        assert!(matches!(session.info(|| false), Err(CatalogSessionError::App(AppError::SourceChanged))));
        assert!(matches!(session.page(1, || false), Err(CatalogSessionError::App(AppError::SourceChanged))));
        assert!(matches!(session.open_reader(owner(71602), 1, file(owner(71601), 1), 4096, || false), Err(CatalogSessionError::App(AppError::SourceChanged))));
    }
}

#[test]
fn root_revocation_and_relative_symlink_swap_deny_new_capture_but_keep_prior_reader() {
    let root = new_root(); fs::create_dir(root.join("src")).unwrap(); fs::write(root.join("src/file.rs"), ORIGINAL).unwrap();
    let catalog_owner = owner(71701);
    let mut session = CatalogSession::open(catalog_owner, &root, || false).unwrap();
    let page = json(&session.page(1, || false).unwrap());
    let id = page.get("files").array()[0].get("file_id").number();
    let (mut reader, _) = session.open_reader(owner(71702), 1, file(catalog_owner, id), 4096, || false).unwrap();
    fs::rename(root.join("src"), root.join("moved")).unwrap();
    symlink(root.join("moved"), root.join("src")).unwrap();
    assert!(matches!(session.open_reader(owner(71703), 1, file(catalog_owner, id), 4096, || false), Err(CatalogSessionError::Source(SourceError::SymlinkForbidden))));
    session.grant().revoke();
    assert!(matches!(session.info(|| false), Err(CatalogSessionError::Source(SourceError::GrantRevoked))));
    assert_eq!(json(&reader.copy_range(0, ORIGINAL.len() as u64, || false).unwrap()).get("original_hex").text(), hex(ORIGINAL));
}

#[test]
fn metadata_policy_reads_no_rule_files_and_depth_or_path_limits_stay_partial() {
    let root = new_root();
    fs::write(root.join(".gitignore"), b"visible.rs\n").unwrap();
    fs::write(root.join("visible.rs"), ORIGINAL).unwrap();
    fs::create_dir(root.join(".fcb-cache")).unwrap(); fs::write(root.join(".fcb-cache/private"), ORIGINAL).unwrap();
    fs::create_dir(root.join(".git")).unwrap(); fs::write(root.join(".git/objects"), ORIGINAL).unwrap();
    let mut session = CatalogSession::open(owner(71801), &root, || false).unwrap();
    let page = json(&session.page(1, || false).unwrap());
    assert!(page.get("files").array().iter().any(|row| raw_path(row) == PathBuf::from("visible.rs")));
    assert_eq!(page.get("policy").text(), "product-defaults/no-rule-files-v1");
    assert!(page.get("discovery").get("excluded_entries").number() >= 2); limits(&page);
    for long_paths in [false, true] {
        let root = new_root(); fs::write(root.join("reachable.rs"), ORIGINAL).unwrap();
        let mut nested = root.clone();
        let depth = if long_paths { 10 } else { 40 };
        for _ in 0..depth {
            nested.push(if long_paths { "x".repeat(240) } else { "directory".to_owned() }); fs::create_dir(&nested).unwrap();
        }
        fs::write(nested.join("unseen.rs"), ORIGINAL).unwrap();
        let mut session = CatalogSession::open(owner(71810 + u64::from(long_paths)), &root, || false).unwrap();
        let mut page = json(&session.page(1, || false).unwrap());
        while page.get("more").flag() { page = json(&session.advance(page.get("page_id").number(), || false).unwrap()); }
        assert_eq!(page.get("state").text(), "partial"); assert!(!page.get("discovery_complete").flag());
        assert!(page.get("discovery").get(if long_paths { "path_limited" } else { "depth_limited" }).number() > 0);
        assert_eq!(page.get("catalogued_files").number(), 1);
    }
}

#[test]
fn cancellation_never_hands_off_a_partial_reader_and_later_retry_still_opens() {
    let root = new_root(); fs::write(root.join("file.rs"), ORIGINAL).unwrap();
    let catalog_owner = owner(71901);
    let mut session = CatalogSession::open(catalog_owner, &root, || false).unwrap();
    let mut polls = 0;
    let (mut retained, _) = session.open_reader(owner(71902), 1, file(catalog_owner, 1), 4096,
        || { polls += 1; false }).unwrap();
    for cancel_at in [1, 2, polls / 2, polls] {
        let mut count = 0;
        let error = match session.open_reader(owner(71910 + cancel_at as u64), 1, file(catalog_owner, 1), 4096,
            || { count += 1; count == cancel_at }) { Ok(_) => panic!("canceled reader was published"), Err(error) => error };
        assert!(error.is_canceled(), "{error}");
        assert_eq!(json(&retained.copy_range(0, ORIGINAL.len() as u64, || false).unwrap()).get("original_hex").text(), hex(ORIGINAL));
    }
    session.open_reader(owner(71903), 1, file(catalog_owner, 1), 4096, || false).unwrap();
}
