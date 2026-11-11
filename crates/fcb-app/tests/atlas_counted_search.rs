#![forbid(unsafe_code)]
#![cfg(unix)]

//! Count-all host composition over real captures, predicates, index cursors and
//! reader activation. Source fixtures only: execution requires strict RCH and
//! these checks do not qualify native presentation or latency.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::HostResponse;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchPathKind as Kind, AtlasSearchScope, AtlasSearchStop, RetainedAtlasSearch};
use support::{Json, parse};

const WORK_BYTES: u64 = 1024 * 1024;
struct Fixture { root: PathBuf, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(49201);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-atlas-counted-{}-{id}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let atlas = AtlasSession::open(owner(id), &root, AtlasSessionOptions::default(), || false).unwrap();
        let search = RetainedAtlasSearch::new(&atlas).unwrap();
        Self { root, atlas, search }
    }
    fn build(&mut self, generation: u64) {
        self.search.prepare_index(&self.atlas, generation, index_options(), || false).unwrap();
    }
    fn finish(&mut self, generation: u64) -> Json {
        for _ in 0..102 {
            let progress = self.search.progress(&self.atlas, generation).unwrap();
            assert_eq!(progress.match_count_complete, progress.complete);
            if !progress.is_running() {
                return json(&self.search.page(&self.atlas, generation, 0, 128, || false).unwrap());
            }
            self.search.step(&self.atlas, generation, || false).unwrap();
            assert!(self.search.progress(&self.atlas, generation).unwrap().last_step_files <= 1);
        }
        panic!("counted query exceeded its bounded fixture membership");
    }
}
// Fixtures remain available for independent replay; source replacement below
// proves that an accepted hit still opens its captured bytes after live edits.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn options(max_matches: usize) -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches, max_files: 100, max_file_bytes: 65536, max_source_bytes: 131072 }
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 65536, max_source_bytes: 131072, max_index_grams: 65536 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}".encode_utf16().chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn counts(page: &Json, seen: u64, rows: u64, files: u64, retained_files: u64) {
    assert!(page.get("count_all_matches").flag());
    assert_eq!(page.get("matches_seen").number(), seen);
    assert_eq!(page.get("retained_hits").number(), rows);
    assert_eq!(page.get("matching_files").number(), files);
    assert_eq!(page.get("retained_matching_files").number(), retained_files);
    assert_eq!(page.get("truncated").flag(), seen > rows);
    assert_eq!(page.get("match_count_complete").flag(), page.get("search_complete").flag());
}

#[test]
fn indexed_counts_continue_after_zero_or_one_row_without_retaining_more_source() {
    let mut f = Fixture::new(&[("a.rs", b"banana banana"), ("b.rs", b"unrelated"),
        ("c.rs", b"banana"), ("d.rs", b"")]);
    f.build(1);
    for (generation, cap) in [(2, 0), (3, 1), (4, 6), (5, 7)] {
        let result = f.search.search_indexed_counted_scoped(&f.atlas, generation, 1, "ana",
            AtlasSearchScope::all(), cap, WORK_BYTES, || false).unwrap();
        let rows = cap.min(6) as u64;
        let retained_files = if cap == 0 { 0 } else if cap == 1 { 1 } else { 2 };
        let page = json(&result);
        counts(&page, 6, rows, 2, retained_files);
        assert!(page.get("match_count_complete").flag());
        assert_eq!(page.get("stop_reason").text(), "all-files-examined");
        assert_eq!(page.get("source_bytes_read").number(), 0);
        assert_eq!(page.get("read_calls").number(), 0);
        assert_eq!(result.exit_code(), if cap < 6 { EXIT_PARTIAL } else { EXIT_OK });
        let progress = f.search.progress(&f.atlas, generation).unwrap();
        assert!(progress.count_all_matches && progress.match_count_complete && progress.complete);
        assert_eq!(progress.matches_seen, 6);
        assert_eq!(progress.matching_files, 2);
        assert_eq!(progress.retained_matching_files as u64, retained_files);
        if cap == 0 { assert_eq!(progress.retained_source_bytes, 0); }
    }
    f.search.search_indexed(&f.atlas, 6, 1, "ana", 1, WORK_BYTES, || false).unwrap();
    let legacy = f.search.progress(&f.atlas, 6).unwrap();
    assert_eq!((legacy.matches_seen, legacy.retained_hits, legacy.matching_files), (2, 1, 1));
    assert_eq!(legacy.stop_reason, Some(AtlasSearchStop::MatchLimit));
    assert!(!legacy.count_all_matches && !legacy.match_count_complete);
    assert_eq!(f.search.begin_indexed(&f.atlas, 7, 1, "ana", 0, WORK_BYTES, || false).err(),
        Some(AtlasSearchError::InvalidLimits));
}

