#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use saved_fixture::{entry, owner, Fixture};
use support::{Json, parse};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, saved_repository::SavedRepositorySession};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn counts(page: &Json, complete_key: &str, total: u64, files: u64, retained: u64, retained_files: u64) {
    assert!(page.get("count_all_matches").flag());
    assert!(page.get(complete_key).flag()); assert!(page.get("matches_seen_exact").flag());
    assert_eq!(page.get("matches_seen").number(), total);
    assert_eq!(page.get("matching_files").number(), files);
    assert_eq!(page.get("retained_hits").number(), retained);
    assert_eq!(page.get("retained_matching_files").number(), retained_files);
    assert_eq!(page.get("truncated").flag(), total > retained);
    assert_eq!(page.get("retained_source_payload_bytes").number(), 0);
}
fn member_reads(session: &mut SavedRepositorySession) -> u64 {
    json(&session.info(|| false).unwrap()).get("member_bytes_read").number()
}

#[test]
fn saved_counts_reuse_fcbd_candidates_and_keep_retained_reader_bytes_after_archive_close() {
    let bytes: Vec<_> = "\u{feff}banana banana".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f = Fixture::new(&[entry(b"a.rs", &bytes), entry(b"b.rs", b"banana"), entry(b"c.rs", b"no match")], true);
    let (index, pin) = f.index(); let mut session = f.open(88701);
    let direct = session.search_counted(1, "ana", 1, || false).unwrap();
    assert_eq!(direct.exit_code(), EXIT_OK);
    counts(&json(&direct), "search_complete", 6, 2, 1, 1);
    assert_eq!(json(&direct).get("max_matches").number(), 1);
    let (mut reader, opened) = session.open_hit_reader(owner(88702), 1, 1, || false).unwrap();
    assert!(!json(&opened).get("live_source_reopened").flag());
    assert_eq!(reader.capture().bytes(), bytes);
    session.attach_index(1, &index, pin, || false).unwrap();
    let indexed = session.search_counted(2, "ana", 1, || false).unwrap();
    counts(&json(&indexed), "search_complete", 6, 2, 1, 1);
    assert_eq!(json(&indexed).get("hits"), json(&direct).get("hits"));
    assert_eq!(json(&indexed).get("index_eliminated_files").number(), 1);
    assert_eq!(json(&indexed).get("fallback_files").number(), 1);
    let reads = member_reads(&mut session);
    let page = session.results(2, 0, 1, || false).unwrap();
    counts(&json(&page), "search_complete", 6, 2, 1, 1);
    assert_eq!(member_reads(&mut session), reads);
    let only = session.search_counted(3, "ana", 0, || false).unwrap();
    counts(&json(&only), "search_complete", 6, 2, 0, 0);
    assert!(json(&only).get("hits").array().is_empty());
    assert!(session.open_hit_reader(owner(88703), 3, 1, || false).is_err());
    let legacy = session.search(4, "ana", 1, || false).unwrap();
    assert_eq!(legacy.exit_code(), EXIT_PARTIAL);
    assert!(!json(&legacy).get("count_all_matches").flag());
    assert!(!json(&legacy).get("search_complete").flag());
    assert_eq!(json(&legacy).get("matches_seen").number(), 2);
    drop(session);
    std::fs::rename(&f.path, f.root.join("moved-counted.fcbs")).unwrap();
    assert_eq!(json(&reader.copy_range(4, 10, || false).unwrap()).get("original_hex").text(), hex(&bytes[4..10]));
}

