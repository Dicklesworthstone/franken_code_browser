#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use support::{parse, Json};
use saved_fixture::{entry, owner, Fixture};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::search::snapshot::{SnapshotEntry, SnapshotData};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskCommand},
    saved_repository::{SavedRepositorySession, desk::{SavedExpression, SavedExpressionOptions}}};

fn json(r: &HostResponse) -> Json { parse(r.as_str().as_bytes()).unwrap() }
fn query(s: &mut SavedRepositorySession, generation: u64, text: &str) -> SavedExpression {
    SavedExpression::prepare(s, generation, text, Default::default(), || false).unwrap()
}
fn desk() -> DeskSession { DeskSession::new(owner(66102), DeskLimits::default()).unwrap() }

#[test]
fn conjunction_and_exclusion_apply_to_whole_members_not_matching_lines() {
    let mut distant = b"needle\n".to_vec(); distant.extend(vec![b'x'; 96 * 1024]); distant.extend_from_slice(b"\nrequired");
    let f = Fixture::new(&[entry(b"a.rs", &distant), entry(b"b.rs", b"needle\nrequired\nforbidden"),
        entry(b"c.rs", b"needle"), entry(b"d.rs", b"required")], true);
    let mut s = f.open(66101); let result = query(&mut s, 1, "needle required -forbidden");
    assert!(result.is_complete()); assert_eq!(result.retained_hits(), 1);
    assert_eq!(result.stats().members_loaded, 4);
    assert!(result.stats().scanned_bytes > distant.len() as u64);
    let page = json(&result.page(&mut s, 1, 0, 128, || false).unwrap());
    assert_eq!(page.get("predicate_scope").text(), "whole-captured-member");
    assert_eq!(page.get("hits").array()[0].get("member").number(), 0);
    assert_eq!(page.get("retained_source_payload_bytes").number(), 0);
}

