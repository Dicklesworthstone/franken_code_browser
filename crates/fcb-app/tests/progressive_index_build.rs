#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real capture/index/query workflows. No native pixels or fixed latency claim.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchOptions, AtlasSearchError,
    AtlasSearchStop, RetainedAtlasSearch};
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};

struct Fixture(PathBuf);
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-index-build-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files { fs::write(root.join(name), bytes).unwrap(); }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(9401), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 1024, max_source_bytes: 65536, max_index_grams: 65536 }
}
fn fixture() -> Fixture { Fixture::new(&[("a.rs", b"old needle"), ("b.rs", b"other needle"), ("c.rs", b"third")]) }
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) {
    for _ in 0..101 {
        if !search.index_build_progress(atlas, generation).unwrap().in_progress { return; }
        search.step_index(atlas, generation, || false).unwrap();
    }
    panic!("index construction failed to terminate within its file limit");
}
fn query(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64, index: u64, text: &str) {
    search.search_indexed(atlas, generation, index, text, 100, 65536, || false).unwrap();
}

#[test]
fn admission_does_no_source_io_and_each_step_captures_and_indexes_only_one_file() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let first = search.begin_index(&atlas, 1, options(), || false).unwrap();
    assert!(first.as_str().contains("\"index_build_in_progress\":true"));
    let p = search.index_build_progress(&atlas, 1).unwrap();
    assert_eq!((p.examined_files, p.source_bytes_read, p.captured_files, p.indexed_files), (0, 0, 0, 0));
    assert_eq!(search.index_generation(), None);
    fs::remove_file(f.0.join("a.rs")).unwrap(); // Begin did not hide a capture.
    for n in 1..=3 {
        search.step_index(&atlas, 1, || false).unwrap();
        let p = search.index_build_progress(&atlas, 1).unwrap();
        assert_eq!(p.examined_files, n); assert_eq!(p.last_step_files, 1); assert_eq!(p.steps, n as u64);
        assert_eq!(p.pending_files, 3 - n); assert!(p.last_step_source_bytes <= 1024);
        assert_eq!(p.unavailable_files, 1);
        assert_eq!(p.captured_files, n - 1); assert_eq!(p.indexed_files, n - 1);
    }
    assert_eq!(search.index_generation(), Some(1));
    query(&mut search, &atlas, 2, 1, "needle");
    assert_eq!(search.progress(&atlas, 2).unwrap().retained_hits, 1);
    assert!(!search.progress(&atlas, 2).unwrap().complete);
}

#[test]
fn first_capture_is_not_reopened_while_later_members_are_still_being_prepared() {
    let f = fixture(); let atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_index(&atlas, 1, options(), || false).unwrap();
    search.step_index(&atlas, 1, || false).unwrap();
    fs::remove_file(f.0.join("a.rs")).unwrap();
    fs::write(f.0.join("b.rs"), b"new second needle").unwrap();
    finish(&mut search, &atlas, 1);
    fs::remove_file(f.0.join("b.rs")).unwrap(); fs::remove_file(f.0.join("c.rs")).unwrap();
    query(&mut search, &atlas, 2, 1, "old");
    assert_eq!(search.progress(&atlas, 2).unwrap().source_bytes_read, 0);
    let (reader, _) = search.open_reader(&atlas, owner(9402), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"old needle");
    query(&mut search, &atlas, 3, 1, "new");
    let (reader, _) = search.open_reader(&atlas, owner(9403), 3, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"new second needle");
}

#[test]
fn old_index_supports_new_queries_between_rebuild_steps_and_accepted_hits_survive_publication() {
    let f = fixture(); let mut atlas = f.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    fs::write(f.0.join("a.rs"), b"new source").unwrap();
    search.begin_index(&atlas, 2, options(), || false).unwrap(); search.step_index(&atlas, 2, || false).unwrap();
    query(&mut search, &atlas, 3, 1, "old");
    assert_eq!(search.pending_index_generation(), Some(2)); assert_eq!(search.index_generation(), Some(1));
    let hit = search.hit(&atlas, 3, 1).unwrap();
    search.focus_hit(&mut atlas, 3, 1, 1, || false).unwrap();
    atlas.acknowledge(1, 1, 1, || false).unwrap();
    finish(&mut search, &atlas, 2);
    assert_eq!(search.index_generation(), Some(2)); assert_eq!(search.hit(&atlas, 3, 1).unwrap(), hit);
    assert_eq!(atlas.presented_plan().unwrap().generation().get(), 1);
    let (reader, _) = search.open_reader(&atlas, owner(9404), 3, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"old needle");
    query(&mut search, &atlas, 4, 2, "new");
    assert_eq!(search.progress(&atlas, 4).unwrap().retained_hits, 1);
}

