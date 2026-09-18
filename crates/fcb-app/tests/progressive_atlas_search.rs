#![forbid(unsafe_code)]
#![cfg(unix)]

//! Public host tests: actual catalog/capture/search/reader operations, not a
//! replacement search model. Native frame presentation is not claimed here.

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::atlas_session::{AtlasAction, AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasSearchError, AtlasSearchOptions, AtlasSearchStop, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-progressive-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files { fs::write(root.join(name), bytes).unwrap(); }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(8401), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 100, max_files: 100, max_file_bytes: 1024,
        max_source_bytes: 64 * 1024 }
}
fn fixture() -> Fixture {
    Fixture::new(&[("a.rs", b"needle a\n"), ("b.rs", b"nothing\n"), ("c.rs", b"needle c needle\n")])
}
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) {
    for _ in 0..101 {
        if !search.progress(atlas, generation).unwrap().is_running() { return; }
        search.step(atlas, generation, || false).unwrap();
    }
    panic!("bounded search did not reach a terminal state");
}

#[test]
fn begin_does_not_capture_source_and_each_step_examines_at_most_one_file() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let begin = search.begin(&atlas, 1, "needle", options(), || false).unwrap();
    assert_eq!(begin.exit_code(), EXIT_PARTIAL);
    let initial = search.progress(&atlas, 1).unwrap();
    assert!(initial.is_running()); assert_eq!(initial.examined_files, 0);
    assert_eq!(initial.source_bytes_read, 0); assert_eq!(initial.retained_source_bytes, 0);
    assert_eq!(initial.pending_files, 3); assert_eq!(search.accepted_generation(), None);
    // Proves admission did not silently capture source that no longer exists.
    fs::remove_file(f.0.join("a.rs")).unwrap();
    for step in 1..=3 {
        let response = search.step(&atlas, 1, || false).unwrap();
        let p = search.progress(&atlas, 1).unwrap();
        assert_eq!(p.examined_files, step); assert_eq!(p.last_step_files, 1);
        assert_eq!(p.step_count, step as u64); assert_eq!(p.pending_files, 3 - step);
        assert!(p.last_step_source_bytes <= options().max_file_bytes as u64);
        assert_eq!(response.exit_code(), EXIT_PARTIAL); // Missing a.rs remains unavailable.
    }
    let end = search.progress(&atlas, 1).unwrap();
    assert!(!end.is_running()); assert!(!end.complete); assert_eq!(end.unavailable_files, 1);
    assert_eq!(end.retained_hits, 2); assert_eq!(search.accepted_generation(), Some(1));
    assert_eq!(search.pending_generation(), None);
}

#[test]
fn partial_hits_remain_exact_and_stable_while_camera_and_next_steps_run() {
    let f = fixture(); let mut atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 9, "needle", options(), || false).unwrap();
    search.step(&atlas, 9, || false).unwrap();
    let first = search.hit(&atlas, 9, 1).unwrap();
    assert_eq!(first.original_range.start().get(), 0); assert_eq!(first.original_range.end().get(), 6);
    fs::write(f.0.join("a.rs"), b"changed after first step").unwrap();
    let (reader, response) = search.open_reader(&atlas, owner(8402), 9, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle a\n");
    assert!(response.as_str().contains("\"source_reopened\":false"));
    assert!(response.as_str().contains("\"search_in_progress\":true"));
    search.focus_hit(&mut atlas, 9, 1, 1, || false).unwrap();
    assert!(atlas.presented_plan().is_none());
    atlas.acknowledge(1, 1, 1, || false).unwrap();
    search.step(&atlas, 9, || false).unwrap();
    assert_eq!(search.hit(&atlas, 9, 1).unwrap(), first);
    assert_eq!(atlas.presented_plan().unwrap().generation().get(), 1);
    finish(&mut search, &atlas, 9);
    assert_eq!(search.hit(&atlas, 9, 1).unwrap(), first);
    assert_eq!(search.progress(&atlas, 9).unwrap().retained_hits, 3);
    assert!(search.progress(&atlas, 9).unwrap().complete);
    assert_eq!(reader.capture().bytes(), b"needle a\n");
}

#[test]
fn synchronous_and_resumable_routes_agree_on_every_occurrence_and_coverage() {
    let f = fixture(); let atlas = f.open();
    let mut sync = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut stepped = RetainedAtlasSearch::new(&atlas).unwrap();
    sync.search(&atlas, 7, "needle", options(), || false).unwrap();
    stepped.begin(&atlas, 7, "needle", options(), || false).unwrap();
    finish(&mut stepped, &atlas, 7);
    assert_eq!(sync.progress(&atlas, 7).unwrap(), stepped.progress(&atlas, 7).unwrap());
    for id in 1..=3 { assert_eq!(sync.hit(&atlas, 7, id).unwrap(), stepped.hit(&atlas, 7, id).unwrap()); }
    assert_eq!(sync.page(&atlas, 7, 0, 100, || false).unwrap().as_str(),
        stepped.page(&atlas, 7, 0, 100, || false).unwrap().as_str());
}

