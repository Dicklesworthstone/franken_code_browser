#![forbid(unsafe_code)]
#![cfg(unix)]

//! Native-host text modes through real catalog, capture, scanner, index and
//! retained-reader APIs. No replacement matcher or native-presentation claim.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::HostResponse;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchPathKind, AtlasSearchScope, AtlasSearchStop, AtlasStructureMode, AtlasStructureOptions,
    RetainedAtlasSearch, MAX_ATLAS_SEARCH_NEEDLE_BYTES};
use support::{Json, parse};

struct Fixture { root: PathBuf, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(38101);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-atlas-case-{}-{id}-{}", std::process::id(),
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
    fn finish(&mut self, generation: u64) {
        for _ in 0..101 {
            if !self.search.progress(&self.atlas, generation).unwrap().is_running() { return; }
            self.search.step(&self.atlas, generation, || false).unwrap();
            assert!(self.search.progress(&self.atlas, generation).unwrap().last_step_files <= 1);
        }
        panic!("bounded query did not finish");
    }
}
// Fixtures remain available for independent replay; no cleanup/delete is
// needed to prove source replacement or retained-capture navigation.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn insensitive() -> AtlasSearchScope { AtlasSearchScope::all().with_case_sensitive(false) }
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 100, max_files: 100,
        max_file_bytes: 65536, max_source_bytes: 131072 }
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 65536,
        max_source_bytes: 131072, max_index_grams: 65536 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}".encode_utf16().chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn default_remains_exact_and_case_mode_does_not_change_membership() {
    let exact = AtlasSearchScope::all();
    assert!(exact.case_sensitive()); assert!(exact.is_all());
    assert!(!insensitive().case_sensitive()); assert!(insensitive().is_all());
    let mut f = Fixture::new(&[("a.rs", b"Needle needle NEEDLE"), ("b.rs", b"quiet")]);
    let negative = f.search.search(&f.atlas, 1, "needle", options(), || false).unwrap();
    let negative = json(&negative);
    assert!(negative.get("case_sensitive").flag());
    assert_eq!(negative.get("mode").text(), "exact-decoded-literal");
    assert_eq!(negative.get("retained_hits").number(), 1);
    assert_eq!(negative.get("hits").array()[0].get("matched_text").text(), "needle");
    let result = f.search.search_scoped(&f.atlas, 2, "needle", insensitive(), options(), || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    let result = json(&result);
    assert!(!result.get("case_sensitive").flag());
    assert_eq!(result.get("mode").text(), "unicode-lowercase-literal");
    assert_eq!(result.get("search_scope").text(), "all");
    assert_eq!(result.get("eligible_files").number(), 2);
    assert_eq!(result.get("excluded_files").number(), 0);
    assert_eq!(result.get("retained_hits").number(), 3);
    assert_eq!(result.get("hits").array().iter().map(|row| row.get("matched_text").text())
        .collect::<Vec<_>>(), ["Needle", "needle", "NEEDLE"]);
    assert_eq!(f.search.hit(&f.atlas, 1, 1).err(), Some(AtlasSearchError::StaleQuery));
}

