#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production-catalog regressions for the frozen visible-atlas membership in
//! fcb-qtb.4. A visible scope selects whole captured files by exact native path;
//! it neither borrows response-local file IDs nor filters already-capped hits.
//! These cases are pending independent execution under the code-first gate.

mod support;

use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::SymbolNameMode;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchPathKind as Kind, AtlasSearchScope, AtlasStructureMode, AtlasStructureOptions,
    RetainedAtlasSearch};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-visible-scope-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        for (name, source) in parts {
            let file = path.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, source).unwrap();
        }
        Self(path)
    }
    fn open(&self) -> AtlasSession {
        self.open_with(AtlasSessionOptions::default())
    }
    fn open_with(&self, options: AtlasSessionOptions) -> AtlasSession {
        AtlasSession::open(ArenaOwnerId::new(32701).unwrap(), &self.0, options, || false).unwrap()
    }
}

fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 10, max_files: 10,
        max_file_bytes: 32768, max_source_bytes: 65536 }
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 32768,
        max_source_bytes: 65536, max_index_grams: 65536 }
}
fn wire(paths: &[&[u8]]) -> String {
    let mut ordered = paths.to_vec();
    ordered.sort_unstable();
    assert!(!ordered.windows(2).any(|pair| pair[0] == pair[1]));
    ordered.iter().map(|path| path.iter().map(|byte| format!("{byte:02x}"))
        .collect::<String>()).collect::<Vec<_>>().join(",")
}
fn visible(paths: &[&[u8]]) -> AtlasSearchScope {
    AtlasSearchScope::all().with_path(Kind::Visible, &wire(paths)).unwrap()
}
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> Json {
    for _ in 0..102 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            let page = search.page(atlas, generation, 0, 128, || false).unwrap();
            return parse(page.as_str().as_bytes()).unwrap();
        }
        search.step(atlas, generation, || false).unwrap();
        assert!(search.progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("visible-scope query exceeded its bounded fixture membership");
}
fn region(page: &Json, paths: &[&[u8]], catalogued: usize, missing: usize) -> String {
    assert_eq!(page.get("scope_path_kind").text(), "visible");
    assert_eq!(page.get("scope_region_files").number(), paths.len() as u64);
    assert_eq!(page.get("scope_region_path_bytes").number(),
        paths.iter().map(|path| path.len() as u64).sum::<u64>());
    assert_eq!(page.get("scope_region_catalogued_files").number(), catalogued as u64);
    assert_eq!(page.get("scope_region_missing_files").number(), missing as u64);
    let digest = page.get("scope_region_sha256").text();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    digest.to_owned()
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
fn visible_membership_precedes_live_and_indexed_file_byte_and_match_quotas() {
    let noise = b"needle ".repeat(1001);
    let fixture = Fixture::new(&[("a-noise.rs", &noise), ("z.rs", b"needle"),
        ("z.rs.bak", &noise)]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    // A broad capped query spends its only file/hit on an excluded source;
    // filtering its rows afterwards could never recover the visible file.
    search.search(&atlas, 1, "needle", AtlasSearchOptions { max_files: 1,
        max_matches: 1, ..options() }, || false).unwrap();
    let outside = search.hit(&atlas, 1, 1).unwrap().file;
    // Both z paths are visible, but the independent file-type predicate keeps
    // the backup out of the query. Region coverage still counts both paths.
    let paths: &[&[u8]] = &[b"z.rs", b"z.rs.bak"];
    let selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(Kind::Visible, &wire(paths)).unwrap();
    let begin = search.begin_scoped(&atlas, 2, "needle", selected.clone(),
        AtlasSearchOptions { max_files: 1, max_matches: 1, max_source_bytes: 6,
            ..options() }, || false).unwrap();
    let begin = parse(begin.as_str().as_bytes()).unwrap();
    assert_eq!(begin.get("source_bytes_read").number(), 0);
    assert_eq!(begin.get("eligible_files").number(), 1);
    assert_eq!(begin.get("excluded_files").number(), 2);
    let signature = region(&begin, paths, 2, 0);
    let page = finish(&mut search, &atlas, 2);
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("source_bytes_read").number(), 6);
    assert_eq!(page.get("examined_files").number(), 1);
    assert_eq!(region(&page, paths, 2, 0), signature);
    let live = search.hit(&atlas, 2, 1).unwrap();
    assert_ne!(live.file, outside);
    assert_eq!(hit_paths(&search, &atlas, 2), vec![b"z.rs".to_vec()]);

    search.prepare_index(&atlas, 3, index_options(), || false).unwrap();
    fs::write(fixture.0.join("z.rs"), b"changed on disk").unwrap();
    search.begin_indexed_scoped(&atlas, 4, 3, "needle", selected.clone(), 1, 6,
        || false).unwrap();
    let indexed = finish(&mut search, &atlas, 4);
    assert!(indexed.get("search_complete").flag());
    assert_eq!(region(&indexed, paths, 2, 0), signature);
    assert_eq!(indexed.get("verification_source_bytes").number(), 6);
    assert_eq!(indexed.get("source_bytes_read").number(), 0);
    let hit = search.hit(&atlas, 4, 1).unwrap();
    assert_eq!(hit.file, live.file);
    assert_eq!(hit.original_range, live.original_range);
    let (reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(32702).unwrap(),
        4, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle");
    search.search_indexed_scoped(&atlas, 5, 3, "NEEDLE",
        selected.with_case_sensitive(false), 1, 6, || false).unwrap();
    assert_eq!(hit_paths(&search, &atlas, 5), vec![b"z.rs".to_vec()]);
    assert!(search.progress(&atlas, 5).unwrap().complete);
}

#[test]
fn empty_visible_region_never_widens_to_workspace_in_any_search_mode() {
    let fixture = Fixture::new(&[("code.rs", b"fn Target() {}\n"),
        ("README.md", b"# Target\n")]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let empty = visible(&[]);
    assert!(!empty.is_all());
    assert!(!empty.includes_path(b"code.rs"));
    search.begin_scoped(&atlas, 1, "Target", empty.clone(), options(), || false).unwrap();
    let live = finish(&mut search, &atlas, 1);
    let signature = region(&live, &[], 0, 0);
    search.prepare_index(&atlas, 2, index_options(), || false).unwrap();
    search.begin_indexed_scoped(&atlas, 3, 2, "Target", empty.clone(), 1, 0,
        || false).unwrap();
    let indexed = finish(&mut search, &atlas, 3);
    let mut pages = vec![live, indexed];
    for (generation, mode) in [(4, AtlasStructureMode::Symbols), (5, AtlasStructureMode::Headings)] {
        search.begin_structure(&atlas, generation, "Target", empty.clone(), AtlasStructureOptions {
            mode, name_mode: SymbolNameMode::Exact, limits: options(),
        }, || false).unwrap();
        pages.push(finish(&mut search, &atlas, generation));
    }
    for page in pages {
        assert_eq!(region(&page, &[], 0, 0), signature);
        assert!(page.get("search_complete").flag());
        for key in ["eligible_files", "examined_files", "retained_hits", "source_bytes_read", "read_calls"] {
            assert_eq!(page.get(key).number(), 0, "unexpected work in {key}");
        }
        assert_eq!(page.get("excluded_files").number(), 2);
    }
}

#[test]
fn raw_visible_paths_preserve_invalid_utf8_and_do_not_normalize_unicode_or_case() {
    let fixture = Fixture::new(&[("unicode-e\u{301}.rs", b"needle"),
        ("unselected.rs", b"needle needle")]);
    let raw = b"raw-\xff.RS".to_vec();
    fs::write(fixture.0.join(OsString::from_vec(raw.clone())), b"needle").unwrap();
    let atlas = fixture.open();
    // Inspect the actual directory spelling instead of assuming that the test
    // filesystem can retain both NFC and NFD aliases as separate filenames.
    let unicode = atlas.atlas().catalog().entries().iter().find_map(|entry| {
        let path = entry.path().raw().as_bytes();
        path.starts_with(b"unicode-").then(|| path.to_vec())
    }).unwrap();
    let alternate = if unicode == "unicode-é.rs".as_bytes() {
        "unicode-e\u{301}.rs".as_bytes()
    } else { "unicode-é.rs".as_bytes() };
    assert_ne!(unicode, alternate);
    let paths: &[&[u8]] = &[&raw, &unicode];
    let selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(Kind::Visible, &wire(paths)).unwrap();
    assert!(selected.includes_path(&raw));
    assert!(selected.includes_path(&unicode));
    assert!(!selected.includes_path(b"raw-\xff.rs"));
    assert!(!selected.includes_path(alternate));
    let mut expected = vec![raw.clone(), unicode.clone()];
    expected.sort_unstable();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_scoped(&atlas, 1, "needle", selected.clone(), options(), || false).unwrap();
    let live = finish(&mut search, &atlas, 1);
    assert!(live.get("search_complete").flag());
    let signature = region(&live, paths, 2, 0);
    assert_eq!(hit_paths(&search, &atlas, 1), expected);
    search.prepare_index(&atlas, 2, index_options(), || false).unwrap();
    search.begin_indexed_scoped(&atlas, 3, 2, "needle", selected, 2, 12,
        || false).unwrap();
    let indexed = finish(&mut search, &atlas, 3);
    assert!(indexed.get("search_complete").flag());
    assert_eq!(region(&indexed, paths, 2, 0), signature);
    assert_eq!(hit_paths(&search, &atlas, 3), expected);
    search.begin_scoped(&atlas, 4, "needle", visible(&[alternate]), options(), || false).unwrap();
    let absent = finish(&mut search, &atlas, 4);
    region(&absent, &[alternate], 0, 1);
    assert!(!absent.get("search_complete").flag());
    assert_eq!(absent.get("retained_hits").number(), 0);
    assert_eq!(absent.get("source_bytes_read").number(), 0);
}

#[test]
fn structural_routes_respect_visible_membership_before_quotas_and_report_missing_members() {
    let symbol = b"fn Target() {}\n";
    let heading = b"# Target\n";
    let fixture = Fixture::new(&[("a-noise.rs", b"fn Target() {}\nfn Target() {}\n"),
        ("b-noise.md", b"# Target\n\n# Target\n"), ("z.rs", symbol), ("z.md", heading)]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    for (generation, mode, path, source, missing) in [
        (1, AtlasStructureMode::Symbols, b"z.rs".as_slice(), symbol.as_slice(), b"missing.rs".as_slice()),
        (3, AtlasStructureMode::Headings, b"z.md".as_slice(), heading.as_slice(), b"missing.md".as_slice()),
    ] {
        let limits = AtlasSearchOptions { max_files: 1, max_matches: 1,
            max_source_bytes: source.len(), ..options() };
        let settings = AtlasStructureOptions { mode, name_mode: SymbolNameMode::Exact, limits };
        search.begin_structure(&atlas, generation, "Target", visible(&[path]), settings,
            || false).unwrap();
        let page = finish(&mut search, &atlas, generation);
        let signature = region(&page, &[path], 1, 0);
        assert!(page.get("search_complete").flag());
        assert_eq!(page.get("requested_mode").text(), mode.requested());
        assert_eq!(page.get("eligible_files").number(), 1);
        assert_eq!(page.get("excluded_files").number(), 3);
        assert_eq!(page.get("source_bytes_read").number(), source.len() as u64);
        assert_eq!(hit_paths(&search, &atlas, generation), vec![path.to_vec()]);
        search.begin_structure(&atlas, generation + 1, "Target", visible(&[path, missing]), settings,
            || false).unwrap();
        let partial = finish(&mut search, &atlas, generation + 1);
        assert_ne!(region(&partial, &[path, missing], 1, 1), signature);
        assert!(!partial.get("search_complete").flag());
        assert_eq!(hit_paths(&search, &atlas, generation + 1), vec![path.to_vec()]);
    }
}

#[test]
fn absent_visible_paths_cannot_be_reported_as_complete_live_or_indexed_coverage() {
    let fixture = Fixture::new(&[("present.rs", b"needle"), ("outside.rs", b"needle needle")]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let paths: &[&[u8]] = &[b"present.rs", b"absent.rs"];
    let selected = visible(paths);
    search.begin_scoped(&atlas, 1, "needle", selected.clone(), options(), || false).unwrap();
    let live = finish(&mut search, &atlas, 1);
    let signature = region(&live, paths, 1, 1);
    assert!(!live.get("search_complete").flag());
    assert_eq!(hit_paths(&search, &atlas, 1), vec![b"present.rs".to_vec()]);
    search.prepare_index(&atlas, 2, index_options(), || false).unwrap();
    search.begin_indexed_scoped(&atlas, 3, 2, "needle", selected, 1, 6, || false).unwrap();
    let indexed = finish(&mut search, &atlas, 3);
    assert_eq!(region(&indexed, paths, 1, 1), signature);
    assert!(!indexed.get("search_complete").flag());
    assert_eq!(hit_paths(&search, &atlas, 3), vec![b"present.rs".to_vec()]);
    search.begin_indexed_scoped(&atlas, 4, 2, "needle", visible(&[b"absent.rs"]), 1, 0,
        || false).unwrap();
    let absent = finish(&mut search, &atlas, 4);
    region(&absent, &[b"absent.rs"], 0, 1);
    assert!(!absent.get("search_complete").flag());
    for key in ["eligible_files", "retained_hits", "source_bytes_read", "verification_source_bytes"] {
        assert_eq!(absent.get(key).number(), 0);
    }
}

#[test]
fn region_replacement_and_cancellation_preserve_accepted_scope_and_captured_bytes() {
    let fixture = Fixture::new(&[("a.rs", b"needle original"), ("b.rs", b"needle other")]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let first = visible(&[b"a.rs"]);
    let next = visible(&[b"b.rs"]);
    search.begin_scoped(&atlas, 1, "needle", first.clone(), options(), || false).unwrap();
    let old_page = finish(&mut search, &atlas, 1);
    let old_signature = region(&old_page, &[b"a.rs"], 1, 0);
    let old_hit = search.hit(&atlas, 1, 1).unwrap();
    search.begin_scoped(&atlas, 2, "needle", first, options(), || false).unwrap();
    search.begin_scoped(&atlas, 3, "needle", next.clone(), options(), || false).unwrap();
    assert_eq!(search.step(&atlas, 2, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(search.pending_generation(), Some(3));
    assert_eq!(search.step(&atlas, 3, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(search.accepted_generation(), Some(1));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old_hit);
    fs::write(fixture.0.join("a.rs"), b"replaced on disk").unwrap();
    let (mut reader, _) = search.open_reader(&atlas, ArenaOwnerId::new(32703).unwrap(),
        1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle original");
    let retained = finish(&mut search, &atlas, 1);
    assert_eq!(region(&retained, &[b"a.rs"], 1, 0), old_signature);
    search.begin_scoped(&atlas, 4, "needle", next, options(), || false).unwrap();
    let replacement = finish(&mut search, &atlas, 4);
    assert_ne!(region(&replacement, &[b"b.rs"], 1, 0), old_signature);
    assert_eq!(hit_paths(&search, &atlas, 4), vec![b"b.rs".to_vec()]);
    assert_eq!(reader.capture().bytes(), b"needle original");
    assert!(reader.copy_range(0, 6, || false).unwrap().as_str().contains("6e6565646c65"));
}

#[test]
fn selected_metadata_beyond_4096_is_admitted_before_a_one_file_query_cap() {
    let fixture = Fixture::new(&[("file-00000.rs", b"needle")]);
    let source = fixture.0.join("file-00000.rs");
    for ordinal in 1..=4096 {
        fs::hard_link(&source, fixture.0.join(format!("file-{ordinal:05}.rs"))).unwrap();
    }
    let atlas = fixture.open_with(AtlasSessionOptions { max_files: 20_000,
        ..AtlasSessionOptions::default() });
    assert_eq!(atlas.atlas().file_count(), 4097);
    let late = atlas.atlas().catalog().file_id(4096).unwrap();
    let path = atlas.atlas().catalog().entry(late).unwrap().path().raw().as_bytes().to_vec();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_scoped(&atlas, 1, "needle", visible(&[&path]), AtlasSearchOptions {
        max_files: 1, max_matches: 1, max_source_bytes: 6, ..options()
    }, || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    region(&page, &[&path], 1, 0);
    assert!(page.get("search_complete").flag());
    assert_eq!(page.get("eligible_files").number(), 1);
    assert_eq!(page.get("excluded_files").number(), 4096);
    assert_eq!(page.get("examined_files").number(), 1);
    assert_eq!(page.get("source_bytes_read").number(), 6);
    assert_eq!(search.hit(&atlas, 1, 1).unwrap().file, late);
}
