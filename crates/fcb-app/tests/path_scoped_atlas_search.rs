#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production catalog/live-capture/retained-index scope intersections. These
//! regressions belong to the native visible-scope slice of fcb-qtb.4; heading,
//! semantic-symbol and visible-region modes remain separate unfinished work.
use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchPathKind as Kind, AtlasSearchScope, AtlasSearchStop, RetainedAtlasSearch,
    MAX_SEARCH_SCOPE_PATH_BYTES};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-path-scope-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in parts {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(ArenaOwnerId::new(29831).unwrap(), &self.0,
            AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn scope(kind: Kind, path: &str) -> AtlasSearchScope {
    AtlasSearchScope::all().with_path(kind, path).unwrap()
}
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 10, max_files: 10, max_file_bytes: 32768, max_source_bytes: 65536 }
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
    panic!("path-scoped query failed to terminate");
}
fn count(json: &str, key: &str, expected: usize) {
    assert!(json.contains(&format!("\"{key}\":\"{expected}\"")), "{key}={expected}: {json}");
}

#[test]
fn directory_type_intersection_precedes_live_and_indexed_quotas() {
    let noise = b"needle ".repeat(1001);
    let fixture = Fixture::new(&[("a-noise.rs", &noise), ("src-old/noise.rs", &noise),
        ("src/skip.txt", &noise), ("src/z.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    // Negative control: selecting after the broad capped query cannot recover
    // src/z.rs. It has already spent its sole file/hit on the outside source.
    search.search(&atlas, 1, "needle", AtlasSearchOptions { max_matches: 1, max_files: 1,
        ..options() }, || false).unwrap();
    let outside = search.hit(&atlas, 1, 1).unwrap().file;
    let selected = AtlasSearchScope::from_csv(Some("RS"), None).unwrap()
        .with_path(Kind::Directory, "src").unwrap();
    let begin = search.begin_scoped(&atlas, 2, "needle", selected,
        AtlasSearchOptions { max_matches: 1, max_files: 1, max_source_bytes: 6, ..options() }, || false).unwrap();
    count(begin.as_str(), "source_bytes_read", 0); count(begin.as_str(), "eligible_files", 1);
    count(begin.as_str(), "excluded_files", 3);
    assert!(begin.as_str().contains("\"scope_path_kind\":\"directory\""));
    assert!(begin.as_str().contains("\"scope_path\":\"src\""));
    assert!(begin.as_str().contains("\"scope_path_hex\":\"737263\""));
    finish(&mut search, &atlas, 2);
    let live = search.hit(&atlas, 2, 1).unwrap(); assert_ne!(live.file, outside);
    let progress = search.progress(&atlas, 2).unwrap();
    assert!(progress.complete); assert_eq!(progress.source_bytes_read, 6);
    assert_eq!((progress.examined_files, progress.unavailable_files, progress.pending_files), (1, 0, 0));
    search.prepare_index(&atlas, 3, index_options(), || false).unwrap();
    fs::write(fixture.0.join("src/z.rs"), b"changed on disk").unwrap();
    search.begin_indexed_scoped(&atlas, 4, 3, "needle", selected, 1, 6, || false).unwrap();
    finish(&mut search, &atlas, 4);
    let indexed = search.hit(&atlas, 4, 1).unwrap();
    assert_eq!(indexed.file, live.file); assert_eq!(indexed.original_range, live.original_range);
    assert!(search.progress(&atlas, 4).unwrap().complete);
    let page = search.page(&atlas, 4, 0, 128, || false).unwrap();
    count(page.as_str(), "verification_source_bytes", 6); count(page.as_str(), "source_bytes_read", 0);
    let (reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(29832).unwrap(), 4, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle");
}

#[test]
fn exact_file_scope_is_not_a_basename_or_prefix_and_does_not_silently_widen() {
    let fixture = Fixture::new(&[("dir/file.rs", b"needle"), ("dir/file.rs.bak", b"needle needle"),
        ("dir/nested/file.rs", b"needle"), ("elsewhere/file.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let selected = scope(Kind::File, "dir/file.rs");
    assert!(!selected.is_all());
    let live = search.search_scoped(&atlas, 1, "needle", selected, options(), || false).unwrap();
    count(live.as_str(), "eligible_files", 1); count(live.as_str(), "retained_hits", 1);
    assert!(live.as_str().contains("\"search_scope\":\"all\""));
    assert!(live.as_str().contains("\"scope_path_kind\":\"file\""));
    let empty_intersection = AtlasSearchScope::from_csv(Some("py"), None).unwrap()
        .with_path(Kind::File, "dir/file.rs").unwrap();
    search.search_scoped(&atlas, 2, "needle", empty_intersection, options(), || false).unwrap();
    let none = search.progress(&atlas, 2).unwrap();
    assert!(none.complete); assert_eq!(none.eligible_files, 0); assert_eq!(none.read_calls, 0);
    search.prepare_index(&atlas, 3, index_options(), || false).unwrap();
    search.search_indexed_scoped(&atlas, 4, 3, "needle", selected, 1, 6, || false).unwrap();
    assert!(search.progress(&atlas, 4).unwrap().complete);
    let absent = scope(Kind::File, "missing/file.rs");
    search.search_indexed_scoped(&atlas, 5, 3, "needle", absent, 1, 0, || false).unwrap();
    let missing = search.progress(&atlas, 5).unwrap();
    assert!(missing.complete); assert_eq!(missing.retained_hits, 0); assert_eq!(missing.examined_files, 0);
}

#[test]
fn directory_membership_preserves_component_case_unicode_and_native_filename_bytes() {
    let dir = scope(Kind::Directory, "src/é");
    assert!(dir.includes_path("src/é/file.rs".as_bytes()));
    assert!(dir.includes_path("src/é/deeper/file.rs".as_bytes()));
    for other in ["src/é", "src/é-old/file.rs", "src/e\u{301}/file.rs", "SRC/é/file.rs", "else/src/é/file.rs"] {
        assert!(!dir.includes_path(other.as_bytes()), "{other}");
    }
    let exact = scope(Kind::File, "src/é.rs");
    assert!(!exact.includes_path("src/e\u{301}.rs".as_bytes()));
    assert!(scope(Kind::File, "src/a\\b.rs").includes_path(b"src/a\\b.rs"));
    let fixture = Fixture::new(&[("src/keep.rs", b"needle"), ("src-old/other.rs", b"needle")]);
    let mut raw = b"src/invalid-".to_vec(); raw.extend_from_slice(b"\xff.RS");
    fs::write(fixture.0.join(OsString::from_vec(raw)), b"needle").unwrap();
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(Kind::Directory, "src").unwrap();
    search.search_scoped(&atlas, 1, "needle", selected, options(), || false).unwrap();
    assert_eq!(search.progress(&atlas, 1).unwrap().retained_hits, 2);
    search.prepare_index(&atlas, 2, index_options(), || false).unwrap();
    search.search_indexed_scoped(&atlas, 3, 2, "needle", selected, 10, 12, || false).unwrap();
    assert_eq!(search.progress(&atlas, 3).unwrap().retained_hits, 2);
    assert!(search.progress(&atlas, 3).unwrap().complete);
}

#[test]
fn selected_capture_omissions_and_unavailability_stay_distinct_from_exclusions() {
    let fixture = Fixture::new(&[("a.rs", b"needle"), ("z/file.rs", b"needle")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, AtlasIndexOptions { max_files: 1, ..index_options() }, || false).unwrap();
    search.search_indexed_scoped(&atlas, 2, 1, "needle", scope(Kind::Directory, "z"), 10, 100, || false).unwrap();
    let omitted = search.progress(&atlas, 2).unwrap();
    assert!(!omitted.complete); assert_eq!(omitted.pending_files, 1); assert_eq!(omitted.unavailable_files, 0);
    assert_eq!(omitted.stop_reason, Some(AtlasSearchStop::FileLimit));
    search.search_indexed_scoped(&atlas, 3, 1, "needle", scope(Kind::File, "a.rs"), 10, 100, || false).unwrap();
    assert!(search.progress(&atlas, 3).unwrap().complete);
    fs::rename(fixture.0.join("z/file.rs"), fixture.0.join("z/moved.rs")).unwrap();
    search.prepare_index(&atlas, 4, index_options(), || false).unwrap();
    search.search_indexed_scoped(&atlas, 5, 4, "needle", scope(Kind::Directory, "z"), 10, 100, || false).unwrap();
    let unavailable = search.progress(&atlas, 5).unwrap();
    assert!(!unavailable.complete); assert_eq!(unavailable.pending_files, 0);
    assert_eq!(unavailable.unavailable_files, 1);
    search.search_indexed_scoped(&atlas, 6, 4, "needle", scope(Kind::File, "a.rs"), 10, 100, || false).unwrap();
    assert!(search.progress(&atlas, 6).unwrap().complete);
}

#[test]
fn scope_replacement_and_cancellation_do_not_retarget_accepted_captures() {
    let fixture = Fixture::new(&[("a/source.rs", b"needle original"), ("b/source.rs", b"needle other")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let first = scope(Kind::Directory, "a"); let next = scope(Kind::Directory, "b");
    search.search_scoped(&atlas, 1, "needle", first, options(), || false).unwrap();
    let old = search.hit(&atlas, 1, 1).unwrap();
    search.begin_scoped(&atlas, 2, "needle", first, options(), || false).unwrap();
    search.begin_scoped(&atlas, 3, "needle", next, options(), || false).unwrap();
    assert_eq!(search.step(&atlas, 2, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(search.pending_generation(), Some(3));
    assert_eq!(search.step(&atlas, 3, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
    fs::write(fixture.0.join("a/source.rs"), b"replacement").unwrap();
    let (reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(29833).unwrap(), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle original");
    let page = search.page(&atlas, 1, 0, 128, || false).unwrap();
    assert!(page.as_str().contains("\"scope_path\":\"a\""));
    search.begin_scoped(&atlas, 4, "needle", next, options(), || false).unwrap();
    finish(&mut search, &atlas, 4);
    assert!(search.page(&atlas, 4, 0, 128, || false).unwrap().as_str().contains("\"scope_path\":\"b\""));
}

#[test]
fn invalid_relative_paths_and_byte_limits_never_broaden_scope() {
    for kind in [Kind::Directory, Kind::File] {
        for path in ["", "/src", "src/", "src//a", ".", "..", "src/./a", "src/../a", "src/\0a"] {
            assert_eq!(AtlasSearchScope::all().with_path(kind, path).err(), Some(AtlasSearchError::InvalidScope), "{path:?}");
        }
        assert!(AtlasSearchScope::all().with_path(kind, &"x".repeat(MAX_SEARCH_SCOPE_PATH_BYTES)).is_ok());
        assert!(AtlasSearchScope::all().with_path(kind, &"x".repeat(MAX_SEARCH_SCOPE_PATH_BYTES + 1)).is_err());
        assert!(AtlasSearchScope::all().with_path(kind, &"é".repeat(MAX_SEARCH_SCOPE_PATH_BYTES / 2)).is_ok());
        assert!(AtlasSearchScope::all().with_path(kind, &"é".repeat(MAX_SEARCH_SCOPE_PATH_BYTES / 2 + 1)).is_err());
    }
    assert!(scope(Kind::Workspace, "").is_all());
    assert!(AtlasSearchScope::all().with_path(Kind::Workspace, "src").is_err());
    assert!(!AtlasSearchScope::from_csv(Some(""), None).unwrap().with_path(Kind::Workspace, "").unwrap()
        .includes_path(b"src/file.rs"));
}
