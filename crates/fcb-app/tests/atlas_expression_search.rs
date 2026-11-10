#![forbid(unsafe_code)]
#![cfg(unix)]

//! Explicit native-project expressions through the actual parser, frozen atlas,
//! capture verifier, owning index cursor, and retained-reader activation.
//! These are source-level regression fixtures; execution requires RCH.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::{IndexError, QueryError};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::HostResponse;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchError, AtlasSearchOptions,
    AtlasSearchPathKind as Kind, AtlasSearchScope, RetainedAtlasSearch,
    MAX_ATLAS_EXPRESSION_SCAN_BYTES};
use support::{Json, parse};

const WORK_BYTES: u64 = 1024 * 1024;
struct Fixture { root: PathBuf, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(48301);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-atlas-expression-{}-{id}-{}", std::process::id(),
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
    fn finish(&mut self, generation: u64) -> Json {
        for _ in 0..102 {
            if !self.search.progress(&self.atlas, generation).unwrap().is_running() {
                return json(&self.search.page(&self.atlas, generation, 0, 128, || false).unwrap());
            }
            self.search.step(&self.atlas, generation, || false).unwrap();
            assert!(self.search.progress(&self.atlas, generation).unwrap().last_step_files <= 1);
        }
        panic!("expression exceeded its bounded fixture membership");
    }
    fn live(&mut self, generation: u64, expression: &str, scope: AtlasSearchScope) -> Json {
        self.search.begin_expression_scoped(&self.atlas, generation, expression, scope,
            options(), WORK_BYTES, || false).unwrap();
        self.finish(generation)
    }
    fn indexed(&mut self, generation: u64, index: u64, expression: &str, scope: AtlasSearchScope) -> Json {
        self.search.begin_indexed_expression_scoped(&self.atlas, generation, index, expression,
            scope, options().max_matches, WORK_BYTES, || false).unwrap();
        self.finish(generation)
    }
    fn build(&mut self, generation: u64) {
        self.search.prepare_index(&self.atlas, generation, index_options(), || false).unwrap();
    }
    fn hits(&self, generation: u64, page: &Json) -> Vec<(Vec<u8>, u64, u64, String)> {
        page.get("hits").array().iter().map(|row| {
            let hit = self.search.hit(&self.atlas, generation, row.get("hit_id").number()).unwrap();
            let path = self.atlas.atlas().catalog().entry(hit.file).unwrap().path().raw().as_bytes().to_vec();
            (path, hit.original_range.start().get(), hit.original_range.end().get(),
                row.get("matched_text").text().to_owned())
        }).collect()
    }
}
// Retain fixtures for independent replay. Source replacement, not cleanup,
// establishes that a selected result still opens its original searched bytes.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 100, max_files: 100, max_file_bytes: 65536, max_source_bytes: 131072 }
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 65536, max_source_bytes: 131072, max_index_grams: 65536 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn insensitive() -> AtlasSearchScope { AtlasSearchScope::all().with_case_sensitive(false) }
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}".encode_utf16().chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn visible(paths: &[&[u8]]) -> AtlasSearchScope {
    let mut paths = paths.to_vec(); paths.sort_unstable();
    let wire = paths.iter().map(|path| path.iter().map(|byte| format!("{byte:02x}"))
        .collect::<String>()).collect::<Vec<_>>().join(",");
    AtlasSearchScope::all().with_path(Kind::Visible, &wire).unwrap()
}
fn receipt(page: &Json, raw: &str, primary: &str) {
    assert_eq!(page.get("mode").text(), "decoded-text-expression");
    assert_eq!(page.get("expression").text(), raw);
    assert_eq!(page.get("needle").text(), primary);
    assert_eq!(page.get("primary_needle").text(), primary);
    assert_eq!(page.get("query_syntax").text(), "fcb-bounded-expression/1");
    assert_eq!(page.get("predicate_scope").text(), "same-complete-capture");
    assert_eq!(page.get("path_filter_domain").text(), "utf8-or-escaped-search-key");
    assert!(page.get("scanned_input_bytes").number() <= page.get("max_scan_bytes").number());
    assert_eq!(page.get("eligible_files").number(), page.get("expression_excluded_files").number()
        + page.get("expression_scope_files").number());
}
fn indexed_counts(page: &Json) {
    assert_eq!(page.get("source_bytes_read").number(), 0);
    assert_eq!(page.get("read_calls").number(), 0);
    assert_eq!(page.get("verification_source_bytes").number(), page.get("scanned_input_bytes").number());
    assert_eq!(page.get("examined_files").number(), page.get("expression_excluded_files").number()
        + page.get("scanned_files").number() + page.get("skipped_by_index").number()
        + page.get("scoped_capture_unavailable_files").number());
}

