#![forbid(unsafe_code)]
#![cfg(unix)]

//! Exact totals and bounded navigable rows over real retained captures.
//! Code-first regression cases; independent batch execution remains pending.

mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError,
    AtlasSearchPathKind, AtlasSearchScope, AtlasSearchStop, RetainedAtlasSearch};
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-counted-index-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(33901), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn owner(id: u64) -> ArenaOwnerId { ArenaOwnerId::new(id).unwrap() }
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 16, max_file_bytes: 8192,
        max_source_bytes: 65536, max_index_grams: 65536 }
}
fn setup(files: &[(&str, &[u8])]) -> (Fixture, AtlasSession, RetainedAtlasSearch) {
    let fixture = Fixture::new(files);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, options(), || false).unwrap();
    (fixture, atlas, search)
}
fn decoded(text: &str) -> Json { parse(text.as_bytes()).unwrap() }
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> Json {
    for _ in 0..=16 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            return decoded(search.page(atlas, generation, 0, 64, || false).unwrap().as_str());
        }
        search.step(atlas, generation, || false).unwrap();
        assert!(search.progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("counted query did not finish within its admitted file count");
}
fn counted(page: &Json, matches: u64, retained: u64, complete: bool) {
    assert!(page.get("count_all_matches").flag());
    assert_eq!(page.get("matches_seen").number(), matches);
    assert_eq!(page.get("retained_hits").number(), retained);
    assert_eq!(page.get("truncated").flag(), matches > retained);
    assert_eq!(page.get("match_count_complete").flag(), complete);
    assert_eq!(page.get("search_complete").flag(), complete);
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("read_calls").number(), 0);
}

#[test]
fn counted_query_continues_after_full_rows_and_keeps_early_source_identity() {
    let (fixture, atlas, mut search) = setup(&[
        ("a.rs", b"banana"), ("b.rs", b"banana banana"), ("c.rs", b"no match")]);
    let begin = search.begin_indexed_counted(&atlas, 2, 1, "ana", 1, 65536, || false).unwrap();
    assert_eq!(begin.exit_code(), EXIT_PARTIAL);
    let begin = decoded(begin.as_str());
    counted(&begin, 0, 0, false);
    assert_eq!(begin.get("verification_source_bytes").number(), 0);
    search.step(&atlas, 2, || false).unwrap();
    let early = search.hit(&atlas, 2, 1).unwrap();
    let progress = search.progress(&atlas, 2).unwrap();
    assert!(progress.is_running()); assert!(progress.truncated);
    assert_eq!(progress.matches_seen, 2); assert_eq!(progress.retained_hits, 1);
    fs::write(fixture.0.join("a.rs"), b"changed live source").unwrap();
    fs::write(fixture.0.join("b.rs"), b"changed live source").unwrap();
    let page = finish(&mut search, &atlas, 2);
    counted(&page, 6, 1, true);
    assert_eq!(page.get("stop_reason").text(), "all-files-examined");
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), early);
    let (reader, _) = search.open_reader(&atlas, owner(33902), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"banana");
    assert_eq!(early.original_range.start().get(), 1);
    assert_eq!(early.original_range.end().get(), 4);
}

#[test]
fn legacy_bounded_query_is_a_negative_control_for_the_exhaustive_total() {
    let (_fixture, atlas, mut search) = setup(&[("a.rs", b"banana"), ("b.rs", b"banana banana")]);
    search.begin_indexed(&atlas, 2, 1, "ana", 1, 65536, || false).unwrap();
    let bounded = finish(&mut search, &atlas, 2);
    assert!(!bounded.get("count_all_matches").flag());
    assert!(!bounded.get("match_count_complete").flag());
    assert_eq!(bounded.get("matches_seen").number(), 2);
    assert_eq!(bounded.get("stop_reason").text(), "match-limit");
    let exact = search.search_indexed_counted_scoped(&atlas, 3, 1, "ana",
        AtlasSearchScope::all(), 1, 65536, || false).unwrap();
    // The response still reports partial row retention independently of the
    // exhaustive occurrence count.
    assert_eq!(exact.exit_code(), EXIT_PARTIAL);
    counted(&decoded(exact.as_str()), 6, 1, true);
}

