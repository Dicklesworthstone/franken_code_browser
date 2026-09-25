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