#[test]
fn phrases_and_full_capture_predicates_keep_only_primary_occurrences() {
    let raw = "\"pub fn\" Result -unsafe lang:rust path:src/";
    let mut f = Fixture::new(&[
        ("src/a.rs", b"pub fn accepted() {}\nResult Result\npub fn second() {}\n"),
        ("src/b.rs", b"pub fn excluded() {}\nResult\nunsafe\n"),
        ("src/c.rs", b"pub fn lacks_required() {}\n"),
        ("src/d.rs", b"Result alone\n"),
        ("README.md", b"pub fn Result"), ("vendor/e.rs", b"pub fn Result"),
    ]);
    let live = f.live(1, raw, AtlasSearchScope::all());
    receipt(&live, raw, "pub fn");
    assert!(live.get("search_complete").flag());
    assert_eq!(live.get("retained_hits").number(), 2);
    assert_eq!(live.get("matches_seen").number(), 2);
    assert_eq!(live.get("expression_excluded_files").number(), 2);
    let expected = f.hits(1, &live);
    assert!(expected.iter().all(|hit| hit.0 == b"src/a.rs" && hit.3 == "pub fn"));
    f.build(2);
    let indexed = f.indexed(3, 2, raw, AtlasSearchScope::all());
    receipt(&indexed, raw, "pub fn"); indexed_counts(&indexed);
    assert!(indexed.get("search_complete").flag());
    assert_eq!(f.hits(3, &indexed), expected);
}

#[test]
fn expression_syntax_is_opt_in_and_quoted_operator_text_stays_literal() {
    let literal = "needle Result -unsafe lang:rust";
    let mut f = Fixture::new(&[("source.rs", literal.as_bytes())]);
    let old = f.search.search(&f.atlas, 1, literal, options(), || false).unwrap();
    assert_eq!(json(&old).get("mode").text(), "exact-decoded-literal");
    assert_eq!(json(&old).get("retained_hits").number(), 1);
    let expression = f.live(2, literal, AtlasSearchScope::all());
    assert_eq!(expression.get("retained_hits").number(), 0);
    let quoted = format!("\"{literal}\"");
    let explicit = f.live(3, &quoted, AtlasSearchScope::all());
    receipt(&explicit, &quoted, literal);
    assert_eq!(explicit.get("retained_hits").number(), 1);
    f.build(4);
    let indexed = f.search.search_indexed(&f.atlas, 5, 4, literal, 100, WORK_BYTES, || false).unwrap();
    assert_eq!(json(&indexed).get("mode").text(), "exact-decoded-literal");
    assert_eq!(json(&indexed).get("retained_hits").number(), 1);
}

#[test]
fn metadata_predicates_precede_file_source_and_match_quotas() {
    let noise = b"needle ".repeat(100);
    let source = b"needle\nok\n";
    let raw = "needle ok lang:rust -path:src/drop.rs";
    let mut f = Fixture::new(&[("a.txt", &noise), ("src/drop.rs", &noise), ("src/z.rs", source)]);
    let begin = f.search.begin_expression_scoped(&f.atlas, 1, raw, AtlasSearchScope::all(),
        AtlasSearchOptions { max_files: 1, max_matches: 1, max_file_bytes: 16,
            max_source_bytes: source.len() }, (2 * source.len()) as u64, || false).unwrap();
    let begin = json(&begin);
    assert_eq!(begin.get("eligible_files").number(), 3);
    assert_eq!(begin.get("excluded_files").number(), 0);
    assert_eq!(begin.get("expression_excluded_files").number(), 2);
    assert_eq!(begin.get("examined_files").number(), 2);
    assert_eq!(begin.get("scanned_files").number(), 0);
    assert_eq!(begin.get("pending_files").number(), 1);
    assert_eq!(begin.get("source_bytes_read").number(), 0);
    let live = f.finish(1);
    assert!(live.get("search_complete").flag());
    assert_eq!(live.get("source_bytes_read").number(), source.len() as u64);
    assert_eq!(live.get("unavailable_files").number(), 0);
    assert_eq!(f.hits(1, &live)[0].0, b"src/z.rs");
    f.build(2);
    f.search.begin_indexed_expression_scoped(&f.atlas, 3, 2, raw, AtlasSearchScope::all(),
        1, (2 * source.len()) as u64, || false).unwrap();
    let indexed = f.finish(3); indexed_counts(&indexed);
    assert!(indexed.get("search_complete").flag());
    assert_eq!(indexed.get("eligible_captured_files").number(), 1);
    assert_eq!(indexed.get("examined_files").number(), 3);
}