#[test]
fn metadata_filters_exclude_unread_members_without_source_work() {
    let f = Fixture::new(&[entry(b"docs/a.rs", b"needle required"), entry(b"src/a.rs", b"needle required"),
        entry(b"src/b.py", b"needle required"), SnapshotEntry { path: b"src/tests/c.rs", observed_bytes: 100,
            data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let mut s = f.open(66101);
    let result = query(&mut s, 1, "needle required path:src/ -path:tests lang:rust");
    assert!(result.is_complete()); assert_eq!(result.retained_hits(), 1);
    assert_eq!(result.stats().metadata_excluded, 3);
    assert_eq!(result.stats().unavailable_files, 0); assert_eq!(result.stats().members_loaded, 1);
}

#[test]
fn scan_limit_cannot_turn_unproven_exclusion_into_a_matching_document() {
    let mut bytes = b"needle\n".to_vec(); bytes.extend(vec![b'x'; 8192]); bytes.extend_from_slice(b"forbidden");
    let f = Fixture::new(&[entry(b"a.rs", &bytes)], true); let mut s = f.open(66101);
    let result = SavedExpression::prepare(&mut s, 1, "needle -forbidden",
        SavedExpressionOptions { max_hits: 10, max_scan_bytes: 4096 }, || false).unwrap();
    assert!(!result.is_complete()); assert_eq!(result.retained_hits(), 0);
    assert!(result.stats().scanned_bytes <= 4096);
    let response = result.page(&mut s, 1, 0, 10, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = json(&response); assert_eq!(page.get("state").text(), "work-limit");
    assert!(!page.get("matches_seen_exact").flag());
}

#[test]
fn unavailable_sources_and_incomplete_discovery_keep_negative_results_partial() {
    let f = Fixture::new(&[entry(b"a.rs", b"nothing"), SnapshotEntry { path: b"b.rs", observed_bytes: 9,
        data: SnapshotData::Unavailable("MISSING") }], true);
    let mut s = f.open(66101); let result = query(&mut s, 1, "needle");
    assert!(!result.is_complete()); assert_eq!(result.stats().unavailable_files, 1);
    assert_eq!(result.page(&mut s, 1, 0, 10, || false).unwrap().exit_code(), EXIT_PARTIAL);
    let f = Fixture::new(&[entry(b"a.rs", b"nothing")], false); let mut s = f.open(66103);
    assert!(!query(&mut s, 1, "needle").is_complete());
}

#[test]
fn exact_result_cap_and_existence_probe_keep_lookahead_semantics() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle"), entry(b"b.rs", b"needle needle")], true);
    let mut s = f.open(66101);
    let opts = |max_hits| SavedExpressionOptions { max_hits, max_scan_bytes: 10000 };
    let exact = SavedExpression::prepare(&mut s, 1, "needle path:a.rs", opts(1), || false).unwrap();
    assert!(exact.is_complete()); assert_eq!(exact.retained_hits(), 1);
    let limited = SavedExpression::prepare(&mut s, 2, "needle", opts(1), || false).unwrap();
    assert!(!limited.is_complete()); assert_eq!(limited.retained_hits(), 1);
    assert!(json(&limited.page(&mut s, 2, 0, 10, || false).unwrap()).get("truncated").flag());
    let probe = SavedExpression::prepare(&mut s, 3, "needle", opts(0), || false).unwrap();
    assert!(!probe.is_complete()); assert_eq!(probe.retained_hits(), 0);
    assert!(SavedExpression::prepare(&mut s, 4, "absent", opts(0), || false).unwrap().is_complete());
}

#[test]
fn paging_keeps_ids_and_performs_no_archive_member_reads() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle needle needle required")], true);
    let mut s = f.open(66101); let result = query(&mut s, 1, "needle required");
    let before = json(&s.info(|| false).unwrap()).get("member_bytes_read").number();
    for n in 0..3 {
        let page = json(&result.page(&mut s, 1, n, 1, || false).unwrap());
        assert_eq!(page.get("hits").array()[0].get("hit_id").number(), n as u64 + 1);
    }
    assert_eq!(json(&s.info(|| false).unwrap()).get("member_bytes_read").number(), before);
    assert!(result.page(&mut s, 1, 4, 1, || false).is_err());
    assert!(result.page(&mut s, 2, 0, 1, || false).is_err());
}

#[test]
fn expression_witnesses_reuse_literal_sources_and_survive_offline_checkpoints() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required needle")], true); let mut s = f.open(66101);
    let mut d = desk(); s.search(7, "needle", 10, || false).unwrap();
    let first = s.open_hit_desk(&mut d, 0, 1, 7, 1, || false).unwrap();
    let result = query(&mut s, 7, "needle required");
    let opened = result.open_hit_desk(&mut s, &mut d, 1, 2, 7, 2, || false).unwrap();
    assert!(opened.imported.reused_capture); assert_eq!(first.imported.file, opened.imported.file);
    assert_eq!(d.model().retained_source_count(), 1);
    let receipt = json(&s.desk_open_response(&d, &opened).unwrap());
    assert_eq!(receipt.get("selection_namespace").text(), "saved-expression");
    assert_eq!(receipt.get("expression_generation").number(), 7);
    let pane = opened.imported.change.active.unwrap();
    assert_eq!(d.model().selected_bytes(pane, 2).unwrap(), b"needle");
    d.apply(2, 3, DeskCommand::Bookmark { pane, label: "matched entire expression".into() }, || false).unwrap();
    let saved = f.root.join("desk.fcbk"); assert_eq!(d.save_checkpoint(3, &saved, 1000, || false).exit_code(), EXIT_OK);
    drop(s); drop(result); drop(d);
    let mut reopened = desk(); reopened.restore_checkpoint_file(0, 1, &saved, || false).unwrap();
    let pane = reopened.model().active().unwrap(); assert_eq!(reopened.model().selected_bytes(pane, 1).unwrap(), b"needle");
    assert_eq!(reopened.model().bookmarks()[0].label(), "matched entire expression");
}

#[test]
fn utf16_quoted_primary_and_raw_names_preserve_original_selection() {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend("🦀 needle phrase\r\nrequired".encode_utf16().flat_map(u16::to_le_bytes));
    let f = Fixture::new(&[entry(b"raw-\xff.rs", &bytes)], true); let mut s = f.open(66101); let mut d = desk();
    let result = query(&mut s, 1, "\"needle phrase\" required lang:rs"); assert!(result.is_complete());
    let opened = result.open_hit_desk(&mut s, &mut d, 0, 1, 1, 1, || false).unwrap();
    let selected: Vec<_> = "needle phrase".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(d.model().selected_bytes(opened.imported.change.active.unwrap(), 1).unwrap(), selected);
}

#[test]
fn attached_disk_index_and_literal_query_are_not_replaced_or_misused() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required"), entry(b"b.rs", b"needle forbidden required")], true);
    let (path, pin) = f.index(); let mut s = f.open(66101);
    s.attach_index(9, &path, pin, || false).unwrap(); s.search(7, "needle", 10, || false).unwrap();
    let before = json(&s.info(|| false).unwrap()).get("index_page_loads").number();
    let result = query(&mut s, 7, "needle required -forbidden");
    assert_eq!(result.retained_hits(), 1); assert_eq!(s.accepted_generation(), Some(7)); assert_eq!(s.index_generation(), Some(9));
    assert!(!json(&result.page(&mut s, 7, 0, 10, || false).unwrap()).get("index_used").flag());
    assert_eq!(json(&s.info(|| false).unwrap()).get("index_page_loads").number(), before);
    assert_eq!(json(&s.results(7, 0, 10, || false).unwrap()).get("retained_hits").number(), 2);
}