#[test]
fn lowercase_expansions_keep_original_spelling_and_exact_utf8_utf16_ranges() {
    let text = "AİZ";
    for bytes in [text.as_bytes().to_vec(), utf16(text, true), utf16(text, false)] {
        let mut f = Fixture::new(&[("source.rs", &bytes)]);
        let expected_start = if bytes.len() == text.len() { 1 } else { 4 };
        // A match inside İ's lowercase expansion still names the entire source
        // scalar. The exact negative control cannot find a literal combining dot.
        let exact = f.search.search(&f.atlas, 1, "\u{0307}", options(), || false).unwrap();
        assert_eq!(json(&exact).get("retained_hits").number(), 0);
        let result = f.search.search_scoped(&f.atlas, 2, "\u{0307}", insensitive(), options(), || false).unwrap();
        assert_eq!(result.exit_code(), EXIT_OK);
        let result = json(&result);
        assert_eq!(result.get("retained_hits").number(), 1);
        assert_eq!(result.get("hits").array()[0].get("matched_text").text(), "İ");
        let hit = f.search.hit(&f.atlas, 2, 1).unwrap();
        assert_eq!(hit.original_range.start().get(), expected_start);
        assert_eq!(hit.original_range.end().get(), expected_start + 2);
        fs::write(f.root.join("source.rs"), b"different live source").unwrap();
        let (mut reader, activation) = f.search.open_reader(&f.atlas, owner(99101), 2, 1, || false).unwrap();
        assert_eq!(reader.capture().bytes(), bytes);
        let activation = json(&activation);
        assert!(!activation.get("source_reopened").flag());
        let spelling = activation.get("matched_text").text();
        reader.search(1, spelling, 8, bytes.len() as u64, || false).unwrap();
        assert_eq!(reader.hit_range(1, 0).unwrap(), hit.original_range);
        let copied = reader.copy_hit(1, 0, || false).unwrap();
        let (start, end) = hit.original_range.as_usize_bounds().unwrap();
        assert_eq!(json(&copied).get("original_hex").text(), hex(&bytes[start..end]));
        f.search.clear(&f.atlas, 3, || false).unwrap();
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn matched_spelling_crosses_decoder_windows_without_cutting_source_scalars() {
    let prefix = "x".repeat(16_383);
    let text = format!("{prefix}KİNeedLE");
    let mut f = Fixture::new(&[("source.rs", text.as_bytes())]);
    let result = f.search.search_scoped(&f.atlas, 1, "ki\u{0307}needle", insensitive(), options(), || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    let wire = json(&result);
    assert_eq!(wire.get("retained_hits").number(), 1);
    assert_eq!(wire.get("hits").array()[0].get("matched_text").text(), "KİNeedLE");
    let live = f.search.hit(&f.atlas, 1, 1).unwrap();
    assert_eq!(live.original_range.start().get(), prefix.len() as u64);
    assert_eq!(live.original_range.end().get(), text.len() as u64);
    f.build(2);
    let indexed = f.search.search_indexed_scoped(&f.atlas, 3, 2, "ki\u{0307}needle",
        insensitive(), 100, text.len() as u64, || false).unwrap();
    assert_eq!(indexed.exit_code(), EXIT_OK);
    assert_eq!(f.search.hit(&f.atlas, 3, 1).unwrap().original_range, live.original_range);
    assert_eq!(json(&indexed).get("hits").array()[0].get("matched_text").text(), "KİNeedLE");
}

#[test]
fn full_hit_storage_probes_later_files_without_inventing_truncation_or_counts() {
    for (later, expected_count, complete) in [(b"QUIET".as_slice(), 1, true),
        (b"nEeDLe Needle".as_slice(), 2, false)] {
        let mut f = Fixture::new(&[("a.rs", b"NEEDLE"), ("b.rs", later)]);
        let limits = AtlasSearchOptions { max_matches: 1, ..options() };
        f.search.begin_scoped(&f.atlas, 1, "needle", insensitive(), limits, || false).unwrap();
        let begin = f.search.progress(&f.atlas, 1).unwrap();
        assert_eq!((begin.source_bytes_read, begin.examined_files), (0, 0));
        f.search.step(&f.atlas, 1, || false).unwrap();
        let first = f.search.progress(&f.atlas, 1).unwrap();
        assert!(first.is_running()); assert!(!first.truncated);
        assert_eq!((first.retained_hits, first.matches_seen), (1, 1));
        f.search.step(&f.atlas, 1, || false).unwrap();
        let live = f.search.progress(&f.atlas, 1).unwrap();
        assert_eq!(live.complete, complete); assert_eq!(live.truncated, !complete);
        assert_eq!((live.retained_hits, live.matches_seen), (1, expected_count));
        assert_eq!(live.retained_source_bytes, 6);
        assert_eq!((live.examined_files, live.scanned_files, live.pending_files), (2, 2, 0));
        assert_eq!(live.stop_reason, Some(if complete { AtlasSearchStop::AllFilesExamined }
            else { AtlasSearchStop::MatchLimit }));
        assert_eq!(f.search.hit(&f.atlas, 1, 2).err(), Some(AtlasSearchError::MissingHit));
        f.build(2);
        let result = f.search.search_indexed_scoped(&f.atlas, 3, 2, "needle", insensitive(),
            1, 131072, || false).unwrap();
        assert_eq!(result.exit_code(), if complete { EXIT_OK } else { EXIT_PARTIAL });
        let indexed = f.search.progress(&f.atlas, 3).unwrap();
        assert_eq!((indexed.retained_hits, indexed.matches_seen), (1, expected_count));
        assert_eq!((indexed.complete, indexed.truncated), (complete, !complete));
        assert_eq!(indexed.source_bytes_read, 0);
        assert_eq!(json(&result).get("hits").array()[0].get("matched_text").text(), "NEEDLE");
    }
}

#[test]
fn malformed_sources_after_a_full_hit_buffer_remain_unavailable() {
    for bad in [b"invalid\xff".to_vec(), vec![0xff, 0xfe, 0x00, 0xd8],
        vec![0xfe, 0xff, 0xd8, 0x00]] {
        let mut f = Fixture::new(&[("a.rs", b"NEEDLE"), ("b.rs", &bad)]);
        f.search.search_scoped(&f.atlas, 1, "needle", insensitive(),
            AtlasSearchOptions { max_matches: 1, ..options() }, || false).unwrap();
        let live = f.search.progress(&f.atlas, 1).unwrap();
        assert!(!live.complete && !live.truncated);
        assert_eq!((live.matches_seen, live.retained_hits, live.unavailable_files), (1, 1, 1));
        assert_eq!(live.pending_files, 0);
        f.build(2);
        f.search.search_indexed_scoped(&f.atlas, 3, 2, "needle", insensitive(), 1, 131072, || false).unwrap();
        let indexed = f.search.progress(&f.atlas, 3).unwrap();
        assert!(!indexed.complete && !indexed.truncated);
        assert_eq!((indexed.matches_seen, indexed.retained_hits, indexed.unavailable_files), (1, 1, 1));
    }
}

#[test]
fn captured_index_bypasses_incompatible_negative_certificates_and_obeys_scan_budget() {
    let mut f = Fixture::new(&[("a.rs", b"NEEDLE"), ("b.rs", b"quiet")]);
    f.build(1);
    // A byte-exact negative certificate is valid for the default mode only.
    let exact = f.search.search_indexed(&f.atlas, 2, 1, "needle", 100, 0, || false).unwrap();
    assert_eq!(exact.exit_code(), EXIT_OK);
    assert_eq!(json(&exact).get("skipped_by_index").number(), 2);
    let exhausted = f.search.search_indexed_scoped(&f.atlas, 3, 1, "needle", insensitive(),
        100, 0, || false).unwrap();
    assert_eq!(exhausted.exit_code(), EXIT_PARTIAL);
    assert_eq!(json(&exhausted).get("stop_reason").text(), "verification-byte-limit");
    assert_eq!(json(&exhausted).get("skipped_by_index").number(), 0);
    fs::write(f.root.join("a.rs"), b"new bytes without a match").unwrap();
    let result = f.search.search_indexed_scoped(&f.atlas, 4, 1, "needle", insensitive(),
        100, 131072, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    let result = json(&result);
    assert_eq!(result.get("fallback_files").number(), 2);
    assert_eq!(result.get("skipped_by_index").number(), 0);
    assert_eq!(result.get("source_bytes_read").number(), 0);
    assert_eq!(result.get("hits").array()[0].get("matched_text").text(), "NEEDLE");
    let exact_again = f.search.search_indexed(&f.atlas, 5, 1, "needle", 100, 0, || false).unwrap();
    assert_eq!(exact_again.exit_code(), EXIT_OK);
    assert_eq!(json(&exact_again).get("retained_hits").number(), 0);
}

#[test]
fn lowercase_file_and_directory_scope_admission_precedes_work_and_hit_limits() {
    let mut f = Fixture::new(&[("a.rs", b"NEEDLE NEEDLE"), ("src/a.txt", b"NEEDLE NEEDLE"),
        ("src/z.rs", b"NEEDLE"), ("src-old/z.rs", b"NEEDLE NEEDLE")]);
    let scope = AtlasSearchScope::from_csv(Some("RS"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap().with_case_sensitive(false);
    assert!(!scope.is_all()); assert!(!scope.case_sensitive());
    assert!(scope.includes_path(b"src/z.rs")); assert!(!scope.includes_path(b"src-old/z.rs"));
    let result = f.search.search_scoped(&f.atlas, 1, "needle", scope.clone(),
        AtlasSearchOptions { max_matches: 1, max_files: 1, max_source_bytes: 6, ..options() }, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    let result = json(&result);
    assert_eq!(result.get("eligible_files").number(), 1);
    assert_eq!(result.get("excluded_files").number(), 3);
    assert_eq!(result.get("source_bytes_read").number(), 6);
    let live = f.search.hit(&f.atlas, 1, 1).unwrap();
    f.build(2);
    let result = f.search.search_indexed_scoped(&f.atlas, 3, 2, "needle", scope, 1, 6, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    assert_eq!(f.search.hit(&f.atlas, 3, 1).unwrap().file, live.file);
    assert_eq!(json(&result).get("verification_source_bytes").number(), 6);
    let empty = AtlasSearchScope::from_csv(Some(""), None).unwrap().with_case_sensitive(false);
    let result = f.search.search_indexed_scoped(&f.atlas, 4, 2, "needle", empty, 1, 0, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    assert_eq!(json(&result).get("eligible_files").number(), 0);
    assert_eq!(json(&result).get("verification_source_bytes").number(), 0);
}

#[test]
fn expanded_original_spelling_can_drive_exact_reader_find_after_search_replacement() {
    let query = "k".repeat(MAX_ATLAS_SEARCH_NEEDLE_BYTES);
    let source = "K".repeat(MAX_ATLAS_SEARCH_NEEDLE_BYTES);
    let mut f = Fixture::new(&[("source.rs", source.as_bytes())]);
    f.build(1);
    let result = f.search.search_indexed_scoped(&f.atlas, 2, 1, &query, insensitive(),
        2, 131072, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);
    let result = json(&result);
    let spelling = result.get("hits").array()[0].get("matched_text").text();
    assert_eq!(spelling, source); assert!(spelling.len() > MAX_ATLAS_SEARCH_NEEDLE_BYTES);
    let hit = f.search.hit(&f.atlas, 2, 1).unwrap();
    let (mut reader, _) = f.search.open_reader(&f.atlas, owner(99102), 2, 1, || false).unwrap();
    fs::write(f.root.join("source.rs"), b"new source").unwrap();
    f.search.search_indexed(&f.atlas, 3, 1, "absent", 2, 131072, || false).unwrap();
    reader.search(1, spelling, 2, source.len() as u64, || false).unwrap();
    assert_eq!(reader.hit_range(1, 0).unwrap(), hit.original_range);
    assert_eq!(reader.capture().bytes(), source.as_bytes());
}

#[test]
fn canceled_mode_replacement_preserves_accepted_rows_and_unsupported_structure_preserves_pending() {
    let mut f = Fixture::new(&[("a.rs", b"old NEEDLE"), ("b.rs", b"NEEDLE")]);
    f.search.search(&f.atlas, 1, "old", options(), || false).unwrap();
    let old = f.search.hit(&f.atlas, 1, 1).unwrap();
    f.search.begin_scoped(&f.atlas, 2, "needle", insensitive(), options(), || false).unwrap();
    assert_eq!(f.search.step(&f.atlas, 2, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(f.search.accepted_generation(), Some(1));
    assert_eq!(f.search.hit(&f.atlas, 1, 1).unwrap(), old);
    let old_page = f.search.page(&f.atlas, 1, 0, 10, || false).unwrap();
    assert!(json(&old_page).get("case_sensitive").flag());
    f.search.begin_scoped(&f.atlas, 3, "needle", insensitive(), options(), || false).unwrap();
    assert_eq!(f.search.begin_structure(&f.atlas, 4, "needle", insensitive(),
        AtlasStructureOptions::new(AtlasStructureMode::Symbols), || false).err(), Some(AtlasSearchError::InvalidScope));
    assert_eq!(f.search.pending_generation(), Some(3));
    f.finish(3);
    assert_eq!(f.search.progress(&f.atlas, 3).unwrap().retained_hits, 2);
    assert_eq!(f.search.hit(&f.atlas, 1, 1).err(), Some(AtlasSearchError::StaleQuery));
}
