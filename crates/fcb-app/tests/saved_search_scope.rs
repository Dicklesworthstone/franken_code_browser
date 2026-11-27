#![forbid(unsafe_code)]
#![cfg(unix)]

//! Saved query scope is part of an accepted generation, including its reader
//! origin. Fixtures are archive bytes; their member labels are never live roots.

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use saved_fixture::{entry, owner, Fixture};
use support::{Json, parse};
use fcb::search::paged_snapshot::PagedSearchError;
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, saved_repository::{SavedRepositoryError, SnapshotScope}};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn scope(page: &Json, label: &str, path: &[u8], members: u64, captured: u64, complete: bool) {
    assert_eq!(page.get("scope").text(), label);
    assert_eq!(page.get("scope_path_hex").text(), hex(path));
    assert_eq!(page.get("scope_known_members").number(), members);
    assert_eq!(page.get("scope_captured_members").number(), captured);
    assert_eq!(page.get("scope_membership_complete").flag(), complete);
    assert!(!page.get("live_root_authority").flag());
}

#[test]
fn literal_scope_survives_index_changes_failed_replacements_and_reader_activation() {
    let f = Fixture::new(&[entry(b"docs/decoy.rs", b"banana"),
        SnapshotEntry { path: b"elsewhere/lost.rs", observed_bytes: 99, data: SnapshotData::Unavailable("missing") },
        entry(b"src/a.rs", b"banana banana"), entry(b"src/nested/\xff.rs", b"banana"),
        entry(b"src-other/decoy.rs", b"banana")], true);
    let (index, pin) = f.index(); let mut saved = f.open(88301);
    let direct = json(&saved.search_scoped_page(1, "ana", 1, true, true, 2,
        SnapshotScope::Directory(b"src"), || false).unwrap());
    scope(&direct, "saved-directory", b"src", 2, 2, true);
    assert!(direct.get("search_complete").flag());
    assert_eq!(direct.get("matches_seen").number(), 6);
    assert_eq!(direct.get("unavailable_files").number(), 0);
    assert_eq!(direct.get("hits").array()[0].get("original_range").get("start").number(), 8);
    let before = json(&saved.info(|| false).unwrap()).get("member_bytes_read").number();
    saved.attach_index(1, &index, pin, || false).unwrap();
    let page = json(&saved.results(1, 0, 1, || false).unwrap());
    scope(&page, "saved-directory", b"src", 2, 2, true);
    assert_eq!(page.get("index_generation"), &Json::Null);
    assert_eq!(json(&saved.info(|| false).unwrap()).get("member_bytes_read").number(), before);

    let mut selected = b"src/nested/\xff.rs".to_vec();
    let indexed = json(&saved.search_scoped_page(2, "ana", 4, true, true, 0,
        SnapshotScope::Member(&selected), || false).unwrap());
    selected.fill(b'x');
    scope(&indexed, "saved-member", b"src/nested/\xff.rs", 1, 1, true);
    assert_eq!(indexed.get("matches_seen").number(), 2);
    assert!(indexed.get("index_used").flag());
    saved.detach_index(2, || false).unwrap();
    let retained = json(&saved.results(2, 0, 1, || false).unwrap());
    scope(&retained, "saved-member", b"src/nested/\xff.rs", 1, 1, true);
    assert_eq!(retained.get("index_generation").number(), 1);

    assert_eq!(saved.search_scoped_page(3, "ana", 4, true, true, 0,
        SnapshotScope::Directory(b"../src"), || false).err(),
        Some(SavedRepositoryError::Query(PagedSearchError::InvalidScope)));
    assert!(saved.search_scoped_page(4, "ana", 4, true, true, 0,
        SnapshotScope::Archive, || true).err().unwrap().is_canceled());
    assert_eq!(saved.search_scoped_page(4, "ana", 4, true, true, 0,
        SnapshotScope::Archive, || false).err(), Some(SavedRepositoryError::StaleQuery));
    scope(&json(&saved.results(2, 0, 2, || false).unwrap()),
        "saved-member", b"src/nested/\xff.rs", 1, 1, true);
    let (reader, receipt) = saved.open_hit_reader(owner(88302), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"banana");
    let receipt = json(&receipt);
    scope(&receipt, "saved-member", b"src/nested/\xff.rs", 1, 1, true);
    assert_eq!(receipt.get("selection").get("text").text(), "ana");
    assert_eq!(receipt.get("selection").get("original_range").get("start").number(), 1);
}

