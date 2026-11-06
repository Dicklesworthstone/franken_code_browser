#![forbid(unsafe_code)]
#![cfg(unix)]

//! Captured search context through the real catalog, scanner, index and reader.
//! These are code-first regressions for independent replay, not execution evidence.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::search::SymbolNameMode;
use fcb_app::host::HostResponse;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchOptions, AtlasSearchScope,
    AtlasStructureMode, AtlasStructureOptions, RetainedAtlasSearch,
    MAX_ATLAS_EXCERPT_SOURCE_BYTES, MAX_ATLAS_EXCERPT_TEXT_BYTES};
use support::{Json, parse};

struct Fixture { root: PathBuf, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(40101);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-atlas-excerpts-{}-{id}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
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
        }
        panic!("bounded query did not finish");
    }
}
// Captures intentionally remain for independent replay; no deletion is needed
// to prove replacement and retained-source behavior.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 100, max_files: 100,
        max_file_bytes: 65536, max_source_bytes: 131072 }
}
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 65536,
        max_source_bytes: 131072, max_index_grams: 65536 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn bounds(value: &Json) -> (usize, usize) {
    (value.get("start").number() as usize, value.get("end").number() as usize)
}
fn selected_text(row: &Json) -> &str {
    let excerpt = row.get("excerpt");
    let text = excerpt.get("text").text();
    let (start, end) = bounds(excerpt.get("original_range"));
    let (match_start, match_end) = bounds(excerpt.get("match_utf8_range"));
    let (hit_start, hit_end) = bounds(row.get("original_range"));
    assert!(start <= hit_start && hit_start < hit_end && hit_end <= end);
    assert!(end - start <= MAX_ATLAS_EXCERPT_SOURCE_BYTES);
    assert!(text.len() <= MAX_ATLAS_EXCERPT_TEXT_BYTES);
    assert!(match_start < match_end && match_end <= text.len());
    assert!(text.is_char_boundary(match_start) && text.is_char_boundary(match_end));
    &text[match_start..match_end]
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    "\u{feff}".encode_utf16().chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}

#[test]
fn utf8_bom_multiline_context_keeps_distinct_overlapping_occurrences() {
    let source = "\u{feff}α before\r\nbanana banana\r\nend\n";
    let mut f = Fixture::new(&[("source.rs", source.as_bytes())]);
    let response = json(&f.search.search(&f.atlas, 1, "ana", options(), || false).unwrap());
    let rows = response.get("hits").array();
    assert_eq!(rows.len(), 4);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.get("hit_id").number(), index as u64 + 1);
        assert_eq!(selected_text(row), "ana");
        let excerpt = row.get("excerpt");
        assert_eq!(excerpt.get("text").text(), &source[3..]);
        assert_eq!(bounds(excerpt.get("original_range")), (3, source.len()));
        assert!(!excerpt.get("clipped_start").flag());
        assert!(!excerpt.get("clipped_end").flag());
        assert!(!excerpt.get("has_replacements").flag());
        let (start, end) = bounds(row.get("original_range"));
        let (local_start, local_end) = bounds(excerpt.get("match_utf8_range"));
        assert_eq!((local_start + 3, local_end + 3), (start, end));
        assert_eq!(&source[start..end], "ana");
    }
    let first = bounds(rows[0].get("excerpt").get("match_utf8_range"));
    let second = bounds(rows[1].get("excerpt").get("match_utf8_range"));
    assert_eq!((second.0 - first.0, second.1 - first.1), (2, 2));
}

#[test]
fn utf16_endianness_and_lowercase_expansion_keep_actual_spelling() {
    let source = "first 😀\r\nİ\nmiddle İ\r\nlast";
    for little in [true, false] {
        let bytes = utf16(source, little);
        let mut f = Fixture::new(&[("source.rs", &bytes)]);
        let response = json(&f.search.search_scoped(&f.atlas, 1, "\u{0307}",
            AtlasSearchScope::all().with_case_sensitive(false), options(), || false).unwrap());
        let rows = response.get("hits").array();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(selected_text(row), "İ");
            assert_eq!(row.get("matched_text").text(), "İ");
            let excerpt = row.get("excerpt");
            assert_eq!(excerpt.get("text").text(), source);
            assert_eq!(bounds(excerpt.get("original_range")), (2, bytes.len()));
            assert!(!excerpt.get("clipped_start").flag());
            assert!(!excerpt.get("clipped_end").flag());
            let (start, end) = bounds(row.get("original_range"));
            assert_eq!(end - start, 2);
            assert_eq!(&bytes[start..end], if little { &[0x30, 0x01] } else { &[0x01, 0x30] });
        }
    }
}

#[test]
fn clipped_context_keeps_crlf_scalar_boundaries_and_the_entire_multiline_match() {
    let needle = "needle\r\n😀";
    // The requested context begins on LF inside the initial CRLF. The extent
    // decoder, not a second excerpt boundary implementation, retains the pair.
    let source = format!("x\r\n{}{needle}{}", "a".repeat(163), "β".repeat(200));
    let mut f = Fixture::new(&[("source.rs", source.as_bytes())]);
    let response = json(&f.search.search(&f.atlas, 1, needle, options(), || false).unwrap());
    let row = &response.get("hits").array()[0];
    assert_eq!(selected_text(row), needle);
    let excerpt = row.get("excerpt");
    let (start, end) = bounds(excerpt.get("original_range"));
    assert_eq!(start, 1);
    assert!(source.is_char_boundary(start) && source.is_char_boundary(end));
    assert_eq!(excerpt.get("text").text(), &source[start..end]);
    assert!(excerpt.get("text").text().starts_with("\r\n"));
    assert!(excerpt.get("clipped_start").flag());
    assert!(excerpt.get("clipped_end").flag());
}

