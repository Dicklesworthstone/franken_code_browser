#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, os::unix::{ffi::OsStringExt, fs::symlink}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteLength, FileId, QueryGeneration};
use fcb::search::{MembershipState, PathEntry, PathIndex, PathIndexLimits, PathSearch,
    PathSearchOptions, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::source::RawPath;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, file_finder_session::{FileFinderSession, FileFinderSessionError,
    FileFinderOptions, PathCase, PathMatchMode, MAX_FILE_FINDER_RESULTS, MAX_FILE_FINDER_BATCHES,
    MAX_FILE_FINDER_RESPONSE_BYTES, MAX_FILE_FINDER_RETAINED_PATH_BYTES}};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn file(owner: ArenaOwnerId, value: u64) -> FileId { FileId::new(owner, value).unwrap() }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-progressive-finder-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path // Retain diagnostics; never destructive cleanup.
}
fn linked_files(root: &Path, count: usize) {
    let seed = root.join("noise-00000.rs"); fs::write(&seed, b"source payload is not a path index\n").unwrap();
    for i in 1..count { fs::hard_link(&seed, root.join(format!("noise-{i:05}.rs"))).unwrap(); }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn raw(row: &Json) -> Vec<u8> {
    let text = row.get("path").get("hex").text();
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}
fn json(response: &HostResponse) -> Json {
    assert!(response.as_str().len() <= MAX_FILE_FINDER_RESPONSE_BYTES);
    parse(response.as_str().as_bytes()).unwrap()
}
fn start(root: &Path, id: u64, needle: &[u8], options: FileFinderOptions) -> FileFinderSession {
    FileFinderSession::open(owner(id), root, 1, needle, options, || false).unwrap()
}
fn invariants(page: &Json) {
    let hits = page.get("hits").array();
    let n = |key| page.get(key).number();
    assert_eq!(n("revision"), n("processed_batches") + 1);
    assert!(n("searched_files") <= n("catalogued_files"));
    assert!(n("catalogued_files") <= page.get("discovery").get("files").number());
    assert!(n("files_examined") <= n("searched_files"));
    assert!(n("files_examined") <= n("candidates_examined"));
    assert!(n("matches_seen") <= n("searched_files"));
    assert_eq!(hits.len() as u64, n("retained_hits"));
    assert!(hits.len() <= MAX_FILE_FINDER_RESULTS);
    assert!(hits.len() as u64 <= page.get("limits").get("max_results").number());
    assert_eq!(n("retained_path_bytes"), hits.iter().map(|row| raw(row).len() as u64).sum::<u64>());
    assert!(n("retained_path_bytes") <= MAX_FILE_FINDER_RETAINED_PATH_BYTES as u64);
    assert_eq!(page.get("truncated").flag(), n("matches_seen") > n("retained_hits"));
    assert_eq!(n("discovery_source_bytes_read"), 0); assert_eq!(n("discovery_source_read_calls"), 0);
    assert!(!page.get("source_payload_read").flag());
    assert_eq!(page.get("more").flag(), matches!(page.get("state").text(), "searching" | "paused"));
    assert_eq!(page.get("paused").flag(), page.get("state").text() == "paused");
    assert_eq!(page.get("search_complete").flag(), page.get("state").text() == "complete");
    if page.get("search_complete").flag() {
        assert!(page.get("discovery_complete").flag()); assert!(!page.get("pending_batch").flag());
    }
    if page.get("replay_after_revision") != &Json::Null {
        assert!(n("replay_after_revision") < n("revision"));
    }
}
fn finish(session: &mut FileFinderSession) -> Json {
    let mut page = json(&session.info(|| false).unwrap());
    for _ in 0..1000 {
        invariants(&page);
        if !page.get("more").flag() { return page; }
        page = json(&session.advance(1, page.get("revision").number(), MAX_FILE_FINDER_BATCHES, || false).unwrap());
    }
    panic!("bounded fixture did not terminate")
}

#[test]
fn a_unique_raw_named_target_after_twenty_thousand_files_opens_an_independent_exact_reader() {
    let root = root(); linked_files(&root, 20_513);
    fs::create_dir(root.join("queued" )).unwrap();
    let relative = b"queued/late_unique-\xff.rs";
    let path = root.join(OsString::from_vec(relative.to_vec())); fs::write(&path, b"x").unwrap();
    let mut session = start(&root, 73001, b"late_unique", FileFinderOptions { mode: PathMatchMode::Prefix, ..Default::default() });
    let first = session.page(1, 1, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_PARTIAL);
    let mut page = json(&first); invariants(&page);
    assert_eq!(page.get("revision").number(), 1);
    assert_eq!(page.get("searched_files").number(), 0);
    assert!(page.get("catalogued_files").number() <= 256);
    assert!(page.get("pending_batch").flag());
    let mut advances = 0;
    while page.get("hits").array().is_empty() && page.get("more").flag() {
        page = json(&session.advance(1, page.get("revision").number(), 16, || false).unwrap());
        invariants(&page); advances += 1;
    }
    assert!(advances <= 7, "bounded chunks should reach beyond20k without dozens of requests");
    let hit = &page.get("hits").array()[0];
    assert_eq!(raw(hit), relative);
    // A queued child is visited only after every actual root-directory file;
    // this proof does not assume any ordering from the native directory stream.
    assert!(hit.get("file_id").number() > 20_513);
    assert!(page.get("searched_files").number() > 20_513);
    let mut bytes = vec![0xff, 0xfe];
    for unit in "current \u{1f980} UTF16\r\n\0 capture".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fs::write(&path, &bytes).unwrap(); // Observed metadata length was1, not capture authority.
    let (mut reader, response) = session.open_reader(owner(73002), 1, page.get("revision").number(),
        file(session.owner(), hit.get("file_id").number()), 4096, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("selection"), hit);
    assert!(receipt.get("source_payload_read").flag());
    assert_eq!(receipt.get("source_bytes_read").number(), bytes.len() as u64);
    assert!(receipt.get("source_read_calls").number() > 0);
    assert_eq!(receipt.get("source_observation").text(), "new-capture-after-metadata-selection");
    let info = receipt.get("reader");
    assert_eq!(info.get("path"), hit.get("path"));
    assert_eq!(info.get("capture_origin").text(), "host-supplied");
    assert_eq!(info.get("initial_source_bytes_read").number(), 0);
    assert_eq!(info.get("initial_read_calls").number(), 0);
    assert_eq!(info.get("capture_sha256").text(), fcb::store::Sha256::digest(&bytes).to_hex());
    for key in ["accepted_query_generation", "accepted_outline_generation", "accepted_document_generation"] {
        assert_eq!(info.get(key), &Json::Null);
    }
    fs::write(&path, b"new live bytes").unwrap(); session.grant().revoke(); drop(session);
    let copy = json(&reader.copy_range(0, bytes.len() as u64, || false).unwrap());
    assert_eq!(copy.get("original_hex").text(), hex(&bytes));
}

#[test]
fn pending_matching_and_empty_discovery_pages_never_claim_a_complete_zero_match_search() {
    let empty = root(); let mut session = start(&empty, 73101, b"missing", Default::default());
    let first = json(&session.info(|| false).unwrap()); invariants(&first);
    assert!(first.get("discovery_complete").flag());
    assert!(!first.get("search_complete").flag()); assert!(first.get("more").flag());
    let response = session.advance(1, 1, 16, || false).unwrap(); assert_eq!(response.exit_code(), EXIT_OK);
    let last = json(&response); invariants(&last); assert_eq!(last.get("state").text(), "complete");
    assert_eq!(last.get("matches_seen").number(), 0);
    assert!(matches!(session.advance(1, 2, 1, || false), Err(FileFinderSessionError::NoMoreResults)));
    let root = root();
    for i in 0..300 { fs::create_dir(root.join(format!("dir-{i:03}"))).unwrap(); }
    fs::write(root.join("dir-299/visible.rs"), b"payload").unwrap();
    let mut session = start(&root, 73102, b"visible", Default::default());
    let first = json(&session.advance(1, 1, 1, || false).unwrap());
    assert!(first.get("hits").array().is_empty()); assert!(first.get("more").flag());
    assert_eq!(first.get("searched_files").number(), 0);
    let last = finish(&mut session); assert_eq!(last.get("matches_seen").number(), 1);
    assert!(last.get("search_complete").flag());
}

#[test]
fn batch_merges_match_the_shared_ranker_for_exact_prefix_fuzzy_case_and_raw_queries() {
    let root = root(); let mut paths = Vec::new();
    for i in 0..600 {
        let name = if i % 2 == 0 { format!("main_extra-{i:03}.rs") } else { format!("Main-{i:03}.rs") };
        fs::write(root.join(&name), b"source").unwrap(); paths.push(RawPath::from_bytes(name.as_bytes()));
    }
    fs::create_dir(root.join("nested")).unwrap();
    for name in [b"nested/main.rs".as_slice(), b"nested/MAIN.rs", b"nested/raw-\xff.rs", "nested/caf\u{e9}.rs".as_bytes(), "nested/cafe\u{301}.rs".as_bytes()] {
        fs::write(root.join(OsString::from_vec(name.to_vec())), b"source").unwrap(); paths.push(RawPath::from_bytes(name));
    }
    let oracle_owner = owner(73200); let root_id = RootId::new(oracle_owner, 1).unwrap();
    let entries: Vec<_> = paths.iter().enumerate().map(|(i, path)| PathEntry::new(file(oracle_owner, i as u64 + 1), root_id, path)).collect();
    let budget = ResourceBudget::new(oracle_owner, ByteLength::new(32 * 1024 * 1024)).unwrap();
    let index = PathIndex::build(SearchManifestId::new(oracle_owner, 1).unwrap(), MembershipState::Closed,
        &entries, PathIndexLimits::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let cases: &[(&[u8], PathMatchMode, PathCase)] = &[
        (b"main.rs", PathMatchMode::Fuzzy, PathCase::UnicodeLowercase),
        (b"main", PathMatchMode::Prefix, PathCase::Sensitive),
        (b"MAIN.rs", PathMatchMode::Exact, PathCase::UnicodeLowercase),
        (b"mnrs", PathMatchMode::Fuzzy, PathCase::Sensitive),
        (b"raw-\xff.rs", PathMatchMode::Exact, PathCase::Sensitive),
        ("caf\u{e9}".as_bytes(), PathMatchMode::Prefix, PathCase::UnicodeLowercase),
    ];
    for (i, &(needle, mode, case)) in cases.iter().enumerate() {
        let options = FileFinderOptions { max_results: 7, mode, case };
        let mut session = start(&root, 73210 + i as u64, needle, options);
        let first = json(&session.info(|| false).unwrap());
        assert_eq!(first.get("searched_files").number(), 0, "every query starts from its whole root universe");
        let actual = finish(&mut session);
        let mut options = PathSearchOptions::new(QueryGeneration::new(oracle_owner, 1).unwrap());
        options.mode = mode; options.case = case; options.max_results = 7;
        let mut query = PathSearch::new(&index, needle, options, &budget, ResourceAllocationId::new(100 + i as u64).unwrap()).unwrap();
        query.run_to_completion(|| false).unwrap();
        let expected: Vec<_> = query.ranked_matches().iter().map(|hit| hit.path().raw_path().as_bytes().to_vec()).collect();
        let observed: Vec<_> = actual.get("hits").array().iter().map(raw).collect();
        assert_eq!(observed, expected, "mode/case/batch partition changed the shared ordering");
        assert_eq!(actual.get("matches_seen").number(), query.matches_seen() as u64);
        assert_eq!(actual.get("searched_files").number(), paths.len() as u64);
        assert!(actual.get("search_complete").flag());
    }
}

#[test]
fn later_better_hits_replace_top_k_and_stale_selection_proofs_refuse_before_source_io() {
    let root = root();
    for i in 0..600 { fs::write(root.join(format!("main-extra-{i:03}.rs")), b"early").unwrap(); }
    fs::create_dir(root.join("later")).unwrap(); fs::write(root.join("later/main.rs"), b"best").unwrap();
    let mut session = start(&root, 73301, b"main.rs", FileFinderOptions { max_results: 1, ..Default::default() });
    let first = json(&session.advance(1, 1, 16, || false).unwrap());
    assert_eq!(first.get("hits").array().len(), 1); assert!(first.get("more").flag());
    let old_file = file(session.owner(), first.get("hits").array()[0].get("file_id").number());
    let old_revision = first.get("revision").number();
    let last = finish(&mut session); let revision = last.get("revision").number();
    assert_eq!(raw(&last.get("hits").array()[0]), b"later/main.rs");
    assert!(last.get("truncated").flag()); assert_eq!(last.get("matches_seen").number(), 601);
    let current_file = file(session.owner(), last.get("hits").array()[0].get("file_id").number());
    fs::rename(&root, root.with_extension("moved")).unwrap(); // Any root metadata access now fails.
    assert!(matches!(session.page(2, revision, || false), Err(FileFinderSessionError::StaleQuery)));
    assert!(matches!(session.page(1, old_revision, || false), Err(FileFinderSessionError::StaleRevision)));
    assert!(matches!(session.advance(1, 0, 1, || false), Err(FileFinderSessionError::StaleRevision)));
    assert!(matches!(session.open_reader(owner(73302), 1, revision, file(owner(73399), 1), 4096, || false), Err(FileFinderSessionError::ForeignOwner)));
    assert!(matches!(session.open_reader(session.owner(), 1, revision, current_file, 4096, || false), Err(FileFinderSessionError::ForeignOwner)));
    assert!(matches!(session.open_reader(owner(73302), 2, revision, current_file, 4096, || false), Err(FileFinderSessionError::StaleQuery)));
    assert!(matches!(session.open_reader(owner(73302), 1, old_revision, old_file, 4096, || false), Err(FileFinderSessionError::StaleRevision)));
    assert!(matches!(session.open_reader(owner(73302), 1, revision, old_file, 4096, || false), Err(FileFinderSessionError::MissingHit)));
    assert!(matches!(session.open_reader(owner(73302), 1, revision, current_file, 4096, || false), Err(FileFinderSessionError::Catalog(_))));
}

#[test]
fn every_small_batch_cancellation_checkpoint_recovers_without_double_counting_or_losing_rows() {
    let root = root(); for name in ["q-a.rs", "q-b.rs", "q-c.rs"] { fs::write(root.join(name), b"source").unwrap(); }
    let mut baseline = start(&root, 73400, b"q", Default::default());
    let mut checkpoints = 0;
    baseline.advance(1, 1, 1, || { checkpoints += 1; false }).unwrap();
    assert!(checkpoints > 10);
    for cancel_at in 1..=checkpoints {
        let mut session = start(&root, 73410 + cancel_at as u64, b"q", Default::default()); let mut polls = 0;
        let error = session.advance(1, 1, 1, || { polls += 1; polls == cancel_at }).err().expect("cancellation published a page");
        assert!(error.is_canceled(), "checkpoint {cancel_at}: {error}");
        let recovery = json(&session.info(|| false).unwrap()); invariants(&recovery);
        assert!(matches!(recovery.get("revision").number(), 1 | 2));
        let page = json(&session.advance(1, 1, 16, || false).unwrap()); invariants(&page);
        assert_eq!(page.get("revision").number(), 2);
        assert_eq!(page.get("matches_seen").number(), 3);
        assert_eq!(page.get("searched_files").number(), 3);
        let replay = json(&session.advance(1, 1, 1, || false).unwrap());
        assert_eq!(page, replay, "changed max_batches must not turn a retry into another advance");
    }
}

#[test]
fn cancellation_after_an_accepted_batch_and_the_next_consumed_batch_preserves_both() {
    let root = root(); linked_files(&root, 600);
    let mut baseline = start(&root, 73501, b"absent", Default::default()); let mut checkpoints = 0;
    baseline.advance(1, 1, 1, || { checkpoints += 1; false }).unwrap();
    let mut session = start(&root, 73502, b"absent", Default::default()); let mut polls = 0;
    let error = session.advance(1, 1, 16, || { polls += 1; polls == checkpoints }).err().unwrap();
    assert!(error.is_canceled());
    let recovery = json(&session.info(|| false).unwrap()); invariants(&recovery);
    assert_eq!(recovery.get("revision").number(), 2);
    assert!(recovery.get("pending_batch").flag());
    assert!(recovery.get("catalogued_files").number() > recovery.get("searched_files").number());
    let replay = json(&session.advance(1, 1, 16, || false).unwrap());
    assert_eq!(replay.get("revision"), recovery.get("revision"));
    assert_eq!(replay.get("catalogued_files"), recovery.get("catalogued_files"));
    assert_eq!(replay.get("searched_files"), recovery.get("searched_files"));
    let paused = json(&session.stop(1, || false).unwrap()); invariants(&paused);
    assert_eq!(paused.get("state").text(), "paused");
    let final_page = finish(&mut session);
    assert_eq!(final_page.get("searched_files").number(), 600);
    assert_eq!(final_page.get("matches_seen").number(), 0);
    assert!(final_page.get("search_complete").flag());
    assert_eq!(json(&session.stop(1, || false).unwrap()).get("state").text(), "complete");
    assert!(matches!(session.advance(1, 1, 16, || false), Err(FileFinderSessionError::StaleRevision)));
}

#[test]
fn escaped_long_top_k_paths_fit_the_declared_global_snapshot_bound() {
    let root = root(); let mut nested = root.clone();
    for i in 0..7 {
        let mut name = vec![if i % 2 == 0 { 0xff } else { 1 }; 230]; name.push(b'a' + i);
        nested.push(OsString::from_vec(name)); fs::create_dir(&nested).unwrap();
    }
    let mut seed = None;
    for i in 0..256 {
        let mut name = vec![b'q']; name.extend_from_slice(&[0xff; 190]); name.extend_from_slice(format!("-{i:03}.rs").as_bytes());
        let path = nested.join(OsString::from_vec(name));
        if let Some(seed) = &seed { fs::hard_link(seed, &path).unwrap(); }
        else { fs::write(&path, b"source").unwrap(); seed = Some(path); }
    }
    let mut session = start(&root, 73601, b"q", FileFinderOptions { mode: PathMatchMode::Prefix, ..Default::default() });
    let final_page = finish(&mut session);
    assert_eq!(final_page.get("retained_hits").number(), 256);
    assert!(final_page.get("retained_path_bytes").number() > 450_000);
    let response = session.page(1, final_page.get("revision").number(), || false).unwrap();
    assert!(response.as_str().len() > 3 * 1024 * 1024, "fixture must exercise the global top-k envelope");
    assert!(response.as_str().len() <= MAX_FILE_FINDER_RESPONSE_BYTES);
    assert!(MAX_FILE_FINDER_RETAINED_PATH_BYTES * 7 + 256 * 512 + 8192 < MAX_FILE_FINDER_RESPONSE_BYTES);
    assert!(final_page.get("search_complete").flag());
}

#[test]
fn metadata_policy_and_omission_counters_keep_coverage_separate_from_top_k_truncation() {
    let root = root();
    fs::write(root.join(".gitignore"), b"visible.rs\n").unwrap();
    fs::write(root.join("visible.rs"), b"source").unwrap();
    fs::create_dir(root.join(".git")).unwrap(); fs::write(root.join(".git/visible-secret.rs"), b"secret").unwrap();
    let mut nested = root.clone();
    for _ in 0..10 { nested.push("x".repeat(240)); fs::create_dir(&nested).unwrap(); }
    fs::write(nested.join("visible-hidden.rs"), b"source").unwrap();
    let mut session = start(&root, 73701, b"visible", Default::default());
    let page = finish(&mut session);
    assert_eq!(page.get("matches_seen").number(), 1);
    assert_eq!(raw(&page.get("hits").array()[0]), b"visible.rs");
    assert_eq!(page.get("policy").text(), "product-defaults/no-rule-files-v1");
    assert_eq!(page.get("state").text(), "partial");
    assert!(!page.get("truncated").flag()); assert!(!page.get("search_complete").flag());
    assert!(page.get("discovery").get("path_limited").number() > 0);
    assert!(page.get("discovery").get("excluded_entries").number() > 0);
}

#[test]
fn root_replacement_and_leaf_symlinks_do_not_rebind_retained_hits() {
    let root = root(); fs::write(root.join("selected.rs"), b"inside").unwrap();
    let mut session = start(&root, 73801, b"selected", Default::default()); let page = finish(&mut session);
    let hit = file(session.owner(), page.get("hits").array()[0].get("file_id").number());
    fs::rename(root.join("selected.rs"), root.join("old.rs")).unwrap();
    let outside = root.with_extension("outside"); fs::write(&outside, b"outside").unwrap();
    symlink(&outside, root.join("selected.rs")).unwrap();
    assert!(session.open_reader(owner(73802), 1, page.get("revision").number(), hit, 4096, || false).is_err());
    fs::rename(&root, root.with_extension("old-root")).unwrap(); fs::create_dir(&root).unwrap();
    fs::write(root.join("selected.rs"), b"replacement").unwrap();
    assert!(session.info(|| false).is_err());
    assert!(session.open_reader(owner(73803), 1, page.get("revision").number(), hit, 4096, || false).is_err());
}