#[test]
fn ui_type_directory_file_and_visible_memberships_intersect_expression_filters() {
    let mut f = Fixture::new(&[("src/a.rs", b"needle"), ("src/b.rs", b"needle"),
        ("src/help.md", b"needle"), ("vendor/c.rs", b"needle")]);
    let scoped = AtlasSearchScope::from_csv(Some("rs"), None).unwrap().with_path(Kind::Directory, "src").unwrap();
    let live = f.live(1, "needle -path:src/b.rs", scoped.clone());
    assert_eq!(live.get("eligible_files").number(), 2);
    assert_eq!(live.get("excluded_files").number(), 2);
    assert_eq!(live.get("expression_excluded_files").number(), 1);
    assert_eq!(f.hits(1, &live)[0].0, b"src/a.rs");
    f.build(2);
    let indexed = f.indexed(3, 2, "needle -path:src/b.rs", scoped); indexed_counts(&indexed);
    assert!(indexed.get("search_complete").flag());
    assert_eq!(f.hits(3, &indexed)[0].0, b"src/a.rs");
    let file = AtlasSearchScope::all().with_path(Kind::File, "src/a.rs").unwrap();
    let empty = f.live(4, "needle path:vendor/", file);
    assert!(empty.get("search_complete").flag());
    assert_eq!(empty.get("expression_excluded_files").number(), 1);
    assert_eq!(empty.get("source_bytes_read").number(), 0);
    let visible = visible(&[b"src/a.rs", b"src/missing.rs"]);
    let live = f.live(5, "needle lang:rust", visible.clone());
    let indexed = f.indexed(6, 2, "needle lang:rust", visible);
    for page in [live, indexed] {
        assert!(!page.get("search_complete").flag());
        assert_eq!(page.get("scope_region_missing_files").number(), 1);
        assert_eq!(page.get("retained_hits").number(), 1);
    }
}

#[test]
fn empty_metadata_selection_is_complete_without_spending_a_zero_scan_budget() {
    let mut f = Fixture::new(&[("source.rs", b"needle")]);
    f.search.begin_expression_scoped(&f.atlas, 1, "needle lang:python", AtlasSearchScope::all(),
        options(), 0, || false).unwrap();
    let live = f.finish(1);
    f.build(2);
    f.search.begin_indexed_expression_scoped(&f.atlas, 3, 2, "needle lang:python", AtlasSearchScope::all(),
        100, 0, || false).unwrap();
    let indexed = f.finish(3); indexed_counts(&indexed);
    for page in [live, indexed] {
        assert!(page.get("search_complete").flag());
        assert_eq!(page.get("expression_scope_files").number(), 0);
        assert_eq!(page.get("examined_files").number(), 1);
        assert_eq!(page.get("scanned_input_bytes").number(), 0);
        assert_eq!(page.get("source_bytes_read").number(), 0);
        assert_eq!(page.get("stop_reason").text(), "all-files-examined");
    }
}

