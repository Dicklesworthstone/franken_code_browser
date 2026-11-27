#![forbid(unsafe_code)]
#![cfg(unix)]

//! Attached FCBD indexes accelerate retained expressions without making index
//! candidates into hits or rewriting accepted query provenance after changes.

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use std::{fs::OpenOptions, io::{Seek, SeekFrom, Write}};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::host::{HostResponse, saved_repository::SnapshotScope};
use fixture::{entry, owner, Fixture};
use support::{Json, parse};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
const INDEX_FIELDS: [&str; 11] = ["index_used", "index_generation", "index_pin", "index_policy",
    "index_eliminated_files", "index_candidates", "fallback_files", "posting_list_lookups",
    "posting_entries_visited", "posting_membership_lookups", "posting_cursor_complete"];
fn same_index(left: &Json, right: &Json) {
    for field in INDEX_FIELDS { assert_eq!(left.get(field), right.get(field), "{field}"); }
}
fn direct(page: &Json) {
    assert!(!page.get("index_used").flag());
    assert_eq!(page.get("index_generation"), &Json::Null);
    assert_eq!(page.get("index_pin"), &Json::Null);
    assert_eq!(page.get("index_policy").text(), "direct-verified-member-scan");
    for field in ["index_eliminated_files", "index_candidates", "fallback_files",
        "posting_list_lookups", "posting_entries_visited", "posting_membership_lookups"] {
        assert_eq!(page.get(field).number(), 0, "{field}");
    }
    assert!(!page.get("posting_cursor_complete").flag());
}

#[test]
fn accepted_pages_and_readers_freeze_query_time_index_across_attach_detach_and_failure() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle required needle"),
        entry(b"src/b.rs", b"filler required"), entry(b"src/c.py", b"needle required"),
        entry(b"src-other/decoy.rs", b"needle required")], true);
    let (path, pin) = f.index(); let mut saved = f.open(66901);
    let expression = "needle required -forbidden lang:rust";
    let first = json(&saved.expression_search_scoped_page(1, expression, 1, 10000,
        true, true, 0, SnapshotScope::Directory(b"src"), || false).unwrap());
    direct(&first); assert_eq!(first.get("matches_seen").number(), 2);
    saved.attach_index(5, &path, pin, || false).unwrap();
    let reads = json(&saved.info(|| false).unwrap()).get("member_read_calls").number();
    let before_attach = json(&saved.expression_results(1, 0, 1, || false).unwrap());
    same_index(&first, &before_attach);
    assert_eq!(json(&saved.info(|| false).unwrap()).get("member_read_calls").number(), reads);

    let indexed = json(&saved.expression_search_scoped_page(2, expression, 1, 10000,
        true, true, 1, SnapshotScope::Directory(b"src"), || false).unwrap());
    assert!(indexed.get("index_used").flag());
    assert_eq!(indexed.get("index_generation").number(), 5);
    assert_eq!(indexed.get("index_pin").text(), pin.to_hex());
    assert_eq!(indexed.get("scope_known_members").number(), 3);
    assert_eq!(indexed.get("scope_files").number(), 2);
    assert_eq!(indexed.get("metadata_excluded").number(), 1);
    assert_eq!(indexed.get("index_eliminated_files").number(), 1);
    assert_eq!(indexed.get("index_candidates").number(), 1);
    assert_eq!(indexed.get("fallback_files").number(), 0);
    assert_eq!(indexed.get("members_loaded").number(), 1);
    assert_eq!(indexed.get("matches_seen").number(), 2);
    assert_eq!(indexed.get("skipped_matches").number(), 1);
    assert!(indexed.get("complete").flag()); assert!(indexed.get("posting_cursor_complete").flag());

    saved.detach_index(6, || false).unwrap();
    let detached = json(&saved.expression_results(2, 0, 1, || false).unwrap());
    same_index(&indexed, &detached);
    let (mut reader, opened) = saved.open_expression_hit_reader(owner(66902), 2, 1, || false).unwrap();
    let opened = json(&opened); same_index(&indexed, &opened);
    assert_eq!(opened.get("scope").text(), "saved-directory");
    assert_eq!(opened.get("scope_path_hex").text(), "737263");
    assert_eq!(opened.get("selection").get("original_range").get("start").number(), 16);
    assert_eq!(opened.get("selection").get("original_range").get("end").number(), 22);
    assert_eq!(opened.get("selection").get("text").text(), "needle");

    saved.attach_index(7, &path, pin, || false).unwrap();
    assert!(saved.expression_search(3, "regex:needle", 10, 10000, || false).is_err());
    assert!(matches!(saved.expression_search(4, "needle", 10, 10000, || true),
        Err(error) if error.is_canceled()));
    assert_eq!(saved.accepted_expression_generation(), Some(2));
    assert_eq!(saved.last_expression_attempt(), 4);
    assert_eq!(saved.index_generation(), Some(7));
    same_index(&indexed, &json(&saved.expression_results(2, 0, 1, || false).unwrap()));

    // A new window uses the current index slot; old accepted receipts do not.
    saved.detach_index(8, || false).unwrap();
    let later = json(&saved.expression_search_scoped_page(5, expression, 1, 10000,
        true, true, 1, SnapshotScope::Directory(b"src"), || false).unwrap());
    direct(&later); assert_eq!(later.get("hits"), indexed.get("hits"));
    drop(saved);
    assert_eq!(json(&reader.copy_range(16, 22, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn metadata_only_closure_and_source_work_limits_keep_index_counters_in_selected_scope() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle required"), entry(b"src/b.rs", b"filler required"),
        entry(b"src/c.py", b"needle required"),
        SnapshotEntry { path: b"lost.rs", observed_bytes: 8, data: SnapshotData::Unavailable("missing") }], false);
    let (path, pin) = f.index(); let mut saved = f.open(66911);
    saved.attach_index(1, &path, pin, || false).unwrap();
    let absent = json(&saved.expression_search_scoped_page(1, "needle required", 0, 0,
        true, true, 0, SnapshotScope::Member(b"src/b.rs"), || false).unwrap());
    assert!(absent.get("complete").flag()); assert!(absent.get("index_used").flag());
    assert_eq!(absent.get("members_loaded").number(), 0);
    assert_eq!(absent.get("index_eliminated_files").number(), 1);
    assert_eq!(absent.get("scanned_bytes").number(), 0);
    let partial = json(&saved.expression_search_scoped_page(2, "needle required", 0, 0,
        true, true, 0, SnapshotScope::Member(b"src/a.rs"), || false).unwrap());
    assert_eq!(partial.get("state").text(), "work-limit"); assert!(!partial.get("complete").flag());
    assert_eq!(partial.get("members_loaded").number(), 0);
    assert_eq!(partial.get("index_candidates").number(), 1);
    assert_eq!(partial.get("index_eliminated_files").number(), 0);
    assert_eq!(partial.get("matches_seen").number(), 0);
    assert!(!partial.get("posting_cursor_complete").flag());

    let excluded = json(&saved.expression_search_scoped_page(3, "needle -path:lost", 0, 0,
        true, true, 0, SnapshotScope::Member(b"lost.rs"), || false).unwrap());
    assert!(excluded.get("complete").flag()); assert_eq!(excluded.get("scope_files").number(), 0);
    assert_eq!(excluded.get("scope_known_members").number(), 1);
    assert_eq!(excluded.get("scope_captured_members").number(), 0);
    assert_eq!(excluded.get("members_loaded").number(), 0);
    assert_eq!(excluded.get("index_candidates").number(), 0);
    assert_eq!(excluded.get("index_eliminated_files").number(), 0);
    assert_eq!(excluded.get("fallback_files").number(), 0);
    assert_eq!(excluded.get("posting_entries_visited").number(), 0);

    let directory = json(&saved.expression_search_scoped_page(4, "needle required lang:rust", 10, 10000,
        true, true, 0, SnapshotScope::Directory(b"src"), || false).unwrap());
    assert_eq!(directory.get("state").text(), "complete"); assert!(!directory.get("complete").flag());
    assert!(!directory.get("scope_membership_complete").flag());
    assert_eq!(directory.get("scope_files").number(), 2);
    assert_eq!(directory.get("index_candidates").number(), 1);
    assert_eq!(directory.get("index_eliminated_files").number(), 1);
    assert_eq!(directory.get("members_loaded").number(), 1);
    assert_eq!(directory.get("metadata_excluded").number(), 1);
    assert!(!directory.get("matches_seen_exact").flag());
}