#[test]
fn live_pages_and_activation_preserve_context_after_live_source_replacement() {
    let source = b"before\nneedle\nafter\n";
    let mut f = Fixture::new(&[("source.rs", source)]);
    let original = json(&f.search.search(&f.atlas, 1, "needle", options(), || false).unwrap());
    let expected = original.get("hits").array()[0].get("excerpt").clone();
    let before = f.search.progress(&f.atlas, 1).unwrap();
    fs::write(f.root.join("source.rs"), b"replacement contains no old source context").unwrap();
    let page = json(&f.search.page(&f.atlas, 1, 0, 1, || false).unwrap());
    assert_eq!(page.get("hits").array()[0].get("excerpt"), &expected);
    let (reader, activated) = f.search.open_reader(&f.atlas, owner(49101), 1, 1, || false).unwrap();
    assert_eq!(reader.capture().bytes(), source);
    let activated = json(&activated);
    assert_eq!(activated.get("excerpt"), &expected);
    assert!(!activated.get("source_reopened").flag());
    let after = f.search.progress(&f.atlas, 1).unwrap();
    assert_eq!(after.source_bytes_read, before.source_bytes_read);
    assert_eq!(after.read_calls, before.read_calls);
}

#[test]
fn captured_index_context_uses_retained_bytes_and_same_exact_mapping_as_live_search() {
    let source = "old context KNeedLE\nsecond line";
    let mut f = Fixture::new(&[("source.rs", source.as_bytes())]);
    let scope = AtlasSearchScope::all().with_case_sensitive(false);
    let live = json(&f.search.search_scoped(&f.atlas, 1, "kneedle", scope, options(), || false).unwrap());
    let expected = live.get("hits").array()[0].get("excerpt").clone();
    f.search.prepare_index(&f.atlas, 2, index_options(), || false).unwrap();
    fs::write(f.root.join("source.rs"), b"changed after index preparation").unwrap();
    let indexed = json(&f.search.search_indexed_scoped(&f.atlas, 3, 2, "kneedle", scope,
        100, 131072, || false).unwrap());
    let row = &indexed.get("hits").array()[0];
    assert_eq!(selected_text(row), "KNeedLE");
    assert_eq!(row.get("excerpt"), &expected);
    assert_eq!(indexed.get("source_bytes_read").number(), 0);
    assert_eq!(indexed.get("fallback_files").number(), 1);
}

#[test]
fn progressive_results_keep_previous_excerpt_and_append_later_occurrences() {
    let mut f = Fixture::new(&[("a.rs", b"first needle context"), ("b.rs", b"second needle context")]);
    f.search.begin(&f.atlas, 1, "needle", options(), || false).unwrap();
    let first = json(&f.search.step(&f.atlas, 1, || false).unwrap());
    assert!(first.get("search_in_progress").flag());
    assert_eq!(first.get("hits").array().len(), 1);
    let row = first.get("hits").array()[0].clone();
    assert_eq!(selected_text(&row), "needle");
    let done = f.finish(1);
    assert_eq!(done.get("hits").array().len(), 2);
    assert_eq!(&done.get("hits").array()[0], &row);
    assert_eq!(selected_text(&done.get("hits").array()[1]), "needle");
}

#[test]
fn structural_excerpts_use_exact_source_spans_including_markdown_markup() {
    let heading = b"\xef\xbb\xbf# Deploy!\r\n\r\nbody\r\n";
    let symbol = b"// nearby context\nfn launch() {}\n";
    for (name, source, needle, mode) in [
        ("README.md", heading.as_slice(), "Deploy!", AtlasStructureMode::Headings),
        ("main.rs", symbol.as_slice(), "launch", AtlasStructureMode::Symbols),
    ] {
        let mut f = Fixture::new(&[(name, source)]);
        let settings = AtlasStructureOptions { mode, name_mode: SymbolNameMode::Exact, limits: options() };
        f.search.begin_structure(&f.atlas, 1, needle, AtlasSearchScope::all(), settings, || false).unwrap();
        let result = f.finish(1);
        assert_eq!(result.get("hits").array().len(), 1);
        let row = &result.get("hits").array()[0];
        let (start, end) = bounds(row.get("original_range"));
        assert_eq!(selected_text(row).as_bytes(), &source[start..end]);
        assert_eq!(row.get("structure").get("name").text(), needle);
        if mode == AtlasStructureMode::Headings {
            assert!(selected_text(row).starts_with("# Deploy!"));
            assert_ne!(selected_text(row), needle);
            assert_eq!(row.get("excerpt").get("original_range").get("start").number(), 3);
            assert!(!row.get("excerpt").get("clipped_start").flag());
        }
    }
}

#[test]
fn largest_native_lowercase_query_keeps_full_wider_original_match() {
    let needle = "k".repeat(1024);
    let spelling = "K".repeat(1024);
    let source = format!("before {spelling} after");
    let mut f = Fixture::new(&[("source.rs", source.as_bytes())]);
    let result = json(&f.search.search_scoped(&f.atlas, 1, &needle,
        AtlasSearchScope::all().with_case_sensitive(false), options(), || false).unwrap());
    assert_eq!(result.get("hits").array().len(), 1);
    let row = &result.get("hits").array()[0];
    assert_eq!(selected_text(row), spelling);
    assert_eq!(row.get("excerpt").get("text").text(), source);
}
