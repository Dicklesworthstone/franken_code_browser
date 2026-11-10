#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production project captures -> documentation backlinks -> independent
//! exact readers. The code-first campaign authors these regression scenarios;
//! the strict RCH verifier owns execution and native qualification.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::document::links::MAX_DOCUMENT_LINK_SOURCE_BYTES;
use fcb::store::Sha256;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{RetainedAtlasSearch, AtlasIndexOptions, AtlasSearchError as Error,
        AtlasSearchScope, AtlasSearchPathKind as PathKind, AtlasRelationshipDirection as Direction,
        AtlasRelationshipSide as Side, MAX_ATLAS_RELATIONSHIPS, MAX_ATLAS_RELATIONSHIP_PARSE_BYTES,
        MAX_ATLAS_RELATIONSHIP_METADATA_BYTES}};
use support::{Json, parse};

struct Fixture { root: PathBuf, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(58301);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-atlas-links-{}-{id}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            let path = root.join(name); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
        }
        let atlas = AtlasSession::open(owner(id), &root, AtlasSessionOptions::default(), || false).unwrap();
        let search = RetainedAtlasSearch::new(&atlas).unwrap();
        Self { root, atlas, search }
    }
    fn build(&mut self, generation: u64) {
        self.search.prepare_index(&self.atlas, generation, index_options(), || false).unwrap();
    }
    fn begin(&mut self, generation: u64, index: u64, limit: usize, bytes: u64) -> Json {
        json(&self.search.begin_relationships(&self.atlas, generation, index, limit, bytes, || false).unwrap())
    }
    fn finish(&mut self, generation: u64) -> Json {
        for _ in 0..102 {
            let response = self.search.step_relationships(&self.atlas, generation, || false).unwrap();
            let page = json(&response); coverage(&page);
            assert!(page.get("last_step_files").number() <= 1);
            if !page.get("relationships_in_progress").flag() {
                assert_eq!(response.exit_code(), if page.get("relationships_complete").flag() { EXIT_OK } else { EXIT_PARTIAL });
                return page;
            }
        }
        panic!("bounded relationship fixture did not terminate");
    }
    fn prepare(&mut self, generation: u64, index: u64) -> Json {
        self.begin(generation, index, MAX_ATLAS_RELATIONSHIPS, MAX_ATLAS_RELATIONSHIP_PARSE_BYTES);
        self.finish(generation)
    }
    fn page(&mut self, generation: u64, direction: Direction, path: Option<&[u8]>, start: usize, limit: usize) -> Json {
        json(&self.search.relationships_page(&self.atlas, generation, direction, path, start, limit, || false).unwrap())
    }
}
// Fixture source files remain available for independent replay. No deletion is
// needed to establish source replacement, stale index refusal or reader pins.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 1024 * 1024,
        max_source_bytes: 2 * 1024 * 1024, max_index_grams: 0 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn ids(page: &Json) -> Vec<u64> { page.get("links").array().iter().map(|link| link.get("link_id").number()).collect() }
fn whole(range: &Json, source: &[u8]) {
    assert_eq!(range.get("start").number(), 0); assert_eq!(range.get("end").number(), source.len() as u64);
}
fn coverage(page: &Json) {
    assert_eq!(page.get("schema").text(), "fcb.atlas-links/1");
    assert_eq!(page.get("source_observation").text(), "retained-index-captures");
    assert_eq!(page.get("evidence_precision").text(), "whole-captured-markdown-document");
    assert_eq!(page.get("markdown_files").number(), page.get("captured_markdown_files").number()
        + page.get("capture_unavailable_markdown_files").number() + page.get("outside_capture_scope_markdown_files").number()
        + page.get("uncaptured_markdown_files").number());
    assert_eq!(page.get("captured_markdown_files").number(), page.get("examined_markdown_files").number()
        + page.get("pending_markdown_files").number());
    assert_eq!(page.get("examined_markdown_files").number(), page.get("scanned_markdown_files").number()
        + page.get("unsupported_markdown_files").number() + page.get("source_limited_markdown_files").number());
    assert_eq!(page.get("source_bytes_read").number(), 0); assert_eq!(page.get("read_calls").number(), 0);
    assert!(page.get("metadata_bytes").number() <= MAX_ATLAS_RELATIONSHIP_METADATA_BYTES as u64);
    assert!(page.get("parsed_source_bytes").number() <= page.get("max_parse_bytes").number());
}