#[test]
fn counted_utf16_and_lowercase_expansions_keep_scope_and_original_capture_bytes() {
    for little in [true, false] {
        let original = "İİ i".as_bytes();
        let encoded = utf16("İ İ", little);
        let mut f = Fixture::new(&[("a.rs", original), ("b.rs", &encoded),
            ("excluded.txt", b"i i i i")]);
        let scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap().with_case_sensitive(false);
        f.build(1);
        f.search.begin_counted_scoped(&f.atlas, 2, "i", scope.clone(), options(1), || false).unwrap();
        let live = f.finish(2);
        counts(&live, 5, 1, 2, 1);
        assert!(live.get("match_count_complete").flag());
        f.search.begin_indexed_counted_scoped(&f.atlas, 3, 1, "i", scope.clone(),
            1, WORK_BYTES, || false).unwrap();
        let pending = f.search.progress(&f.atlas, 3).unwrap();
        assert!(pending.is_running() && pending.count_all_matches && !pending.match_count_complete);
        let indexed = f.finish(3);
        counts(&indexed, 5, 1, 2, 1);
        assert!(indexed.get("match_count_complete").flag());
        assert_eq!(indexed.get("excluded_files").number(), 1);
        assert_eq!(indexed.get("source_bytes_read").number(), 0);
        assert_eq!(indexed.get("fallback_files").number(), 2);
        assert_eq!(indexed.get("hits").array()[0].get("matched_text").text(), "İ");
        let hit = f.search.hit(&f.atlas, 3, 1).unwrap();
        assert_eq!((hit.original_range.start().get(), hit.original_range.end().get()), (0, 2));
        fs::write(f.root.join("a.rs"), b"changed").unwrap();
        fs::write(f.root.join("b.rs"), b"changed").unwrap();
        let (reader, _) = f.search.open_reader(&f.atlas, owner(99201), 3, 1, || false).unwrap();
        assert_eq!(reader.capture().bytes(), original);
        f.search.begin_indexed_counted_scoped(&f.atlas, 4, 1, "i", scope,
            0, WORK_BYTES, || false).unwrap();
        let count_only = f.finish(4);
        counts(&count_only, 5, 0, 2, 0);
        assert!(count_only.get("match_count_complete").flag());
        assert_eq!(count_only.get("retained_source_bytes").number(), 0);
    }
}

#[test]
fn expression_counts_primary_occurrences_without_predicate_or_metadata_inflation() {
    let mut f = Fixture::new(&[("src/a.rs", b"needle needle allow allow"),
        ("src/b.rs", b"needle allow"), ("src/c.rs", b"needle needle deny allow"),
        ("src/d.rs", b"allow"), ("src/e.rs", b""), ("other.txt", b"needle allow")]);
    let query = "needle allow -deny lang:rust path:src/";
    f.build(1);
    for (generation, cap) in [(2, 0), (4, 1)] {
        f.search.begin_expression_counted_scoped(&f.atlas, generation, query,
            AtlasSearchScope::all(), options(cap), WORK_BYTES, || false).unwrap();
        let live = f.finish(generation);
        f.search.begin_indexed_expression_counted_scoped(&f.atlas, generation + 1, 1, query,
            AtlasSearchScope::all(), cap, WORK_BYTES, || false).unwrap();
        let indexed = f.finish(generation + 1);
        for page in [&live, &indexed] {
            counts(page, 3, cap as u64, 2, cap as u64);
            assert!(page.get("match_count_complete").flag());
            assert_eq!(page.get("expression_excluded_files").number(), 1);
            assert_eq!(page.get("stop_reason").text(), "all-files-examined");
            assert!(page.get("scanned_input_bytes").number() <= WORK_BYTES);
        }
        assert_eq!(indexed.get("source_bytes_read").number(), 0);
        assert_eq!(indexed.get("verification_source_bytes").number(), indexed.get("scanned_input_bytes").number());
    }
}

