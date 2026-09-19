#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production capture/index/query/activation composition. Tests deliberately
//! mutate fixture paths after preparation; no native presentation is inferred.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::atlas_session::{AtlasAction, AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchStop, RetainedAtlasSearch};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
struct Fixture { root: PathBuf, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(27010);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-index-host-{}-{}-{id}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        for (path, bytes) in files { fs::write(root.join(path), bytes).unwrap(); }
        let atlas = AtlasSession::open(owner(id), &root, AtlasSessionOptions::default(), || false).unwrap();
        let search = RetainedAtlasSearch::new(&atlas).unwrap();
        Self { root, atlas, search }
    }
    fn build(&mut self, generation: u64) {
        self.search.prepare_index(&self.atlas, generation, options(), || false).unwrap();
    }
    fn query(&mut self, generation: u64, index: u64, needle: &str) -> String {
        self.search.search_indexed(&self.atlas, generation, index, needle, 100, 65536, || false)
            .unwrap().as_str().to_owned()
    }
}
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 16384, max_source_bytes: 65536, max_index_grams: 65536 }
}

#[test]
fn repeated_queries_retain_even_previously_nonmatching_files_without_reopening_paths() {
    let mut f = Fixture::new(&[("a.rs", b"first needle\n"), ("b.rs", b"second target\n")]);
    f.build(1);
    assert!(f.query(2, 1, "needle").contains("\"retained_hits\":\"1\""));
    fs::rename(f.root.join("a.rs"), f.root.join("a.moved")).unwrap();
    fs::rename(f.root.join("b.rs"), f.root.join("b.moved")).unwrap();
    let found = f.query(3, 1, "target");
    assert!(found.contains("\"source_bytes_read\":\"0\""));
    assert!(found.contains("\"read_calls\":\"0\""));
    assert!(found.contains("\"search_complete\":true"));
    let hit = f.search.hit(&f.atlas, 3, 1).unwrap();
    assert_eq!(hit.revision.get(), 1); // Source generation is NOT the new query.
    let (reader, reply) = f.search.open_reader(&f.atlas, owner(90001), 3, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"second target\n");
    assert!(reply.as_str().contains("\"source_reopened\":false"));
    assert!(reply.as_str().contains("\"index_generation\":\"1\""));
}

#[test]
fn negative_certificates_skip_verification_but_not_source_membership() {
    let mut f = Fixture::new(&[("a.rs", b"banana needle"), ("b.rs", b"unrelated source")]);
    f.build(1);
    let found = f.query(2, 1, "needle");
    assert!(found.contains("\"skipped_by_index\":\"1\""));
    assert!(found.contains("\"verified_files\":\"1\""));
    let absent = f.search.search_indexed(&f.atlas, 3, 1, "xyzxyz", 100, 0, || false).unwrap();
    assert_eq!(absent.exit_code(), EXIT_OK);
    assert!(absent.as_str().contains("\"skipped_by_index\":\"2\""));
    assert!(absent.as_str().contains("\"verification_source_bytes\":\"0\""));
    assert_eq!(f.search.progress(&f.atlas, 3).unwrap().examined_files, 2);
}

#[test]
fn utf16_and_overlapping_short_queries_take_sound_fallback_and_keep_original_bytes() {
    for little in [true, false] {
        let bytes: Vec<u8> = "\u{feff}🦀 banana needle".encode_utf16()
            .flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect();
        let mut f = Fixture::new(&[("utf16.rs", &bytes)]); f.build(1);
        let result = f.query(2, 1, "ana");
        assert!(result.contains("\"fallback_files\":\"1\""));
        assert_eq!(f.search.progress(&f.atlas, 2).unwrap().retained_hits, 2);
        for id in 1..=2 {
            let hit = f.search.hit(&f.atlas, 2, id).unwrap();
            assert_eq!(hit.original_range.len().get(), 6);
            let expected: Vec<u8> = "ana".encode_utf16()
                .flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect();
            let (a, b) = hit.original_range.as_usize_bounds().unwrap();
            assert_eq!(&bytes[a..b], expected);
        }
        assert!(f.search.progress(&f.atlas, 2).unwrap().complete);
        assert!(f.query(3, 1, "an").contains("\"fallback_files\":\"1\""));
    }
}