#[test]
fn exact_paths_occurrence_multiplicity_and_filtered_pages_share_one_project_inventory() {
    let readme = b"# Overview\n\n[implementation](src/a%20b.rs#L2) [again](src/a%20b.rs)\n\n[once](src/a%2520b.rs)\n\n[missing](src/missing.rs)\n\n[network](https://example.invalid/never-fetch)\n\n[escape](../outside.rs)\n\n[anchor](#overview)\n";
    let guide = b"[readme](../README.md#overview)\n\n[code](../src/a%20b.rs)\n";
    let mut f = Fixture::new(&[("README.md", readme), ("docs/guide.md", guide),
        ("src/a b.rs", b"captured code"), ("src/a%20b.rs", b"percent filename")]);
    f.build(1);
    let initial = f.begin(1, 1, 64, 1024 * 1024); coverage(&initial);
    assert_eq!(initial.get("parsed_source_bytes").number(), 0);
    assert_eq!(initial.get("hashed_source_bytes").number(), 0);
    assert_eq!(initial.get("retained_links").number(), 0);
    let result = f.finish(1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("retained_links").number(), 9);
    assert_eq!(result.get("links_seen").number(), 9);
    assert_eq!(result.get("parsed_source_bytes").number(), (readme.len() + guide.len()) as u64);
    assert_eq!(result.get("hashed_source_bytes").number(), f.search.indexed_source_bytes());
    let links = result.get("links").array();
    for (ordinal, link) in links.iter().enumerate() {
        assert_eq!(link.get("link_id").number(), ordinal as u64 + 1);
        assert_eq!(link.get("source_revision").number(), 1);
        whole(link.get("evidence_range"), if ordinal < 7 { readme.as_slice() } else { guide.as_slice() });
    }
    assert_eq!(links[0].get("target_path").get("hex").text(), hex(b"src/a b.rs"));
    assert_eq!(links[0].get("destination_suffix").text(), "#L2");
    assert_eq!(links[0].get("fragment_resolution").text(), "unqualified");
    assert_eq!(links[0].get("source_sha256").text(), Sha256::digest(readme).to_hex());
    assert_eq!(links[1].get("source_link_id").number(), 2);
    assert_eq!(links[2].get("target_path").get("hex").text(), hex(b"src/a%20b.rs"));
    for (position, resolution) in [(3, "missing"), (4, "external"), (5, "unresolved")] {
        assert_eq!(links[position].get("resolution").text(), resolution);
        assert_eq!(links[position].get("target_file_id"), &Json::Null);
    }
    assert_eq!(links[6].get("target_file_id"), links[6].get("source_file_id"));
    assert_eq!(links[6].get("fragment_resolution").text(), "unqualified");
    let first = f.page(1, Direction::Incoming, Some(b"src/a b.rs"), 0, 2);
    assert_eq!(first.get("matching_links").number(), 3); assert_eq!(ids(&first), [1, 2]);
    assert_eq!(first.get("next_offset").number(), 2);
    let last = f.page(1, Direction::Incoming, Some(b"src/a b.rs"), 2, 2);
    assert_eq!(ids(&last), [9]); assert_eq!(last.get("next_offset"), &Json::Null);
    assert_eq!(last.get("hashed_source_bytes"), result.get("hashed_source_bytes"));
    let outgoing = f.page(1, Direction::Outgoing, Some(b"docs/guide.md"), 0, 64);
    assert_eq!(ids(&outgoing), [8, 9]);
    let either = f.page(1, Direction::All, Some(b"README.md"), 0, 64);
    assert_eq!(either.get("matching_links").number(), 8);
    assert_eq!(either.get("filter_path").get("hex").text(), hex(b"README.md"));
}

