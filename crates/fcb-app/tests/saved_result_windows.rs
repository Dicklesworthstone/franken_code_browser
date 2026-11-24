#![forbid(unsafe_code)]
#![cfg(unix)]

//! Public-host result windows over real FCBS archives. A later bounded window
//! is a new accepted query over the pinned archive, not metadata pagination or
//! permission to reopen an archived workspace path.

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use saved_fixture::{entry, owner, Fixture};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositoryError, SavedRepositorySession,
    structure::{StructureMode, SymbolNameMode}}};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    format!("\u{feff}{text}").encode_utf16().flat_map(|unit|
        if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn rows(page: &Json) -> Vec<(u64, u64, u64)> {
    page.get("hits").array().iter().map(|hit| (hit.get("member").number(),
        hit.get("original_range").get("start").number(),
        hit.get("original_range").get("end").number())).collect()
}
fn window(page: &Json, offset: u64, skipped: u64, next: Option<u64>) {
    window_with_evidence(page, offset, skipped, next.is_some(), next);
}
fn window_with_evidence(page: &Json, offset: u64, skipped: u64, has_more: bool, next: Option<u64>) {
    assert_eq!(page.get("result_offset").number(), offset);
    assert_eq!(page.get("skipped_matches").number(), skipped);
    assert_eq!(page.get("has_more_matches").flag(), has_more);
    match next {
        Some(next) => assert_eq!(page.get("next_match_offset").number(), next),
        None => assert_eq!(page.get("next_match_offset"), &Json::Null),
    }
    assert_eq!(page.get("retained_source_payload_bytes").number(), 0);
}
fn member_reads(session: &mut SavedRepositorySession) -> u64 {
    json(&session.info(|| false).unwrap()).get("member_bytes_read").number()
}

#[test]
fn literal_windows_reach_beyond_4096_and_keep_the_open_archive_after_path_replacement() {
    let bytes = b"needle ".repeat(4_100);
    let f = Fixture::new(&[entry(b"a.rs", &bytes), entry(b"b.rs", b"needle")], true);
    let replacement = Fixture::new(&[entry(b"a.rs", b"replacement must not be read")], true);
    let mut session = f.open(88901);
    let first = session.search_matching_page(1, "needle", 4096, true, false, 0, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_PARTIAL);
    let first = json(&first); window(&first, 0, 0, Some(4096));
    assert_eq!(first.get("retained_hits").number(), 4096);
    assert_eq!(first.get("matches_seen").number(), 4097);
    assert_eq!(first.get("next_offset").number(), 64);
    let before = member_reads(&mut session);
    let metadata = json(&session.results(1, 4095, 1, || false).unwrap());
    assert_eq!(metadata.get("hits").array()[0].get("hit_id").number(), 4096);
    assert_eq!(member_reads(&mut session), before);

    // The session's open descriptor, not its original pathname, is authority.
    fs::rename(&f.path, f.root.join("retained.fcbs")).unwrap();
    fs::write(&f.path, &replacement.bytes).unwrap();
    let middle = json(&session.search_matching_page(2, "needle", 3, true, false, 4096, || false).unwrap());
    window(&middle, 4096, 4096, Some(4099));
    assert_eq!(rows(&middle), [(0, 28672, 28678), (0, 28679, 28685), (0, 28686, 28692)]);
    assert_eq!(middle.get("hits").array()[0].get("hit_id").number(), 1);
    assert_eq!(middle.get("next_offset"), &Json::Null);
    assert_eq!(middle.get("archive_digest"), first.get("archive_digest"));
    assert_eq!(session.results(1, 0, 1, || false).err(), Some(SavedRepositoryError::StaleQuery));
    let (mut reader, opened) = session.open_hit_reader(owner(88902), 2, 2, || false).unwrap();
    let opened = json(&opened);
    assert!(!opened.get("live_source_reopened").flag());
    assert_eq!(opened.get("selection").get("query_generation").number(), 2);
    assert_eq!(opened.get("selection").get("original_range").get("start").number(), 28679);
    assert_eq!(reader.capture().bytes(), bytes);

    let last = session.search_matching_page(3, "needle", 2, true, false, 4099, || false).unwrap();
    assert_eq!(last.exit_code(), EXIT_OK);
    let last = json(&last); window(&last, 4099, 4099, None);
    assert_eq!(rows(&last), [(0, 28693, 28699), (1, 0, 6)]);
    assert_eq!(last.get("matches_seen").number(), 4101);
    assert!(last.get("search_complete").flag()); assert!(last.get("truncated").flag());
    assert_eq!(session.open_hit_reader(owner(88903), 2, 2, || false).err(), Some(SavedRepositoryError::StaleQuery));
    drop(session);
    assert_eq!(json(&reader.copy_range(28679, 28685, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn indexed_and_lowercase_windows_keep_counts_original_utf16_ranges_and_large_offsets() {
    let le = utf16("BANANA", true); let be = utf16("BANANA", false);
    let f = Fixture::new(&[entry(b"a.rs", b"BANANA"), entry(b"b.rs", &le),
        entry(b"c.rs", &be), entry(b"d.rs", b"unrelated")], true);
    let (index, pin) = f.index(); let mut session = f.open(88911);
    let direct = json(&session.search_matching_page(1, "ANA", 2, true, true, 3, || false).unwrap());
    window(&direct, 3, 3, Some(5)); assert_eq!(rows(&direct), [(1, 8, 14), (2, 4, 10)]);
    assert_eq!(direct.get("matches_seen").number(), 6); assert!(direct.get("matches_seen_exact").flag());
    session.attach_index(1, &index, pin, || false).unwrap();
    let indexed = json(&session.search_matching_page(2, "ANA", 2, true, true, 3, || false).unwrap());
    assert!(indexed.get("index_used").flag()); assert_eq!(indexed.get("hits"), direct.get("hits"));
    let index_reads = json(&session.info(|| false).unwrap()).get("index_page_bytes_read").clone();
    let lowercase = json(&session.search_matching_page(3, "ana", 2, false, true, 3, || false).unwrap());
    window(&lowercase, 3, 3, Some(5)); assert!(lowercase.get("index_used").flag());
    assert_eq!(lowercase.get("index_eliminated_files").number(), 1);
    assert_eq!(lowercase.get("fallback_files").number(), 2);
    assert_eq!(lowercase.get("hits"), direct.get("hits"));
    assert_eq!(json(&session.info(|| false).unwrap()).get("index_page_bytes_read"), &index_reads);
    let (reader, receipt) = session.open_hit_reader(owner(88912), 3, 1, || false).unwrap();
    let receipt = json(&receipt); let selection = receipt.get("selection");
    assert_eq!(selection.get("text").text(), "ANA"); assert!(!selection.get("case_sensitive").flag());
    assert_eq!(selection.get("original_hex").text(), hex(&le[8..14]));
    assert_eq!(reader.capture().bytes(), le);
    let last = json(&session.search_matching_page(4, "ana", 2, false, false, 4, || false).unwrap());
    window(&last, 4, 4, None); assert!(last.get("search_complete").flag());
    assert_eq!(last.get("matches_seen").number(), 6); assert_eq!(last.get("retained_hits").number(), 2);
    let offset = (1u64 << 53) + 7;
    let beyond = json(&session.search_matching_page(5, "ana", 1, false, false, offset, || false).unwrap());
    window(&beyond, offset, 6, None); assert!(beyond.get("hits").array().is_empty());
    assert!(beyond.get("search_complete").flag());
}

#[test]
fn missing_and_malformed_members_do_not_consume_offsets_or_turn_partial_counts_exact() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana\xff"),
        SnapshotEntry { path: b"b.rs", observed_bytes: 90, data: SnapshotData::Unavailable("NOT_CAPTURED") },
        entry(b"c.rs", b"banana")], true);
    let mut session = f.open(88921);
    let response = session.search_matching_page(1, "ana", 1, true, true, 1, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let page = json(&response); window(&page, 1, 1, None);
    assert_eq!(rows(&page), [(2, 3, 6)]); assert_eq!(page.get("matches_seen").number(), 2);
    assert_eq!(page.get("matching_files").number(), 1);
    assert_eq!(page.get("unsupported_files").number(), 1); assert_eq!(page.get("unavailable_files").number(), 1);
    assert!(!page.get("search_complete").flag()); assert!(!page.get("matches_seen_exact").flag());
    let only = json(&session.search_matching_page(2, "ana", 0, true, true, 1, || false).unwrap());
    window_with_evidence(&only, 1, 1, true, None); assert_eq!(only.get("matches_seen").number(), 2);
    assert!(only.get("hits").array().is_empty()); assert!(!only.get("matches_seen_exact").flag());
}

#[test]
fn count_only_saved_queries_report_omitted_matches_without_a_continuation() {
    let f = Fixture::new(&[entry(b"a.rs", b"banana required"), entry(b"b.rs", b"banana required")], true);
    let mut session = f.open(88925);
    let literal = session.search_matching_page(1, "ana", 0, true, true, 1, || false).unwrap();
    assert_eq!(literal.exit_code(), EXIT_OK);
    let literal = json(&literal);
    window_with_evidence(&literal, 1, 1, true, None);
    assert!(literal.get("hits").array().is_empty());
    assert!(literal.get("matches_seen_exact").flag());
    assert_eq!(literal.get("matches_seen").number(), 4);
    let expression = session.expression_search_matching_page(2, "ana required", 0,
        100_000, true, true, 1, || false).unwrap();
    assert_eq!(expression.exit_code(), EXIT_OK);
    let expression = json(&expression);
    window_with_evidence(&expression, 1, 1, true, None);
    assert!(expression.get("hits").array().is_empty());
    assert!(expression.get("matches_seen_exact").flag());
    assert_eq!(expression.get("matches_seen").number(), 4);
    // A positive limit explicitly turns the same count offset into a readable
    // result window; no zero-row cursor is fed back into an endless loop.
    let retained = json(&session.expression_search_matching_page(3, "ana required", 2,
        100_000, true, true, 1, || false).unwrap());
    window(&retained, 1, 1, Some(3));
    assert_eq!(rows(&retained), [(0, 3, 6), (1, 1, 4)]);
}

#[test]
fn expression_windows_do_not_skip_rejected_candidates_or_unknown_predicates() {
    let bytes = b"needle required needle";
    let f = Fixture::new(&[entry(b"src/a.rs", bytes), entry(b"src/b.rs", b"needle forbidden required"),
        entry(b"src/c.rs", bytes), SnapshotEntry { path: b"tests/unavailable.rs", observed_bytes: 50,
            data: SnapshotData::Unavailable("NOT_CAPTURED") }], true);
    let mut session = f.open(88931); let query = "needle required -forbidden path:src/";
    let first = json(&session.expression_search_matching_page(1, query, 2, 100_000, true, true, 1, || false).unwrap());
    window(&first, 1, 1, Some(3)); assert_eq!(rows(&first), [(0, 16, 22), (2, 0, 6)]);
    assert_eq!(first.get("matches_seen").number(), 4); assert!(first.get("complete").flag());
    assert_eq!(first.get("predicate_rejections").number(), 1); assert_eq!(first.get("unavailable_files").number(), 0);
    let reads = member_reads(&mut session);
    let metadata = json(&session.expression_results(1, 1, 1, || false).unwrap());
    window(&metadata, 1, 1, Some(3)); assert_eq!(metadata.get("hits").array()[0].get("hit_id").number(), 2);
    assert_eq!(member_reads(&mut session), reads);
    let (mut reader, receipt) = session.open_expression_hit_reader(owner(88932), 1, 2, || false).unwrap();
    assert!(!json(&receipt).get("live_source_reopened").flag()); assert_eq!(reader.capture().bytes(), bytes);
    let last = json(&session.expression_search_matching_page(2, query, 1, 100_000, true, false, 3, || false).unwrap());
    window(&last, 3, 3, None); assert_eq!(rows(&last), [(2, 16, 22)]);
    assert!(last.get("complete").flag()); assert_eq!(last.get("matches_seen").number(), 4);
    assert!(session.open_expression_hit_reader(owner(88933), 1, 2, || false).is_err());
    let partial = json(&session.expression_search_matching_page(3, query, 2, 4, true, true, 999, || false).unwrap());
    window(&partial, 999, 0, None); assert_eq!(partial.get("state").text(), "work-limit");
    assert_eq!(partial.get("matches_seen").number(), 0); assert!(partial.get("hits").array().is_empty());
    assert!(!partial.get("matches_seen_exact").flag());
    drop(session);
    assert_eq!(json(&reader.copy_range(0, 6, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn occurrence_windows_open_exact_utf16_tokens_after_the_first_4096_matches() {
    let bytes = utf16(&"needle ".repeat(4_100), true);
    let f = Fixture::new(&[entry(b"a.txt", &bytes), entry(b"b.txt", b"needle")], true);
    let mut session = f.open(88941);
    let first = json(&session.structure_search_page(1, StructureMode::Occurrences, SymbolNameMode::Exact,
        "needle", 3, 64 * 1024 * 1024, 4096, || false).unwrap());
    window(&first, 4096, 4096, Some(4099));
    assert_eq!(rows(&first), [(0, 57346, 57358), (0, 57360, 57372), (0, 57374, 57386)]);
    let (mut reader, receipt) = session.open_structure_hit_reader(owner(88942), 1, 1, || false).unwrap();
    let receipt = json(&receipt); assert!(!receipt.get("live_source_reopened").flag());
    assert_eq!(receipt.get("selection").get("structure_generation").number(), 1);
    assert_eq!(receipt.get("selection").get("original_hex").text(), hex(&bytes[57346..57358]));
    assert_eq!(reader.capture().bytes(), bytes);
    let last = json(&session.structure_search_page(2, StructureMode::Occurrences, SymbolNameMode::Exact,
        "needle", 2, 64 * 1024 * 1024, 4099, || false).unwrap());
    window(&last, 4099, 4099, None); assert_eq!(rows(&last), [(0, 57388, 57400), (1, 0, 6)]);
    assert!(last.get("complete").flag()); assert_eq!(last.get("matches_seen").number(), 4101);
    assert_eq!(last.get("hits").array()[0].get("hit_id").number(), 1);
    assert!(session.open_structure_hit_reader(owner(88943), 1, 1, || false).is_err());
    let reads = member_reads(&mut session);
    let metadata = json(&session.structure_results(2, 1, 1, || false).unwrap());
    window(&metadata, 4099, 4099, None); assert_eq!(rows(&metadata), [(1, 0, 6)]);
    assert_eq!(member_reads(&mut session), reads);
    session.clear_structure(3, || false).unwrap(); drop(session);
    assert_eq!(json(&reader.copy_range(57346, 57358, || false).unwrap()).get("original_hex").text(),
        "6e006500650064006c006500");
}

#[test]
fn canceled_final_publication_and_invalid_replacements_preserve_each_accepted_window() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required needle"), entry(b"b.rs", b"needle required needle")], true);
    let mut probe = f.open(88951); let mut session = f.open(88952);
    let mut polls = 0;
    probe.search_matching_page(1, "needle", 2, true, true, 1, || { polls += 1; false }).unwrap();
    session.search_matching_page(1, "needle", 1, true, true, 0, || false).unwrap();
    let literal_before = json(&session.results(1, 0, 64, || false).unwrap());
    let mut attempt = 0;
    assert!(session.search_matching_page(2, "needle", 2, true, true, 1,
        || { attempt += 1; attempt == polls }).err().unwrap().is_canceled());
    assert!(session.search_matching_page(3, "needle", 4097, true, true, 1, || false).is_err());
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), literal_before);

    let mut polls = 0;
    probe.expression_search_matching_page(1, "needle required", 2, 100_000, true, true, 1,
        || { polls += 1; false }).unwrap();
    session.expression_search_matching_page(1, "needle required", 1, 100_000, true, true, 0, || false).unwrap();
    let expression_before = json(&session.expression_results(1, 0, 64, || false).unwrap());
    let mut attempt = 0;
    assert!(session.expression_search_matching_page(2, "needle required", 2, 100_000, true, true, 1,
        || { attempt += 1; attempt == polls }).err().unwrap().is_canceled());
    assert!(session.expression_search_matching_page(3, "regex:x.*", 1, 100_000, true, true, 1, || false).is_err());
    assert_eq!(session.accepted_expression_generation(), Some(1));
    assert_eq!(json(&session.expression_results(1, 0, 64, || false).unwrap()), expression_before);

    let mut polls = 0;
    probe.structure_search_page(1, StructureMode::Occurrences, SymbolNameMode::Exact, "needle", 2,
        64 * 1024 * 1024, 1, || { polls += 1; false }).unwrap();
    session.structure_search_page(1, StructureMode::Occurrences, SymbolNameMode::Exact, "needle", 1,
        64 * 1024 * 1024, 0, || false).unwrap();
    let structure_before = json(&session.structure_results(1, 0, 64, || false).unwrap());
    let mut attempt = 0;
    assert!(session.structure_search_page(2, StructureMode::Occurrences, SymbolNameMode::Exact, "needle", 2,
        64 * 1024 * 1024, 1, || { attempt += 1; attempt == polls }).err().unwrap().is_canceled());
    assert!(session.structure_search_page(3, StructureMode::Occurrences, SymbolNameMode::Prefix, "needle", 2,
        64 * 1024 * 1024, 1, || false).is_err());
    assert_eq!(session.accepted_structure_generation(), Some(1));
    assert_eq!(json(&session.structure_results(1, 0, 64, || false).unwrap()), structure_before);
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), literal_before);
    assert_eq!(json(&session.expression_results(1, 0, 64, || false).unwrap()), expression_before);
}

#[test]
fn later_scans_reverify_member_digests_without_discarding_the_accepted_window() {
    let payload = b"unique needle source needle";
    let f = Fixture::new(&[entry(b"a.rs", payload)], true); let mut session = f.open(88961);
    session.search_matching_page(1, "needle", 1, true, false, 0, || false).unwrap();
    let before = json(&session.results(1, 0, 64, || false).unwrap());
    let (mut reader, _) = session.open_hit_reader(owner(88962), 1, 1, || false).unwrap();
    let at = f.bytes.windows(payload.len()).position(|bytes| bytes == payload).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(session.search_matching_page(2, "needle", 1, true, false, 1, || false).is_err());
    assert_eq!(session.accepted_generation(), Some(1));
    assert_eq!(json(&session.results(1, 0, 64, || false).unwrap()), before);
    assert!(session.open_hit_reader(owner(88963), 1, 1, || false).is_err());
    assert_eq!(reader.capture().bytes(), payload);
    assert_eq!(json(&reader.copy_range(7, 13, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}