#[test]
fn counted_expression_filters_precede_primary_counts_and_paging_is_metadata_only() {
    let le: Vec<_> = "\u{feff}banana required".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f = Fixture::new(&[entry(b"src/a.rs", b"banana required"), entry(b"src/b.rs", &le),
        entry(b"src/c.rs", b"banana forbidden required"), entry(b"src/d.rs", b"banana"),
        SnapshotEntry { path: b"tests/missing.rs", observed_bytes: 90, data: SnapshotData::Unavailable("NOT_CAPTURED") }], true);
    let mut session = f.open(88711);
    let query = "ana required -forbidden path:src/";
    let result = session.expression_search_counted(1, query, 3, 100_000, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    counts(&json(&result), "complete", 4, 2, 3, 2);
    assert_eq!(json(&result).get("state").text(), "complete");
    assert_eq!(json(&result).get("predicate_rejections").number(), 2);
    assert_eq!(json(&result).get("unavailable_files").number(), 0);
    let reads = member_reads(&mut session);
    let page = session.expression_results(1, 2, 1, || false).unwrap();
    counts(&json(&page), "complete", 4, 2, 3, 2);
    assert_eq!(json(&page).get("hits").array()[0].get("hit_id").number(), 3);
    assert_eq!(member_reads(&mut session), reads);
    let (mut reader, receipt) = session.open_expression_hit_reader(owner(88712), 1, 3, || false).unwrap();
    assert_eq!(json(&receipt).get("selection_namespace").text(), "saved-expression");
    assert!(!json(&receipt).get("live_source_reopened").flag());
    assert_eq!(reader.capture().bytes(), le);
    let only = session.expression_search_counted(2, query, 0, 100_000, || false).unwrap();
    counts(&json(&only), "complete", 4, 2, 0, 0);
    assert!(session.open_expression_hit_reader(owner(88713), 2, 1, || false).is_err());
    drop(session);
    assert_eq!(json(&reader.copy_range(4, 10, || false).unwrap()).get("original_hex").text(), hex(&le[4..10]));
}

#[test]
fn unknown_scope_and_predicate_limits_cannot_report_exact_zero_counts() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana"), SnapshotEntry { path: b"b.rs", observed_bytes: 30,
        data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let mut session = f.open(88721);
    let result = session.search_counted(1, "absent", 0, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert_eq!(json(&result).get("matches_seen").number(), 0);
    assert!(!json(&result).get("search_complete").flag());
    assert!(!json(&result).get("matches_seen_exact").flag());
    assert_eq!(json(&result).get("unavailable_files").number(), 1);
    let result = session.expression_search_counted(1, "absent", 0, 100_000, || false).unwrap();
    assert!(!json(&result).get("complete").flag()); assert!(!json(&result).get("matches_seen_exact").flag());
    let f = Fixture::new(&[entry(b"a.rs", b"banana..........forbidden")], true);
    let mut session = f.open(88722);
    let result = session.expression_search_counted(1, "ana -forbidden", 0, 4, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    let result = json(&result);
    assert_eq!(result.get("state").text(), "work-limit"); assert!(!result.get("matches_seen_exact").flag());
    assert_eq!(result.get("matches_seen").number(), 0); assert_eq!(result.get("matching_files").number(), 0);
    assert!(result.get("scanned_bytes").number() <= 4);
    let f = Fixture::new(&[entry(b"a.rs", b"no match")], false); let mut session = f.open(88723);
    let result = session.search_counted(1, "absent", 0, || false).unwrap();
    assert!(!json(&result).get("matches_seen_exact").flag());
}

#[test]
fn final_checkpoint_cancellation_and_invalid_replacements_preserve_accepted_counts() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana required"), entry(b"b.rs", b"banana banana required")], true);
    let mut probe = f.open(88731); let mut polls = 0;
    probe.search_counted(1, "ana", 0, || { polls += 1; false }).unwrap();
    let mut session = f.open(88732);
    session.search_counted(1, "ana", 1, || false).unwrap();
    let before = json(&session.results(1, 0, 64, || false).unwrap());
    let mut attempt_polls = 0;
    assert!(session.search_counted(2, "ana", 0, || {
        attempt_polls += 1; attempt_polls == polls
    }).err().unwrap().is_canceled());
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), before);
    assert!(session.search_counted(3, "ana", 4097, || false).is_err());
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), before);
    let mut polls = 0;
    probe.expression_search_counted(1, "ana required", 0, 100_000, || { polls += 1; false }).unwrap();
    session.expression_search_counted(1, "ana required", 1, 100_000, || false).unwrap();
    let before = json(&session.expression_results(1, 0, 64, || false).unwrap());
    let mut attempt_polls = 0;
    assert!(session.expression_search_counted(2, "ana required", 0, 100_000, || {
        attempt_polls += 1; attempt_polls == polls
    }).err().unwrap().is_canceled());
    assert_eq!(json(&session.expression_results(1, 0, 64, || false).unwrap()), before);
    assert!(session.expression_search_counted(3, "ana regex:x.*", 1, 100_000, || false).is_err());
    assert_eq!(json(&session.expression_results(1, 0, 64, || false).unwrap()), before);
}