#[test]
fn original_markdown_bom_and_utf16_targets_survive_live_source_replacement_and_index_clear() {
    let doc = b"\xef\xbb\xbf---\ntitle: Original\n---\n\n[code](src/lib.rs#entry)\n";
    let code: Vec<u8> = "\u{feff}fn original() {}\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut f = Fixture::new(&[("README.md", doc), ("src/lib.rs", &code)]);
    f.build(1); f.prepare(1, 1);
    fs::write(f.root.join("README.md"), b"changed documentation").unwrap();
    fs::write(f.root.join("src/lib.rs"), b"changed implementation").unwrap();
    let (mut evidence, receipt) = f.search.open_relationship_reader(&f.atlas, owner(59901), 1, 1, Side::Evidence, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(doc).to_hex());
    assert_eq!(receipt.get("reader").get("capture_sha256").text(), Sha256::digest(doc).to_hex());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    assert!(!receipt.get("source_reopened").flag());
    let (mut target, receipt) = f.search.open_relationship_reader(&f.atlas, owner(59902), 1, 1, Side::Target, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_bytes").number(), code.len() as u64);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(&code).to_hex());
    assert_eq!(receipt.get("fragment_resolution").text(), "unqualified");
    assert_ne!(evidence.capture().request().file().owner(), target.capture().request().file().owner());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::All, None, 0, 64, || false).err(), Some(Error::MissingIndex));
    drop(f.search);
    assert_eq!(json(&evidence.copy_range(0, doc.len() as u64, || false).unwrap()).get("original_hex").text(), hex(doc));
    assert_eq!(json(&target.copy_range(0, code.len() as u64, || false).unwrap()).get("original_hex").text(), hex(&code));
}

#[test]
fn target_heading_uses_retained_original_bytes_without_consuming_preview_state() {
    use fcb_app::host::reader::ReaderDocumentOptions;
    let doc = b"[guide](guide.MARKDOWN?view=source#real-%68eading)\n";
    let guide = b"\xef\xbb\xbfpreamble\r\n\r\n## Real Heading\r\n\r\nCaptured body.\r\n";
    let expected = guide.windows(b"## Real Heading".len()).position(|bytes| bytes == b"## Real Heading").unwrap();
    let mut f = Fixture::new(&[("README.md", doc), ("guide.MARKDOWN", guide)]);
    f.build(1); let page = f.prepare(1, 1);
    // Building/listing the graph must not eagerly parse every target fragment.
    assert_eq!(page.get("links").array()[0].get("fragment_resolution").text(), "unqualified");
    fs::write(f.root.join("guide.MARKDOWN"), b"# Replacement\n\nNo old heading.\n").unwrap();
    let (mut reader, response) = f.search.open_relationship_reader(&f.atlas, owner(59801), 1, 1, Side::Target, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("fragment_status").text(), "resolved");
    assert_eq!(receipt.get("fragment_suffix").text(), "#real-%68eading");
    assert_eq!(receipt.get("fragment_slug").text(), "real-heading");
    assert_eq!(receipt.get("destination_suffix").text(), "?view=source#real-%68eading");
    let range = receipt.get("target_range");
    assert_eq!(range.get("start").number(), expected as u64);
    assert_eq!(receipt.get("initial_original_offset").number(), expected as u64);
    assert_eq!(std::str::from_utf8(&guide[expected..range.get("end").number() as usize]).unwrap().trim_end(), "## Real Heading");
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(guide).to_hex());
    assert!(!receipt.get("source_reopened").flag());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    assert_eq!(receipt.get("reader").get("accepted_document_generation"), &Json::Null);
    assert_eq!(reader.document_generation(), None);
    assert_eq!(reader.document_links_generation(), None);
    assert_eq!(reader.capture().bytes(), guide);
    let (_, evidence) = f.search.open_relationship_reader(&f.atlas, owner(59802), 1, 1, Side::Evidence, || false).unwrap();
    let evidence = json(&evidence);
    assert_eq!(evidence.get("fragment_status").text(), "none");
    assert_eq!(evidence.get("fragment_suffix").text(), "");
    assert_eq!(evidence.get("target_range"), &Json::Null);
    assert_eq!(evidence.get("initial_original_offset").number(), 0);
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    // The independent reader and its first preview remain usable after index retirement.
    reader.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap();
    assert_eq!(reader.document_generation(), Some(1));
    assert_eq!(json(&reader.copy_range(expected as u64, range.get("end").number(), || false).unwrap())
        .get("original_hex").text(), hex(&guide[expected..range.get("end").number() as usize]));
}