#[test]
fn sync_and_progressive_counting_share_scope_totals_and_capture_offsets() {
    let utf16: Vec<u8> = "\u{feff}banana".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let (_fixture, atlas, mut search) = setup(&[
        ("src/a.rs", b"banana"), ("src/b.rs", &utf16), ("other/c.rs", b"banana banana")]);
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    search.begin_indexed_counted_scoped(&atlas, 2, 1, "ana", scope.clone(), 4, 65536, || false).unwrap();
    let page = finish(&mut search, &atlas, 2);
    counted(&page, 4, 4, true);
    assert_eq!(page.get("eligible_files").number(), 2);
    assert_eq!(page.get("excluded_files").number(), 1);
    let ranges = (1..=4).map(|id| search.hit(&atlas, 2, id).unwrap().original_range).collect::<Vec<_>>();
    assert_eq!((ranges[2].start().get(), ranges[2].end().get()), (4, 10));
    assert_eq!((ranges[3].start().get(), ranges[3].end().get()), (8, 14));
    let response = search.search_indexed_counted_scoped(&atlas, 3, 1, "ana", scope, 4, 65536, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    counted(&decoded(response.as_str()), 4, 4, true);
    assert_eq!((1..=4).map(|id| search.hit(&atlas, 3, id).unwrap().original_range).collect::<Vec<_>>(), ranges);
}

#[test]
fn counting_after_row_limit_still_obeys_verification_budget() {
    let (_fixture, atlas, mut search) = setup(&[("a.rs", b"banana"), ("b.rs", b"banana banana")]);
    search.begin_indexed_counted(&atlas, 2, 1, "ana", 1, 6, || false).unwrap();
    let page = finish(&mut search, &atlas, 2);
    counted(&page, 2, 1, false);
    assert_eq!(page.get("verification_source_bytes").number(), 6);
    assert_eq!(page.get("stop_reason").text(), "verification-byte-limit");
    assert_eq!(search.progress(&atlas, 2).unwrap().stop_reason, Some(AtlasSearchStop::VerificationByteLimit));
}

#[test]
fn incomplete_capture_scope_cannot_be_promoted_to_an_exact_workspace_total() {
    let fixture = Fixture::new(&[("a.rs", b"banana"), ("b.rs", b"banana banana")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "a.rs").unwrap();
    search.prepare_index_scoped(&atlas, 1, scope.clone(), options(), || false).unwrap();
    search.begin_indexed_counted(&atlas, 2, 1, "ana", 1, 65536, || false).unwrap();
    let page = finish(&mut search, &atlas, 2);
    counted(&page, 2, 1, false);
    assert_eq!(page.get("pending_files").number(), 1);
    assert_eq!(page.get("stop_reason").text(), "capture-scope");
    search.begin_indexed_counted_scoped(&atlas, 3, 1, "ana", scope, 1, 65536, || false).unwrap();
    counted(&finish(&mut search, &atlas, 3), 2, 1, true);
}

#[test]
fn missing_visible_members_and_unsupported_text_keep_counts_partial() {
    let (_fixture, atlas, mut search) = setup(&[("a.rs", b"banana"), ("b.rs", &[0xff, 0xfe, 0x61])]);
    search.begin_indexed_counted(&atlas, 2, 1, "ana", 1, 65536, || false).unwrap();
    let page = finish(&mut search, &atlas, 2);
    counted(&page, 2, 1, false);
    assert_eq!(page.get("unavailable_files").number(), 1);
    let visible = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible,
        "612e7273,6d697373696e672e7273").unwrap(); // a.rs, missing.rs
    search.begin_indexed_counted_scoped(&atlas, 3, 1, "ana", visible, 1, 65536, || false).unwrap();
    let page = finish(&mut search, &atlas, 3);
    counted(&page, 2, 1, false);
    assert_eq!(page.get("scope_region_missing_files").number(), 1);
}

#[test]
fn cancellation_and_invalid_case_preserve_the_previously_accepted_capture() {
    let (_fixture, atlas, mut search) = setup(&[("a.rs", b"banana"), ("b.rs", b"banana banana")]);
    search.search_indexed_counted_scoped(&atlas, 2, 1, "ana", AtlasSearchScope::all(), 1, 65536, || false).unwrap();
    let old = search.hit(&atlas, 2, 1).unwrap();
    search.begin_indexed_counted(&atlas, 3, 1, "ana", 1, 65536, || false).unwrap();
    search.step(&atlas, 3, || false).unwrap();
    assert_eq!(search.begin_indexed_counted_scoped(&atlas, 4, 1, "ANA",
        AtlasSearchScope::all().with_case_sensitive(false), 1, 65536, || false).err(),
        Some(AtlasSearchError::InvalidScope));
    assert_eq!(search.pending_generation(), Some(3));
    assert_eq!(search.step(&atlas, 3, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.pending_generation(), None);
    assert_eq!(search.accepted_generation(), Some(2));
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), old);
    search.begin_indexed_counted(&atlas, 4, 1, "ana", 1, 65536, || false).unwrap();
    assert_eq!(search.step(&atlas, 3, || false).err(), Some(AtlasSearchError::StaleQuery));
    counted(&finish(&mut search, &atlas, 4), 6, 1, true);
}