#[test]
fn negative_prefilter_skips_payload_io_but_failed_direct_verification_preserves_accepted_hits() {
    let good = b"needle required needle"; let irrelevant = b"unique filler required";
    let f = Fixture::new(&[entry(b"a.rs", good), entry(b"b.rs", irrelevant)], true);
    let (path, pin) = f.index(); let mut saved = f.open(66921);
    saved.attach_index(1, &path, pin, || false).unwrap();
    // Change only a member proved negative by the independently pinned index,
    // after opening the archive. Any accidental read must fail its saved digest.
    let at = f.bytes.windows(irrelevant.len()).position(|bytes| bytes == irrelevant).unwrap();
    let mut changed = OpenOptions::new().write(true).open(&f.path).unwrap();
    changed.seek(SeekFrom::Start(at as u64)).unwrap();
    changed.write_all(b"X").unwrap(); changed.flush().unwrap();
    let accepted = json(&saved.expression_search_counted(1, "needle required", 10, 10000, || false).unwrap());
    assert!(accepted.get("complete").flag()); assert_eq!(accepted.get("matches_seen").number(), 2);
    assert_eq!(accepted.get("index_eliminated_files").number(), 1);
    assert_eq!(accepted.get("members_loaded").number(), 1);
    assert_eq!(accepted.get("member_bytes_read").number(), good.len() as u64);

    saved.detach_index(2, || false).unwrap();
    assert!(saved.expression_search_counted(2, "needle required", 10, 10000, || false).is_err());
    assert_eq!(saved.accepted_expression_generation(), Some(1));
    same_index(&accepted, &json(&saved.expression_results(1, 0, 10, || false).unwrap()));
    let (mut reader, receipt) = saved.open_expression_hit_reader(owner(66922), 1, 2, || false).unwrap();
    same_index(&accepted, &json(&receipt));
    assert_eq!(json(&reader.copy_range(16, 22, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}
