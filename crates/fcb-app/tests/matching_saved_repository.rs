#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use saved_fixture::{entry, owner, Fixture};
use support::{Json, parse};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositorySession, SavedRepositoryError}};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn field(session: &mut SavedRepositorySession, name: &str) -> Json {
    json(&session.info(|| false).unwrap()).get(name).clone()
}
fn policy(page: &Json, case_sensitive: bool, indexed: bool) {
    assert_eq!(page.get("case_sensitive").flag(), case_sensitive);
    assert_eq!(page.get("mode").text(), if case_sensitive { "exact-decoded-literal" } else { "unicode-lowercase-literal" });
    assert_eq!(page.get("index_used").flag(), indexed);
    assert_eq!(page.get("index_policy").text(), if indexed { "exact-compatible-postings" } else { "direct-verified-member-scan" });
    if !indexed {
        assert_eq!(page.get("index_generation"), &Json::Null);
        assert_eq!(page.get("index_pin"), &Json::Null);
        assert_eq!(page.get("index_eliminated_files").number(), 0);
        assert_eq!(page.get("posting_entries_visited").number(), 0);
    }
}

#[test]
fn lowercase_bypasses_exact_postings_without_losing_utf8_or_utf16_matches() {
    let le: Vec<_> = "\u{feff}BANANA".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let be: Vec<_> = "\u{feff}BANANA".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let f = Fixture::new(&[entry(b"a.rs", b"BANANA"), entry(b"b.rs", &le),
        entry(b"c.rs", &be), entry(b"d.rs", b"absent")], true);
    let (index, pin) = f.index(); let mut session = f.open(88801);
    let direct = json(&session.search_matching(1, "ana", 6, false, true, || false).unwrap());
    policy(&direct, false, false);
    assert!(direct.get("search_complete").flag()); assert!(direct.get("matches_seen_exact").flag());
    assert_eq!(direct.get("matches_seen").number(), 6);
    assert_eq!(direct.get("matching_files").number(), 3);
    session.attach_index(1, &index, pin, || false).unwrap();
    let exact = json(&session.search_counted(2, "ana", 6, || false).unwrap());
    policy(&exact, true, true); assert_eq!(exact.get("matches_seen").number(), 0);
    let index_reads = field(&mut session, "index_page_bytes_read");
    let matched = session.search_matching(3, "ana", 6, false, true, || false).unwrap();
    assert_eq!(matched.exit_code(), EXIT_OK);
    let matched = json(&matched); policy(&matched, false, false);
    assert_eq!(matched.get("hits"), direct.get("hits"));
    assert_eq!(field(&mut session, "index_page_bytes_read"), index_reads);
    assert_eq!(field(&mut session, "index_generation").number(), 1);
    let reads = field(&mut session, "member_bytes_read");
    let page = json(&session.results(3, 4, 1, || false).unwrap());
    policy(&page, false, false);
    assert_eq!(page.get("matches_seen").number(), 6);
    assert_eq!(page.get("hits").array()[0].get("hit_id").number(), 5);
    assert_eq!(field(&mut session, "member_bytes_read"), reads);
    for (id, bytes, start, end) in [
        (1, b"BANANA".as_slice(), 1, 4), (3, le.as_slice(), 4, 10), (5, be.as_slice(), 4, 10),
    ] {
        let (mut reader, response) = session.open_hit_reader(owner(88801 + id), 3, id, || false).unwrap();
        let receipt = json(&response); let selection = receipt.get("selection");
        assert!(!selection.get("case_sensitive").flag());
        assert_eq!(selection.get("mode").text(), "unicode-lowercase-literal");
        assert_eq!(selection.get("text").text(), "ANA");
        assert_eq!(selection.get("original_range").get("start").number(), start as u64);
        assert_eq!(selection.get("original_range").get("end").number(), end as u64);
        assert_eq!(selection.get("original_hex").text(), hex(&bytes[start..end]));
        assert_eq!(reader.capture().bytes(), bytes);
        assert_eq!(json(&reader.copy_range(start as u64, end as u64, || false).unwrap())
            .get("original_hex").text(), hex(&bytes[start..end]));
    }
    let exact = json(&session.search_matching(4, "ANA", 6, true, true, || false).unwrap());
    policy(&exact, true, true);
    assert_eq!(exact.get("matches_seen").number(), 6);
}

