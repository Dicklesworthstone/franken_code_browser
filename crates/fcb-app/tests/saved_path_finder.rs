#![forbid(unsafe_code)]
#![cfg(unix)]

//! Saved Quick Open exercises the production metadata directory, shared path
//! ranker and verified reader handoff. Archive/source fixtures are retained for
//! independent replay; these authored scenarios are not execution receipts.

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;

use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::ByteLength;
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositoryError, SavedRepositorySession,
    paths::{SavedPathOptions, PathCase, PathMatchMode, MAX_SAVED_PATH_RESULTS,
        MAX_SAVED_PATH_QUERY_BYTES, MAX_SAVED_PATH_WORK_UNITS}}};
use saved_fixture::{Fixture, entry, owner};
use support::{Json, parse};

fn fixture(entries: &[SnapshotEntry<'_>], discovery_complete: bool) -> Fixture {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(b.path));
    Fixture::new(&sorted, discovery_complete)
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn paths(page: &Json) -> Vec<&str> {
    page.get("hits").array().iter().map(|hit| hit.get("path").get("hex").text()).collect()
}
fn search(saved: &mut SavedRepositorySession, generation: u64, query: &[u8]) -> Json {
    json(&saved.path_search(generation, query, SavedPathOptions::default(), || false).unwrap())
}
fn exact() -> SavedPathOptions { SavedPathOptions { mode: PathMatchMode::Exact, ..Default::default() } }
fn no_source_work(page: &Json) {
    assert!(!page.get("source_payload_read").flag());
    assert!(!page.get("live_roots_accessed").flag());
    assert!(!page.get("live_root_authority").flag());
    assert_eq!(page.get("member_bytes_read").number(), 0);
    assert_eq!(page.get("member_read_calls").number(), 0);
    assert_eq!(page.get("retained_source_payload_bytes").number(), 0);
}

#[test]
fn quick_open_searches_beyond_member_page_and_paginates_retained_metadata_only() {
    let names: Vec<_> = (0..180).map(|n| format!("src/file-{n:03}.rs")).collect();
    let entries: Vec<_> = names.iter().map(|name| entry(name.as_bytes(), b"payload is not a filename")).collect();
    let f = fixture(&entries, true);
    let mut saved = f.open(78201);
    let first_members = json(&saved.members(0, 64, || false).unwrap());
    assert_eq!(first_members.get("members").array().len(), 64);
    let found = search(&mut saved, 1, b"file-179.rs");
    assert_eq!(paths(&found), [hex(b"src/file-179.rs")]);
    assert_eq!(found.get("hits").array()[0].get("member").number(), 179);
    assert!(found.get("search_complete").flag());
    no_source_work(&found);

    let all = search(&mut saved, 2, b"file");
    assert_eq!(all.get("matches_seen").number(), 180);
    assert_eq!(all.get("retained_hits").number(), 180);
    assert_eq!(all.get("hits").array().len(), 64);
    assert_eq!(all.get("next_offset").number(), 64);
    assert_eq!(all.get("selection_namespace").text(), "saved-paths");
    assert_eq!(all.get("metadata_scope").text(), "archive-directory");
    assert_eq!(all.get("query_hex").text(), hex(b"file"));
    let later = json(&saved.path_results(2, 64, 128, || false).unwrap());
    assert_eq!(later.get("hits").array().len(), 116);
    assert_eq!(later.get("hits").array()[0].get("hit_id").number(), 65);
    assert_eq!(later.get("next_offset"), &Json::Null);
    assert_eq!(later.get("offset").number(), 64);
    no_source_work(&later);
    let info = json(&saved.info(|| false).unwrap());
    assert_eq!(info.get("member_bytes_read").number(), 0);
    assert_eq!(info.get("member_read_calls").number(), 0);
    assert_eq!(info.get("accepted_path_generation").number(), 2);
    assert_eq!(info.get("last_path_attempt").number(), 2);
    assert!(saved.path_results(2, 181, 10, || false).is_err());
    assert!(saved.path_results(2, 0, 129, || false).is_err());
}

#[test]
fn long_native_labels_shorten_pages_without_losing_rows_or_rank_completeness() {
    let names: Vec<_> = (0..160).map(|n| {
        let mut path = b"f/".to_vec();
        path.resize(16_380, 0x1f);
        path.extend_from_slice(format!("{n:04}").as_bytes());
        path
    }).collect();
    let entries: Vec<_> = names.iter().map(|name| entry(name, b"")).collect();
    let limits = SnapshotLimits { max_path_bytes: 16_384, ..Default::default() };
    let budget = ResourceBudget::new(owner(78221), ByteLength::new(64 * 1024 * 1024)).unwrap();
    let encoded = SnapshotBytes::encode(owner(78221), true, "long-paths", &entries, limits,
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let f = fixture(&[], true);
    fs::write(&f.path, encoded.bytes()).unwrap();
    let mut saved = SavedRepositorySession::open(owner(78222), &f.path, limits, || false).unwrap();
    let response = saved.path_search(1, b"f", SavedPathOptions {
        max_work_units: MAX_SAVED_PATH_WORK_UNITS, ..Default::default() }, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let first = json(&response);
    assert_eq!(first.get("retained_hits").number(), names.len() as u64);
    assert!(first.get("top_k_complete").flag());
    let mut page = first;
    let mut offset = 0usize;
    let mut saw_short_page = false;
    loop {
        let hits = page.get("hits").array();
        let requested = page.get("limit").number() as usize;
        assert!(!hits.is_empty());
        if hits.len() < requested.min(names.len() - offset) { saw_short_page = true; }
        for (position, hit) in hits.iter().enumerate() {
            let ordinal = offset + position;
            assert_eq!(hit.get("hit_id").number(), ordinal as u64 + 1);
            assert_eq!(hit.get("member").number(), ordinal as u64);
            assert_eq!(hit.get("path").get("hex").text(), hex(&names[ordinal]));
        }
        assert!(page.get("search_complete").flag());
        assert!(!page.get("truncated").flag());
        no_source_work(&page);
        offset += hits.len();
        if page.get("next_offset") == &Json::Null { break; }
        assert_eq!(page.get("next_offset").number(), offset as u64);
        page = json(&saved.path_results(1, offset, 128, || false).unwrap());
    }
    assert!(saw_short_page, "128 expanded native labels exceed one bounded response");
    assert_eq!(offset, names.len());
    assert_eq!(json(&saved.info(|| false).unwrap()).get("member_read_calls").number(), 0);
}

#[test]
fn ranking_and_exact_prefix_fuzzy_modes_reuse_the_shared_filename_contract() {
    let f = fixture(&[entry(b"foo", b""), entry(b"foo/bar.rs", b""),
        entry(b"foobar", b""), entry(b"f_a_o_o.rs", b"")], true);
    let mut saved = f.open(78202);
    let fuzzy = search(&mut saved, 1, b"foo");
    assert_eq!(paths(&fuzzy), [hex(b"foo"), hex(b"foo/bar.rs"), hex(b"foobar"), hex(b"f_a_o_o.rs")]);
    let kinds: Vec<_> = fuzzy.get("hits").array().iter().map(|hit| hit.get("match_kind").text()).collect();
    assert_eq!(kinds, ["exact-filename", "exact-component", "filename-prefix", "filename-subsequence"]);
    let exact = json(&saved.path_search(2, b"foo", exact(), || false).unwrap());
    assert_eq!(paths(&exact), [hex(b"foo"), hex(b"foo/bar.rs")]);
    let prefix = json(&saved.path_search(3, b"foo", SavedPathOptions {
        mode: PathMatchMode::Prefix, ..Default::default() }, || false).unwrap());
    assert_eq!(paths(&prefix), [hex(b"foo"), hex(b"foo/bar.rs"), hex(b"foobar")]);
    assert_eq!(prefix.get("matches_seen").number(), 3, "a filename/component does not count the same member twice");
    assert!(prefix.get("top_k_complete").flag());
    assert_eq!(prefix.get("key_version").number(), 1);
}

#[test]
fn case_and_unicode_matching_preserve_distinct_original_paths() {
    let f = fixture(&[entry(b"src/Foo.rs", b"upper"), entry(b"src/foo.rs", b"lower"),
        entry("docs/résumé.md".as_bytes(), b"accent"), entry("docs/re\u{301}sume\u{301}.md".as_bytes(), b"decomposed")], true);
    let mut saved = f.open(78203);
    let insensitive = json(&saved.path_search(1, b"Foo.rs", exact(), || false).unwrap());
    assert_eq!(paths(&insensitive), [hex(b"src/Foo.rs"), hex(b"src/foo.rs")]);
    assert_eq!(insensitive.get("case").text(), "unicode-lowercase");
    let sensitive = json(&saved.path_search(2, b"Foo.rs", SavedPathOptions {
        case: PathCase::Sensitive, ..exact() }, || false).unwrap());
    assert_eq!(paths(&sensitive), [hex(b"src/Foo.rs")]);
    let accent = json(&saved.path_search(3, "RÉSUMÉ.md".as_bytes(), exact(), || false).unwrap());
    assert_eq!(paths(&accent), [hex("docs/résumé.md".as_bytes())]);
    assert_eq!(accent.get("unicode_normalization").text(), "none");
    let row = &sensitive.get("hits").array()[0];
    let (reader, receipt) = saved.open_path_hit_reader(owner(78213), 3, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"accent");
    assert_eq!(json(&receipt).get("selection"), &Json::Null);
    assert_ne!(row.get("member"), json(&receipt).get("member"));
}

#[test]
fn unavailable_and_raw_names_remain_searchable_without_claiming_live_discovery() {
    let f = fixture(&[entry(b"raw-\xff.rs", b"original"), entry("raw-\u{fffd}.rs".as_bytes(), b"replacement scalar"),
        entry(b"raw-\\xff.rs", b"printed escape"), SnapshotEntry {
            path: b"missing.rs", observed_bytes: 123, data: SnapshotData::Unavailable("READ_DENIED") }], false);
    let mut saved = f.open(78204);
    let raw = search(&mut saved, 1, &[0xff]);
    assert_eq!(paths(&raw), [hex(b"raw-\xff.rs")], "invalid bytes must not alias U+FFFD or an escaped label");
    assert!(!raw.get("discovery_complete").flag());
    assert!(raw.get("search_complete").flag(), "the saved directory is fully searchable despite incomplete historical discovery");
    assert!(raw.get("matches_seen_exact").flag());
    let (reader, _) = saved.open_path_hit_reader(owner(78214), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"original");
    let before = json(&saved.info(|| false).unwrap()).get("member_bytes_read").number();
    let missing = search(&mut saved, 2, b"missing.rs");
    let row = &missing.get("hits").array()[0];
    assert!(!row.get("captured").flag());
    assert_eq!(row.get("reason").text(), "READ_DENIED");
    assert_eq!(row.get("observed_bytes").number(), 123);
    assert!(missing.get("search_complete").flag());
    no_source_work(&missing);
    assert!(saved.open_path_hit_reader(owner(78215), 2, 1, || false).is_err());
    assert_eq!(json(&saved.info(|| false).unwrap()).get("member_bytes_read").number(), before);
}

#[test]
fn exact_total_and_global_top_k_remain_distinct_from_retained_row_limit() {
    let f = fixture(&[entry(b"foo", b"a"), entry(b"foo/bar", b"b"), entry(b"foobar", b"c")], true);
    let mut saved = f.open(78205);
    for (generation, limit) in [(1, 0), (2, 1), (3, 3)] {
        let response = saved.path_search(generation, b"foo", SavedPathOptions {
            max_results: limit, ..Default::default() }, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK, "capped rows do not invalidate an exhaustive metadata count");
        let page = json(&response);
        assert_eq!(page.get("matches_seen").number(), 3);
        assert_eq!(page.get("retained_hits").number(), limit as u64);
        assert!(page.get("matches_seen_exact").flag() && page.get("top_k_complete").flag());
        assert_eq!(page.get("truncated").flag(), limit < 3);
        if limit > 0 { assert_eq!(paths(&page)[0], hex(b"foo")); }
    }
    let absent = json(&saved.path_search(4, b"absent", SavedPathOptions {
        max_results: 0, ..Default::default() }, || false).unwrap());
    assert_eq!(absent.get("matches_seen").number(), 0);
    assert!(!absent.get("truncated").flag());
    assert!(absent.get("search_complete").flag());
}

#[test]
fn work_stops_are_explicit_and_a_new_query_restarts_from_all_archive_names() {
    let names: Vec<_> = (0..90).map(|n| format!("src/file-{n:03}.rs")).collect();
    let entries: Vec<_> = names.iter().map(|name| entry(name.as_bytes(), b"ignored source")).collect();
    let f = fixture(&entries, true); let mut saved = f.open(78206);
    let opts = SavedPathOptions { max_results: 1, ..Default::default() };
    let first = json(&saved.path_search(1, b"file", opts, || false).unwrap());
    assert_eq!(first.get("retained_hits").number(), 1);
    let total_work = first.get("work_units").number();
    assert!(total_work > 0);
    for (generation, allowance) in [(2, 0), (3, total_work / 2)] {
        let response = saved.path_search(generation, b"file", SavedPathOptions {
            max_work_units: allowance, ..opts }, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let page = json(&response);
        assert_eq!(page.get("state").text(), "work-limit");
        assert!(page.get("work_limited").flag());
        assert!(!page.get("search_complete").flag() && !page.get("matches_seen_exact").flag()
            && !page.get("top_k_complete").flag());
        assert!(page.get("work_units").number() <= allowance);
        assert!(page.get("files_examined").number() < 90);
    }
    let refined = json(&saved.path_search(4, b"file-089.rs", opts, || false).unwrap());
    assert_eq!(paths(&refined), [hex(b"src/file-089.rs")], "previous top-k/partial rows are not the query universe");
    assert!(refined.get("search_complete").flag());
    // Component lookup can prove an empty candidate range without spending a
    // ranker unit. This is distinct from a zero-budget fuzzy scan.
    let indexed_absence = json(&saved.path_search(5, b"absent", SavedPathOptions {
        max_work_units: 0, ..exact() }, || false).unwrap());
    assert!(indexed_absence.get("search_complete").flag());
    assert_eq!(indexed_absence.get("work_units").number(), 0);
}

#[test]
fn every_cancellation_checkpoint_preserves_prior_paths_and_consumes_its_attempt() {
    let f = fixture(&[entry(b"old.rs", b"needle"), entry(b"new.rs", b"other")], true);
    let mut saved = f.open(78207);
    saved.search(1, "needle", 10, || false).unwrap();
    search(&mut saved, 1, b"old.rs");
    let mut checkpoints = 0;
    saved.path_search(2, b"new.rs", Default::default(), || { checkpoints += 1; false }).unwrap();
    assert!(checkpoints > 0);
    for stop_at in 1..=checkpoints {
        let generation = 2 + stop_at as u64;
        let mut polls = 0;
        let result = saved.path_search(generation, b"new.rs", Default::default(), || {
            polls += 1; polls == stop_at
        });
        assert!(result.as_ref().err().is_some_and(|error| error.is_canceled()), "checkpoint {stop_at}");
        assert_eq!(saved.accepted_path_generation(), Some(2));
        assert_eq!(saved.last_path_attempt(), generation);
        assert_eq!(paths(&json(&saved.path_results(2, 0, 64, || false).unwrap())), [hex(b"new.rs")]);
        assert_eq!(saved.accepted_generation(), Some(1), "path operations do not discard content search");
        assert_eq!(saved.path_search(generation, b"old.rs", Default::default(), || false).err(),
            Some(SavedRepositoryError::StaleQuery));
    }
    let next = saved.last_path_attempt() + 1;
    assert!(saved.clear_paths(next, || true).is_err());
    assert_eq!(saved.accepted_path_generation(), Some(2));
    saved.clear_paths(next + 1, || false).unwrap();
    assert_eq!(saved.accepted_path_generation(), None);
    assert_eq!(saved.open_path_hit_reader(owner(78217), 2, 1, || false).err(), Some(SavedRepositoryError::MissingQuery));
    assert_eq!(saved.accepted_generation(), Some(1));
}

#[test]
fn accepted_path_hits_open_only_the_retained_archive_and_reader_outlives_the_query() {
    let bytes = b"\xff\xfeo\0l\0d\0";
    let f = fixture(&[entry(b"src/source.rs", bytes)], true);
    let replacement = fixture(&[entry(b"src/source.rs", b"new live bytes")], true);
    let mut saved = f.open(78208);
    let found = search(&mut saved, 1, b"source.rs");
    let digest = found.get("archive_digest").text().to_owned();
    fs::rename(&f.path, f.root.join("original.fcbs")).unwrap();
    fs::write(&f.path, replacement.bytes).unwrap();
    let (reader, receipt) = saved.open_path_hit_reader(owner(78218), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), bytes);
    let receipt = json(&receipt);
    assert_eq!(receipt.get("archive_digest").text(), digest);
    assert_eq!(receipt.get("member").number(), found.get("hits").array()[0].get("member").number());
    assert_eq!(receipt.get("selection"), &Json::Null);
    assert!(!receipt.get("live_source_reopened").flag());
    search(&mut saved, 2, b"source.rs");
    assert_eq!(saved.open_path_hit_reader(owner(78219), 1, 1, || false).err(), Some(SavedRepositoryError::StaleQuery));
    assert_eq!(saved.open_path_hit_reader(owner(78219), 2, 0, || false).err(), Some(SavedRepositoryError::MissingHit));
    saved.clear_paths(3, || false).unwrap();
    drop(saved);
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn metadata_search_does_not_read_damaged_source_and_activation_refuses_it() {
    let bytes = b"unique captured member body";
    let f = fixture(&[entry(b"src/original.rs", bytes)], true);
    let mut saved = f.open(78209);
    let offset = f.bytes.windows(bytes.len()).position(|w| w == bytes).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(offset as u64)).unwrap();
    writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    let found = search(&mut saved, 1, b"original.rs");
    assert_eq!(found.get("retained_hits").number(), 1);
    assert_eq!(json(&saved.info(|| false).unwrap()).get("member_bytes_read").number(), 0);
    no_source_work(&found);
    assert!(saved.open_path_hit_reader(owner(78219), 1, 1, || false).is_err(),
        "a filename result never bypasses the saved member's digest check");
    assert_eq!(saved.accepted_path_generation(), Some(1));
    assert_eq!(paths(&json(&saved.path_results(1, 0, 64, || false).unwrap())), [hex(b"src/original.rs")]);
}

#[test]
fn invalid_input_limits_and_empty_archives_do_not_create_false_successes() {
    let f = fixture(&[entry(b"file.rs", b"contents")], true);
    let mut saved = f.open(78210);
    search(&mut saved, 1, b"file.rs");
    let oversized = vec![b'x'; MAX_SAVED_PATH_QUERY_BYTES + 1];
    for (n, query) in [b"".as_slice(), b"bad\0query".as_slice(), oversized.as_slice()].into_iter().enumerate() {
        assert!(saved.path_search(2 + n as u64, query, Default::default(), || false).is_err());
        assert_eq!(saved.accepted_path_generation(), Some(1));
    }
    for (generation, opts) in [(5, SavedPathOptions { max_results: MAX_SAVED_PATH_RESULTS + 1, ..Default::default() }),
        (6, SavedPathOptions { max_work_units: MAX_SAVED_PATH_WORK_UNITS + 1, ..Default::default() })] {
        assert!(saved.path_search(generation, b"file", opts, || false).is_err());
        assert_eq!(saved.accepted_path_generation(), Some(1));
    }
    assert!(saved.path_search(0, b"file", Default::default(), || false).is_err());
    assert_eq!(json(&saved.info(|| false).unwrap()).get("member_bytes_read").number(), 0);
    let empty = fixture(&[], false); let mut saved = empty.open(78211);
    let page = json(&saved.path_search(1, b"any", SavedPathOptions {
        max_work_units: 0, max_results: 0, ..Default::default() }, || false).unwrap());
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("matches_seen").number(), 0);
    assert!(!page.get("truncated").flag());
    assert!(!page.get("discovery_complete").flag());
}