#[test]
fn unqualified_target_fragments_keep_exact_file_reading_and_distinct_fallbacks() {
    let doc = b"[ok](guide.md#repeat) [collision](guide.md#repeat-2) [missing](guide.md#absent) \
        [once](guide.md#%2572epeat) [case](guide.md#Repeat) [invalid](guide.md#%ff) \
        [zero](guide.md#%00) [top](guide.md#) [code](code.rs#L2) [query](guide.md?plain=1)\n";
    let guide = b"# Repeat\n\n# Repeat\n\n# Repeat-2\n\n";
    let mut f = Fixture::new(&[("README.md", doc), ("guide.md", guide), ("code.rs", b"first\nsecond\n")]);
    f.build(1); f.prepare(1, 1);
    let expected = [
        ("resolved", Some("repeat")), ("ambiguous", Some("repeat-2")),
        ("missing-heading", Some("absent")), ("missing-heading", Some("%72epeat")),
        ("missing-heading", Some("Repeat")), ("invalid", None), ("invalid", None),
        ("document-start", None), ("unsupported", Some("L2")), ("none", None),
    ];
    for (ordinal, (status, slug)) in expected.iter().enumerate() {
        let (reader, response) = f.search.open_relationship_reader(&f.atlas, owner(59810 + ordinal as u64),
            1, ordinal as u64 + 1, Side::Target, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("fragment_status").text(), *status, "link {}", ordinal + 1);
        match slug {
            Some(slug) => assert_eq!(receipt.get("fragment_slug").text(), *slug),
            None => assert_eq!(receipt.get("fragment_slug"), &Json::Null),
        }
        assert_eq!(receipt.get("initial_original_offset").number(), 0);
        if *status != "resolved" { assert_eq!(receipt.get("target_range"), &Json::Null); }
        assert_eq!(reader.document_generation(), None);
        assert_eq!(reader.capture().bytes(), if ordinal == 8 { b"first\nsecond\n".as_slice() } else { guide.as_slice() });
    }
}

#[test]
fn heading_limits_do_not_turn_unqualified_source_into_a_missing_heading() {
    for (ordinal, (guide, status)) in [
        (vec![b'x'; MAX_DOCUMENT_LINK_SOURCE_BYTES + 1], "limited"),
        (b"\xff\xfe#\0 \0T\0a\0r\0g\0e\0t\0".to_vec(), "unsupported"),
        (b"\xff".to_vec(), "unsupported"),
        (b"> # Target\n".to_vec(), "unsupported"),
    ].into_iter().enumerate() {
        let mut f = Fixture::new(&[("README.md", b"[target](guide.md#absent)"), ("guide.md", &guide)]);
        f.build(1); f.prepare(1, 1);
        let (reader, response) = f.search.open_relationship_reader(&f.atlas, owner(59830 + ordinal as u64),
            1, 1, Side::Target, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("fragment_status").text(), status);
        assert_eq!(receipt.get("target_range"), &Json::Null);
        assert_eq!(receipt.get("initial_original_offset").number(), 0);
        assert_eq!(reader.capture().bytes(), guide.as_slice());
    }
}

