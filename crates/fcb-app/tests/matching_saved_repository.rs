#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use saved_fixture::{entry, owner, Fixture};
use support::{Json, parse};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositorySession, SavedRepositoryError}};
use fcb_app::host::saved_repository::structure::{StructureMode, SymbolNameMode};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn field(session: &mut SavedRepositorySession, name: &str) -> Json {
    json(&session.info(|| false).unwrap()).get(name).clone()
}
fn policy(page: &Json, case_sensitive: bool, indexed: bool) {
    assert_eq!(page.get("case_sensitive").flag(), case_sensitive);
    assert_eq!(page.get("mode").text(), if case_sensitive { "exact-decoded-literal" } else { "unicode-lowercase-literal" });
    assert_eq!(page.get("index_used").flag(), indexed);
    assert_eq!(page.get("index_policy").text(), if !indexed { "direct-verified-member-scan" }
        else if case_sensitive { "exact-compatible-postings" } else { "ascii-case-variants-with-source-fallback" });
    if !indexed {
        assert_eq!(page.get("index_generation"), &Json::Null);
        assert_eq!(page.get("index_pin"), &Json::Null);
        assert_eq!(page.get("index_eliminated_files").number(), 0);
        assert_eq!(page.get("posting_entries_visited").number(), 0);
    }
}