#[test]
fn lowercase_expansions_keep_complete_original_scalars_without_folding_or_normalizing() {
    let utf8 = "\u{130} i\u{307} I \u{1e9e} \u{df} \u{e9} e\u{301}";
    let utf16: Vec<_> = "\u{feff}\u{130}".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f = Fixture::new(&[entry(b"a.rs", utf8.as_bytes()), entry(b"b.rs", &utf16)], true);
    let mut session = f.open(88811);
    let response = json(&session.search_matching(1, "i", 8, false, true, || false).unwrap());
    assert_eq!(response.get("matches_seen").number(), 4);
    for (hit_id, text, original) in [(1, "\u{130}", vec![0xc4, 0xb0]), (4, "\u{130}", vec![0x30, 0x01])] {
        let (_, receipt) = session.open_hit_reader(owner(88811 + hit_id), 1, hit_id, || false).unwrap();
        let selection = json(&receipt).get("selection").clone();
        assert_eq!(selection.get("text").text(), text);
        assert_eq!(selection.get("original_hex").text(), hex(&original));
    }
    let response = json(&session.search_matching(2, "\u{307}", 8, false, true, || false).unwrap());
    assert_eq!(response.get("matches_seen").number(), 3);
    let (_, receipt) = session.open_hit_reader(owner(88816), 2, 1, || false).unwrap();
    assert_eq!(json(&receipt).get("selection").get("text").text(), "\u{130}");
    // Scalar lowercase maps capital sharp S to sharp S, not to "ss".
    let response = json(&session.search_matching(3, "SS", 0, false, true, || false).unwrap());
    assert_eq!(response.get("matches_seen").number(), 0);
    let response = json(&session.search_matching(4, "\u{df}", 8, false, true, || false).unwrap());
    assert_eq!(response.get("matches_seen").number(), 2);
    // Canonically equivalent sequences remain distinct.
    let response = json(&session.search_matching(5, "\u{e9}", 8, false, true, || false).unwrap());
    assert_eq!(response.get("matches_seen").number(), 1);
    let response = json(&session.search_matching(6, "e\u{301}", 8, false, true, || false).unwrap());
    assert_eq!(response.get("matches_seen").number(), 1);
}

#[test]
fn matching_policy_is_frozen_across_counts_paging_failed_replacement_and_activation() {
    let f = Fixture::new(&[entry(b"a.rs", b"BANANA banana")], true);
    let mut session = f.open(88821);
    let original = json(&session.search_matching(1, "ana", 1, false, true, || false).unwrap());
    policy(&original, false, false);
    assert_eq!(original.get("matches_seen").number(), 4);
    assert!(original.get("truncated").flag()); assert!(original.get("search_complete").flag());
    let before = json(&session.results(1, 0, 64, || false).unwrap());
    let mut probe = f.open(88822); let mut polls = 0;
    probe.search_matching(1, "ana", 1, true, true, || { polls += 1; false }).unwrap();
    let mut attempt_polls = 0;
    assert!(session.search_matching(2, "ana", 1, true, true, || {
        attempt_polls += 1; attempt_polls == polls
    }).err().unwrap().is_canceled());
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), before);
    assert!(session.search_matching(3, "ana", 0, true, false, || false).is_err());
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), before);
    let (mut reader, receipt) = session.open_hit_reader(owner(88823), 1, 1, || false).unwrap();
    assert_eq!(json(&receipt).get("selection").get("text").text(), "ANA");
    let counted = json(&session.search_matching(4, "ana", 0, false, true, || false).unwrap());
    assert_eq!(counted.get("matches_seen").number(), 4);
    assert_eq!(counted.get("retained_hits").number(), 0);
    assert!(session.open_hit_reader(owner(88824), 1, 1, || false)
        .err().is_some_and(|error| error == SavedRepositoryError::StaleQuery));
    assert!(session.open_hit_reader(owner(88824), 4, 1, || false).is_err());
    let stopped = session.search_matching(5, "ana", 1, false, false, || false).unwrap();
    assert_eq!(stopped.exit_code(), EXIT_PARTIAL);
    let stopped = json(&stopped);
    assert!(!stopped.get("matches_seen_exact").flag());
    assert_eq!(stopped.get("matches_seen").number(), 2);
    drop(session);
    std::fs::rename(&f.path, f.root.join("moved-matching.fcbs")).unwrap();
    assert_eq!(json(&reader.copy_range(1, 4, || false).unwrap()).get("original_hex").text(), "414e41");
}

#[test]
fn incompatible_postings_cannot_turn_unknown_saved_members_into_exact_absence() {
    let f = Fixture::new(&[entry(b"a.rs", b"BANANA"),
        SnapshotEntry { path: b"b.rs", observed_bytes: 20, data: SnapshotData::Unavailable("NOT_CAPTURED") }], true);
    let (index, pin) = f.index(); let mut session = f.open(88831);
    session.attach_index(1, &index, pin, || false).unwrap();
    let response = session.search_matching(1, "missing", 0, false, true, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = json(&response); policy(&page, false, false);
    assert_eq!(page.get("matches_seen").number(), 0);
    assert_eq!(page.get("unavailable_files").number(), 1);
    assert!(!page.get("search_complete").flag()); assert!(!page.get("matches_seen_exact").flag());
}
