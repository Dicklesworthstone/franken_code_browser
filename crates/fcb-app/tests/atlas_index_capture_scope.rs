#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production-path regressions for scope admission before source/index quotas.
//! Capture selection and current query selection are independent frozen facts;
//! widening a query never authorizes an implicit read of current disk bytes.
//! Code-first tests; independent batch execution remains pending.

mod support;

use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError,
    AtlasSearchPathKind as Kind, AtlasSearchScope, RetainedAtlasSearch};
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-index-capture-scope-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        for (name, bytes) in parts {
            let file = path.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, bytes).unwrap();
        }
        Self(path)
    }
    fn open(&self) -> AtlasSession {
        self.open_with(4096)
    }
    fn open_with(&self, files: usize) -> AtlasSession {
        AtlasSession::open(owner(32901), &self.0,
            AtlasSessionOptions { max_files: files, ..AtlasSessionOptions::default() },
            || false).unwrap()
    }
}
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 8, max_file_bytes: 1024,
        max_source_bytes: 65536, max_index_grams: 65536 }
}
fn file(path: &str) -> AtlasSearchScope {
    AtlasSearchScope::all().with_path(Kind::File, path).unwrap()
}
fn manifest(paths: &[&[u8]]) -> String {
    let mut paths = paths.to_vec();
    paths.sort_unstable();
    assert!(!paths.windows(2).any(|pair| pair[0] == pair[1]));
    paths.iter().map(|path| path.iter().map(|byte| format!("{byte:02x}"))
        .collect::<String>()).collect::<Vec<_>>().join(",")
}
fn visible(paths: &[&[u8]]) -> AtlasSearchScope {
    AtlasSearchScope::all().with_path(Kind::Visible, &manifest(paths)).unwrap()
}
fn decoded(text: &str) -> Json { parse(text.as_bytes()).unwrap() }
fn finish_index(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> Json {
    for _ in 0..=4096 {
        if !search.index_build_progress(atlas, generation).unwrap().in_progress {
            return decoded(search.index_build_info(atlas, generation, || false).unwrap().as_str());
        }
        search.step_index(atlas, generation, || false).unwrap();
        assert!(search.index_build_progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("scoped index preparation exceeded its file-work bound");
}
fn query(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64,
    index: u64, needle: &str, scope: AtlasSearchScope) -> Json {
    search.begin_indexed_scoped(atlas, generation, index, needle, scope, 16, 65536,
        || false).unwrap();
    for _ in 0..=4096 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            return decoded(search.page(atlas, generation, 0, 128, || false).unwrap().as_str());
        }
        search.step(atlas, generation, || false).unwrap();
        assert!(search.progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("indexed query exceeded the retained capture universe");
}
fn capture_counts(page: &Json, catalogued: usize, eligible: usize) {
    assert_eq!(page.get("catalogued_files").number(), catalogued as u64);
    assert_eq!(page.get("capture_eligible_files").number(), eligible as u64);
    assert_eq!(page.get("capture_excluded_files").number(), (catalogued - eligible) as u64);
    assert!(page.get("capture_case_sensitive").flag());
}
fn capture_region(page: &Json, paths: &[&[u8]], known: usize, missing: usize) -> String {
    assert_eq!(page.get("capture_scope_path_kind").text(), "visible");
    assert_eq!(page.get("capture_scope_path").text(), "");
    assert_eq!(page.get("capture_scope_path_hex").text(), "");
    assert_eq!(page.get("capture_scope_region_files").number(), paths.len() as u64);
    assert_eq!(page.get("capture_scope_region_path_bytes").number(),
        paths.iter().map(|path| path.len() as u64).sum::<u64>());
    assert_eq!(page.get("capture_scope_region_catalogued_files").number(), known as u64);
    assert_eq!(page.get("capture_scope_region_missing_files").number(), missing as u64);
    let digest = page.get("capture_scope_region_sha256").text();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    digest.to_owned()
}
fn no_query_io(page: &Json) {
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("read_calls").number(), 0);
}
fn hit_paths(search: &RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> Vec<Vec<u8>> {
    let count = search.progress(atlas, generation).unwrap().retained_hits;
    let mut paths = (1..=count).map(|id| {
        let hit = search.hit(atlas, generation, id as u64).unwrap();
        atlas.atlas().catalog().entry(hit.file).unwrap().path().raw().as_bytes().to_vec()
    }).collect::<Vec<_>>();
    paths.sort_unstable();
    paths
}

#[test]
fn selected_member_beyond_4096_precedes_a_single_file_capture_budget() {
    let fixture = Fixture::new(&[("file-00000.rs", b"needle")]);
    let original = fixture.0.join("file-00000.rs");
    for ordinal in 1..=4096 {
        fs::hard_link(&original, fixture.0.join(format!("file-{ordinal:05}.rs"))).unwrap();
    }
    let atlas = fixture.open_with(20_000);
    assert_eq!(atlas.atlas().file_count(), 4097);
    let late = atlas.atlas().catalog().file_id(4096).unwrap();
    let path = atlas.atlas().catalog().entry(late).unwrap().path().raw().as_bytes().to_vec();
    let selected = visible(&[&path]);
    let limits = AtlasIndexOptions { max_files: 1, max_file_bytes: 6,
        max_source_bytes: 6, ..options() };
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    // Negative control: filtering a globally capped index cannot recover a
    // selected source that was never captured in that index.
    search.prepare_index(&atlas, 1, limits, || false).unwrap();
    let old = query(&mut search, &atlas, 2, 1, "needle", selected.clone());
    assert_eq!(old.get("retained_hits").number(), 0);
    assert_eq!(old.get("pending_files").number(), 1);
    assert!(!old.get("search_complete").flag());
    assert_eq!(old.get("stop_reason").text(), "file-limit");

    let begin = decoded(search.begin_index_scoped(&atlas, 3, selected.clone(), limits,
        || false).unwrap().as_str());
    capture_counts(&begin, 4097, 1);
    let digest = capture_region(&begin, &[&path], 1, 0);
    assert_eq!(begin.get("initial_source_bytes_read").number(), 0);
    assert_eq!(begin.get("examined_files").number(), 0);
    assert_eq!(begin.get("pending_files").number(), 1);
    let prepared = finish_index(&mut search, &atlas, 3);
    capture_counts(&prepared, 4097, 1);
    assert_eq!(capture_region(&prepared, &[&path], 1, 0), digest);
    assert!(prepared.get("capture_complete").flag());
    assert_eq!(prepared.get("initial_source_bytes_read").number(), 6);
    assert_eq!(prepared.get("examined_files").number(), 1);
    assert_eq!(prepared.get("pending_files").number(), 0);
    assert_eq!(prepared.get("stop_reason").text(), "all-files-examined");
    let page = query(&mut search, &atlas, 4, 3, "needle", selected);
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("retained_hits").number(), 1);
    assert_eq!(search.hit(&atlas, 4, 1).unwrap().file, late);
    capture_counts(&page, 4097, 1);
    assert_eq!(capture_region(&page, &[&path], 1, 0), digest);
    no_query_io(&page);
}

#[test]
fn capture_intersects_types_and_directory_components_before_all_source_quotas() {
    let noise = b"needle ".repeat(1024);
    let fixture = Fixture::new(&[("a.rs", &noise), ("src/a.txt", &noise),
        ("src/main.RS", b"needle"), ("src/Cargo.toml", b"needle"),
        ("src-extra/outside.rs", &noise)]);
    let atlas = fixture.open();
    let selected = AtlasSearchScope::from_csv(Some("RS,rs"), Some("Cargo.toml")).unwrap()
        .with_path(Kind::Directory, "src").unwrap().with_case_sensitive(false);
    let limits = AtlasIndexOptions { max_files: 2, max_file_bytes: 6,
        max_source_bytes: 12, max_index_grams: 0 };
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let prepared = decoded(search.prepare_index_scoped(&atlas, 1, selected.clone(), limits,
        || false).unwrap().as_str());
    capture_counts(&prepared, 5, 2);
    assert_eq!(prepared.get("capture_search_scope").text(), "file-types");
    assert_eq!(prepared.get("capture_scope_extensions").text(), "rs");
    assert_eq!(prepared.get("capture_scope_filenames").text(), "cargo.toml");
    assert_eq!(prepared.get("capture_scope_path_kind").text(), "directory");
    assert_eq!(prepared.get("capture_scope_path").text(), "src");
    assert_eq!(prepared.get("captured_files").number(), 2);
    assert_eq!(prepared.get("unavailable_files").number(), 0);
    assert_eq!(prepared.get("pending_files").number(), 0);
    assert_eq!(prepared.get("initial_source_bytes_read").number(), 12);
    assert_eq!(prepared.get("uncovered_files").number(), 2);
    assert!(prepared.get("capture_complete").flag());
    let page = query(&mut search, &atlas, 2, 1, "NEEDLE", selected);
    assert!(!page.get("case_sensitive").flag());
    assert!(page.get("capture_case_sensitive").flag());
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("fallback_files").number(), 2);
    assert_eq!(hit_paths(&search, &atlas, 2), vec![b"src/Cargo.toml".to_vec(), b"src/main.RS".to_vec()]);
    no_query_io(&page);
}

#[test]
fn empty_capture_selections_do_no_source_work_and_never_become_all_files() {
    let fixture = Fixture::new(&[("a.rs", b"needle"), ("b.md", b"needle")]);
    let atlas = fixture.open();
    for empty in [visible(&[]), AtlasSearchScope::from_csv(Some(""), None).unwrap()] {
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        let begin = decoded(search.begin_index_scoped(&atlas, 1, empty.clone(), options(),
            || false).unwrap().as_str());
        capture_counts(&begin, 2, 0);
        let prepared = finish_index(&mut search, &atlas, 1);
        capture_counts(&prepared, 2, 0);
        assert!(prepared.get("capture_complete").flag());
        for key in ["captured_files", "examined_files", "pending_files", "initial_source_bytes_read", "initial_read_calls"] {
            assert_eq!(prepared.get(key).number(), 0, "unexpected source work in {key}");
        }
        if empty.region_files().is_some() { capture_region(&prepared, &[], 0, 0); }
        let narrowed = query(&mut search, &atlas, 2, 1, "needle", empty);
        assert!(narrowed.get("search_complete").flag());
        assert_eq!(narrowed.get("retained_hits").number(), 0);
        let widened = query(&mut search, &atlas, 3, 1, "needle", AtlasSearchScope::all());
        capture_counts(&widened, 2, 0);
        assert!(!widened.get("search_complete").flag());
        assert_eq!(widened.get("eligible_files").number(), 2);
        assert_eq!(widened.get("pending_files").number(), 2);
        assert_eq!(widened.get("retained_hits").number(), 0);
        assert_eq!(widened.get("stop_reason").text(), "capture-scope");
        no_query_io(&widened);
    }
}

#[test]
fn visible_capture_proof_freezes_missing_members_independently_of_type_filters() {
    let fixture = Fixture::new(&[("a.rs", b"needle"), ("b.md", b"needle"), ("c.rs", b"needle")]);
    let atlas = fixture.open();
    let paths: &[&[u8]] = &[b"a.rs", b"b.md", b"missing.rs"];
    let mut selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(Kind::Visible, &manifest(paths)).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let begin = decoded(search.begin_index_scoped(&atlas, 1, selected.clone(), options(),
        || false).unwrap().as_str());
    let digest = capture_region(&begin, paths, 2, 1);
    capture_counts(&begin, 3, 1);
    // Neither replacing the caller's scope nor adding a path after discovery
    // changes the selection already admitted by the capture generation.
    selected = visible(&[b"c.rs"]);
    fs::write(fixture.0.join("missing.rs"), b"needle").unwrap();
    let prepared = finish_index(&mut search, &atlas, 1);
    assert_eq!(capture_region(&prepared, paths, 2, 1), digest);
    assert!(!prepared.get("capture_complete").flag());
    assert_eq!(prepared.get("captured_files").number(), 1);
    assert_eq!(prepared.get("pending_files").number(), 0);
    assert_eq!(prepared.get("initial_source_bytes_read").number(), 6);
    let missing_query = query(&mut search, &atlas, 2, 1, "needle", visible(paths));
    capture_counts(&missing_query, 3, 1);
    assert_eq!(capture_region(&missing_query, paths, 2, 1), digest);
    assert_eq!(missing_query.get("scope_region_missing_files").number(), 1);
    assert!(!missing_query.get("search_complete").flag());
    assert_eq!(missing_query.get("pending_files").number(), 1); // b.md was type-excluded during capture.
    let exact = query(&mut search, &atlas, 3, 1, "needle", file("a.rs"));
    assert!(exact.get("search_complete").flag());
    assert_eq!(exact.get("scope_path_kind").text(), "file");
    assert_eq!(capture_region(&exact, paths, 2, 1), digest);
    let outside = query(&mut search, &atlas, 4, 1, "needle", selected);
    assert_eq!(outside.get("scope_region_files").number(), 1);
    assert_eq!(outside.get("retained_hits").number(), 0);
    assert_eq!(outside.get("pending_files").number(), 1);
    assert_eq!(capture_region(&outside, paths, 2, 1), digest);
    no_query_io(&outside);
}

#[test]
fn scoped_capture_accepts_exact_raw_paths_without_using_a_lossy_label() {
    let fixture = Fixture::new(&[("a.rs", b"noise")]);
    let raw = b"raw-\xff.RS";
    fs::write(fixture.0.join(OsString::from_vec(raw.to_vec())), b"needle").unwrap();
    let atlas = fixture.open();
    let selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(Kind::Visible, &manifest(&[raw])).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let prepared = decoded(search.prepare_index_scoped(&atlas, 1, selected.clone(),
        AtlasIndexOptions { max_files: 1, max_source_bytes: 6, ..options() },
        || false).unwrap().as_str());
    capture_counts(&prepared, 2, 1);
    let digest = capture_region(&prepared, &[raw], 1, 0);
    let page = query(&mut search, &atlas, 2, 1, "needle", selected);
    assert!(page.get("search_complete").flag());
    assert_eq!(hit_paths(&search, &atlas, 2), vec![raw.to_vec()]);
    assert_eq!(capture_region(&page, &[raw], 1, 0), digest);
    let (reader, receipt) = search.open_reader(&atlas, owner(32902), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle");
    assert_eq!(capture_region(&decoded(receipt.as_str()), &[raw], 1, 0), digest);
}

#[test]
fn query_edits_and_wider_scopes_reuse_only_the_exact_accepted_capture() {
    let fixture = Fixture::new(&[("a.rs", b"old needle"), ("b.rs", b"uncaptured")]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index_scoped(&atlas, 1, file("a.rs"), options(), || false).unwrap();
    let first = query(&mut search, &atlas, 2, 1, "old", file("a.rs"));
    assert!(first.get("search_complete").flag());
    let (reader, _) = search.open_reader(&atlas, owner(32903), 2, 1, || false).unwrap();
    fs::write(fixture.0.join("a.rs"), b"new needle").unwrap();
    fs::write(fixture.0.join("b.rs"), b"new needle").unwrap();

    let widened = query(&mut search, &atlas, 3, 1, "needle", AtlasSearchScope::all());
    capture_counts(&widened, 2, 1);
    assert_eq!(widened.get("scope_path_kind").text(), "workspace");
    assert_eq!(widened.get("capture_scope_path_kind").text(), "file");
    assert_eq!(widened.get("capture_scope_path").text(), "a.rs");
    assert_eq!(widened.get("eligible_files").number(), 2);
    assert_eq!(widened.get("eligible_captured_files").number(), 1);
    assert_eq!(widened.get("retained_hits").number(), 1);
    assert_eq!(widened.get("pending_files").number(), 1);
    assert_eq!(widened.get("stop_reason").text(), "capture-scope");
    assert!(!widened.get("search_complete").flag());
    no_query_io(&widened);
    let absent = query(&mut search, &atlas, 4, 1, "new", AtlasSearchScope::all());
    assert_eq!(absent.get("retained_hits").number(), 0);
    assert_eq!(absent.get("pending_files").number(), 1);
    no_query_io(&absent);
    let index = decoded(search.index_info(&atlas, || false).unwrap().as_str());
    assert_eq!(index.get("initial_source_bytes_read").number(), b"old needle".len() as u64);
    assert_eq!(reader.capture().bytes(), b"old needle");
}

#[test]
fn explicit_scoped_rebuild_reads_current_scope_and_preserves_old_query_proof_and_source() {
    let fixture = Fixture::new(&[("a.rs", b"old needle"), ("b.rs", b"old second")]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index_scoped(&atlas, 1, file("a.rs"), options(), || false).unwrap();
    let before = query(&mut search, &atlas, 2, 1, "old", file("a.rs"));
    let (pinned, _) = search.open_reader(&atlas, owner(32904), 2, 1, || false).unwrap();
    let next_scope = visible(&[b"b.rs"]);
    let begin = decoded(search.begin_index_scoped(&atlas, 3, next_scope.clone(), options(),
        || false).unwrap().as_str());
    assert_eq!(begin.get("initial_source_bytes_read").number(), 0);
    fs::write(fixture.0.join("a.rs"), b"changed origin").unwrap();
    fs::write(fixture.0.join("b.rs"), b"new second").unwrap();
    assert_eq!(search.index_generation(), Some(1));
    assert_eq!(decoded(search.page(&atlas, 2, 0, 128, || false).unwrap().as_str()), before);
    let prepared = finish_index(&mut search, &atlas, 3);
    capture_region(&prepared, &[b"b.rs"], 1, 0);
    assert_eq!(search.index_generation(), Some(3));
    assert_eq!(decoded(search.page(&atlas, 2, 0, 128, || false).unwrap().as_str()), before);
    let (old_reader, old_receipt) = search.open_reader(&atlas, owner(32905), 2, 1, || false).unwrap();
    let old_receipt = decoded(old_receipt.as_str());
    assert_eq!(old_receipt.get("capture_scope_path_kind").text(), "file");
    assert_eq!(old_receipt.get("capture_scope_path").text(), "a.rs");
    assert_eq!(old_reader.capture().bytes(), b"old needle");
    assert_eq!(pinned.capture().bytes(), b"old needle");
    let page = query(&mut search, &atlas, 4, 3, "new", next_scope);
    assert!(page.get("search_complete").flag());
    assert_eq!(hit_paths(&search, &atlas, 4), vec![b"b.rs".to_vec()]);
    capture_region(&page, &[b"b.rs"], 1, 0);
    let (new_reader, _) = search.open_reader(&atlas, owner(32906), 4, 1, || false).unwrap();
    assert_eq!(new_reader.capture().bytes(), b"new second");
    no_query_io(&page);
}

#[test]
fn scoped_quota_exhaustion_remains_distinct_from_a_widened_capture_scope() {
    let fixture = Fixture::new(&[("outside.rs", b"noise"),
        ("src/a.rs", b"needle"), ("src/b.rs", b"needle")]);
    let atlas = fixture.open();
    let selected = AtlasSearchScope::all().with_path(Kind::Directory, "src").unwrap();
    for (limits, reason) in [
        (AtlasIndexOptions { max_files: 1, ..options() }, "file-limit"),
        (AtlasIndexOptions { max_source_bytes: 6, ..options() }, "source-byte-limit"),
    ] {
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        let prepared = decoded(search.prepare_index_scoped(&atlas, 1, selected.clone(), limits,
            || false).unwrap().as_str());
        capture_counts(&prepared, 3, 2);
        assert_eq!(prepared.get("examined_files").number(), 1);
        assert_eq!(prepared.get("pending_files").number(), 1);
        assert_eq!(prepared.get("initial_source_bytes_read").number(), 6);
        assert_eq!(prepared.get("stop_reason").text(), reason);
        assert!(!prepared.get("capture_complete").flag());
        let page = query(&mut search, &atlas, 2, 1, "needle", selected.clone());
        assert_eq!(page.get("pending_files").number(), 1);
        assert_eq!(page.get("retained_hits").number(), 1);
        assert_eq!(page.get("stop_reason").text(), reason);
        assert!(!page.get("search_complete").flag());
        no_query_io(&page);
    }
}

#[test]
fn canceled_or_replaced_scoped_work_cannot_replace_the_accepted_index_or_rows() {
    let fixture = Fixture::new(&[("a.rs", b"old needle"), ("b.rs", b"new needle"), ("c.rs", b"third")]);
    let atlas = fixture.open();
    let prepared = || {
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search.prepare_index_scoped(&atlas, 1, file("a.rs"), options(), || false).unwrap();
        query(&mut search, &atlas, 2, 1, "old", file("a.rs"));
        search.begin_index_scoped(&atlas, 3, visible(&[b"b.rs"]), options(), || false).unwrap();
        search
    };
    let mut baseline = prepared();
    let mut polls = 0;
    baseline.step_index(&atlas, 3, || { polls += 1; false }).unwrap();
    assert!(polls > 1);
    // The final cancellation gate exercises fully prepared but unpublished
    // source/index storage, not merely an early refusal before useful work.
    for at in [1, polls] {
        let mut search = prepared();
        let before = decoded(search.page(&atlas, 2, 0, 128, || false).unwrap().as_str());
        let mut seen = 0;
        assert_eq!(search.step_index(&atlas, 3, || { seen += 1; seen == at }).err(),
            Some(AtlasSearchError::Canceled), "cancel checkpoint {at}");
        assert_eq!(search.pending_index_generation(), None);
        assert_eq!(search.index_generation(), Some(1));
        assert_eq!(search.accepted_generation(), Some(2));
        assert_eq!(decoded(search.page(&atlas, 2, 0, 128, || false).unwrap().as_str()), before);
        let info = decoded(search.index_info(&atlas, || false).unwrap().as_str());
        assert_eq!(info.get("capture_scope_path").text(), "a.rs");
    }

    let mut search = prepared();
    search.begin_index_scoped(&atlas, 4, file("c.rs"), options(), || false).unwrap();
    assert_eq!(search.step_index(&atlas, 3, || false).err(), Some(AtlasSearchError::StaleIndex));
    assert_eq!(search.pending_index_generation(), Some(4));
    assert_eq!(search.index_build_progress(&atlas, 4).unwrap().source_bytes_read, 0);
    assert!(search.cancel_index_build());
    assert_eq!(search.index_generation(), Some(1));
    let old = query(&mut search, &atlas, 5, 1, "old", file("a.rs"));
    assert!(old.get("search_complete").flag());
    capture_counts(&old, 3, 1);
    no_query_io(&old);
}