#[test]
fn finished_query_survives_canceled_replacement_and_opened_progress_reader_survives_both() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "nothing", options(), || false).unwrap();
    let previous = search.page(&atlas, 1, 0, 100, || false).unwrap();
    search.begin(&atlas, 2, "needle", options(), || false).unwrap();
    search.step(&atlas, 2, || false).unwrap();
    let (reader, _) = search.open_reader(&atlas, owner(8402), 2, 1, || false).unwrap();
    assert_eq!(search.accepted_generation(), Some(1));
    assert!(matches!(search.step(&atlas, 2, || true), Err(AtlasSearchError::Canceled)));
    assert_eq!(search.pending_generation(), None); assert_eq!(search.accepted_generation(), Some(1));
    assert!(search.page(&atlas, 2, 0, 100, || false).is_err());
    assert_eq!(search.page(&atlas, 1, 0, 100, || false).unwrap().as_str(), previous.as_str());
    search.clear(&atlas, 3, || false).unwrap(); drop(search); drop(atlas);
    assert_eq!(reader.capture().bytes(), b"needle a\n");
}

#[test]
fn canceled_steps_at_multiple_internal_boundaries_never_replace_finished_query() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "nothing", options(), || false).unwrap();
    let old = search.hit(&atlas, 1, 1).unwrap();
    for cancel_at in 1..=40 {
        let generation = cancel_at + 1;
        search.begin(&atlas, generation, "needle", options(), || false).unwrap();
        let mut polls = 0;
        let result = search.step(&atlas, generation, || { polls += 1; polls >= cancel_at });
        assert_eq!(search.accepted_generation(), Some(1));
        assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
        if result.is_err() { assert_eq!(search.pending_generation(), None); }
        else { assert_eq!(search.progress(&atlas, generation).unwrap().examined_files, 1); }
    }
}

#[test]
fn supersession_and_stale_steps_do_not_resurrect_or_destroy_a_newer_request() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "old", options(), || false).unwrap();
    search.begin(&atlas, 2, "needle", options(), || false).unwrap();
    let before = search.progress(&atlas, 2).unwrap();
    assert!(matches!(search.step(&atlas, 1, || false), Err(AtlasSearchError::StaleQuery)));
    assert_eq!(search.progress(&atlas, 2).unwrap(), before);
    assert!(search.begin(&atlas, 1, "stale", options(), || false).is_err());
    assert_eq!(search.progress(&atlas, 2).unwrap(), before);
    assert!(search.begin(&atlas, 3, "", options(), || false).is_err());
    assert_eq!(search.pending_generation(), None);
    assert!(search.step(&atlas, 2, || false).is_err());
    assert!(search.begin(&atlas, 3, "reuse", options(), || false).is_err());
    search.begin(&atlas, 4, "needle", options(), || false).unwrap();
}

#[test]
fn terminal_retries_pages_and_overlays_do_no_source_work() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "needle", options(), || false).unwrap(); finish(&mut search, &atlas, 1);
    let before = search.progress(&atlas, 1).unwrap();
    for name in ["a.rs", "b.rs", "c.rs"] { fs::remove_file(f.0.join(name)).unwrap(); }
    let reply = search.step(&atlas, 1, || false).unwrap();
    assert_eq!(reply.exit_code(), EXIT_OK);
    assert_eq!(search.progress(&atlas, 1).unwrap(), before);
    assert!(search.page(&atlas, 1, 0, 1, || false).unwrap().as_str().contains("\"next_offset\":\"1\""));
    assert!(search.overlay(&atlas, 1, || false).unwrap().as_str().contains("\"count_basis\":\"retained-hits\""));
}

#[test]
fn progressive_pagination_can_resume_at_previous_end_after_more_hits_arrive() {
    let f = Fixture::new(&[("a.rs", b"x x x"), ("b.rs", b"x x")]);
    let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "x", options(), || false).unwrap();
    search.step(&atlas, 1, || false).unwrap();
    let early = search.page(&atlas, 1, 3, 2, || false).unwrap();
    assert!(early.as_str().contains("\"hits\":[]"));
    assert!(early.as_str().contains("\"search_in_progress\":true"));
    let first = search.hit(&atlas, 1, 1).unwrap();
    search.step(&atlas, 1, || false).unwrap();
    let late = search.page(&atlas, 1, 3, 2, || false).unwrap();
    assert!(late.as_str().contains("\"hit_id\":\"4\""));
    assert!(late.as_str().contains("\"hit_id\":\"5\""));
    assert!(late.as_str().contains("\"search_in_progress\":false"));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), first);
}