#[test]
fn counted_empty_and_negative_scopes_are_exhaustive_without_inventing_truncation() {
    let mut f = Fixture::new(&[("a.rs", b""), ("b.rs", b"unrelated")]);
    f.build(1);
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 2, 1, "absent",
        AtlasSearchScope::all(), 0, WORK_BYTES, || false).unwrap();
    let negative = json(&result);
    counts(&negative, 0, 0, 0, 0);
    assert!(negative.get("match_count_complete").flag());
    assert_eq!(result.exit_code(), EXIT_OK);
    let empty = AtlasSearchScope::all().with_path(Kind::Directory, "missing").unwrap();
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 3, 1, "absent",
        empty, 0, 0, || false).unwrap();
    let empty = json(&result);
    counts(&empty, 0, 0, 0, 0);
    assert!(empty.get("match_count_complete").flag());
    assert_eq!(empty.get("verification_source_bytes").number(), 0);
    f.search.begin_expression_counted_scoped(&f.atlas, 4, "absent lang:python",
        AtlasSearchScope::all(), options(0), 0, || false).unwrap();
    let empty = f.finish(4);
    counts(&empty, 0, 0, 0, 0);
    assert!(empty.get("match_count_complete").flag());
    assert_eq!(empty.get("source_bytes_read").number(), 0);
}

#[test]
fn byte_limits_remain_partial_after_row_omission_in_live_and_indexed_expressions() {
    let bytes = b"needle needle";
    let mut f = Fixture::new(&[("a.rs", bytes), ("b.rs", bytes)]);
    f.build(1);
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 2, 1, "needle",
        AtlasSearchScope::all(), 1, bytes.len() as u64, || false).unwrap();
    let page = json(&result);
    counts(&page, 2, 1, 1, 1);
    assert!(!page.get("match_count_complete").flag());
    assert_eq!(page.get("stop_reason").text(), "verification-byte-limit");
    assert_eq!(page.get("verification_source_bytes").number(), bytes.len() as u64);
    f.search.begin_expression_counted_scoped(&f.atlas, 3, "needle",
        AtlasSearchScope::all(), options(1), bytes.len() as u64, || false).unwrap();
    let live = f.finish(3);
    f.search.begin_indexed_expression_counted_scoped(&f.atlas, 4, 1, "needle",
        AtlasSearchScope::all(), 1, bytes.len() as u64, || false).unwrap();
    let indexed = f.finish(4);
    for page in [&live, &indexed] {
        counts(page, 2, 1, 1, 1);
        assert!(!page.get("match_count_complete").flag());
        assert_eq!(page.get("stop_reason").text(), "verification-byte-limit");
        assert_eq!(page.get("scanned_input_bytes").number(), bytes.len() as u64);
    }
}

#[test]
fn unavailable_or_malformed_captures_do_not_become_complete_counted_negatives() {
    let large = b"needle ".repeat(20);
    let mut f = Fixture::new(&[("a.rs", b"needle needle"), ("b.rs", b"needle needle"),
        ("c.rs", b"needle\xff"), ("d.rs", &large), ("e.rs", b"needle")]);
    fs::rename(f.root.join("e.rs"), f.root.join("e.moved")).unwrap();
    f.search.prepare_index(&f.atlas, 1, AtlasIndexOptions {
        max_file_bytes: 32, ..index_options() }, || false).unwrap();
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 2, 1, "needle",
        AtlasSearchScope::all(), 1, WORK_BYTES, || false).unwrap();
    let page = json(&result);
    counts(&page, 4, 1, 2, 1);
    assert!(!page.get("match_count_complete").flag());
    assert_eq!(page.get("unavailable_files").number(), 3);
    assert_eq!(page.get("pending_files").number(), 0);
    assert_eq!(page.get("stop_reason").text(), "all-files-examined");
    let scope = AtlasSearchScope::all().with_path(Kind::File, "a.rs").unwrap();
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 3, 1, "absent",
        scope, 0, WORK_BYTES, || false).unwrap();
    let selected = json(&result);
    counts(&selected, 0, 0, 0, 0);
    assert!(selected.get("match_count_complete").flag());
    assert_eq!(selected.get("unavailable_files").number(), 0);
}

