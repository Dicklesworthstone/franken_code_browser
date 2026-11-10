#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real catalog/capture/reference-engine regressions for project Occurrences.
//! These are source-authored cases for the independent verifier, not a test-run receipt.

mod support;
use support::{Json, parse};
use std::{collections::BTreeMap, fs, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::{SymbolNameMode, symbols::{MAX_REFERENCE_ITEMS, MAX_REFERENCE_SOURCE_BYTES}};
use fcb_app::host::{HostResponse, atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{AtlasSearchError, AtlasSearchOptions, AtlasSearchPathKind,
        AtlasSearchScope, AtlasStructureMode, AtlasStructureOptions, RetainedAtlasSearch}};

struct Fixture(PathBuf);
impl Fixture {
    fn new(parts: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-occurrences-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
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
        AtlasSession::open(ArenaOwnerId::new(31881).unwrap(), &self.0,
            AtlasSessionOptions::default(), || false).unwrap()
    }
}
fn reader_owner() -> ArenaOwnerId {
    static NEXT: AtomicU64 = AtomicU64::new(31882);
    ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap()
}
fn options() -> AtlasStructureOptions {
    AtlasStructureOptions { mode: AtlasStructureMode::Occurrences, name_mode: SymbolNameMode::Exact,
        limits: AtlasSearchOptions { max_matches: 100, max_files: 100,
            max_file_bytes: 1024 * 1024, max_source_bytes: 1024 * 1024 } }
}
fn file_scope(name: &str) -> AtlasSearchScope {
    AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, name).unwrap()
}
fn json(response: HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) -> Json {
    for _ in 0..4100 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            return json(search.page(atlas, generation, 0, 128, || false).unwrap());
        }
        search.step(atlas, generation, || false).unwrap();
        assert!(search.progress(atlas, generation).unwrap().last_step_files <= 1);
    }
    panic!("bounded occurrence query did not finish");
}
fn count(value: &Json, field: &str, expected: u64) {
    assert_eq!(value.get(field).number(), expected, "{field}: {value:?}");
}
fn policy(value: &Json, file_limit: u64) {
    assert_eq!(value.get("requested_mode").text(), "occurrences");
    assert_eq!(value.get("realized_mode").text(), "whole-token-text-candidate");
    assert_eq!(value.get("evidence_level").text(), "whole-token-text-candidate");
    assert_eq!(value.get("mode").text(), "identifier-occurrence");
    assert_eq!(value.get("name_mode").text(), "exact");
    assert_eq!(value.get("name_case").text(), "sensitive");
    assert_eq!(value.get("completeness_basis").text(), "admitted-extraction-processing");
    assert!(!value.get("semantic_complete").flag());
    assert!(!value.get("compiler_resolved").flag());
    assert!(value.get("includes_comments_and_literals").flag());
    assert_eq!(value.get("token_policy").text(),
        "ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1");
    assert_eq!(value.get("name_syntax").text(), "single-token-no-leading-ascii-digit");
    assert_eq!(value.get("unicode_normalization").text(), "none");
    count(value, "structure_file_byte_limit", file_limit);
}
fn row_policy(row: &Json, needle: &str) {
    let structure = row.get("structure");
    assert_eq!(structure.get("name").text().as_bytes(), needle.as_bytes());
    assert_eq!(structure.get("kind").text(), "identifier-occurrence");
    assert_eq!(structure.get("language").text(), "text");
    assert_eq!(structure.get("evidence_level").text(), "whole-token-text-candidate");
    assert_eq!(structure.get("heading_slug"), &Json::Null);
    assert_eq!(structure.get("evidence_range"), row.get("original_range"));
}
fn range(row: &Json) -> (u64, u64) {
    let value = row.get("original_range");
    (value.get("start").number(), value.get("end").number())
}
fn ranges_by_path(search: &RetainedAtlasSearch, atlas: &AtlasSession,
    generation: u64, page: &Json) -> BTreeMap<String, Vec<(u64, u64)>> {
    let mut result: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for row in page.get("hits").array() {
        let hit = search.hit(atlas, generation, row.get("hit_id").number()).unwrap();
        let path = atlas.atlas().catalog().entry(hit.file).unwrap().path().raw().as_bytes();
        result.entry(std::str::from_utf8(path).unwrap().to_owned()).or_default().push(range(row));
    }
    result
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn cross_file_occurrences_respect_whole_tokens_and_include_comments_and_literals() {
    let a = b"run runner _run run_ $run run$ 1run Run // run\n\"run\" (run)\n";
    let b = b"run\trun\r\nrun";
    let fixture = Fixture::new(&[("a.rs", a), ("b.unrecognized", b), ("c", b"runner")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let begin = json(search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(),
        options(), || false).unwrap());
    policy(&begin, MAX_REFERENCE_SOURCE_BYTES as u64);
    count(&begin, "source_bytes_read", 0); count(&begin, "read_calls", 0);
    count(&begin, "retained_hits", 0);
    let page = finish(&mut search, &atlas, 1);
    policy(&page, MAX_REFERENCE_SOURCE_BYTES as u64);
    assert!(page.get("search_complete").flag());
    assert!(!page.get("truncated").flag());
    count(&page, "scanned_files", 3); count(&page, "matching_files", 2);
    count(&page, "unsupported_structure_files", 0); count(&page, "unavailable_files", 0);
    count(&page, "retained_hits", 7); count(&page, "matches_seen", 7);
    count(&page, "structure_metadata_bytes", 7 * 3); count(&page, "structure_metadata_omitted", 0);
    count(&page, "source_bytes_read", (a.len() + b.len() + b"runner".len()) as u64);
    let expected = BTreeMap::from([
        ("a.rs".to_owned(), vec![(0, 3), (43, 46), (48, 51), (54, 57)]),
        ("b.unrecognized".to_owned(), vec![(0, 3), (4, 7), (9, 12)]),
    ]);
    assert_eq!(ranges_by_path(&search, &atlas, 1, &page), expected);
    for row in page.get("hits").array() { row_policy(row, "run"); }
}

#[test]
fn utf8_and_both_utf16_byte_orders_keep_original_ranges_and_no_normalization() {
    let name = "cafe\u{301}";
    let text = "cafe\u{301} café cafe\u{301}x cafe\u{301}\n";
    let utf8 = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
    let little: Vec<u8> = [0xff, 0xfe].into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
    let big: Vec<u8> = [0xfe, 0xff].into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect();
    let fixture = Fixture::new(&[("a.txt", &utf8), ("b.data", &little), ("c.bin", &big)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_structure(&atlas, 1, name, AtlasSearchScope::all(), options(), || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    assert!(page.get("search_complete").flag());
    count(&page, "retained_hits", 6);
    let last = text.rfind(name).unwrap();
    let utf16_first = (2, 2 + name.encode_utf16().count() as u64 * 2);
    let utf16_last_start = 2 + text[..last].encode_utf16().count() as u64 * 2;
    let utf16_last = (utf16_last_start, utf16_last_start + utf16_first.1 - utf16_first.0);
    let expected = BTreeMap::from([
        ("a.txt".to_owned(), vec![(3, 3 + name.len() as u64),
            (3 + last as u64, 3 + last as u64 + name.len() as u64)]),
        ("b.data".to_owned(), vec![utf16_first, utf16_last]),
        ("c.bin".to_owned(), vec![utf16_first, utf16_last]),
    ]);
    assert_eq!(ranges_by_path(&search, &atlas, 1, &page), expected);
    for row in page.get("hits").array() {
        row_policy(row, name);
        let id = row.get("hit_id").number();
        let hit = search.hit(&atlas, 1, id).unwrap();
        let (mut reader, receipt) = search.open_reader(&atlas, reader_owner(), 1, id, || false).unwrap();
        assert!(!json(receipt).get("source_reopened").flag());
        let (start, end) = range(row);
        assert_eq!(hit.original_range.start().get(), start);
        let selected = reader.capture().bytes()[start as usize..end as usize].to_vec();
        let copy = json(reader.copy_range(start, end, || false).unwrap());
        assert_eq!(copy.get("original_hex").text(), hex(&selected));
    }
}

#[test]
fn directory_extension_file_and_empty_scopes_admit_before_quotas() {
    let fixture = Fixture::new(&[("a.rs", b"run"), ("src-old/a.rs", b"run"),
        ("src/a.txt", b"run"), ("src/target.rs", b"run")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let selected = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let mut limits = options(); limits.limits.max_files = 1;
    limits.limits.max_matches = 1; limits.limits.max_source_bytes = 3;
    search.begin_structure(&atlas, 1, "run", selected, limits, || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    assert!(page.get("search_complete").flag());
    count(&page, "eligible_files", 1); count(&page, "excluded_files", 3);
    count(&page, "source_bytes_read", 3); count(&page, "retained_hits", 1);
    assert_eq!(ranges_by_path(&search, &atlas, 1, &page).keys().next().unwrap(), "src/target.rs");
    search.begin_structure(&atlas, 2, "run", file_scope("src/a.txt"), limits, || false).unwrap();
    let file = finish(&mut search, &atlas, 2);
    assert!(file.get("search_complete").flag());
    assert_eq!(ranges_by_path(&search, &atlas, 2, &file).keys().next().unwrap(), "src/a.txt");
    let empty = AtlasSearchScope::from_csv(Some(""), None).unwrap();
    search.begin_structure(&atlas, 3, "run", empty, limits, || false).unwrap();
    let empty = finish(&mut search, &atlas, 3);
    assert!(empty.get("search_complete").flag());
    count(&empty, "eligible_files", 0); count(&empty, "source_bytes_read", 0);
    count(&empty, "read_calls", 0); count(&empty, "retained_hits", 0);
}

#[test]
fn an_exact_result_cap_continues_through_negative_files_and_one_extra_hit_truncates() {
    let fixture = Fixture::new(&[("a.txt", b"run"), ("b.txt", b"runner"),
        ("c.txt", b"Run"), ("d.txt", b"run")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut limits = options(); limits.limits.max_matches = 1;
    // The first three files have exactly one occurrence despite filling the cap
    // in the first slice. A zero-cap probe must process both later negatives.
    let selected = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible,
        "612e747874,622e747874,632e747874").unwrap();
    search.begin_structure(&atlas, 1, "run", selected, limits, || false).unwrap();
    search.step(&atlas, 1, || false).unwrap();
    let first = search.progress(&atlas, 1).unwrap();
    assert!(first.is_running()); assert_eq!(first.retained_hits, 1);
    let complete = finish(&mut search, &atlas, 1);
    assert!(complete.get("search_complete").flag()); assert!(!complete.get("truncated").flag());
    count(&complete, "scanned_files", 3); count(&complete, "matches_seen", 1);
    count(&complete, "pending_files", 0);
    search.begin_structure(&atlas, 2, "run", AtlasSearchScope::all(), limits, || false).unwrap();
    let limited = finish(&mut search, &atlas, 2);
    assert!(!limited.get("search_complete").flag()); assert!(limited.get("truncated").flag());
    assert_eq!(limited.get("stop_reason").text(), "match-limit");
    count(&limited, "scanned_files", 4); count(&limited, "retained_hits", 1);
    count(&limited, "matches_seen", 2); count(&limited, "matching_files", 1);
    count(&limited, "unavailable_files", 0); count(&limited, "extraction_limited_files", 0);
    count(&limited, "source_bytes_read", 15);
    let before = search.progress(&atlas, 2).unwrap();
    search.page(&atlas, 2, 0, 128, || false).unwrap(); search.overlay(&atlas, 2, || false).unwrap();
    let after = search.progress(&atlas, 2).unwrap();
    assert_eq!(before.source_bytes_read, after.source_bytes_read);
    assert_eq!(before.read_calls, after.read_calls);
}

#[test]
fn within_file_lookahead_counts_only_one_unstored_occurrence_and_leaves_later_files_pending() {
    let fixture = Fixture::new(&[("a.txt", b"run run run"), ("b.txt", b"run")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut limits = options(); limits.limits.max_matches = 1;
    search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), limits, || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    count(&page, "retained_hits", 1); count(&page, "matches_seen", 2);
    count(&page, "scanned_files", 1); count(&page, "pending_files", 1);
    count(&page, "source_bytes_read", 11); count(&page, "retained_source_bytes", 11);
    assert!(page.get("truncated").flag());
}

#[test]
fn the_full_reference_item_cap_preserves_candidate_ids_and_exact_completion() {
    let bytes = "run ".repeat(MAX_REFERENCE_ITEMS);
    let fixture = Fixture::new(&[("many.txt", bytes.as_bytes())]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut limits = options(); limits.limits.max_matches = MAX_REFERENCE_ITEMS;
    search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), limits, || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    assert!(page.get("search_complete").flag()); assert!(!page.get("truncated").flag());
    count(&page, "matches_seen", MAX_REFERENCE_ITEMS as u64);
    count(&page, "retained_hits", MAX_REFERENCE_ITEMS as u64);
    count(&page, "structure_metadata_omitted", 0);
    let last = json(search.page(&atlas, 1, MAX_REFERENCE_ITEMS - 1, 1, || false).unwrap());
    let row = &last.get("hits").array()[0];
    count(row, "hit_id", MAX_REFERENCE_ITEMS as u64);
    count(row.get("structure"), "candidate_id", MAX_REFERENCE_ITEMS as u64);
    row_policy(row, "run");
    assert_eq!(range(row), ((MAX_REFERENCE_ITEMS as u64 - 1) * 4,
        (MAX_REFERENCE_ITEMS as u64 - 1) * 4 + 3));
}

#[test]
fn invalid_names_and_matching_policies_fail_before_source_work_and_preserve_accepted_hits() {
    let fixture = Fixture::new(&[("source.txt", b"run")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), options(), || false).unwrap();
    finish(&mut search, &atlas, 1);
    let old = search.hit(&atlas, 1, 1).unwrap();
    let invalid = ["", "1run", "run more", "run.more", "run\0", "run\n"].into_iter()
        .map(str::to_owned).chain(std::iter::once("a".repeat(1025)));
    let mut generation = 2;
    for name in invalid {
        let mut polls = 0;
        let error = search.begin_structure(&atlas, generation, &name, AtlasSearchScope::all(),
            options(), || { polls += 1; false }).err().unwrap();
        assert_eq!(error.to_string(), "REFERENCE_INVALID_NAME");
        assert_eq!(polls, 0); assert_eq!(search.pending_generation(), None);
        assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
        generation += 1;
    }
    for mode in [SymbolNameMode::Prefix, SymbolNameMode::Contains] {
        let mut invalid = options(); invalid.name_mode = mode;
        let mut polls = 0;
        assert_eq!(search.begin_structure(&atlas, generation, "run", AtlasSearchScope::all(),
            invalid, || { polls += 1; false }).err(), Some(AtlasSearchError::InvalidLimits));
        assert_eq!(polls, 0); generation += 1;
    }
    assert_eq!(search.begin_structure(&atlas, generation, "run",
        AtlasSearchScope::all().with_case_sensitive(false), options(), || false).err(),
        Some(AtlasSearchError::InvalidScope));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
    assert_eq!(AtlasStructureOptions::new(AtlasStructureMode::Occurrences).name_mode, SymbolNameMode::Exact);
}

#[test]
fn unavailable_encodings_and_file_or_source_quotas_never_claim_a_complete_negative() {
    let fixture = Fixture::new(&[("a.txt", b"run \xff"), ("b.unknown", b"run")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), options(), || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    assert!(!page.get("search_complete").flag());
    count(&page, "retained_hits", 1); count(&page, "unavailable_files", 1);
    count(&page, "unsupported_structure_files", 0); count(&page, "source_bytes_read", 8);
    assert_eq!(page.get("diagnostics").array()[0].get("reason").text(), "REFERENCE_UNSUPPORTED_ENCODING");

    let quota = Fixture::new(&[("a.txt", b"run"), ("b.txt", b"run")]);
    let atlas = quota.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut limits = options(); limits.limits.max_files = 1;
    search.begin_structure(&atlas, 1, "absent", AtlasSearchScope::all(), limits, || false).unwrap();
    let file_limited = finish(&mut search, &atlas, 1);
    assert!(!file_limited.get("search_complete").flag());
    assert_eq!(file_limited.get("stop_reason").text(), "file-limit");
    count(&file_limited, "pending_files", 1); count(&file_limited, "retained_hits", 0);
    limits.limits.max_files = 100; limits.limits.max_source_bytes = 3;
    search.begin_structure(&atlas, 2, "absent", AtlasSearchScope::all(), limits, || false).unwrap();
    let byte_limited = finish(&mut search, &atlas, 2);
    assert!(!byte_limited.get("search_complete").flag());
    assert_eq!(byte_limited.get("stop_reason").text(), "source-byte-limit");
    count(&byte_limited, "pending_files", 1); count(&byte_limited, "source_bytes_read", 3);
}

#[test]
fn cancellation_during_extraction_preserves_the_previous_accepted_query() {
    let mut source = vec![b' '; 128 * 1024]; source[..3].copy_from_slice(b"run");
    let fixture = Fixture::new(&[("source.txt", &source)]);
    let atlas = fixture.open();
    let mut probe = RetainedAtlasSearch::new(&atlas).unwrap();
    probe.begin_structure(&atlas, 1, "run", AtlasSearchScope::all(), options(), || false).unwrap();
    let mut polls = 0;
    probe.step(&atlas, 1, || { polls += 1; false }).unwrap();
    assert!(polls > 32, "exercise cancellation inside bounded source work");
    drop(probe);
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "run", options().limits, || false).unwrap();
    let old = search.hit(&atlas, 1, 1).unwrap();
    let before = search.progress(&atlas, 1).unwrap();
    search.begin_structure(&atlas, 2, "run", AtlasSearchScope::all(), options(), || false).unwrap();
    let mut observed = 0;
    assert_eq!(search.step(&atlas, 2, || { observed += 1; observed == polls / 2 }).err(),
        Some(AtlasSearchError::Canceled));
    assert_eq!(search.pending_generation(), None);
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
    assert_eq!(search.progress(&atlas, 1).unwrap(), before);
    let accepted = json(search.page(&atlas, 1, 0, 128, || false).unwrap());
    assert_eq!(accepted.get("mode").text(), "exact-decoded-literal");
}

#[test]
fn progressive_reader_activation_and_retained_copy_survive_disk_changes_and_cancellation() {
    let fixture = Fixture::new(&[("a.txt", b"run first"), ("b.txt", b"run later")]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "first", options().limits, || false).unwrap();
    let old = search.hit(&atlas, 1, 1).unwrap();
    search.begin_structure(&atlas, 2, "run", AtlasSearchScope::all(), options(), || false).unwrap();
    search.step(&atlas, 2, || false).unwrap();
    assert!(search.progress(&atlas, 2).unwrap().is_running());
    fs::write(fixture.0.join("a.txt"), b"new bytes").unwrap();
    let before = search.progress(&atlas, 2).unwrap();
    let (mut reader, receipt) = search.open_reader(&atlas, reader_owner(), 2, 1, || false).unwrap();
    let receipt = json(receipt);
    assert!(receipt.get("search_in_progress").flag());
    assert!(!receipt.get("source_reopened").flag());
    row_policy(&receipt, "run");
    assert_eq!(reader.capture().bytes(), b"run first");
    assert_eq!(search.progress(&atlas, 2).unwrap(), before);
    assert!(search.cancel_pending());
    assert_eq!(search.step(&atlas, 2, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
    search.clear(&atlas, 3, || false).unwrap();
    let copy = json(reader.copy_range(0, 3, || false).unwrap());
    assert_eq!(copy.get("original_hex").text(), "72756e");
}

#[test]
fn the_default_query_budget_admits_a_full_512_kib_file_and_refuses_the_next_byte_before_io() {
    let mut exact = vec![b' '; MAX_REFERENCE_SOURCE_BYTES];
    exact[MAX_REFERENCE_SOURCE_BYTES - 3..].copy_from_slice(b"run");
    let beyond = vec![b' '; MAX_REFERENCE_SOURCE_BYTES + 1];
    let fixture = Fixture::new(&[("a-exact.txt", &exact), ("b-beyond.txt", &beyond)]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let defaults = AtlasStructureOptions::new(AtlasStructureMode::Occurrences);
    search.begin_structure(&atlas, 1, "run", file_scope("a-exact.txt"), defaults, || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    policy(&page, MAX_REFERENCE_SOURCE_BYTES as u64);
    assert!(page.get("search_complete").flag());
    count(&page, "source_bytes_read", MAX_REFERENCE_SOURCE_BYTES as u64);
    count(&page, "retained_source_bytes", MAX_REFERENCE_SOURCE_BYTES as u64);
    count(&page, "retained_hits", 1); count(&page, "unavailable_files", 0);
    assert_eq!(range(&page.get("hits").array()[0]),
        (MAX_REFERENCE_SOURCE_BYTES as u64 - 3, MAX_REFERENCE_SOURCE_BYTES as u64));
    drop(search);
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.begin_structure(&atlas, 1, "run", file_scope("b-beyond.txt"), defaults, || false).unwrap();
    let page = finish(&mut search, &atlas, 1);
    assert!(!page.get("search_complete").flag());
    count(&page, "source_bytes_read", 0); count(&page, "read_calls", 0);
    count(&page, "retained_hits", 0); count(&page, "unavailable_files", 1);
    assert_eq!(page.get("diagnostics").array()[0].get("reason").text(), "STRUCTURE_FILE_BYTE_LIMIT");
}

#[test]
fn a_replacement_decoder_admission_failure_retains_the_old_inventory_and_allows_a_smaller_retry() {
    let mut large = vec![b' '; MAX_REFERENCE_SOURCE_BYTES];
    large[..3].copy_from_slice(b"run");
    let fixture = Fixture::new(&[("a-small.txt", b"run"), ("b-large.txt", &large)]);
    let atlas = fixture.open(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let defaults = AtlasStructureOptions::new(AtlasStructureMode::Occurrences);
    search.begin_structure(&atlas, 1, "run", file_scope("a-small.txt"), defaults, || false).unwrap();
    finish(&mut search, &atlas, 1);
    let old = search.hit(&atlas, 1, 1).unwrap();
    let old_progress = search.progress(&atlas, 1).unwrap();
    // Both default query envelopes stay admitted until successful replacement.
    // Adding the shared 512 KiB decoder lease must explicitly refuse this
    // overlap rather than silently accepting an empty partial inventory.
    search.begin_structure(&atlas, 2, "run", file_scope("b-large.txt"), defaults, || false).unwrap();
    let error = search.step(&atlas, 2, || false).err().unwrap();
    assert_eq!(error.to_string(), "REFERENCE_RESOURCE_DENIED");
    assert_eq!(search.pending_generation(), None);
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), old);
    assert_eq!(search.progress(&atlas, 1).unwrap(), old_progress);
    assert!(search.hit(&atlas, 2, 1).is_err());
    let (mut reader, _) = search.open_reader(&atlas, reader_owner(), 1, 1, || false).unwrap();
    assert_eq!(json(reader.copy_range(0, 3, || false).unwrap()).get("original_hex").text(), "72756e");
    search.begin_structure(&atlas, 3, "run", file_scope("a-small.txt"), defaults, || false).unwrap();
    let retry = finish(&mut search, &atlas, 3);
    assert!(retry.get("search_complete").flag()); count(&retry, "retained_hits", 1);
    assert!(search.hit(&atlas, 1, 1).is_err());
}