#[test]
fn cancellation_during_selected_target_qualification_preserves_the_graph() {
    let doc = b"[plain](guide.md) [heading](guide.md#target)\n";
    let guide = format!("{}# Target\n", "# Other\n\n".repeat(100));
    let mut f = Fixture::new(&[("README.md", doc), ("guide.md", guide.as_bytes())]);
    f.build(1); f.prepare(1, 1);
    let mut plain_checks = 0;
    f.search.open_relationship_reader(&f.atlas, owner(59840), 1, 1, Side::Target, || {
        plain_checks += 1; false
    }).unwrap();
    let mut heading_checks = 0;
    assert!(f.search.open_relationship_reader(&f.atlas, owner(59841), 1, 2, Side::Target, || {
        heading_checks += 1; heading_checks > plain_checks
    }).is_err());
    assert!(heading_checks > plain_checks, "Negative control must reach extra heading qualification work");
    let (_, response) = f.search.open_relationship_reader(&f.atlas, owner(59842), 1, 2, Side::Target, || false).unwrap();
    assert_eq!(json(&response).get("fragment_status").text(), "resolved");
    assert!(f.page(1, Direction::All, None, 0, 64).get("relationships_complete").flag());
}

#[test]
fn narrower_capture_scope_cannot_claim_no_backlinks_from_excluded_markdown() {
    let mut f = Fixture::new(&[("README.md", b"[code](src/lib.rs)"),
        ("docs/extra.md", b"[code](../src/lib.rs)"), ("src/lib.rs", b"source")]);
    let only_readme = AtlasSearchScope::all().with_path(PathKind::File, "README.md").unwrap();
    f.search.prepare_index_scoped(&f.atlas, 1, only_readme, index_options(), || false).unwrap();
    let result = f.prepare(1, 1);
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("markdown_files").number(), 2);
    assert_eq!(result.get("captured_markdown_files").number(), 1);
    assert_eq!(result.get("outside_capture_scope_markdown_files").number(), 1);
    assert_eq!(result.get("pending_markdown_files").number(), 0);
    let backlinks = f.page(1, Direction::Incoming, Some(b"src/lib.rs"), 0, 64);
    assert_eq!(backlinks.get("matching_links").number(), 1);
    assert!(!backlinks.get("relationships_complete").flag());
    assert_eq!(backlinks.get("links").array()[0].get("resolution").text(), "outside-capture-scope");
    assert_eq!(f.search.open_relationship_reader(&f.atlas, owner(59903), 1, 1, Side::Target, || false).err(),
        Some(Error::RelationshipTargetUnavailable));
    // Capturing every Markdown member proves the link inventory even when a
    // non-Markdown target has not been captured; target opening stays refused.
    let docs = AtlasSearchScope::from_csv(Some("md"), None).unwrap();
    f.search.prepare_index_scoped(&f.atlas, 2, docs, index_options(), || false).unwrap();
    let all_docs = f.prepare(2, 2);
    assert!(all_docs.get("relationships_complete").flag());
    assert_eq!(all_docs.get("retained_links").number(), 2);
    assert_eq!(all_docs.get("outside_capture_scope_markdown_files").number(), 0);
}

#[test]
fn uncaptured_and_unavailable_markdown_have_distinct_coverage_counts() {
    let large = b"x".repeat(40);
    let mut f = Fixture::new(&[("a.md", b""), ("b.md", &large), ("c.md", b"[a](a.md)")]);
    f.search.prepare_index(&f.atlas, 1, AtlasIndexOptions { max_files: 1, ..index_options() }, || false).unwrap();
    let pending = f.prepare(1, 1);
    assert_eq!(pending.get("captured_markdown_files").number(), 1);
    assert_eq!(pending.get("uncaptured_markdown_files").number(), 2);
    assert_eq!(pending.get("capture_unavailable_markdown_files").number(), 0);
    assert!(!pending.get("relationships_complete").flag());
    f.search.prepare_index(&f.atlas, 2, AtlasIndexOptions { max_file_bytes: 16, ..index_options() }, || false).unwrap();
    let unavailable = f.prepare(2, 2);
    assert_eq!(unavailable.get("captured_markdown_files").number(), 2);
    assert_eq!(unavailable.get("uncaptured_markdown_files").number(), 0);
    assert_eq!(unavailable.get("capture_unavailable_markdown_files").number(), 1);
    assert!(!unavailable.get("relationships_complete").flag());
}