#[test]
fn case_option_applies_to_primary_required_and_excluded_terms_in_utf16() {
    let little = utf16("NEEDLE\nKeep\nsafe\n", true);
    let big = utf16("NEEDLE\nKeep\nsafe\n", false);
    let mut f = Fixture::new(&[("a.rs", &little), ("b.rs", &big)]);
    assert_eq!(f.live(1, "needle keep -danger", AtlasSearchScope::all()).get("retained_hits").number(), 0);
    assert_eq!(f.live(2, "needle keep -SAFE", insensitive()).get("retained_hits").number(), 0);
    let live = f.live(3, "needle keep -danger", insensitive());
    let expected = f.hits(3, &live);
    assert_eq!(expected.len(), 2);
    assert!(expected.iter().all(|hit| hit.1 == 2 && hit.2 == 14 && hit.3 == "NEEDLE"));
    f.build(4);
    let indexed = f.indexed(5, 4, "needle keep -danger", insensitive()); indexed_counts(&indexed);
    assert_eq!(f.hits(5, &indexed), expected);
    assert!(!indexed.get("case_sensitive").flag());
    let (reader, activation) = f.search.open_reader(&f.atlas, owner(49811), 5, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), little);
    assert_eq!(json(&activation).get("matched_text").text(), "NEEDLE");
}

#[test]
fn lowercase_expansions_keep_complete_original_scalars_for_exact_activation() {
    for (source, query, spelling, start, end) in [
        ("AİZ required", "\u{0307} required -absent", "İ", 1, 3),
        ("K required", "k required -absent", "K", 0, 3),
    ] {
        let mut f = Fixture::new(&[("source.rs", source.as_bytes())]);
        let live = f.live(1, query, insensitive());
        let expected = f.hits(1, &live);
        assert_eq!(expected, vec![(b"source.rs".to_vec(), start, end, spelling.to_owned())]);
        f.build(2);
        fs::write(f.root.join("source.rs"), b"changed live source").unwrap();
        let indexed = f.indexed(3, 2, query, insensitive());
        assert_eq!(f.hits(3, &indexed), expected);
        let (reader, _) = f.search.open_reader(&f.atlas, owner(49812), 3, 1, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source.as_bytes());
    }
}

#[test]
fn predicate_scan_work_is_separate_from_captured_bytes_and_charged_cumulatively() {
    let source = b"needle required\n";
    let raw = "needle required -absent";
    let mut f = Fixture::new(&[("source.rs", source)]);
    let live = f.live(1, raw, AtlasSearchScope::all());
    assert!(live.get("search_complete").flag());
    assert_eq!(live.get("source_bytes_read").number(), source.len() as u64);
    assert_eq!(live.get("scanned_input_bytes").number(), (3 * source.len()) as u64);
    f.build(2);
    let indexed = f.indexed(3, 2, raw, AtlasSearchScope::all()); indexed_counts(&indexed);
    assert!(indexed.get("search_complete").flag());
    assert_eq!(indexed.get("verification_source_bytes").number(), (3 * source.len()) as u64);
    assert!(indexed.get("verification_source_bytes").number() > f.search.indexed_source_bytes());
}

#[test]
fn partial_not_probe_never_establishes_absence_or_publishes_an_unqualified_hit() {
    let source = b"needle required and a suffix that must be verified";
    let raw = "needle required -absent";
    let mut f = Fixture::new(&[("source.rs", source)]);
    let budget = source.len() as u64 - 1;
    f.search.begin_expression_scoped(&f.atlas, 1, raw, AtlasSearchScope::all(), options(), budget, || false).unwrap();
    let live = f.finish(1);
    f.build(2);
    f.search.begin_indexed_expression_scoped(&f.atlas, 3, 2, raw, AtlasSearchScope::all(), 100, budget, || false).unwrap();
    let indexed = f.finish(3); indexed_counts(&indexed);
    for page in [live, indexed] {
        assert!(!page.get("search_complete").flag());
        assert_eq!(page.get("retained_hits").number(), 0);
        assert_eq!(page.get("matches_seen").number(), 0);
        assert_eq!(page.get("stop_reason").text(), "verification-byte-limit");
        assert!(page.get("scanned_input_bytes").number() <= budget);
    }
    f.search.begin_expression_scoped(&f.atlas, 4, raw, AtlasSearchScope::all(), options(), 0, || false).unwrap();
    let zero = f.finish(4);
    assert_eq!(zero.get("source_bytes_read").number(), 0);
    assert_eq!(zero.get("scanned_files").number(), 0);
    assert_eq!(zero.get("pending_files").number(), 1);
    assert_eq!(zero.get("stop_reason").text(), "verification-byte-limit");
}

