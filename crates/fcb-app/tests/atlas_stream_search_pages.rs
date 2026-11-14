#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real-file host regressions for later match windows. Every request rescans a
//! fresh observation; retained-row paging and activation must remain zero-I/O.
//! These authored sources do not claim strict-RCH or physical-Mac execution.
mod support;

use std::{cell::Cell, fs, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::host::{HostResponse,
    atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{AtlasSearchPathKind, AtlasSearchScope},
    atlas_stream_search::{AtlasStreamSearchError, AtlasStreamSearchOptions, RetainedAtlasStreamSearch}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-stream-pages-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn atlas(&self) -> AtlasSession {
        static NEXT: AtomicU64 = AtomicU64::new(79_000);
        AtlasSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(), &self.0,
            AtlasSessionOptions { max_files: 64, ..Default::default() }, || false).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn options(retained: usize) -> AtlasStreamSearchOptions {
    AtlasStreamSearchOptions { max_matches: retained, ..Default::default() }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) {
    (value.get("start").number(), value.get("end").number())
}
fn scan(service: &mut RetainedAtlasStreamSearch, atlas: &AtlasSession, needle: &str,
    options: AtlasStreamSearchOptions, scope: AtlasSearchScope, start_after: u64,
    counting: bool, occurrences: bool) -> Json {
    json(&service.search_scoped_page_with_progress(atlas, needle, options, scope,
        start_after, counting, occurrences, || false, || false, |_| {}).unwrap())
}
fn assert_window(report: &Json, requested: u64, skipped: u64, more: bool) {
    assert_eq!(report.get("page_start_after").number(), requested);
    assert_eq!(report.get("skipped_matches").number(), skipped);
    assert_eq!(report.get("page_has_more_matches").flag(), more);
}

#[test]
fn skipping_crosses_file_boundaries_and_lookahead_can_be_in_an_unretained_file() {
    let fixture = Fixture::new();
    for path in ["a.rs", "b.rs", "c.rs"] { fixture.write(path, b"aaaa"); }
    let atlas = fixture.atlas();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = scan(&mut service, &atlas, "aa", options(2),
        AtlasSearchScope::all(), 4, false, false);
    assert_window(&report, 4, 4, true);
    assert_eq!(report.get("retained_hits").number(), 2);
    assert_eq!(report.get("matches_seen").number(), 7);
    assert_eq!(report.get("matching_files").number(), 3);
    assert_eq!(report.get("retained_matching_files").number(), 1);
    assert_eq!(report.get("files_examined").number(), 3);
    assert_eq!(report.get("payload_bytes_read").number(), 12);
    assert_eq!(report.get("stop_reason").text(), "match-limit");
    assert!(!report.get("search_complete").flag());
    assert!(!report.get("match_count_complete").flag());
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    for (position, row) in report.get("hits").array().iter().enumerate() {
        assert_eq!(row.get("path").get("hex").text(), hex(b"b.rs"));
        assert_eq!(row.get("occurrence_id").number(), position as u64 + 2);
        assert_eq!(bounds(row.get("original_range")), (position as u64 + 1, position as u64 + 3));
        let selected = json(&service.hit(&atlas, generation, position).unwrap());
        assert_eq!(selected.get("page_start_after").number(), 4);
        assert_eq!(selected.get("hit").get("original_hex").text(), "6161");
    }
    let retained = json(&service.page(&atlas, generation, 0, 32).unwrap());
    assert_window(&retained, 4, 4, true);
    assert_eq!(retained.get("hits"), report.get("hits"));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn omitted_prefix_does_not_advertise_a_nonexistent_next_page_and_large_offsets_stay_exact() {
    let fixture = Fixture::new();
    fixture.write("a.rs", b"aaaa");
    let atlas = fixture.atlas();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let last = scan(&mut service, &atlas, "aa", options(2),
        AtlasSearchScope::all(), 2, false, false);
    assert_window(&last, 2, 2, false);
    assert!(last.get("search_complete").flag());
    assert!(last.get("match_count_complete").flag());
    assert!(last.get("truncated").flag(), "the skipped prefix is still omitted");
    assert_eq!(last.get("matches_seen").number(), 3);
    assert_eq!(last.get("retained_hits").number(), 1);
    assert_eq!(last.get("stop_reason"), &Json::Null);
    assert_eq!(last.get("hits").array()[0].get("occurrence_id").number(), 3);
    assert_eq!(bounds(last.get("hits").array()[0].get("original_range")), (2, 4));

    let beyond_exact_double = 9_007_199_254_740_993u64;
    let empty = scan(&mut service, &atlas, "aa", options(2),
        AtlasSearchScope::all(), beyond_exact_double, false, false);
    assert_window(&empty, beyond_exact_double, 3, false);
    assert_eq!(empty.get("page_start_after").text(), "9007199254740993");
    assert_eq!(empty.get("retained_hits").number(), 0);
    assert_eq!(empty.get("matches_seen").number(), 3);
    assert!(empty.get("search_complete").flag());
    assert!(empty.get("match_count_complete").flag());
    assert_eq!(empty.get("stop_reason"), &Json::Null);

    let generation = empty.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    let invalid = service.search_scoped_page_with_progress(&atlas, "aa", options(2),
        AtlasSearchScope::all(), u64::MAX, false, false, || false, || false,
        |_| panic!("overflowing page admission must not publish progress"));
    assert!(matches!(invalid, Err(AtlasStreamSearchError::InvalidLimits)));
    assert_eq!(service.accepted_generation(), Some(generation));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    assert_window(&json(&service.page(&atlas, generation, 0, 32).unwrap()),
        beyond_exact_double, 3, false);
}

#[test]
fn counting_later_rows_preserves_whole_observation_counts_and_bounded_witnesses() {
    let fixture = Fixture::new();
    for path in ["a.rs", "b.rs", "c.rs"] { fixture.write(path, b"aaaa"); }
    let atlas = fixture.atlas();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut previews = Vec::new();
    let report = json(&service.search_scoped_page_with_progress(&atlas, "aa", options(2),
        AtlasSearchScope::all(), 4, true, false, || false, || false,
        |response| previews.push(json(response))).unwrap());
    assert_window(&report, 4, 4, true);
    assert!(report.get("count_all_matches").flag());
    assert!(report.get("search_complete").flag());
    assert!(report.get("match_count_complete").flag());
    assert_eq!(report.get("matches_seen").number(), 9);
    assert_eq!(report.get("matching_files").number(), 3);
    assert_eq!(report.get("retained_matching_files").number(), 1);
    assert_eq!(report.get("retained_hits").number(), 2);
    assert_eq!(report.get("retained_witness_bytes").number(), 2);
    assert_eq!(report.get("payload_bytes_read").number(), 12);
    assert_eq!(report.get("stop_reason"), &Json::Null);
    assert!(!previews.is_empty());
    for preview in previews {
        let seen = preview.get("matches_seen").number();
        assert_window(&preview, 4, seen.min(4),
            seen.saturating_sub(4) > preview.get("retained_hits").number());
        assert!(!preview.get("match_count_complete").flag());
        assert!(!preview.get("selection_available").flag());
        assert!(preview.get("retained_hits").number() <= 2);
        assert!(preview.get("retained_witness_bytes").number() <= 2);
    }
}

#[test]
fn io_limits_before_the_requested_window_remain_partial_with_all_skipped_work_charged() {
    let fixture = Fixture::new();
    for path in ["a.rs", "b.rs", "c.rs"] { fixture.write(path, b"aaaa"); }
    let atlas = fixture.atlas();
    for (options, reason, bytes, examined) in [
        (AtlasStreamSearchOptions { max_scan_bytes: 5, ..options(2) }, "byte-limit", 5, 2),
        (AtlasStreamSearchOptions { max_read_calls: 1, ..options(2) }, "read-call-limit", 4, 1),
    ] {
        let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
        let report = scan(&mut service, &atlas, "aa", options,
            AtlasSearchScope::all(), 4, false, false);
        assert_window(&report, 4, 3, false);
        assert_eq!(report.get("matches_seen").number(), 3);
        assert_eq!(report.get("retained_hits").number(), 0);
        assert_eq!(report.get("payload_bytes_read").number(), bytes);
        assert_eq!(service.source_bytes_read(), bytes);
        assert_eq!(report.get("files_examined").number(), examined);
        assert_eq!(report.get("stop_reason").text(), reason);
        assert!(!report.get("search_complete").flag());
        assert!(!report.get("match_count_complete").flag(),
            "an empty later window is not exhaustive absence");
    }
}

#[test]
fn excluded_paths_do_not_consume_the_global_match_skip_or_source_budget() {
    let fixture = Fixture::new();
    fixture.write("a-excluded.bin", &vec![b'a'; 64 * 1024]);
    fixture.write("src/a.rs", b"aaaa");
    fixture.write("src/b.rs", b"aaaa");
    fixture.write("src-old/outside.rs", b"aaaaaaaa");
    let atlas = fixture.atlas();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let report = scan(&mut service, &atlas, "aa", options(2), scope, 2, false, false);
    assert_window(&report, 2, 2, true);
    assert_eq!(report.get("eligible_files").number(), 2);
    assert_eq!(report.get("excluded_files").number(), 2);
    assert_eq!(report.get("payload_bytes_read").number(), 8);
    assert_eq!(report.get("matches_seen").number(), 5);
    let rows = report.get("hits").array();
    assert_eq!(rows[0].get("path").get("hex").text(), hex(b"src/a.rs"));
    assert_eq!(rows[0].get("occurrence_id").number(), 3);
    assert_eq!(rows[1].get("path").get("hex").text(), hex(b"src/b.rs"));
    assert_eq!(rows[1].get("occurrence_id").number(), 1);
}

#[test]
fn skipped_whole_identifiers_preserve_utf16_offsets_and_original_occurrence_ids() {
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let encode = |text: &str| -> Vec<u8> { match encoding {
            "utf16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            "utf16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => text.as_bytes().to_vec(),
        } };
        let text = "\u{feff}needlework needle needle_more needle needle\n";
        let bytes = encode(text);
        let retained_at = text.find(" needle needle\n").unwrap() + 1;
        let start = encode(&text[..retained_at]).len();
        let end = start + encode("needle").len();
        let fixture = Fixture::new();
        fixture.write("a.rs", &bytes);
        let atlas = fixture.atlas();
        let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
        let report = scan(&mut service, &atlas, "needle", options(1),
            AtlasSearchScope::all(), 1, true, true);
        assert_window(&report, 1, 1, true);
        assert_eq!(report.get("mode").text(), "whole-token-occurrences");
        assert_eq!(report.get("matches_seen").number(), 3);
        assert_eq!(report.get("retained_hits").number(), 1);
        assert!(report.get("search_complete").flag());
        let row = &report.get("hits").array()[0];
        assert_eq!(row.get("occurrence_id").number(), 2);
        assert_eq!(row.get("encoding").text(), encoding);
        assert_eq!(bounds(row.get("original_range")), (start as u64, end as u64));
        let generation = report.get("query_generation").number();
        let io = (service.source_bytes_read(), service.read_calls());
        let witness = json(&service.hit(&atlas, generation, 0).unwrap());
        assert_eq!(witness.get("page_start_after").number(), 1);
        assert_eq!(witness.get("hit").get("original_hex").text(), hex(&bytes[start..end]));
        assert_eq!(witness.get("hit").get("text").text(), "needle");
        assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    }
}

#[test]
fn lowercase_later_rows_keep_their_original_spelling_and_raw_bytes() {
    let fixture = Fixture::new();
    fixture.write("a.rs", "KK".as_bytes());
    let utf16: Vec<u8> = "\u{feff}kK".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fixture.write("b.rs", &utf16);
    let atlas = fixture.atlas();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = scan(&mut service, &atlas, "k", options(2),
        AtlasSearchScope::all().with_case_sensitive(false), 1, true, false);
    assert_window(&report, 1, 1, true);
    assert_eq!(report.get("matches_seen").number(), 4);
    assert_eq!(report.get("retained_witness_bytes").number(), 3);
    assert!(report.get("match_count_complete").flag());
    let generation = report.get("query_generation").number();
    let expected = [("K", "4b", (3, 4), 2), ("k", "006b", (2, 4), 1)];
    for (position, (spelling, bytes, range, occurrence)) in expected.into_iter().enumerate() {
        let selected = json(&service.hit(&atlas, generation, position).unwrap());
        let hit = selected.get("hit");
        assert_eq!(hit.get("text").text(), spelling);
        assert_eq!(hit.get("original_hex").text(), bytes);
        assert_eq!(bounds(hit.get("original_range")), range);
        assert_eq!(hit.get("occurrence_id").number(), occurrence);
    }
}

#[test]
fn later_scans_replace_identity_while_canceled_attempts_keep_accepted_evidence() {
    let fixture = Fixture::new();
    fixture.write("a.rs", b"aaaa");
    let atlas = fixture.atlas();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let initial = scan(&mut service, &atlas, "aa", options(1),
        AtlasSearchScope::all(), 0, false, false);
    let old_generation = initial.get("query_generation").number();
    let old_revision = initial.get("hits").array()[0].get("source_revision").number();
    fixture.write("a.rs", b"xaaaa");
    let later = scan(&mut service, &atlas, "aa", options(1),
        AtlasSearchScope::all(), 1, false, false);
    let generation = later.get("query_generation").number();
    let row = &later.get("hits").array()[0];
    assert!(generation > old_generation);
    assert!(row.get("source_revision").number() > old_revision);
    assert_eq!(bounds(row.get("original_range")), (2, 4));
    assert_eq!(row.get("occurrence_id").number(), 2);
    assert!(matches!(service.hit(&atlas, old_generation, 0),
        Err(AtlasStreamSearchError::StaleQuery)));

    let io = (service.source_bytes_read(), service.read_calls());
    let cancel = Cell::new(false);
    let rejected = service.search_scoped_page_with_progress(&atlas, "aa", options(1),
        AtlasSearchScope::all(), 2, false, false, || cancel.get(), || false,
        |_| cancel.set(true));
    assert!(cancel.get());
    assert!(matches!(rejected, Err(AtlasStreamSearchError::Canceled)));
    assert_eq!(service.accepted_generation(), Some(generation));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    fixture.write("a.rs", b"current file has no matching literal");
    let retained = json(&service.hit(&atlas, generation, 0).unwrap());
    assert_eq!(retained.get("hit").get("original_hex").text(), "6161");
    assert_eq!(bounds(retained.get("hit").get("original_range")), (2, 4));
    assert_eq!(retained.get("hit").get("source_revision").number(),
        row.get("source_revision").number());
    assert_eq!(retained.get("page_start_after").number(), 1);
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn unavailable_earlier_files_do_not_consume_unobserved_matches_or_claim_complete_counts() {
    let fixture = Fixture::new();
    for path in ["a.rs", "b.rs", "c.rs"] { fixture.write(path, b"aaaa"); }
    let atlas = fixture.atlas();
    fs::remove_file(fixture.0.join("a.rs")).unwrap();
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = scan(&mut service, &atlas, "aa", options(2),
        AtlasSearchScope::all(), 2, true, false);
    assert_window(&report, 2, 2, true);
    assert_eq!(report.get("matches_seen").number(), 6);
    assert_eq!(report.get("unavailable_files").number(), 1);
    assert_eq!(report.get("matching_files").number(), 2);
    assert_eq!(report.get("payload_bytes_read").number(), 8);
    assert!(!report.get("search_complete").flag());
    assert!(!report.get("match_count_complete").flag());
    let rows = report.get("hits").array();
    assert_eq!(rows[0].get("path").get("hex").text(), hex(b"b.rs"));
    assert_eq!(rows[0].get("occurrence_id").number(), 3);
    assert_eq!(rows[1].get("path").get("hex").text(), hex(b"c.rs"));
    assert_eq!(rows[1].get("occurrence_id").number(), 1);
}