#[test]
fn unsupported_utf16_malformed_names_and_oversized_docs_are_visible_partial_coverage() {
    let utf16: Vec<u8> = "\u{feff}[code](b.rs)".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let oversized = vec![b' '; MAX_DOCUMENT_LINK_SOURCE_BYTES + 1];
    let doc = b"[code](b.rs)";
    let mut f = Fixture::new(&[("a.md", doc), ("b.rs", b"fn code() {}"), ("c.md", &utf16),
        ("d.md", b"\xff"), ("e.md", &oversized), ("f.MARKDOWN", b""), ("g\\bad.md", doc)]);
    f.build(1);
    let result = f.prepare(1, 1);
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("markdown_files").number(), 6);
    assert_eq!(result.get("scanned_markdown_files").number(), 2);
    assert_eq!(result.get("unsupported_markdown_files").number(), 3);
    assert_eq!(result.get("source_limited_markdown_files").number(), 1);
    assert_eq!(result.get("parsed_source_bytes").number(), (doc.len() + utf16.len() + 1) as u64);
    assert_eq!(result.get("retained_links").number(), 1);
}

#[test]
fn parse_budget_never_truncates_a_capture_into_successful_absence() {
    let mut f = Fixture::new(&[("a.md", b""), ("b.md", b"[a](a.md)")]);
    f.build(1);
    f.begin(1, 1, 64, 0);
    let first = json(&f.search.step_relationships(&f.atlas, 1, || false).unwrap());
    assert!(first.get("relationships_in_progress").flag());
    assert_eq!(first.get("last_step_files").number(), 1);
    assert_eq!(first.get("parsed_source_bytes").number(), 0);
    let result = f.finish(1);
    assert_eq!(result.get("stop_reason").text(), "parse-byte-limit");
    assert_eq!(result.get("pending_markdown_files").number(), 1);
    assert_eq!(result.get("last_step_files").number(), 0);
    assert_eq!(result.get("retained_links").number(), 0);
    assert!(!result.get("relationships_complete").flag());
    f.begin(2, 1, 64, b"[a](a.md)".len() as u64);
    let exact = f.finish(2);
    assert!(exact.get("relationships_complete").flag());
    assert_eq!(exact.get("retained_links").number(), 1);
}

#[test]
fn link_limit_counts_all_scanned_occurrences_without_hiding_omissions() {
    let doc = b"[a](code.rs) [b](code.rs) [c](code.rs)";
    let mut f = Fixture::new(&[("a.md", doc), ("b.md", doc), ("code.rs", b"code")]);
    f.build(1); f.begin(1, 1, 2, 1024);
    let result = f.finish(1);
    assert_eq!(result.get("scanned_markdown_files").number(), 2);
    assert_eq!(result.get("links_seen").number(), 6);
    assert_eq!(result.get("retained_links").number(), 2);
    assert!(result.get("truncated").flag()); assert!(!result.get("relationships_complete").flag());
    assert_eq!(ids(&result), [1, 2]);
    let empty = f.page(1, Direction::All, None, 2, 64);
    assert!(empty.get("links").array().is_empty());
    assert_eq!(empty.get("next_offset"), &Json::Null);
    let complete = f.prepare(2, 1);
    assert_eq!(complete.get("retained_links").number(), 6);
    assert!(complete.get("relationships_complete").flag());
}