#[test]
fn utf16_overlapping_hits_are_usable_before_repository_scan_finishes() {
    let mut raw = vec![0xff, 0xfe];
    for unit in "α ababa\r\n".encode_utf16() { raw.extend_from_slice(&unit.to_le_bytes()); }
    let f = Fixture::new(&[("a.rs", &raw), ("b.rs", b"unsearched")]);
    let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "aba", options(), || false).unwrap(); search.step(&atlas, 1, || false).unwrap();
    assert!(search.progress(&atlas, 1).unwrap().is_running());
    for (id, start) in [(1, 6), (2, 10)] {
        let hit = search.hit(&atlas, 1, id).unwrap();
        assert_eq!(hit.original_range.start().get(), start);
        assert_eq!(hit.original_range.end().get(), start + 6);
    }
    fs::write(f.0.join("a.rs"), b"different now").unwrap();
    let (reader, _) = search.open_reader(&atlas, owner(8402), 1, 2, || false).unwrap();
    assert_eq!(reader.capture().bytes(), raw);
    assert!(search.cancel_pending()); assert!(!search.cancel_pending());
    assert_eq!(&reader.capture().bytes()[10..16], b"a\0b\0a\0");
}

#[test]
fn quotas_have_terminal_reasons_and_never_claim_complete_coverage() {
    let f = Fixture::new(&[("a.rs", b"needle"), ("b.rs", b"needle"), ("c.rs", b"needle")]);
    let atlas = f.open();
    for (limits, expected) in [
        (AtlasSearchOptions { max_files: 1, ..options() }, AtlasSearchStop::FileLimit),
        (AtlasSearchOptions { max_source_bytes: 6, ..options() }, AtlasSearchStop::SourceByteLimit),
        (AtlasSearchOptions { max_matches: 1, ..options() }, AtlasSearchStop::MatchLimit),
    ] {
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search.begin(&atlas, 1, "needle", limits, || false).unwrap();
        search.step(&atlas, 1, || false).unwrap();
        let p = search.progress(&atlas, 1).unwrap();
        assert_eq!(p.stop_reason, Some(expected)); assert!(!p.complete);
        assert_eq!(p.examined_files, 1); assert_eq!(p.pending_files, 2); assert_eq!(p.source_bytes_read, 6);
    }
}

#[test]
fn oversized_files_are_reported_as_unavailable_one_per_step() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "needle", AtlasSearchOptions { max_file_bytes: 1, ..options() }, || false).unwrap();
    for count in 1..=3 {
        search.step(&atlas, 1, || false).unwrap();
        let p = search.progress(&atlas, 1).unwrap();
        assert_eq!(p.unavailable_files, count); assert_eq!(p.last_step_files, 1);
        assert_eq!(p.last_step_source_bytes, 0); assert_eq!(p.read_calls, 0);
    }
    assert!(!search.progress(&atlas, 1).unwrap().complete);
}

#[test]
fn empty_catalog_finishes_without_io_and_incomplete_discovery_stays_incomplete() {
    let empty = Fixture::new(&[]); let atlas = empty.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "needle", options(), || false).unwrap();
    assert_eq!(search.step(&atlas, 1, || false).unwrap().exit_code(), EXIT_OK);
    let p = search.progress(&atlas, 1).unwrap(); assert!(p.complete);
    assert_eq!(p.last_step_files, 0); assert_eq!(p.source_bytes_read, 0);
    let f = fixture();
    let partial = AtlasSession::open(owner(8401), &f.0, AtlasSessionOptions { max_files: 1, ..Default::default() }, || false).unwrap();
    let mut search = RetainedAtlasSearch::new(&partial).unwrap();
    search.begin(&partial, 1, "needle", options(), || false).unwrap(); finish(&mut search, &partial, 1);
    assert!(!search.progress(&partial, 1).unwrap().complete);
}

#[test]
fn foreign_atlas_refusal_preserves_pending_job_and_revocation_stops_new_delivery() {
    let f = fixture(); let atlas = f.open(); let other = f.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, 1, "needle", options(), || false).unwrap();
    assert!(matches!(search.step(&other, 1, || false), Err(AtlasSearchError::WrongAtlas)));
    assert_eq!(search.progress(&atlas, 1).unwrap().examined_files, 0);
    search.step(&atlas, 1, || false).unwrap();
    atlas.atlas().catalog().grant().revoke();
    assert!(search.step(&atlas, 1, || false).is_err());
    assert!(search.page(&atlas, 1, 0, 64, || false).is_err());
    assert!(search.open_reader(&atlas, owner(8402), 1, 1, || false).is_err());
    assert!(search.cancel_pending());
}

#[test]
fn generation_exhaustion_and_clear_never_resurrect_provisional_results() {
    let f = fixture(); let mut atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin(&atlas, u64::MAX - 1, "needle", options(), || false).unwrap();
    search.step(&atlas, u64::MAX - 1, || false).unwrap();
    search.clear(&atlas, u64::MAX, || false).unwrap();
    assert_eq!(search.pending_generation(), None); assert_eq!(search.accepted_generation(), None);
    assert!(search.step(&atlas, u64::MAX - 1, || false).is_err());
    assert!(search.begin(&atlas, 1, "needle", options(), || false).is_err());
    assert!(search.begin(&atlas, u64::MAX, "needle", options(), || false).is_err());
    // Exhausted search identities do not poison atlas camera operations.
    assert!(atlas.prepare(1, AtlasAction::View, || false).is_ok());
}
