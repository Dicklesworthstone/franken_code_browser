#![cfg(unix)]
#![forbid(unsafe_code)]

//! Real filesystem -> existing capture/search -> retained reader/atlas workflow.
//! These tests do not claim native rendering or physical-Mac qualification.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, Point2D};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::atlas_session::{AtlasAction, AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasSearchError, AtlasSearchOptions, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-atlas-search-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(root.join("src")).unwrap();
        Self(root)
    }
    fn write(&self, path: &str, bytes: &[u8]) { fs::write(self.0.join(path), bytes).unwrap(); }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(701), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
// Fixtures remain available for independent replay; no production or fixture
// deletion is required to exercise missing/replaced namespace entries.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn opts() -> AtlasSearchOptions { AtlasSearchOptions::default() }

#[test]
fn repository_search_opens_retained_whole_source_after_live_replacement() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"before needle after\n"); fixture.write("src/b.rs", b"not a match\n");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let result = search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    assert!(result.as_str().contains("\"search_complete\":true"));
    assert!(result.as_str().contains("\"retained_hits\":\"1\""));
    assert_eq!(search.retained_source_bytes(), b"before needle after\n".len());
    fixture.write("src/a.rs", b"changed live file\n");
    let (mut reader, linked) = search.open_reader(&atlas, owner(702), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"before needle after\n");
    assert!(linked.as_str().contains("\"source_reopened\":false"));
    assert!(linked.as_str().contains("retained-search-capture"));
    reader.search(1, "needle", 8, 4096, || false).unwrap();
    assert!(reader.copy_hit(1, 0, || false).unwrap().as_str().contains("6e6565646c65"));
    drop(search); drop(atlas);
    assert_eq!(reader.capture().bytes(), b"before needle after\n");
}

#[test]
fn utf16_and_overlapping_occurrences_preserve_original_byte_offsets() {
    let fixture = Fixture::new(); let mut bytes = vec![0xff, 0xfe];
    for unit in "head\r\naaaa😀\n".encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
    fixture.write("src/a.rs", &bytes);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let response = search.search(&atlas, 7, "aa", opts(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    assert!(response.as_str().contains("\"retained_hits\":\"3\""));
    for (id, offset) in [(1, 14), (2, 16), (3, 18)] {
        let hit = search.hit(&atlas, 7, id).unwrap();
        assert_eq!(hit.original_range.start().get(), offset);
        assert_eq!(hit.original_range.end().get(), offset + 4);
        assert_eq!(hit.revision.get(), 7);
    }
    let (reader, _) = search.open_reader(&atlas, owner(703), 7, 2, || false).unwrap();
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn failed_and_canceled_replacement_preserve_old_rows_and_consume_generations() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"old needle");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    assert!(search.search(&atlas, 2, "", opts(), || false).is_err());
    assert!(matches!(search.search(&atlas, 2, "needle", opts(), || false), Err(AtlasSearchError::StaleQuery)));
    assert!(matches!(search.search(&atlas, 3, "old", opts(), || true), Err(AtlasSearchError::Canceled)));
    assert_eq!(search.accepted_generation(), Some(1));
    assert!(search.hit(&atlas, 1, 1).is_ok());
    assert!(search.hit(&atlas, 3, 1).is_err());
    let mut polls = 0;
    assert!(search.search(&atlas, 4, "old", opts(), || { polls += 1; polls > 10 }).is_err());
    assert_eq!(search.accepted_generation(), Some(1));
    search.search(&atlas, 5, "old", opts(), || false).unwrap();
    assert!(matches!(search.hit(&atlas, 1, 1), Err(AtlasSearchError::StaleQuery)));
}

#[test]
fn result_pages_and_overlay_survive_namespace_movement_without_rereading() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", "needle ".repeat(200).as_bytes());
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let first = search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    assert!(first.as_str().contains("\"next_offset\":\"64\""));
    assert!(first.as_str().contains("\"search_complete\":true"));
    fs::rename(fixture.0.join("src"), fixture.0.join("moved")).unwrap();
    let page = search.page(&atlas, 1, 64, 128, || false).unwrap();
    assert!(page.as_str().contains("\"next_offset\":\"192\""));
    assert!(page.as_str().contains("\"hit_id\":\"65\""));
    assert!(search.page(&atlas, 1, 192, 128, || false).unwrap().as_str().contains("\"next_offset\":null"));
    let overlay = search.overlay(&atlas, 1, || false).unwrap();
    assert!(overlay.as_str().contains("\"matching_files\":\"1\""));
    assert!(overlay.as_str().contains("\"retained_occurrences\":\"200\""));
    assert_eq!(overlay.as_str().matches("\"node\":").count(), 1);
    assert!(search.open_reader(&atlas, owner(704), 1, 200, || false).is_ok());
}

