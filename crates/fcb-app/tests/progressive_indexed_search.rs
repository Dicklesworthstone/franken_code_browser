#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchStop, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-stepped-index-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in parts { fs::write(root.join(name), bytes).unwrap(); }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(29101), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 8192, max_source_bytes: 128 * 1024, max_index_grams: 65536 }
}
fn setup() -> (Fixture, AtlasSession, RetainedAtlasSearch) {
    let fixture = Fixture::new(&[("a.rs", b"needle a"), ("b.rs", b"nothing"), ("c.rs", b"needle c needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    (fixture, atlas, search)
}
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) {
    for _ in 0..101 {
        if !search.progress(atlas, generation).unwrap().is_running() { return; }
        search.step(atlas, generation, || false).unwrap();
    }
    panic!("query failed to terminate");
}

#[test]
fn admission_has_no_verification_and_every_step_examines_at_most_one_capture() {
    let (_fixture, atlas, mut search) = setup();
    let begin = search.begin_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    assert_eq!(begin.exit_code(), EXIT_PARTIAL);
    assert!(begin.as_str().contains("\"verification_source_bytes\":\"0\""));
    let initial = search.progress(&atlas, 2).unwrap();
    assert_eq!(initial.examined_files, 0); assert_eq!(initial.scanned_files, 0);
    assert_eq!(initial.step_count, 0); assert_eq!(initial.retained_hits, 0);
    assert_eq!(initial.source_bytes_read, 0); assert!(initial.is_running());
    for step in 1..=3 {
        search.step(&atlas, 2, || false).unwrap();
        let progress = search.progress(&atlas, 2).unwrap();
        assert_eq!(progress.last_step_files, 1); assert_eq!(progress.examined_files, step);
        assert_eq!(progress.step_count, step as u64); assert_eq!(progress.source_bytes_read, 0);
        assert_eq!(progress.last_step_source_bytes, 0); assert_eq!(progress.read_calls, 0);
    }
    assert!(search.progress(&atlas, 2).unwrap().complete);
    assert_eq!(search.pending_generation(), None); assert_eq!(search.accepted_generation(), Some(2));
}

#[test]
fn early_hit_opens_captured_source_and_camera_does_not_advance_the_query() {
    let (fixture, mut atlas, mut search) = setup();
    search.begin_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    search.step(&atlas, 2, || false).unwrap();
    let early = search.hit(&atlas, 2, 1).unwrap();
    fs::write(fixture.0.join("a.rs"), b"replacement source").unwrap();
    let (mut reader, response) = search.open_reader(&atlas, owner(29102), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle a");
    assert!(response.as_str().contains("\"search_in_progress\":true"));
    let before = search.progress(&atlas, 2).unwrap();
    search.focus_hit(&mut atlas, 2, 1, 1, || false).unwrap();
    assert!(atlas.presented_plan().is_none());
    atlas.acknowledge(1, 1, 1, || false).unwrap();
    assert_eq!(search.progress(&atlas, 2).unwrap(), before);
    finish(&mut search, &atlas, 2);
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), early);
    assert!(reader.copy_range(0, 6, || false).unwrap().as_str().contains("6e6565646c65"));
}

#[test]
fn synchronous_and_stepped_queries_share_all_results_and_work_counters() {
    let (_fixture, atlas, mut sync) = setup();
    let mut stepped = RetainedAtlasSearch::new(&atlas).unwrap();
    stepped.prepare_index(&atlas, 1, options(), || false).unwrap();
    sync.search_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    stepped.begin_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    finish(&mut stepped, &atlas, 2);
    assert_eq!(sync.progress(&atlas, 2).unwrap(), stepped.progress(&atlas, 2).unwrap());
    assert_eq!(sync.page(&atlas, 2, 0, 100, || false).unwrap().as_str(),
        stepped.page(&atlas, 2, 0, 100, || false).unwrap().as_str());
}

#[test]
fn partial_page_end_is_not_query_completion_and_ids_append_without_renumbering() {
    let many = b"needle ".repeat(80);
    let fixture = Fixture::new(&[("a.rs", &many), ("b.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    search.begin_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    let first = search.step(&atlas, 2, || false).unwrap();
    assert!(first.as_str().contains("\"next_offset\":\"64\""));
    let page = search.page(&atlas, 2, 64, 128, || false).unwrap();
    assert!(page.as_str().contains("\"hit_id\":\"80\""));
    assert!(page.as_str().contains("\"next_offset\":null"));
    assert!(page.as_str().contains("\"search_in_progress\":true"));
    let early = search.hit(&atlas, 2, 80).unwrap();
    finish(&mut search, &atlas, 2);
    assert_eq!(search.hit(&atlas, 2, 80).unwrap(), early);
    assert_ne!(search.hit(&atlas, 2, 81).unwrap().file, early.file);
}

#[test]
fn failed_replacement_preserves_old_rows_and_independent_early_reader() {
    let (_fixture, atlas, mut search) = setup();
    search.search_indexed(&atlas, 2, 1, "nothing", 100, 8192, || false).unwrap();
    let old = search.hit(&atlas, 2, 1).unwrap();
    search.begin_indexed(&atlas, 3, 1, "needle", 100, 8192, || false).unwrap();
    search.step(&atlas, 3, || false).unwrap();
    let (reader, _) = search.open_reader(&atlas, owner(29103), 3, 1, || false).unwrap();
    assert_eq!(search.step(&atlas, 3, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.pending_generation(), None); assert_eq!(search.accepted_generation(), Some(2));
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), old);
    assert!(search.step(&atlas, 3, || false).is_err());
    assert_eq!(reader.capture().bytes(), b"needle a");
    assert_eq!(search.index_generation(), Some(1));
}

#[test]
fn live_and_indexed_requests_share_one_supersession_and_cancellation_domain() {
    let (_fixture, atlas, mut search) = setup();
    let live = AtlasSearchOptions { max_matches: 100, max_files: 100, max_file_bytes: 8192, max_source_bytes: 128 * 1024 };
    search.begin(&atlas, 2, "needle", live, || false).unwrap();
    search.begin_indexed(&atlas, 3, 1, "nothing", 100, 8192, || false).unwrap();
    assert_eq!(search.step(&atlas, 2, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(search.pending_generation(), Some(3));
    search.begin(&atlas, 4, "needle", live, || false).unwrap();
    assert_eq!(search.step(&atlas, 3, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert!(search.cancel_pending()); assert!(!search.cancel_pending());
    assert_eq!(search.index_generation(), Some(1));
    search.begin_indexed(&atlas, 5, 1, "needle", 100, 8192, || false).unwrap();
    finish(&mut search, &atlas, 5);
    assert!(search.progress(&atlas, 5).unwrap().complete);
}

#[test]
fn index_replacement_and_clear_retire_paused_work_but_preserve_accepted_source_pins() {
    let (fixture, atlas, mut search) = setup();
    search.search_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    search.begin_indexed(&atlas, 3, 1, "nothing", 100, 8192, || false).unwrap();
    fs::write(fixture.0.join("a.rs"), b"new source").unwrap();
    search.prepare_index(&atlas, 4, options(), || false).unwrap();
    assert_eq!(search.pending_generation(), None);
    assert!(search.step(&atlas, 3, || false).is_err());
    let (old, _) = search.open_reader(&atlas, owner(29104), 2, 1, || false).unwrap();
    assert_eq!(old.capture().bytes(), b"needle a");
    search.begin_indexed(&atlas, 5, 4, "new", 100, 8192, || false).unwrap();
    search.clear_index(&atlas, 6, || false).unwrap();
    assert!(search.step(&atlas, 5, || false).is_err());
    assert_eq!(search.accepted_generation(), Some(2));
    assert!(search.open_reader(&atlas, owner(29105), 2, 1, || false).is_ok());
}

#[test]
fn utf16_fallback_and_search_after_rename_keep_original_byte_ranges() {
    let bytes: Vec<u8> = "\u{feff}a🦀 needle".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let fixture = Fixture::new(&[("a.rs", &bytes), ("b.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    fs::rename(fixture.0.join("a.rs"), fixture.0.join("moved.rs")).unwrap();
    search.begin_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    search.step(&atlas, 2, || false).unwrap();
    let hit = search.hit(&atlas, 2, 1).unwrap();
    assert_eq!(hit.original_range.len().get(), 12); assert_eq!(hit.revision.get(), 1);
    let (reader, _) = search.open_reader(&atlas, owner(29106), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), bytes);
    finish(&mut search, &atlas, 2);
    let page = search.page(&atlas, 2, 0, 100, || false).unwrap();
    assert!(page.as_str().contains("\"fallback_files\":\"1\""));
    assert_eq!(search.progress(&atlas, 2).unwrap().source_bytes_read, 0);
}

#[test]
fn unavailable_and_unsupported_files_are_not_recounted_on_later_steps() {
    let fixture = Fixture::new(&[("a.rs", b"needle"), ("b.rs", b"\xffneedle"), ("c.rs", b"needle"), ("d.rs", b"needle")]);
    let atlas = fixture.open();
    fs::rename(fixture.0.join("d.rs"), fixture.0.join("moved.rs")).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    search.begin_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    assert_eq!(search.progress(&atlas, 2).unwrap().unavailable_files, 1);
    finish(&mut search, &atlas, 2);
    let progress = search.progress(&atlas, 2).unwrap();
    assert_eq!(progress.unavailable_files, 2); assert_eq!(progress.pending_files, 0);
    assert_eq!(progress.examined_files, 4); assert!(!progress.complete);
    let before = search.page(&atlas, 2, 0, 100, || false).unwrap();
    search.step(&atlas, 2, || false).unwrap();
    assert_eq!(search.page(&atlas, 2, 0, 100, || false).unwrap().as_str(), before.as_str());
}

#[test]
fn exact_full_buffer_is_not_truncated_without_an_additional_match() {
    let fixture = Fixture::new(&[("a.rs", b"needle"), ("b.rs", b"needle"), ("c.rs", b"nothing")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    search.begin_indexed(&atlas, 2, 1, "needle", 2, 8192, || false).unwrap();
    finish(&mut search, &atlas, 2);
    assert!(search.progress(&atlas, 2).unwrap().complete);
    assert!(!search.progress(&atlas, 2).unwrap().truncated);
    search.begin_indexed(&atlas, 3, 1, "needle", 1, 8192, || false).unwrap();
    finish(&mut search, &atlas, 3);
    let limited = search.progress(&atlas, 3).unwrap();
    assert_eq!(limited.retained_hits, 1); assert_eq!(limited.matches_seen, 2);
    assert!(limited.truncated); assert_eq!(limited.stop_reason, Some(AtlasSearchStop::MatchLimit));
}

#[test]
fn failed_and_canceled_begin_never_destroy_the_last_accepted_rows() {
    let (_fixture, atlas, mut search) = setup();
    search.search_indexed(&atlas, 2, 1, "needle", 100, 8192, || false).unwrap();
    assert_eq!(search.begin_indexed(&atlas, 3, 99, "needle", 100, 8192, || false).err(), Some(AtlasSearchError::StaleIndex));
    assert_eq!(search.accepted_generation(), Some(2));
    assert_eq!(search.begin_indexed(&atlas, 4, 1, "needle", 100, 8192, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.accepted_generation(), Some(2));
    let before = search.page(&atlas, 2, 0, 100, || false).unwrap();
    assert!(search.begin_indexed(&atlas, 5, 1, "", 100, 8192, || false).is_err());
    assert_eq!(search.page(&atlas, 2, 0, 100, || false).unwrap().as_str(), before.as_str());
}

#[test]
fn one_shot_cancellation_during_steps_is_sticky_and_index_remains_reusable() {
    let (_fixture, atlas, mut search) = setup();
    let mut generation = 2;
    for checkpoint in [1, 2, 4, 8, 12] {
        search.begin_indexed(&atlas, generation, 1, "needle", 100, 8192, || false).unwrap();
        let mut polls = 0;
        let response = search.step(&atlas, generation, || { polls += 1; polls == checkpoint });
        if polls >= checkpoint {
            assert!(response.is_err()); assert_eq!(search.pending_generation(), None);
        }
        generation += 1;
    }
    search.begin_indexed(&atlas, generation, 1, "needle", 100, 8192, || false).unwrap();
    finish(&mut search, &atlas, generation);
    assert_eq!(search.page(&atlas, generation, 0, 100, || false).unwrap().exit_code(), EXIT_OK);
}