#[test]
fn failed_or_canceled_rebuild_preserves_both_previous_index_and_visible_results() {
    let mut f = Fixture::new(&[("a.rs", b"old needle")]); f.build(1); f.query(2, 1, "needle");
    let old = f.search.hit(&f.atlas, 2, 1).unwrap();
    fs::write(f.root.join("a.rs"), b"new source").unwrap();
    assert_eq!(f.search.prepare_index(&f.atlas, 3,
        AtlasIndexOptions { max_files: 0, ..options() }, || false).err(), Some(AtlasSearchError::InvalidLimits));
    assert_eq!(f.search.prepare_index(&f.atlas, 4, options(), || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(f.search.index_generation(), Some(1));
    assert_eq!(f.search.hit(&f.atlas, 2, 1).unwrap(), old);
    assert_eq!(f.search.prepare_index(&f.atlas, 4, options(), || false).err(), Some(AtlasSearchError::StaleQuery));
    assert!(f.query(5, 1, "needle").contains("\"retained_hits\":\"1\""));
}

#[test]
fn one_shot_query_cancellation_pulses_do_not_lose_postings_or_publish_partial_results() {
    let mut f = Fixture::new(&[("a.rs", b"old needle needle"), ("b.rs", b"another needle")]);
    f.build(1); f.query(2, 1, "old");
    let mut failures = 0;
    for pulse in 1..=24 {
        let old = f.search.accepted_generation();
        let generation = 2 + pulse as u64;
        let mut polls = 0;
        let result = f.search.search_indexed(&f.atlas, generation, 1, "needle", 100, 65536, || {
            polls += 1; polls == pulse
        });
        if result.is_err() { failures += 1; assert_eq!(f.search.accepted_generation(), old); }
        assert_eq!(f.search.index_generation(), Some(1));
    }
    assert!(failures >= 5);
    f.query(100, 1, "needle");
    assert_eq!(f.search.progress(&f.atlas, 100).unwrap().retained_hits, 3);
}

#[test]
fn successful_index_refresh_does_not_rebind_an_accepted_hit_or_delivered_reader() {
    let mut f = Fixture::new(&[("a.rs", b"old needle")]); f.build(1); f.query(2, 1, "needle");
    fs::write(f.root.join("a.rs"), b"new target").unwrap(); f.build(3);
    let (reader, _) = f.search.open_reader(&f.atlas, owner(90002), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"old needle");
    assert_eq!(f.search.search_indexed(&f.atlas, 4, 1, "needle", 100, 65536, || false).err(), Some(AtlasSearchError::StaleIndex));
    f.query(5, 3, "target");
    assert_eq!(f.search.hit(&f.atlas, 5, 1).unwrap().revision.get(), 3);
    assert_eq!(f.search.hit(&f.atlas, 2, 1).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(reader.capture().bytes(), b"old needle");
}

#[test]
fn index_and_result_clear_have_independent_source_lifetimes() {
    let mut f = Fixture::new(&[("a.rs", b"needle here")]); f.build(1); f.query(2, 1, "needle");
    f.search.clear(&f.atlas, 3, || false).unwrap();
    assert_eq!(f.search.index_generation(), Some(1));
    f.query(4, 1, "needle");
    f.search.clear_index(&f.atlas, 5, || false).unwrap();
    assert_eq!(f.search.indexed_source_bytes(), 0);
    assert_eq!(f.search.accepted_generation(), Some(4));
    let (reader, _) = f.search.open_reader(&f.atlas, owner(90003), 4, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle here");
    assert_eq!(f.search.search_indexed(&f.atlas, 6, 1, "needle", 10, 100, || false).err(), Some(AtlasSearchError::MissingIndex));
    assert!(f.search.page(&f.atlas, 4, 0, 10, || false).is_ok());
}

#[test]
fn missing_files_and_unprepared_catalog_suffixes_are_not_complete_negative_results() {
    let mut f = Fixture::new(&[("a.rs", b"needle"), ("b.rs", b"unavailable"), ("c.rs", b"later")]);
    fs::rename(f.root.join("b.rs"), f.root.join("b.moved")).unwrap();
    let prepared = f.search.prepare_index(&f.atlas, 1,
        AtlasIndexOptions { max_files: 2, ..options() }, || false).unwrap();
    assert_eq!(prepared.exit_code(), EXIT_PARTIAL);
    assert!(prepared.as_str().contains("\"pending_files\":\"1\""));
    assert!(prepared.as_str().contains("\"unavailable_files\":\"1\""));
    f.query(2, 1, "absent");
    let progress = f.search.progress(&f.atlas, 2).unwrap();
    assert!(!progress.complete); assert!(!progress.is_running());
    assert_eq!(progress.unavailable_files, 1); assert_eq!(progress.pending_files, 1);
}

#[test]
fn gram_quota_is_fallback_not_exclusion_and_malformed_text_stays_unavailable() {
    let mut f = Fixture::new(&[("a.rs", b"needle"), ("b.rs", b"bad\xfftext")]);
    f.search.prepare_index(&f.atlas, 1, AtlasIndexOptions { max_index_grams: 0, ..options() }, || false).unwrap();
    let result = f.query(2, 1, "needle");
    assert!(result.contains("\"fallback_files\":\"2\""));
    assert_eq!(f.search.progress(&f.atlas, 2).unwrap().retained_hits, 1);
    assert_eq!(f.search.progress(&f.atlas, 2).unwrap().unavailable_files, 1);
    assert!(!f.search.progress(&f.atlas, 2).unwrap().complete);
    assert!(result.contains("UNSUPPORTED_TEXT"));
}

#[test]
fn literal_syntax_paging_overlay_and_focus_reuse_existing_navigation() {
    let bytes = "path:src -not \"quoted\" ".repeat(80);
    let mut f = Fixture::new(&[("raw.rs", bytes.as_bytes())]); f.build(1);
    f.atlas.prepare(1, AtlasAction::View, || false).unwrap();
    f.atlas.acknowledge(1, 1, 1, || false).unwrap();
    let found = f.query(2, 1, "path:src -not \"quoted\"");
    assert!(found.contains("\"next_offset\":\"64\""));
    let page = f.search.page(&f.atlas, 2, 64, 128, || false).unwrap();
    assert!(page.as_str().contains("\"hit_id\":\"65\""));
    assert!(page.as_str().contains("\"next_offset\":null"));
    let overlay = f.search.overlay(&f.atlas, 2, || false).unwrap();
    assert_eq!(overlay.as_str().matches("\"node\":").count(), 1);
    f.search.focus_hit(&mut f.atlas, 2, 80, 2, || false).unwrap();
    assert_eq!(f.atlas.presented_plan().unwrap().generation().get(), 1);
}

#[test]
fn verification_budget_and_hit_lookahead_are_independent_from_disk_and_index_coverage() {
    let mut f = Fixture::new(&[("a.rs", b"needle needle needle")]); f.build(1);
    let limited = f.search.search_indexed(&f.atlas, 2, 1, "needle", 2, 65536, || false).unwrap();
    assert_eq!(limited.exit_code(), EXIT_PARTIAL);
    assert!(f.search.progress(&f.atlas, 2).unwrap().truncated);
    assert_eq!(f.search.progress(&f.atlas, 2).unwrap().matches_seen, 3);
    let limited = f.search.search_indexed(&f.atlas, 3, 1, "needle", 10, 1, || false).unwrap();
    assert_eq!(limited.exit_code(), EXIT_PARTIAL);
    let progress = f.search.progress(&f.atlas, 3).unwrap();
    assert_eq!(progress.source_bytes_read, 0);
    assert_eq!(progress.stop_reason, Some(AtlasSearchStop::VerificationByteLimit));
    assert!(f.query(4, 1, "needle").contains("\"search_complete\":true"));
}

#[test]
fn generation_exhaustion_and_new_attempts_cannot_resume_obsolete_live_work() {
    let mut f = Fixture::new(&[("a.rs", b"needle source")]); f.build(1);
    f.search.begin(&f.atlas, 2, "needle", AtlasSearchOptions { max_source_bytes: 65536, ..Default::default() }, || false).unwrap();
    f.query(3, 1, "needle");
    assert_eq!(f.search.pending_generation(), None);
    assert_eq!(f.search.step(&f.atlas, 2, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(f.search.prepare_index(&f.atlas, u64::MAX, options(), || false).err(), Some(AtlasSearchError::IdentityExhausted));
    assert_eq!(f.search.index_generation(), Some(1));
    assert_eq!(f.search.search_indexed(&f.atlas, 1, 1, "needle", 10, 100, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert!(f.search.page(&f.atlas, 3, 0, 10, || false).is_ok());
}