#[test]
fn counting_preserves_capture_scope_and_live_file_limits() {
    let mut f = Fixture::new(&[("a.rs", b"needle needle"), ("b.rs", b"needle")]);
    let only_a = AtlasSearchScope::all().with_path(Kind::File, "a.rs").unwrap();
    f.search.prepare_index_scoped(&f.atlas, 1, only_a.clone(), index_options(), || false).unwrap();
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 2, 1, "needle",
        AtlasSearchScope::all(), 0, WORK_BYTES, || false).unwrap();
    let page = json(&result);
    counts(&page, 2, 0, 1, 0);
    assert!(!page.get("match_count_complete").flag());
    assert_eq!(page.get("pending_files").number(), 1);
    assert_eq!(page.get("stop_reason").text(), "capture-scope");
    let result = f.search.search_indexed_counted_scoped(&f.atlas, 3, 1, "needle",
        only_a, 0, WORK_BYTES, || false).unwrap();
    let page = json(&result);
    counts(&page, 2, 0, 1, 0);
    assert!(page.get("match_count_complete").flag());
    f.search.begin_counted_scoped(&f.atlas, 4, "needle", AtlasSearchScope::all(),
        AtlasSearchOptions { max_files: 1, ..options(0) }, || false).unwrap();
    let live = f.finish(4);
    counts(&live, 2, 0, 1, 0);
    assert!(!live.get("match_count_complete").flag());
    assert_eq!(live.get("pending_files").number(), 1);
    assert_eq!(live.get("stop_reason").text(), "file-limit");
}

#[test]
fn cancel_and_stale_steps_preserve_accepted_counts_and_captured_reader() {
    let mut f = Fixture::new(&[("a.rs", b"needle needle"), ("b.rs", b"needle")]);
    f.build(1);
    f.search.search_indexed_counted_scoped(&f.atlas, 2, 1, "needle",
        AtlasSearchScope::all(), 1, WORK_BYTES, || false).unwrap();
    let accepted = f.search.progress(&f.atlas, 2).unwrap();
    assert!(accepted.match_count_complete);
    f.search.begin_indexed_expression_counted_scoped(&f.atlas, 3, 1, "needle",
        AtlasSearchScope::all(), 0, WORK_BYTES, || false).unwrap();
    f.search.step(&f.atlas, 3, || false).unwrap();
    let pending = f.search.progress(&f.atlas, 3).unwrap();
    assert_eq!((pending.matches_seen, pending.retained_hits, pending.matching_files), (2, 0, 1));
    assert!(pending.is_running() && !pending.match_count_complete);
    assert_eq!(f.search.step(&f.atlas, 99, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(f.search.pending_generation(), Some(3));
    assert_eq!(f.search.step(&f.atlas, 3, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(f.search.pending_generation(), None);
    assert_eq!(f.search.progress(&f.atlas, 2).unwrap(), accepted);
    let (reader, _) = f.search.open_reader(&f.atlas, owner(99202), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle needle");
    f.search.begin_indexed_counted_scoped(&f.atlas, 4, 1, "needle",
        AtlasSearchScope::all(), 0, WORK_BYTES, || false).unwrap();
    let final_page = f.finish(4);
    counts(&final_page, 3, 0, 2, 0);
    assert!(final_page.get("match_count_complete").flag());
    let final_progress = f.search.progress(&f.atlas, 4).unwrap();
    f.search.step(&f.atlas, 4, || false).unwrap();
    assert_eq!(f.search.progress(&f.atlas, 4).unwrap(), final_progress);
}
