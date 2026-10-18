#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real catalog/capture/index routes: excluded early matches must not consume
//! the quota needed by later selected files. Counts use a frozen manifest.
use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchScope, AtlasSearchStop, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-scoped-search-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in parts { fs::write(root.join(name), bytes).unwrap(); }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(ArenaOwnerId::new(29801).unwrap(), &self.0,
            AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn rust_scope() -> AtlasSearchScope {
    AtlasSearchScope::from_csv(Some("RS,rs"), Some("Cargo.toml,Cargo.lock,rust-toolchain.toml,rust-toolchain")).unwrap()
}
fn live_options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 1, max_files: 1, max_file_bytes: 32768, max_source_bytes: 65536 }
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 32768, max_source_bytes: 65536, max_index_grams: 65536 }
}
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) {
    for _ in 0..101 {
        if !search.progress(atlas, generation).unwrap().is_running() { return; }
        search.step(atlas, generation, || false).unwrap();
        assert!(search.progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("scoped query failed to terminate");
}
fn count(json: &str, key: &str, value: usize) {
    assert!(json.contains(&format!("\"{key}\":\"{value}\"")), "{key}={value}: {json}");
}

#[test]
fn scope_precedes_live_and_indexed_hit_file_and_verification_limits() {
    let noise = b"needle ".repeat(2000);
    let fixture = Fixture::new(&[("a.txt", &noise), ("z.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    // Negative control: the old workspace-wide result page contains only the
    // early excluded file; filtering that page cannot find the later Rust hit.
    search.search(&atlas, 1, "needle", live_options(), || false).unwrap();
    let excluded = search.hit(&atlas, 1, 1).unwrap().file;
    search.begin_scoped(&atlas, 2, "needle", rust_scope(), live_options(), || false).unwrap();
    assert_eq!(search.progress(&atlas, 2).unwrap().source_bytes_read, 0);
    finish(&mut search, &atlas, 2);
    let live = search.hit(&atlas, 2, 1).unwrap();
    assert_ne!(live.file, excluded);
    let progress = search.progress(&atlas, 2).unwrap();
    assert_eq!((progress.eligible_files, progress.excluded_files), (1, 1));
    assert_eq!((progress.examined_files, progress.pending_files), (1, 0));
    assert_eq!(progress.retained_hits, 1); assert!(progress.complete);
    let page = search.page(&atlas, 2, 0, 128, || false).unwrap();
    count(page.as_str(), "catalogued_files", 2); count(page.as_str(), "eligible_files", 1);
    assert!(page.as_str().contains("\"scope_extensions\":\"rs\""));
    search.prepare_index(&atlas, 3, index_options(), || false).unwrap();
    // Six verification bytes cannot inspect the large excluded file first.
    search.begin_indexed_scoped(&atlas, 4, 3, "needle", rust_scope(), 1, 6, || false).unwrap();
    finish(&mut search, &atlas, 4);
    let indexed = search.hit(&atlas, 4, 1).unwrap();
    assert_eq!(indexed.file, live.file); assert_eq!(indexed.original_range, live.original_range);
    assert!(search.progress(&atlas, 4).unwrap().complete);
    assert_eq!(search.progress(&atlas, 4).unwrap().source_bytes_read, 0);
    let page = search.page(&atlas, 4, 0, 128, || false).unwrap();
    count(page.as_str(), "eligible_captured_files", 1);
    count(page.as_str(), "scoped_capture_unavailable_files", 0);
    count(page.as_str(), "verification_source_bytes", 6);
}

#[test]
fn explicit_empty_is_distinct_from_all_and_finishes_without_source_work() {
    let fixture = Fixture::new(&[("a.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let empty = AtlasSearchScope::from_csv(Some(""), None).unwrap();
    assert!(!empty.is_all()); assert!(AtlasSearchScope::from_csv(None, None).unwrap().is_all());
    search.begin_scoped(&atlas, 1, "needle", empty, live_options(), || false).unwrap();
    assert!(search.progress(&atlas, 1).unwrap().is_running());
    finish(&mut search, &atlas, 1);
    let live = search.progress(&atlas, 1).unwrap();
    assert!(live.complete); assert_eq!(live.examined_files, 0); assert_eq!(live.read_calls, 0);
    assert_eq!(live.excluded_files, 1); assert_eq!(live.retained_hits, 0);
    search.prepare_index(&atlas, 2, index_options(), || false).unwrap();
    search.begin_indexed_scoped(&atlas, 3, 2, "needle", empty, 1, 0, || false).unwrap();
    finish(&mut search, &atlas, 3);
    let indexed = search.progress(&atlas, 3).unwrap();
    assert!(indexed.complete); assert_eq!(indexed.examined_files, 0); assert_eq!(indexed.read_calls, 0);
    count(search.page(&atlas, 3, 0, 128, || false).unwrap().as_str(), "verification_source_bytes", 0);
    search.search_indexed_scoped(&atlas, 4, 2, "needle", AtlasSearchScope::all(), 1, 100, || false).unwrap();
    assert_eq!(search.progress(&atlas, 4).unwrap().retained_hits, 1);
}

#[test]
fn extensions_and_basenames_use_raw_native_component_and_ascii_case() {
    let scope = rust_scope();
    assert!(scope.includes_path(b"\xff/untitled.\xff.RS"));
    assert!(scope.includes_path(b"project/CARGO.TOML"));
    assert!(scope.includes_path(b"project/rust-toolchain"));
    assert!(!scope.includes_path(b"project/rs"));
    assert!(!scope.includes_path(b"project/.rs"));
    assert!(!scope.includes_path(b"project/not-cargo.toml"));
    let fixture = Fixture::new(&[("Cargo.toml", b"needle"), ("a.txt", b"needle")]);
    fs::write(fixture.0.join(OsString::from_vec(b"raw-\xff.RS".to_vec())), b"needle").unwrap();
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let options = AtlasSearchOptions { max_matches: 10, max_files: 10, ..live_options() };
    search.search_scoped(&atlas, 1, "needle", scope, options, || false).unwrap();
    let live_files = [search.hit(&atlas, 1, 1).unwrap().file, search.hit(&atlas, 1, 2).unwrap().file];
    search.prepare_index(&atlas, 2, index_options(), || false).unwrap();
    search.search_indexed_scoped(&atlas, 3, 2, "needle", scope, 10, 100, || false).unwrap();
    assert_eq!([search.hit(&atlas, 3, 1).unwrap().file, search.hit(&atlas, 3, 2).unwrap().file], live_files);
    assert!(search.progress(&atlas, 3).unwrap().complete);
}

#[test]
fn index_scope_separates_excluded_capture_failures_from_selected_unavailability() {
    let fixture = Fixture::new(&[("a.txt", b"needle"), ("b.rs", b"needle"), ("c.rs", b"needle")]);
    let atlas = fixture.open();
    fs::rename(fixture.0.join("a.txt"), fixture.0.join("moved.txt")).unwrap();
    fs::rename(fixture.0.join("b.rs"), fixture.0.join("moved.rs")).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    let begin = search.begin_indexed_scoped(&atlas, 2, 1, "needle", rust_scope(), 10, 100, || false).unwrap();
    count(begin.as_str(), "catalogued_files", 3); count(begin.as_str(), "eligible_files", 2);
    count(begin.as_str(), "excluded_files", 1); count(begin.as_str(), "scoped_capture_unavailable_files", 1);
    finish(&mut search, &atlas, 2);
    let scoped = search.progress(&atlas, 2).unwrap();
    assert_eq!(scoped.unavailable_files, 1); assert_eq!(scoped.pending_files, 0);
    assert_eq!(scoped.examined_files, 2); assert!(!scoped.complete);
    search.search_indexed_scoped(&atlas, 3, 1, "needle",
        AtlasSearchScope::from_csv(None, Some("c.rs")).unwrap(), 10, 100, || false).unwrap();
    assert!(search.progress(&atlas, 3).unwrap().complete);
    assert_eq!(search.progress(&atlas, 3).unwrap().unavailable_files, 0);
}

#[test]
fn index_build_limit_leaves_selected_unindexed_files_pending_but_excluded_files_do_not_block() {
    let fixture = Fixture::new(&[("a.rs", b"needle"), ("z.txt", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, AtlasIndexOptions { max_files: 1, ..index_options() }, || false).unwrap();
    search.search_indexed_scoped(&atlas, 2, 1, "needle", rust_scope(), 10, 100, || false).unwrap();
    assert!(search.progress(&atlas, 2).unwrap().complete);
    search.search_indexed_scoped(&atlas, 3, 1, "needle",
        AtlasSearchScope::from_csv(Some("txt"), None).unwrap(), 10, 100, || false).unwrap();
    let pending = search.progress(&atlas, 3).unwrap();
    assert_eq!(pending.pending_files, 1); assert_eq!(pending.unavailable_files, 0);
    assert!(!pending.complete); assert_eq!(pending.stop_reason, Some(AtlasSearchStop::FileLimit));
}

#[test]
fn changing_scope_rejects_stale_work_and_keeps_old_accepted_capture_on_cancellation() {
    let fixture = Fixture::new(&[("a.rs", b"needle rust"), ("b.py", b"needle python")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, index_options(), || false).unwrap();
    search.search_indexed_scoped(&atlas, 2, 1, "needle", rust_scope(), 10, 100, || false).unwrap();
    let old = search.hit(&atlas, 2, 1).unwrap();
    let python = AtlasSearchScope::from_csv(Some("py,pyi,pyw"), None).unwrap();
    search.begin_indexed_scoped(&atlas, 3, 1, "needle", rust_scope(), 10, 100, || false).unwrap();
    search.begin_indexed_scoped(&atlas, 4, 1, "needle", python, 10, 100, || false).unwrap();
    assert_eq!(search.step(&atlas, 3, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(search.pending_generation(), Some(4));
    assert_eq!(search.step(&atlas, 4, || true).err(), Some(AtlasSearchError::Canceled));
    fs::write(fixture.0.join("a.rs"), b"changed source").unwrap();
    assert_eq!(search.hit(&atlas, 2, 1).unwrap(), old);
    let (reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(29802).unwrap(), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle rust");
    assert_eq!(search.begin_indexed_scoped(&atlas, 5, 99, "needle", python, 10, 100, || false).err(), Some(AtlasSearchError::StaleIndex));
    assert_eq!(search.accepted_generation(), Some(2));
}

#[test]
fn malformed_or_over_limit_scope_never_broadens_to_all() {
    for extensions in [".rs", "rs,", "rs,,py", "r/s", "rs py", "r\\s", "🦀", "*"] {
        assert_eq!(AtlasSearchScope::from_csv(Some(extensions), None).err(), Some(AtlasSearchError::InvalidScope));
    }
    for filename in [".", "..", "../Cargo.toml", "a/b", "a\n"] {
        assert_eq!(AtlasSearchScope::from_csv(None, Some(filename)).err(), Some(AtlasSearchError::InvalidScope));
    }
    assert!(AtlasSearchScope::from_csv(Some(&"x".repeat(65)), None).is_err());
    let too_many = (0..33).map(|n| format!("x{n}")).collect::<Vec<_>>().join(",");
    assert!(AtlasSearchScope::from_csv(Some(&too_many), None).is_err());
    assert!(AtlasSearchScope::from_csv(Some(&"x,".repeat(513)), None).is_err());
}