#[test]
fn capture_and_file_work_budgets_are_explicit_partial_coverage() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"needle"); fixture.write("src/b.rs", b"needle");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let bytes = search.search(&atlas, 1, "needle", AtlasSearchOptions { max_source_bytes: 6, ..opts() }, || false).unwrap();
    assert_eq!(bytes.exit_code(), EXIT_PARTIAL);
    assert!(bytes.as_str().contains("\"source_bytes_read\":\"6\""));
    assert!(bytes.as_str().contains("\"pending_files\":\"1\""));
    let files = search.search(&atlas, 2, "absent", AtlasSearchOptions { max_files: 1, ..opts() }, || false).unwrap();
    assert_eq!(files.exit_code(), EXIT_PARTIAL);
    assert!(files.as_str().contains("\"retained_hits\":\"0\""));
    assert!(files.as_str().contains("\"pending_files\":\"1\""));
    let big = search.search(&atlas, 3, "needle", AtlasSearchOptions { max_file_bytes: 5, ..opts() }, || false).unwrap();
    assert!(big.as_str().contains("\"unavailable_files\":\"2\""));
    assert!(big.as_str().contains("\"source_bytes_read\":\"0\""));
}

#[test]
fn result_truncation_is_not_a_complete_count_or_an_unbounded_overlay() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", &vec![b'a'; 10_000]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let result = search.search(&atlas, 1, "a", AtlasSearchOptions { max_matches: 2, ..opts() }, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(result.as_str().contains("\"truncated\":true"));
    assert!(result.as_str().contains("\"retained_hits\":\"2\""));
    assert!(matches!(search.hit(&atlas, 1, 3), Err(AtlasSearchError::MissingHit)));
    let overlay = search.overlay(&atlas, 1, || false).unwrap();
    assert!(overlay.as_str().contains("\"retained_occurrences\":\"2\""));
}

#[test]
fn unsupported_text_and_missing_files_do_not_become_successful_no_matches() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", &[0xff, 0x80, 0]); fixture.write("src/b.rs", b"needle");
    let atlas = fixture.open();
    fs::rename(fixture.0.join("src/b.rs"), fixture.0.join("moved.rs")).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let result = search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(result.as_str().contains("\"unavailable_files\":\"2\""));
    assert!(result.as_str().contains("\"search_complete\":false"));
    assert!(result.as_str().contains("unsupported-text"));
}

#[test]
fn focus_uses_existing_geometry_and_does_not_acknowledge_new_pixels() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"needle"); fixture.write("src/b.rs", b"other");
    let mut atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    atlas.prepare(1, AtlasAction::View, || false).unwrap();
    atlas.acknowledge(1, 1, 1, || false).unwrap();
    let old_nodes = atlas.atlas().layout().nodes().as_ptr();
    search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    fs::rename(fixture.0.join("src"), fixture.0.join("moved")).unwrap();
    let hit = search.hit(&atlas, 1, 1).unwrap();
    search.focus_hit(&mut atlas, 1, 1, 2, || false).unwrap();
    assert_eq!(atlas.pending_plan().unwrap().focus(), hit.node);
    assert_eq!(atlas.presented_plan().unwrap().generation().get(), 1);
    assert_eq!(atlas.atlas().layout().nodes().as_ptr(), old_nodes);
    atlas.prepare(3, AtlasAction::Pan(Point2D::new(10.0, 2.0).unwrap()), || false).unwrap();
    assert!(search.hit(&atlas, 1, 1).is_ok());
    atlas.prepare(4, AtlasAction::Back, || false).unwrap();
    assert!(search.open_reader(&atlas, owner(705), 1, 1, || false).is_ok());
}