#[test]
fn exact_result_cap_and_one_omitted_occurrence_have_distinct_coverage() {
    for (second, truncated, counted) in [(b"quiet required".as_slice(), false, 1),
        (b"needle required".as_slice(), true, 2)] {
        let mut f = Fixture::new(&[("a.rs", b"needle required"), ("b.rs", second)]);
        f.search.begin_expression_scoped(&f.atlas, 1, "needle required", AtlasSearchScope::all(),
            AtlasSearchOptions { max_matches: 1, ..options() }, WORK_BYTES, || false).unwrap();
        let live = f.finish(1);
        f.build(2);
        f.search.begin_indexed_expression_scoped(&f.atlas, 3, 2, "needle required", AtlasSearchScope::all(),
            1, WORK_BYTES, || false).unwrap();
        let indexed = f.finish(3); indexed_counts(&indexed);
        for page in [live, indexed] {
            assert_eq!(page.get("retained_hits").number(), 1);
            assert_eq!(page.get("matches_seen").number(), counted);
            assert_eq!(page.get("truncated").flag(), truncated);
            assert_eq!(page.get("search_complete").flag(), !truncated);
        }
    }
}

#[test]
fn overlapping_primary_matches_survive_conjunctions_without_multiplication() {
    let mut f = Fixture::new(&[("source.rs", b"banana required required")]);
    let live = f.live(1, "ana required", AtlasSearchScope::all());
    let expected = vec![(b"source.rs".to_vec(), 1, 4, "ana".to_owned()),
        (b"source.rs".to_vec(), 3, 6, "ana".to_owned())];
    assert_eq!(f.hits(1, &live), expected);
    f.build(2);
    let indexed = f.indexed(3, 2, "ana required", AtlasSearchScope::all());
    assert_eq!(f.hits(3, &indexed), expected);
    assert_eq!(indexed.get("matches_seen").number(), 2);
}

#[test]
fn running_indexed_hits_open_retained_sources_after_disk_replacement_and_cancellation() {
    let source = b"needle required\n";
    let mut f = Fixture::new(&[("a.rs", source), ("b.rs", source)]);
    f.build(1);
    fs::write(f.root.join("a.rs"), b"replacement source").unwrap();
    f.search.begin_indexed_expression_scoped(&f.atlas, 2, 1, "needle required", AtlasSearchScope::all(),
        100, WORK_BYTES, || false).unwrap();
    let first = f.search.step(&f.atlas, 2, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_PARTIAL);
    let first = json(&first); indexed_counts(&first);
    assert_eq!(first.get("retained_hits").number(), 1);
    assert!(f.search.progress(&f.atlas, 2).unwrap().is_running());
    let (mut reader, activation) = f.search.open_reader(&f.atlas, owner(49813), 2, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), source);
    let activation = json(&activation);
    assert_eq!(activation.get("matched_text").text(), "needle");
    assert!(!activation.get("source_reopened").flag());
    assert!(f.search.cancel_pending());
    f.search.clear_index(&f.atlas, 3, || false).unwrap();
    reader.search(1, "needle", 10, source.len() as u64, || false).unwrap();
    assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn metadata_exclusion_can_close_a_query_over_a_narrower_capture_scope() {
    let mut f = Fixture::new(&[("src/a.rs", b"needle required"), ("docs/a.md", b"needle required")]);
    let rust = AtlasSearchScope::from_csv(Some("rs"), None).unwrap();
    f.search.prepare_index_scoped(&f.atlas, 1, rust, index_options(), || false).unwrap();
    let selected = f.indexed(2, 1, "needle required lang:rust", AtlasSearchScope::all());
    indexed_counts(&selected);
    assert!(selected.get("search_complete").flag());
    assert_eq!(selected.get("eligible_files").number(), 2);
    assert_eq!(selected.get("expression_excluded_files").number(), 1);
    assert_eq!(selected.get("eligible_captured_files").number(), 1);
    assert_eq!(selected.get("pending_files").number(), 0);
    let broad = f.indexed(3, 1, "needle required", AtlasSearchScope::all()); indexed_counts(&broad);
    assert!(!broad.get("search_complete").flag());
    assert_eq!(broad.get("pending_files").number(), 1);
    assert_eq!(broad.get("stop_reason").text(), "capture-scope");
    let absent = f.indexed(4, 1, "needle required -lang:rust", AtlasSearchScope::all());
    assert_eq!(absent.get("expression_scope_files").number(), 1);
    assert_eq!(absent.get("eligible_captured_files").number(), 0);
    assert_eq!(absent.get("pending_files").number(), 1);
    assert_eq!(absent.get("stop_reason").text(), "capture-scope");
}