#[test]
fn expression_scope_precedes_metadata_and_preserves_original_reader_evidence() {
    let f = Fixture::new(&[entry(b"docs/a.rs", b"needle required"),
        entry(b"src/a.rs", b"needle required needle"), entry(b"src/b.py", b"needle required"),
        entry(b"src/c.rs", b"needle required forbidden"),
        SnapshotEntry { path: b"src-other/lost.rs", observed_bytes: 99, data: SnapshotData::Unavailable("missing") }], true);
    let mut saved = f.open(88311);
    let response = saved.expression_search_scoped_page(1, "needle required -forbidden lang:rust",
        1, 1024, true, true, 1, SnapshotScope::Directory(b"src"), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let page = json(&response);
    scope(&page, "saved-directory", b"src", 3, 3, true);
    assert_eq!(page.get("metadata_examined").number(), 3);
    assert_eq!(page.get("metadata_excluded").number(), 1);
    assert_eq!(page.get("scope_files").number(), 2);
    assert_eq!(page.get("unavailable_files").number(), 0);
    assert_eq!(page.get("matches_seen").number(), 2);
    assert_eq!(page.get("hits").array()[0].get("original_range").get("start").number(), 16);
    assert!(saved.expression_search_scoped_page(2, "needle", 1, 1024, true, true, 0,
        SnapshotScope::Member(b"../src/a.rs"), || false).is_err());
    assert!(saved.expression_search_scoped_page(3, "needle", 1, 1024, true, true, 0,
        SnapshotScope::Archive, || true).is_err());
    scope(&json(&saved.expression_results(1, 0, 1, || false).unwrap()),
        "saved-directory", b"src", 3, 3, true);
    let (reader, receipt) = saved.open_expression_hit_reader(owner(88312), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle required needle");
    let receipt = json(&receipt);
    scope(&receipt, "saved-directory", b"src", 3, 3, true);
    assert_eq!(receipt.get("selection").get("original_range").get("start").number(), 16);
    assert_eq!(receipt.get("selection").get("text").text(), "needle");
}

#[test]
fn an_observed_member_closes_independently_of_incomplete_archive_discovery() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle required"),
        SnapshotEntry { path: b"missing.rs", observed_bytes: 8, data: SnapshotData::Unavailable("not-captured") }], false);
    let mut saved = f.open(88321);
    for (i, selection, label, path, members, captured, membership, complete) in [
        (0, SnapshotScope::Member(b"src/a.rs"), "saved-member", b"src/a.rs".as_slice(), 1, 1, true, true),
        (1, SnapshotScope::Member(b"missing.rs"), "saved-member", b"missing.rs".as_slice(), 1, 0, true, false),
        (2, SnapshotScope::Member(b"unknown.rs"), "saved-member", b"unknown.rs".as_slice(), 0, 0, false, false),
        (3, SnapshotScope::Directory(b"src"), "saved-directory", b"src".as_slice(), 1, 1, false, false),
    ] {
        let literal = saved.search_scoped_page(i + 1, "needle", 8, true, true, 0, selection, || false).unwrap();
        assert_eq!(literal.exit_code(), if complete { EXIT_OK } else { EXIT_PARTIAL });
        let page = json(&literal); scope(&page, label, path, members, captured, membership);
        assert!(!page.get("discovery_complete").flag());
        assert_eq!(page.get("matches_seen_exact").flag(), complete);
        let expression = saved.expression_search_scoped_page(i + 1, "needle required", 8, 1024,
            true, true, 0, selection, || false).unwrap();
        assert_eq!(expression.exit_code(), if complete { EXIT_OK } else { EXIT_PARTIAL });
        let page = json(&expression); scope(&page, label, path, members, captured, membership);
        assert_eq!(page.get("matches_seen_exact").flag(), complete);
    }
}

#[test]
fn empty_directory_is_complete_but_missing_member_is_an_unfulfilled_selection() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"needle")], true);
    let mut saved = f.open(88331);
    for (generation, selection, expected) in [
        (1, SnapshotScope::Directory(b"none"), EXIT_OK),
        (2, SnapshotScope::Member(b"none"), EXIT_PARTIAL),
    ] {
        let literal = saved.search_scoped_page(generation, "needle", 0, true, true, 0, selection, || false).unwrap();
        assert_eq!(literal.exit_code(), expected);
        let page = json(&literal); assert_eq!(page.get("scope_known_members").number(), 0);
        assert_eq!(page.get("matches_seen").number(), 0);
        assert_eq!(page.get("matches_seen_exact").flag(), expected == EXIT_OK);
        let expression = saved.expression_search_scoped_page(generation, "needle", 0, 0,
            true, true, 0, selection, || false).unwrap();
        assert_eq!(expression.exit_code(), expected);
        assert_eq!(json(&expression).get("matches_seen").number(), 0);
    }
}