#[test]
fn synchronous_and_stepped_construction_produce_identical_query_hits_and_coverage() {
    let f = fixture(); let atlas = f.open();
    let mut a = RetainedAtlasSearch::new(&atlas).unwrap(); let mut b = RetainedAtlasSearch::new(&atlas).unwrap();
    a.prepare_index(&atlas, 1, options(), || false).unwrap();
    b.begin_index(&atlas, 1, options(), || false).unwrap(); finish(&mut b, &atlas, 1);
    assert_eq!(a.index_build_progress(&atlas, 1).unwrap(), b.index_build_progress(&atlas, 1).unwrap());
    assert_eq!(a.index_info(&atlas, || false).unwrap().as_str(), b.index_info(&atlas, || false).unwrap().as_str());
    for (generation, text) in [(2, "needle"), (3, "old"), (4, "missing")] {
        query(&mut a, &atlas, generation, 1, text); query(&mut b, &atlas, generation, 1, text);
        assert_eq!(a.page(&atlas, generation, 0, 100, || false).unwrap().as_str(),
            b.page(&atlas, generation, 0, 100, || false).unwrap().as_str());
    }
}

#[test]
fn each_single_pulse_cancellation_checkpoint_preserves_prior_index_and_rows() {
    let f = fixture(); let atlas = f.open();
    let prepared = || {
        let mut s = RetainedAtlasSearch::new(&atlas).unwrap();
        s.prepare_index(&atlas, 1, options(), || false).unwrap(); query(&mut s, &atlas, 2, 1, "old");
        s.begin_index(&atlas, 3, options(), || false).unwrap(); s
    };
    let mut baseline = prepared(); let mut polls = 0;
    baseline.step_index(&atlas, 3, || { polls += 1; false }).unwrap(); drop(baseline);
    assert!(polls > 5);
    for at in 1..=polls {
        let mut s = prepared(); let mut seen = 0;
        assert!(s.step_index(&atlas, 3, || { seen += 1; seen == at }).is_err(), "checkpoint {at}");
        assert_eq!(s.pending_index_generation(), None);
        assert_eq!(s.index_generation(), Some(1)); assert_eq!(s.accepted_generation(), Some(2));
        assert!(s.page(&atlas, 2, 0, 10, || false).is_ok());
        query(&mut s, &atlas, 4, 1, "needle"); assert_eq!(s.progress(&atlas, 4).unwrap().retained_hits, 2);
    }
}

#[test]
fn quotas_terminate_with_explicit_partial_membership_not_a_complete_negative() {
    let f = Fixture::new(&[("a.rs", b"needle"), ("b.rs", b"needle")]); let atlas = f.open();
    for (limits, reason) in [
        (AtlasIndexOptions { max_files: 1, ..options() }, AtlasSearchStop::FileLimit),
        (AtlasIndexOptions { max_source_bytes: 6, ..options() }, AtlasSearchStop::SourceByteLimit)] {
        let mut s = RetainedAtlasSearch::new(&atlas).unwrap(); s.begin_index(&atlas, 1, limits, || false).unwrap();
        finish(&mut s, &atlas, 1); let p = s.index_build_progress(&atlas, 1).unwrap();
        assert!(!p.in_progress); assert_eq!(p.stop_reason, Some(reason)); assert_eq!(p.pending_files, 1);
        assert_eq!(p.source_bytes_read, 6);
        query(&mut s, &atlas, 2, 1, "absent"); assert!(!s.progress(&atlas, 2).unwrap().complete);
    }
}

#[test]
fn zero_gram_budget_preserves_utf16_sources_for_complete_exact_queries() {
    let bytes: Vec<u8> = "\u{feff}needle 🦀".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let f = Fixture::new(&[("a.rs", &bytes)]); let atlas = f.open(); let mut s = RetainedAtlasSearch::new(&atlas).unwrap();
    s.begin_index(&atlas, 1, AtlasIndexOptions { max_index_grams: 0, ..options() }, || false).unwrap();
    finish(&mut s, &atlas, 1); assert_eq!(s.index_build_progress(&atlas, 1).unwrap().uncovered_files, 1);
    fs::remove_file(f.0.join("a.rs")).unwrap(); query(&mut s, &atlas, 2, 1, "needle");
    assert!(s.progress(&atlas, 2).unwrap().complete);
    assert_eq!(s.hit(&atlas, 2, 1).unwrap().original_range.start().get(), 2);
    assert_eq!(s.hit(&atlas, 2, 1).unwrap().original_range.len().get(), 12);
}