#[test]
fn metadata_quota_is_independent_of_link_count_and_parsed_source_budget() {
    let destination = format!("https://example.invalid/{}", "x".repeat(4000));
    let document = format!("[long]({destination})\n").repeat(50);
    assert!(document.len() < MAX_DOCUMENT_LINK_SOURCE_BYTES);
    let names: Vec<_> = (0..50).map(|id| format!("doc-{id:02}.md")).collect();
    let files: Vec<_> = names.iter().map(|name| (name.as_str(), document.as_bytes())).collect();
    let mut f = Fixture::new(&files);
    f.search.prepare_index(&f.atlas, 1, AtlasIndexOptions { max_source_bytes: 12 * 1024 * 1024,
        ..index_options() }, || false).unwrap();
    let result = f.prepare(1, 1);
    assert_eq!(result.get("scanned_markdown_files").number(), 50);
    assert_eq!(result.get("links_seen").number(), 2500);
    assert!(result.get("retained_links").number() < 2500);
    assert!(result.get("retained_links").number() > 0);
    assert!(result.get("truncated").flag()); assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("pending_markdown_files").number(), 0);
    assert!(result.get("metadata_bytes").number() <= MAX_ATLAS_RELATIONSHIP_METADATA_BYTES as u64);
}

#[test]
fn index_replacement_makes_old_relationships_stale_without_retargeting_open_readers() {
    let original = b"[old](old.rs)";
    let mut f = Fixture::new(&[("README.md", original), ("old.rs", b"old bytes"), ("new.rs", b"new bytes")]);
    f.build(1); f.prepare(1, 1);
    let (reader, _) = f.search.open_relationship_reader(&f.atlas, owner(59904), 1, 1, Side::Target, || false).unwrap();
    f.begin(2, 1, 64, 1024);
    fs::write(f.root.join("README.md"), b"[new](new.rs)").unwrap();
    f.build(2);
    assert_eq!(f.search.pending_relationship_generation(), None);
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::All, None, 0, 64, || false).err(), Some(Error::StaleIndex));
    assert_eq!(f.search.open_relationship_reader(&f.atlas, owner(59905), 1, 1, Side::Target, || false).err(), Some(Error::StaleIndex));
    assert_eq!(reader.capture().bytes(), b"old bytes");
    let replacement = f.prepare(3, 2);
    assert_eq!(replacement.get("links").array()[0].get("target_path").get("hex").text(), hex(b"new.rs"));
    assert_eq!(replacement.get("links").array()[0].get("source_revision").number(), 2);
}

#[test]
fn final_cancellation_and_failed_attempts_preserve_accepted_links_and_search_results() {
    let mut f = Fixture::new(&[("README.md", b"[needle](a.rs)"), ("a.rs", b"needle")]);
    f.build(1); f.prepare(1, 1);
    f.search.search_indexed(&f.atlas, 2, 1, "needle", 10, 1024, || false).unwrap();
    f.begin(2, 1, 64, 1024);
    let mut polls = 0;
    f.search.step_relationships(&f.atlas, 2, || { polls += 1; false }).unwrap();
    assert!(polls > 1);
    f.begin(3, 1, 64, 1024);
    let mut canceled_polls = 0;
    assert_eq!(f.search.step_relationships(&f.atlas, 3, || { canceled_polls += 1; canceled_polls == polls }).err(), Some(Error::Canceled));
    assert_eq!(f.search.relationship_generation(), Some(2));
    assert_eq!(f.search.pending_relationship_generation(), None);
    assert_eq!(f.search.accepted_generation(), Some(2));
    assert_eq!(f.page(2, Direction::All, None, 0, 64).get("retained_links").number(), 1);
    assert_eq!(f.search.begin_relationships(&f.atlas, 3, 1, 64, 1024, || false).err(), Some(Error::StaleRelationships));
    assert_eq!(f.search.begin_relationships(&f.atlas, 4, 999, 64, 1024, || false).err(), Some(Error::StaleIndex));
    assert_eq!(f.search.begin_relationships(&f.atlas, 5, 1, 0, 1024, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.begin_relationships(&f.atlas, 6, 1, 64, MAX_ATLAS_RELATIONSHIP_PARSE_BYTES + 1, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.relationship_generation(), Some(2));
    assert_eq!(json(&f.search.page(&f.atlas, 2, 0, 64, || false).unwrap()).get("retained_hits").number(), 2);
}

#[test]
fn canonically_equivalent_path_spellings_do_not_alias_a_catalog_member() {
    let doc = "[one](src/é.rs) [two](src/e\u{301}.rs)";
    let mut f = Fixture::new(&[("README.md", doc.as_bytes()), ("src/é.rs", b"captured code")]);
    // Some Mac filesystems cannot store both canonical spellings at once.
    // Use the actual catalogued bytes and prove that the other spelling does
    // not silently acquire its source identity through native normalization.
    let path = f.atlas.atlas().catalog().entries().iter()
        .find(|entry| entry.path().raw().as_bytes().ends_with(b".rs")).unwrap()
        .path().raw().as_bytes().to_vec();
    let composed = "src/é.rs".as_bytes();
    let decomposed = "src/e\u{301}.rs".as_bytes();
    assert!(path == composed || path == decomposed);
    let (expected, other) = if path == composed { (1, decomposed) } else { (2, composed) };
    f.build(1); f.prepare(1, 1);
    let selected = f.page(1, Direction::Incoming, Some(&path), 0, 64);
    assert_eq!(ids(&selected), [expected]);
    assert_eq!(selected.get("filter_path").get("hex").text(), hex(&path));
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::Incoming, Some(other), 0, 64, || false).err(),
        Some(Error::MissingRelationshipPath));
    let all = f.page(1, Direction::All, None, 0, 64);
    for link in all.get("links").array() {
        assert_eq!(link.get("resolution").text(),
            if link.get("link_id").number() == expected { "captured" } else { "missing" });
    }
}