#[test]
fn clear_invalidates_old_hits_without_recycling_exhausted_ids() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"needle");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, u64::MAX - 1, "needle", opts(), || false).unwrap();
    search.clear(&atlas, u64::MAX, || false).unwrap();
    assert_eq!(search.accepted_generation(), None);
    assert_eq!(search.retained_source_bytes(), 0);
    assert!(search.open_reader(&atlas, owner(706), u64::MAX - 1, 1, || false).is_err());
    assert!(matches!(search.search(&atlas, 1, "needle", opts(), || false), Err(AtlasSearchError::StaleQuery)));
}

#[test]
fn fresh_query_reads_new_bytes_but_rejects_old_generation_activation() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"needle OLD");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    let (old_reader, _) = search.open_reader(&atlas, owner(707), 1, 1, || false).unwrap();
    fixture.write("src/a.rs", b"needle NEW");
    search.search(&atlas, 2, "needle", opts(), || false).unwrap();
    assert!(search.open_reader(&atlas, owner(708), 1, 1, || false).is_err());
    let (new_reader, _) = search.open_reader(&atlas, owner(708), 2, 1, || false).unwrap();
    assert_eq!(old_reader.capture().bytes(), b"needle OLD");
    assert_eq!(new_reader.capture().bytes(), b"needle NEW");
}

#[test]
fn root_revocation_and_foreign_atlas_reject_all_delivery_routes() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"needle");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    let foreign = AtlasSession::open(owner(799), &fixture.0, Default::default(), || false).unwrap();
    assert!(matches!(search.page(&foreign, 1, 0, 10, || false), Err(AtlasSearchError::WrongAtlas)));
    assert!(search.open_reader(&atlas, owner(701), 1, 1, || false).is_err());
    assert!(search.hit(&atlas, 1, 0).is_err());
    assert!(search.hit(&atlas, 1, u64::MAX).is_err());
    // Even accidentally reused public counters cannot alias independent grants.
    let same_counters = fixture.open();
    assert!(matches!(search.page(&same_counters, 1, 0, 10, || false), Err(AtlasSearchError::WrongAtlas)));
    atlas.atlas().catalog().grant().revoke();
    assert!(search.page(&atlas, 1, 0, 10, || false).is_err());
    assert!(search.overlay(&atlas, 1, || false).is_err());
    assert!(search.open_reader(&atlas, owner(710), 1, 1, || false).is_err());
    assert!(search.search(&atlas, 2, "needle", opts(), || false).is_err());
}

#[test]
fn complete_empty_success_replaces_old_hits_and_partial_discovery_stays_partial() {
    let fixture = Fixture::new(); fixture.write("src/a.rs", b"needle"); fixture.write("src/b.rs", b"needle");
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    let none = search.search(&atlas, 2, "absent", opts(), || false).unwrap();
    assert_eq!(none.exit_code(), EXIT_OK);
    assert!(none.as_str().contains("\"search_complete\":true"));
    assert_eq!(search.retained_source_bytes(), 0);
    assert!(search.hit(&atlas, 2, 1).is_err());
    let partial = AtlasSession::open(owner(798), &fixture.0,
        AtlasSessionOptions { max_files: 1, ..Default::default() }, || false).unwrap();
    let mut partial_search = RetainedAtlasSearch::new(&partial).unwrap();
    let result = partial_search.search(&partial, 1, "absent", opts(), || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(result.as_str().contains("\"discovery_complete\":false"));
}

#[test]
fn raw_native_paths_stay_distinct_and_filename_controls_cannot_forge_rows() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let name = std::ffi::OsString::from_vec(vec![b'x', 0xff, b'\n', b'y']);
    fs::write(fixture.0.join("src").join(name), b"needle").unwrap();
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let result = search.search(&atlas, 1, "needle", opts(), || false).unwrap();
    assert!(result.as_str().contains("7372632f78ff0a79"));
    assert_eq!(result.as_str().lines().count(), 1);
    let (reader, _) = search.open_reader(&atlas, owner(709), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle");
}