#[test]
fn newer_build_supersedes_older_work_and_terminal_retries_do_not_read_source() {
    let f = fixture(); let atlas = f.open(); let mut s = RetainedAtlasSearch::new(&atlas).unwrap();
    s.begin_index(&atlas, 1, options(), || false).unwrap(); s.step_index(&atlas, 1, || false).unwrap();
    s.begin_index(&atlas, 2, options(), || false).unwrap();
    assert_eq!(s.step_index(&atlas, 1, || false).err(), Some(AtlasSearchError::StaleIndex));
    assert_eq!(s.index_build_progress(&atlas, 2).unwrap().examined_files, 0);
    finish(&mut s, &atlas, 2); let old = s.index_build_progress(&atlas, 2).unwrap();
    fs::remove_file(f.0.join("a.rs")).unwrap();
    for _ in 0..3 { s.step_index(&atlas, 2, || false).unwrap(); }
    assert_eq!(s.index_build_progress(&atlas, 2).unwrap(), old);
    s.begin_index(&atlas, 3, options(), || false).unwrap();
    assert!(s.begin_index(&atlas, 4, AtlasIndexOptions { max_files: 0, ..options() }, || false).is_err());
    assert_eq!(s.pending_index_generation(), None); assert_eq!(s.index_generation(), Some(2));
    assert!(s.begin_index(&atlas, 4, options(), || false).is_err());
}

#[test]
fn publishing_replacement_retires_only_queries_tied_to_the_old_index() {
    let f = fixture(); let atlas = f.open(); let mut s = RetainedAtlasSearch::new(&atlas).unwrap();
    s.prepare_index(&atlas, 1, options(), || false).unwrap(); query(&mut s, &atlas, 2, 1, "old");
    s.begin_index(&atlas, 3, options(), || false).unwrap();
    s.begin_indexed(&atlas, 4, 1, "needle", 100, 65536, || false).unwrap(); s.step(&atlas, 4, || false).unwrap();
    finish(&mut s, &atlas, 3);
    assert_eq!(s.pending_generation(), None); assert_eq!(s.accepted_generation(), Some(2));
    assert!(s.step(&atlas, 4, || false).is_err());
    s.begin_index(&atlas, 5, options(), || false).unwrap();
    s.begin(&atlas, 6, "needle", AtlasSearchOptions { max_files: 100, max_file_bytes: 1024,
        max_source_bytes: 65536, max_matches: 100 }, || false).unwrap();
    finish(&mut s, &atlas, 5); assert_eq!(s.pending_generation(), Some(6));
    while s.progress(&atlas, 6).unwrap().is_running() { s.step(&atlas, 6, || false).unwrap(); }
    assert!(s.progress(&atlas, 6).unwrap().complete);
}

#[test]
fn explicit_builder_cancel_clear_and_empty_finalization_are_independent_of_result_pins() {
    let f = fixture(); let atlas = f.open(); let mut s = RetainedAtlasSearch::new(&atlas).unwrap();
    s.prepare_index(&atlas, 1, options(), || false).unwrap(); query(&mut s, &atlas, 2, 1, "old");
    s.begin_index(&atlas, 3, options(), || false).unwrap(); assert!(s.cancel_index_build()); assert!(!s.cancel_index_build());
    assert_eq!(s.index_generation(), Some(1)); assert_eq!(s.accepted_generation(), Some(2));
    s.begin_index(&atlas, 4, options(), || false).unwrap(); s.clear_index(&atlas, 5, || false).unwrap();
    assert_eq!(s.index_generation(), None); assert_eq!(s.pending_index_generation(), None);
    assert!(s.open_reader(&atlas, owner(9405), 2, 1, || false).is_ok());
    let empty = Fixture::new(&[]); let atlas = empty.open(); let mut s = RetainedAtlasSearch::new(&atlas).unwrap();
    s.begin_index(&atlas, 1, options(), || false).unwrap(); s.step_index(&atlas, 1, || false).unwrap();
    let p = s.index_build_progress(&atlas, 1).unwrap();
    assert!(!p.in_progress); assert_eq!((p.examined_files, p.source_bytes_read), (0, 0));
    query(&mut s, &atlas, 2, 1, "absent"); assert!(s.progress(&atlas, 2).unwrap().complete);
}