#[test]
fn changed_archive_and_foreign_owner_cannot_rebind_predicate_evidence() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required")], true); let mut s = f.open(66101); let mut d = desk();
    let result = query(&mut s, 1, "needle required");
    let mut other = f.open(66103);
    assert!(result.open_hit_desk(&mut other, &mut d, 0, 1, 1, 1, || false).is_err());
    assert!(result.open_hit_desk(&mut s, &mut d, 0, 1, 2, 1, || false).is_err());
    let at = f.bytes.windows(b"needle required".len()).position(|s| s == b"needle required").unwrap();
    let mut file = OpenOptions::new().write(true).open(&f.path).unwrap();
    file.seek(SeekFrom::Start(at as u64 + 7)).unwrap(); file.write_all(b"CHANGED!").unwrap(); file.flush().unwrap();
    assert!(result.open_hit_desk(&mut s, &mut d, 0, 2, 1, 1, || false).is_err());
    assert_eq!(d.model().revision(), 0); assert_eq!(d.model().retained_source_count(), 0);
}

#[test]
fn every_observed_prepare_cancellation_keeps_existing_rows_and_reader() {
    let f = Fixture::new(&[entry(b"a.rs", b"old new required")], true); let mut s = f.open(66101); let mut d = desk();
    let old = query(&mut s, 1, "old required");
    old.open_hit_desk(&mut s, &mut d, 0, 1, 1, 1, || false).unwrap();
    let mut polls = 0;
    drop(SavedExpression::prepare(&mut s, 2, "new required", Default::default(), || { polls += 1; false }).unwrap());
    for n in 1..=polls {
        let mut at = 0;
        let stopped = SavedExpression::prepare(&mut s, n as u64 + 2, "new required", Default::default(), || { at += 1; at == n });
        assert!(matches!(stopped, Err(e) if e.is_canceled()), "checkpoint {n}");
        assert_eq!(old.retained_hits(), 1); assert_eq!(d.model().revision(), 1);
        assert_eq!(d.model().selected_bytes(d.model().active().unwrap(), 1).unwrap(), b"old");
    }
}

#[test]
fn invalid_grammar_oversized_queries_and_case_sensitive_negatives_remain_explicit() {
    let f = Fixture::new(&[entry(b"a.rs", b"Needle required")], true); let mut s = f.open(66101);
    for q in ["\"unterminated", "regex:needle", "-only-exclusion", "path:src/"] {
        assert!(SavedExpression::prepare(&mut s, 1, q, Default::default(), || false).is_err());
    }
    assert!(SavedExpression::prepare(&mut s, 1, &"x".repeat(1025), Default::default(), || false).is_err());
    let result = query(&mut s, 1, "needle required"); assert!(result.is_complete()); assert_eq!(result.retained_hits(), 0);
    assert!(json(&result.page(&mut s, 1, 0, 10, || false).unwrap()).get("case_sensitive").flag());
    let _ = fs::metadata(&f.path).unwrap(); // The archive remains available after all refusals.
}