#[test]
fn missing_filter_and_foreign_atlas_cannot_be_reported_as_empty_successful_results() {
    let mut f = Fixture::new(&[("README.md", b"[a](a.rs)"), ("a.rs", b"source")]);
    f.build(1); f.prepare(1, 1);
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::Incoming, Some(b"missing.rs"), 0, 64, || false).err(),
        Some(Error::MissingRelationshipPath));
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::Incoming, None, 0, 64, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::All, Some(b""), 0, 64, || false).err(), Some(Error::InvalidScope));
    assert_eq!(f.search.relationships_page(&f.atlas, 1, Direction::All, None, 2, 64, || false).err(), Some(Error::InvalidLimits));
    let other = Fixture::new(&[("README.md", b"[a](a.rs)"), ("a.rs", b"source")]);
    assert_eq!(f.search.relationships_page(&other.atlas, 1, Direction::All, None, 0, 64, || false).err(), Some(Error::WrongAtlas));
    assert_eq!(f.search.open_relationship_reader(&other.atlas, owner(59906), 1, 1, Side::Target, || false).err(), Some(Error::WrongAtlas));
    assert!(f.page(1, Direction::All, None, 0, 64).get("relationships_complete").flag());
}

#[test]
fn an_empty_captured_document_universe_still_qualifies_missing_visible_membership() {
    let mut f = Fixture::new(&[("code.rs", b"source")]);
    f.build(1);
    let complete = f.prepare(1, 1);
    assert!(complete.get("relationships_complete").flag());
    assert_eq!(complete.get("markdown_files").number(), 0);
    assert_eq!(complete.get("last_step_files").number(), 0);
    assert_eq!(complete.get("hashed_source_bytes").number(), 0);
    let missing = AtlasSearchScope::all().with_path(PathKind::Visible, &hex(b"missing.md")).unwrap();
    f.search.prepare_index_scoped(&f.atlas, 2, missing, index_options(), || false).unwrap();
    let incomplete = f.prepare(2, 2);
    assert!(!incomplete.get("capture_region_complete").flag());
    assert!(!incomplete.get("relationships_complete").flag());
    assert_eq!(incomplete.get("markdown_files").number(), 0);
    assert_eq!(incomplete.get("stop_reason").text(), "all-markdown-examined");
}