#[test]
fn lowercase_prunes_ascii_negatives_without_losing_utf8_or_utf16_matches() {
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
    let matched = session.search_matching(3, "ana", 6, false, true, || false).unwrap();
    assert_eq!(matched.exit_code(), EXIT_OK);
    let matched = json(&matched); policy(&matched, false, true);
    assert_eq!(matched.get("hits"), direct.get("hits"));
    assert_eq!(matched.get("member_bytes_read").number(), 34);
    assert!(matched.get("member_bytes_read").number() < direct.get("member_bytes_read").number());
    assert_eq!(matched.get("index_eliminated_files").number(), 1);
    assert_eq!(matched.get("fallback_files").number(), 2);
    assert_eq!(field(&mut session, "index_generation").number(), 1);
    let reads = field(&mut session, "member_bytes_read");
    let page = json(&session.results(3, 4, 1, || false).unwrap());
    policy(&page, false, true);
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
fn compatible_pruning_cannot_turn_unknown_saved_members_into_exact_absence() {
    let f = Fixture::new(&[entry(b"a.rs", b"BANANA"),
        SnapshotEntry { path: b"b.rs", observed_bytes: 20, data: SnapshotData::Unavailable("NOT_CAPTURED") }], true);
    let (index, pin) = f.index(); let mut session = f.open(88831);
    session.attach_index(1, &index, pin, || false).unwrap();
    let response = session.search_matching(1, "missing", 0, false, true, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = json(&response); policy(&page, false, true);
    assert_eq!(page.get("matches_seen").number(), 0);
    assert_eq!(page.get("index_eliminated_files").number(), 1);
    assert_eq!(page.get("unavailable_files").number(), 1);
    assert!(!page.get("search_complete").flag()); assert!(!page.get("matches_seen_exact").flag());
}

#[test]
fn lowercase_saved_occurrences_keep_query_spelling_counts_and_original_utf16_handoffs() {
    let le: Vec<_> = "\u{feff}K K k Kx".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f = Fixture::new(&[entry(b"a.txt", &le), entry(b"b.txt", "K K".as_bytes())], true);
    let mut session = f.open(88861);
    let first = session.structure_search_matching_page(1, StructureMode::Occurrences,
        SymbolNameMode::Exact, "K", 1, 4096, false, true, 1, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_OK);
    let first = json(&first);
    assert!(!first.get("case_sensitive").flag());
    assert_eq!(first.get("name_case").text(), "unicode-lowercase");
    assert_eq!(first.get("needle").text(), "K");
    assert_eq!(first.get("matches_seen").number(), 5);
    assert_eq!(first.get("skipped_matches").number(), 1);
    assert_eq!(first.get("next_match_offset").number(), 2);
    assert!(first.get("complete").flag()); assert!(first.get("matches_seen_exact").flag());
    let hit = &first.get("hits").array()[0];
    assert_eq!(hit.get("structure").get("name").text(), "K");
    assert_eq!(hit.get("original_range").get("start").number(), 6);
    assert_eq!(hit.get("original_range").get("end").number(), 8);
    let reads = field(&mut session, "member_bytes_read");
    assert_eq!(json(&session.structure_results(1, 0, 1, || false).unwrap()), first);
    assert_eq!(field(&mut session, "member_bytes_read"), reads);

    let (mut reader, receipt) = session.open_structure_hit_reader(owner(88862), 1, 1, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("needle").text(), "K");
    assert_eq!(receipt.get("structure").get("name").text(), "K");
    assert_eq!(receipt.get("selection").get("original_hex").text(), "2a21");
    assert!(!receipt.get("live_source_reopened").flag());
    assert_eq!(reader.capture().bytes(), le);
    let later = json(&session.structure_search_matching_page(2, StructureMode::Occurrences,
        SymbolNameMode::Exact, "K", 1, 4096, false, true, 3, || false).unwrap());
    assert_eq!(later.get("hits").array()[0].get("member").number(), 1);
    assert_eq!(later.get("hits").array()[0].get("structure").get("name").text(), "K");
    assert_eq!(later.get("matches_seen").number(), 5);
    assert_eq!(later.get("next_match_offset").number(), 4);
    assert!(session.structure_search_matching(3, StructureMode::Occurrences,
        SymbolNameMode::Prefix, "K", 1, 4096, false, true, || false).is_err());
    assert!(session.structure_search_matching(4, StructureMode::Occurrences,
        SymbolNameMode::Exact, "K", 1, 4096, false, true, || true).err().unwrap().is_canceled());
    assert_eq!(json(&session.structure_results(2, 0, 1, || false).unwrap()), later);
    let counted = json(&session.structure_search_matching(5, StructureMode::Occurrences,
        SymbolNameMode::Exact, "K", 0, 4096, false, true, || false).unwrap());
    assert_eq!(counted.get("matches_seen").number(), 5);
    assert_eq!(counted.get("retained_hits").number(), 0);
    assert!(counted.get("complete").flag()); assert!(!counted.get("has_more_matches").flag());
    session.clear_structure(6, || false).unwrap(); drop(session);
    assert_eq!(json(&reader.copy_range(6, 8, || false).unwrap()).get("original_hex").text(), "2a21");
}

#[test]
fn saved_whole_token_lowercase_does_not_reuse_literal_expansion_or_source_name_as_query() {
    let text = "İ i\u{307} I\u{307} i I ß ẞ ss SS";
    let f = Fixture::new(&[entry(b"tokens.txt", text.as_bytes())], true);
    let mut session = f.open(88871);
    for (generation, needle, names) in [(1, "i", vec!["i", "I"]),
        (2, "i\u{307}", vec!["İ", "i\u{307}", "I\u{307}"]),
        (3, "ß", vec!["ß", "ẞ"]), (4, "ss", vec!["ss", "SS"])] {
        let report = json(&session.structure_search_matching(generation, StructureMode::Occurrences,
            SymbolNameMode::Exact, needle, 16, 4096, false, true, || false).unwrap());
        assert_eq!(report.get("matches_seen").number(), names.len() as u64);
        assert_eq!(report.get("hits").array().iter()
            .map(|hit| hit.get("structure").get("name").text()).collect::<Vec<_>>(), names);
        assert!(report.get("complete").flag());
    }

    let needle = "K".repeat(512); let spelling = "K".repeat(512);
    let f = Fixture::new(&[entry(b"wide.txt", spelling.as_bytes())], true);
    let mut session = f.open(88872);
    let report = json(&session.structure_search_matching(1, StructureMode::Occurrences,
        SymbolNameMode::Exact, &needle, 1, 4096, false, true, || false).unwrap());
    assert_eq!(report.get("needle").text(), needle);
    assert_eq!(report.get("hits").array()[0].get("structure").get("name").text(), spelling);
    let (_, receipt) = session.open_structure_hit_reader(owner(88873), 1, 1, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("needle").text(), needle);
    assert_eq!(receipt.get("structure").get("name").text(), spelling);
    assert_eq!(receipt.get("selection").get("original_hex").text(), hex(spelling.as_bytes()));
}