#[test]
fn retained_expression_attempts_recover_without_replacing_literal_results_or_readers() {
    let f = Fixture::new(&[entry(b"source.rs", b"needle required needle forbidden")], true);
    let mut s = f.open(66201);
    let (index, pin) = f.index();
    s.attach_index(1, &index, pin, || false).unwrap();
    s.search(1, "forbidden", 10, || false).unwrap();
    let first = s.expression_search(1, "needle required", 10, 10000, || false).unwrap();
    assert_eq!(json(&first).get("expression_generation").number(), 1);
    assert_eq!(json(&first).get("retained_hits").number(), 2);
    let (mut reader, opened) = s.open_expression_hit_reader(owner(66202), 1, 2, || false).unwrap();
    let receipt = json(&opened);
    assert_eq!(receipt.get("command").text(), "expression-open-reader");
    assert_eq!(receipt.get("selection_namespace").text(), "saved-expression");
    assert_eq!(receipt.get("expression_generation").number(), 1);
    assert_eq!(receipt.get("primary_needle").text(), "needle");
    assert_eq!(receipt.get("selection").get("hit_id").number(), 2);
    assert_eq!(receipt.get("selection").get("expression_generation").number(), 1);
    assert_eq!(receipt.get("selection").get("original_range").get("start").number(), 16);
    assert_eq!(receipt.get("selection").get("original_hex").text(), "6e6565646c65");
    assert_eq!(receipt.get("reader").get("capture_origin").text(), "host-supplied");
    assert_eq!(receipt.get("reader").get("initial_read_calls").number(), 0);
    assert!(!receipt.get("live_source_reopened").flag());

    let reads = json(&s.info(|| false).unwrap()).get("member_read_calls").number();
    let page = json(&s.expression_results(1, 1, 1, || false).unwrap());
    assert_eq!(page.get("hits").array()[0].get("hit_id").number(), 2);
    assert_eq!(json(&s.info(|| false).unwrap()).get("member_read_calls").number(), reads);
    assert!(s.expression_search(2, "regex:needle", 10, 10000, || false).is_err());
    assert_eq!(s.last_expression_attempt(), 2);
    assert!(s.expression_search(2, "needle", 10, 10000, || false).is_err());
    assert!(matches!(s.expression_search(3, "forbidden", 10, 10000, || true), Err(e) if e.is_canceled()));
    let recovery = json(&s.info(|| false).unwrap());
    assert_eq!(recovery.get("accepted_expression_generation").number(), 1);
    assert_eq!(recovery.get("last_expression_attempt").number(), 3);
    assert_eq!(s.accepted_generation(), Some(1));
    assert_eq!(s.index_generation(), Some(1));
    assert_eq!(json(&s.results(1, 0, 10, || false).unwrap()).get("retained_hits").number(), 1);
    assert!(matches!(s.clear_expression(4, || true), Err(e) if e.is_canceled()));
    assert_eq!(s.accepted_expression_generation(), Some(1));
    let cleared = json(&s.clear_expression(5, || false).unwrap());
    assert_eq!(cleared.get("command").text(), "expression-clear");
    assert_eq!(cleared.get("accepted_expression_generation"), &Json::Null);
    assert_eq!(s.accepted_expression_generation(), None);
    assert_eq!(s.last_expression_attempt(), 5);
    assert!(s.expression_results(1, 0, 10, || false).is_err());
    assert!(s.open_expression_hit_reader(owner(66203), 1, 1, || false).is_err());
    assert_eq!(s.accepted_generation(), Some(1));
    drop(s);
    assert_eq!(reader.capture().bytes(), b"needle required needle forbidden");
    assert_eq!(json(&reader.copy_range(16, 22, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn expression_reader_witnesses_preserve_utf16_overlaps_and_raw_archived_labels() {
    for big_endian in [false, true] {
        let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
        for unit in "🦀 banana\r\nrequired \u{feff}".encode_utf16() {
            bytes.extend(if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
        }
        let f = Fixture::new(&[entry(b"raw-\xff.rs", &bytes)], true);
        let mut s = f.open(66301);
        let result = query(&mut s, 9, "ana required lang:rs");
        assert_eq!(result.retained_hits(), 2);
        for (id, start, end) in [(1, 10, 16), (2, 14, 20)] {
            let (mut reader, response) = result.open_hit_reader(&mut s, owner(66301 + id), 9, id, || false).unwrap();
            let opened = json(&response);
            assert_eq!(reader.capture().bytes(), bytes);
            assert_eq!(opened.get("selection").get("original_range").get("start").number(), start);
            assert_eq!(opened.get("selection").get("original_range").get("end").number(), end);
            let expected = if big_endian { "0061006e0061" } else { "61006e006100" };
            assert_eq!(opened.get("selection").get("original_hex").text(), expected);
            assert_eq!(json(&reader.copy_range(start, end, || false).unwrap()).get("original_hex").text(), expected);
            assert_eq!(opened.get("reader").get("path").get("hex").text(), "7261772dff2e7273");
            assert!(opened.get("window").get("text").text().contains("🦀 banana\r\nrequired \u{feff}"));
            assert_eq!(opened.get("source_digest"), opened.get("reader").get("capture_sha256"));
        }
    }
}

#[test]
fn expression_reader_rejects_foreign_stale_or_damaged_predicate_evidence() {
    let mut source = b"needle\n".to_vec(); source.extend(vec![b'x'; 20 * 1024]); source.extend_from_slice(b"required");
    let f = Fixture::new(&[entry(b"a.rs", &source)], true);
    let mut s = f.open(66401);
    let query = query(&mut s, 1, "needle required -forbidden");
    let mut other = f.open(66402);
    assert!(query.open_hit_reader(&mut other, owner(66403), 1, 1, || false).is_err());
    let reads = json(&s.info(|| false).unwrap()).get("member_read_calls").number();
    assert!(query.open_hit_reader(&mut s, owner(66401), 1, 1, || false).is_err());
    assert!(query.open_hit_reader(&mut s, owner(66403), 2, 1, || false).is_err());
    for id in [0, 2, u64::MAX] {
        assert!(query.open_hit_reader(&mut s, owner(66403), 1, id, || false).is_err());
    }
    assert_eq!(json(&s.info(|| false).unwrap()).get("member_read_calls").number(), reads);
    let (reader, _) = query.open_hit_reader(&mut s, owner(66403), 1, 1, || false).unwrap();
    let at = f.bytes.windows(source.len()).position(|bytes| bytes == source).unwrap();
    let mut file = OpenOptions::new().write(true).open(&f.path).unwrap();
    file.seek(SeekFrom::Start((at + source.len() - 8) as u64)).unwrap();
    file.write_all(b"REQUIRED").unwrap(); file.flush().unwrap();
    // The primary occurrence is unchanged. Revalidating only its context
    // would wrongly reuse the now-invalid whole-member predicate proof.
    assert!(query.open_hit_reader(&mut s, owner(66404), 1, 1, || false).is_err());
    assert_eq!(reader.capture().bytes(), source);
    assert_eq!(query.retained_hits(), 1);
}

#[test]
fn every_retained_expression_publication_and_reader_cancellation_preserves_old_state() {
    let f = Fixture::new(&[entry(b"a.rs", b"old new required")], true);
    let mut s = f.open(66501);
    s.expression_search(1, "old required", 10, 10000, || false).unwrap();
    let (reader, _) = s.open_expression_hit_reader(owner(66502), 1, 1, || false).unwrap();
    let mut counter = f.open(66503);
    counter.expression_search(1, "old required", 10, 10000, || false).unwrap();
    let mut polls = 0;
    counter.expression_search(2, "new required", 10, 10000, || { polls += 1; false }).unwrap();
    for stop in 1..=polls {
        let mut at = 0;
        let attempt = stop as u64 + 1;
        assert!(matches!(s.expression_search(attempt, "new required", 10, 10000,
            || { at += 1; at == stop }), Err(e) if e.is_canceled()), "replacement checkpoint {stop}");
        assert_eq!(s.last_expression_attempt(), attempt);
        assert_eq!(s.accepted_expression_generation(), Some(1));
        assert_eq!(json(&s.expression_results(1, 0, 10, || false).unwrap()).get("primary_needle").text(), "old");
    }
    let mut clear_polls = 0;
    counter.clear_expression(3, || { clear_polls += 1; false }).unwrap();
    for stop in 1..=clear_polls {
        let attempt = s.last_expression_attempt() + 1;
        let mut at = 0;
        assert!(matches!(s.clear_expression(attempt, || { at += 1; at == stop }), Err(e) if e.is_canceled()),
            "clear checkpoint {stop}");
        assert_eq!(s.accepted_expression_generation(), Some(1));
    }
    let mut open_polls = 0;
    drop(s.open_expression_hit_reader(owner(66504), 1, 1, || { open_polls += 1; false }).unwrap());
    for stop in 1..=open_polls {
        let mut at = 0;
        assert!(matches!(s.open_expression_hit_reader(owner(66600 + stop as u64), 1, 1,
            || { at += 1; at == stop }), Err(e) if e.is_canceled()), "open checkpoint {stop}");
        assert_eq!(s.accepted_expression_generation(), Some(1));
        assert_eq!(reader.capture().bytes(), b"old new required");
    }
    let (later, _) = s.open_expression_hit_reader(owner(66701), 1, 1, || false).unwrap();
    assert_eq!(later.capture().bytes(), reader.capture().bytes());
}

#[test]
fn retained_partial_expression_rows_still_open_only_their_verified_witnesses() {
    let mut large = b"needle\n".to_vec(); large.extend(vec![b'x'; 32 * 1024]); large.extend_from_slice(b"required");
    let f = Fixture::new(&[entry(b"a.rs", b"needle required"), entry(b"b.rs", &large)], true);
    let mut s = f.open(66801);
    let response = s.expression_search(1, "needle required", 10, 4096, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = json(&response);
    assert_eq!(page.get("state").text(), "work-limit");
    assert_eq!(page.get("retained_hits").number(), 1);
    assert!(!page.get("matches_seen_exact").flag());
    let (reader, opened) = s.open_expression_hit_reader(owner(66802), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle required");
    assert_eq!(json(&opened).get("selection_namespace").text(), "saved-expression");
    assert!(s.open_expression_hit_reader(owner(66803), 1, 2, || false).is_err());
}