#[test]
fn metadata_exclusion_does_not_misclassify_unavailable_selected_captures() {
    let large = b"needle ".repeat(100);
    let mut f = Fixture::new(&[("a.rs", &large), ("b.md", b"needle")]);
    f.search.prepare_index(&f.atlas, 1, AtlasIndexOptions { max_file_bytes: 16, ..index_options() }, || false).unwrap();
    let markdown = f.indexed(2, 1, "needle lang:markdown", AtlasSearchScope::all()); indexed_counts(&markdown);
    assert!(markdown.get("search_complete").flag());
    assert_eq!(markdown.get("scoped_capture_unavailable_files").number(), 0);
    assert_eq!(markdown.get("expression_excluded_files").number(), 1);
    let rust = f.indexed(3, 1, "needle lang:rust", AtlasSearchScope::all()); indexed_counts(&rust);
    assert!(!rust.get("search_complete").flag());
    assert_eq!(rust.get("scoped_capture_unavailable_files").number(), 1);
    assert_eq!(rust.get("unavailable_files").number(), 1);
    assert_eq!(rust.get("expression_excluded_files").number(), 1);
    assert_eq!(rust.get("pending_files").number(), 0);
}

#[test]
fn failed_and_canceled_replacements_preserve_the_last_accepted_generation() {
    let mut f = Fixture::new(&[("source.rs", b"needle required")]);
    f.live(1, "needle required", AtlasSearchScope::all());
    let errors = [("re:needle", QueryError::RegexUnqualified),
        ("\"unterminated", QueryError::SyntaxError), ("-needle lang:rust", QueryError::EmptyNeedle)];
    for (offset, (query, expected)) in errors.into_iter().enumerate() {
        assert_eq!(f.search.begin_expression_scoped(&f.atlas, offset as u64 + 2, query,
            AtlasSearchScope::all(), options(), WORK_BYTES, || false).err(),
            Some(AtlasSearchError::Index(IndexError::Query(expected))));
        assert_eq!(f.search.accepted_generation(), Some(1));
        assert_eq!(f.search.page(&f.atlas, 1, 0, 64, || false).unwrap().exit_code(), EXIT_OK);
    }
    f.search.begin_expression_scoped(&f.atlas, 5, "needle required", AtlasSearchScope::all(),
        options(), WORK_BYTES, || false).unwrap();
    assert_eq!(f.search.step(&f.atlas, 5, || true).err(), Some(AtlasSearchError::Canceled));
    assert_eq!(f.search.accepted_generation(), Some(1));
    assert_eq!(f.search.pending_generation(), None);
    assert_eq!(f.search.begin_expression_scoped(&f.atlas, 5, "needle", AtlasSearchScope::all(),
        options(), WORK_BYTES, || false).err(), Some(AtlasSearchError::StaleQuery));
    assert_eq!(f.search.begin_expression_scoped(&f.atlas, 6, "needle", AtlasSearchScope::all(),
        options(), MAX_ATLAS_EXPRESSION_SCAN_BYTES + 1, || false).err(), Some(AtlasSearchError::InvalidLimits));
    assert_eq!(f.search.accepted_generation(), Some(1));
    let oversized = "x".repeat(fcb::search::MAX_QUERY_LEN + 1);
    assert_eq!(f.search.begin_expression_scoped(&f.atlas, 7, &oversized, AtlasSearchScope::all(),
        options(), WORK_BYTES, || false).err(), Some(AtlasSearchError::Index(IndexError::Query(QueryError::NeedleTooLong))));
    assert_eq!(f.search.accepted_generation(), Some(1));
}
