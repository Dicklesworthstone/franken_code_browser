#![forbid(unsafe_code)]
#![cfg(unix)]

//! Captured project sources -> lexical import candidates -> independent exact
//! readers. Scenarios are authored for the code-first verification batch; no
//! compiler or package-resolution completeness is implied by the inventory.
mod support;

use std::{ffi::OsStr, fs, os::unix::ffi::OsStrExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
use fcb::store::Sha256;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{RetainedAtlasSearch, AtlasIndexOptions, AtlasSearchError as Error,
        AtlasSearchScope, AtlasSearchPathKind as PathKind, AtlasImportDirection as Direction,
        AtlasImportSide as Side, AtlasRelationshipDirection,
        MAX_ATLAS_IMPORTS, MAX_ATLAS_IMPORT_PARSE_BYTES, MAX_ATLAS_IMPORT_METADATA_BYTES,
        MAX_ATLAS_IMPORT_SOURCE_BYTES}};
use support::{Json, parse};

struct Fixture { root: PathBuf, owner: ArenaOwnerId, atlas: AtlasSession, search: RetainedAtlasSearch }
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        let raw: Vec<_> = files.iter().map(|(name, bytes)| (name.as_bytes(), *bytes)).collect();
        Self::raw(&raw)
    }
    fn raw(files: &[(&[u8], &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(72101);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fcb-atlas-imports-{}-{id}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            let path = root.join(OsStr::from_bytes(name));
            fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
        }
        let owner = owner(id);
        let atlas = AtlasSession::open(owner, &root, AtlasSessionOptions::default(), || false).unwrap();
        let search = RetainedAtlasSearch::new(&atlas).unwrap();
        Self { root, owner, atlas, search }
    }
    fn build(&mut self, generation: u64) {
        self.search.prepare_index(&self.atlas, generation, index_options(), || false).unwrap();
    }
    fn begin(&mut self, generation: u64, index: u64, max_links: usize, bytes: u64) -> Json {
        json(&self.search.begin_imports(&self.atlas, generation, index, max_links, bytes, || false).unwrap())
    }
    fn finish(&mut self, generation: u64) -> Json {
        for _ in 0..102 {
            let response = self.search.step_imports(&self.atlas, generation, || false).unwrap();
            let page = json(&response); coverage(&page);
            assert!(page.get("last_step_files").number() <= 1);
            if !page.get("relationships_in_progress").flag() {
                assert_eq!(response.exit_code(), if page.get("relationships_complete").flag() { EXIT_OK } else { EXIT_PARTIAL });
                return page;
            }
        }
        panic!("bounded import fixture did not terminate");
    }
    fn prepare(&mut self, generation: u64, index: u64) -> Json {
        self.begin(generation, index, MAX_ATLAS_IMPORTS, MAX_ATLAS_IMPORT_PARSE_BYTES);
        self.finish(generation)
    }
    fn page(&mut self, generation: u64, direction: Direction, path: Option<&[u8]>, start: usize, limit: usize) -> Json {
        json(&self.search.imports_page(&self.atlas, generation, direction, path, start, limit, || false).unwrap())
    }
}
// Source fixtures remain available for independent replay. No deletion is needed
// to establish stale inventory refusal, exact reader pins or source replacement.
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn index_options() -> AtlasIndexOptions {
    AtlasIndexOptions { max_files: 100, max_file_bytes: 1024 * 1024,
        max_source_bytes: 2 * 1024 * 1024, max_index_grams: 0 }
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn ids(page: &Json) -> Vec<u64> { page.get("links").array().iter().map(|link| link.get("link_id").number()).collect() }
fn link<'a>(page: &'a Json, path: &[u8], destination: &str) -> &'a Json {
    let path = hex(path);
    page.get("links").array().iter().find(|link| link.get("source_path").get("hex").text() == path
        && link.get("destination").text() == destination).unwrap()
}
fn range<'a>(value: &Json, source: &'a [u8]) -> &'a [u8] {
    &source[value.get("start").number() as usize..value.get("end").number() as usize]
}
fn coverage(page: &Json) {
    assert_eq!(page.get("schema").text(), "fcb.atlas-imports/1");
    assert_eq!(page.get("projection").text(), "lexical-source-imports");
    assert_eq!(page.get("source_observation").text(), "retained-index-captures");
    assert_eq!(page.get("evidence_precision").text(), "exact-import-source-range");
    assert_eq!(page.get("relationship_kind").text(), "lexical-import");
    assert_eq!(page.get("resolution_domain").text(), "declared-catalog-candidates");
    assert_eq!(page.get("extractor").text(), "franken-markdown-d6fbd1d5/import-statements-v2");
    assert_eq!(page.get("extraction_policy").text(), "rust-mod-use-js-static-and-python-import-v2");
    assert!(!page.get("compiler_resolved").flag()); assert!(!page.get("semantic_complete").flag());
    assert_eq!(page.get("source_files").number(), page.get("captured_source_files").number()
        + page.get("capture_unavailable_source_files").number() + page.get("outside_capture_scope_source_files").number()
        + page.get("uncaptured_source_files").number());
    assert_eq!(page.get("captured_source_files").number(), page.get("examined_source_files").number()
        + page.get("pending_source_files").number());
    assert_eq!(page.get("examined_source_files").number(), page.get("scanned_source_files").number()
        + page.get("unsupported_source_files").number() + page.get("source_limited_source_files").number());
    assert_eq!(page.get("source_bytes_read").number(), 0); assert_eq!(page.get("read_calls").number(), 0);
    assert_eq!(page.get("max_source_bytes").number(), MAX_ATLAS_IMPORT_SOURCE_BYTES as u64);
    assert!(page.get("metadata_bytes").number() <= MAX_ATLAS_IMPORT_METADATA_BYTES as u64);
    assert!(page.get("parsed_source_bytes").number() <= page.get("max_parse_bytes").number());
    assert!(page.get("retained_links").number() <= page.get("links_seen").number());
}

#[test]
fn exact_source_occurrences_and_incoming_outgoing_pages_share_one_inventory() {
    let rust = b"// capture\nmod worker;\nuse crate::worker::Thing;\nmod worker;\n";
    let javascript = b"import { value } from './dep';\nexport { value } from './dep';\n";
    let worker = b"pub struct Thing;\n"; let dep = b"export const value = 1;\n";
    let mut f = Fixture::new(&[("src/lib.rs", rust), ("src/worker.rs", worker),
        ("web/main.ts", javascript), ("web/dep.ts", dep)]);
    f.build(1);
    let initial = f.begin(1, 1, MAX_ATLAS_IMPORTS, MAX_ATLAS_IMPORT_PARSE_BYTES); coverage(&initial);
    assert_eq!(initial.get("parsed_source_bytes").number(), 0);
    assert_eq!(initial.get("hashed_source_bytes").number(), 0);
    assert_eq!(initial.get("retained_links").number(), 0);
    let result = f.finish(1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("source_files").number(), 4);
    assert_eq!(result.get("links_seen").number(), 5);
    assert_eq!(result.get("parsed_source_bytes").number(), (rust.len() + worker.len() + javascript.len() + dep.len()) as u64);
    assert_eq!(result.get("hashed_source_bytes").number(), f.search.indexed_source_bytes());
    assert_eq!(ids(&result), [1, 2, 3, 4, 5]);
    let module = link(&result, b"src/lib.rs", "worker");
    assert_eq!(range(module.get("evidence_range"), rust), b"mod worker;");
    assert_eq!(range(module.get("specifier_range"), rust), b"worker");
    assert_eq!(module.get("source_sha256").text(), Sha256::digest(rust).to_hex());
    assert_eq!(module.get("target_sha256").text(), Sha256::digest(worker).to_hex());
    assert_eq!(module.get("lexical_import_kind").text(), "rust-module");
    assert_eq!(module.get("resolution_basis").text(), "rust-module-file-candidate");
    assert_eq!(module.get("target_path").get("hex").text(), hex(b"src/worker.rs"));
    let usage = link(&result, b"src/lib.rs", "crate::worker::Thing");
    assert_eq!(usage.get("resolution").text(), "unresolved");
    assert_eq!(usage.get("resolution_basis").text(), "unresolved-rust-use");
    assert_eq!(range(usage.get("evidence_range"), rust), b"use crate::worker::Thing;");
    let import = link(&result, b"web/main.ts", "./dep");
    assert_eq!(range(import.get("evidence_range"), javascript), b"import { value } from './dep';");
    assert_eq!(range(import.get("specifier_range"), javascript), b"./dep");
    assert_eq!(import.get("language").text(), "typescript");
    assert_eq!(import.get("resolution_basis").text(), "unique-relative-module-candidate");
    let incoming = f.page(1, Direction::Incoming, Some(b"src/worker.rs"), 0, 1);
    assert_eq!(incoming.get("matching_links").number(), 2); assert_eq!(ids(&incoming), [1]);
    assert_eq!(incoming.get("next_offset").number(), 1);
    let second = f.page(1, Direction::Incoming, Some(b"src/worker.rs"), 1, 1);
    assert_eq!(ids(&second), [3]); assert_eq!(second.get("next_offset"), &Json::Null);
    assert_eq!(second.get("links").array()[0].get("source_link_id").number(), 3);
    assert_eq!(second.get("hashed_source_bytes"), result.get("hashed_source_bytes"));
    assert_eq!(ids(&f.page(1, Direction::Outgoing, Some(b"web/main.ts"), 0, 64)), [4, 5]);
    assert_eq!(ids(&f.page(1, Direction::All, Some(b"web/dep.ts"), 0, 64)), [4, 5]);
}

#[test]
fn catalog_ambiguity_external_names_and_unsupported_specifiers_never_open_guessed_targets() {
    let source = b"import './pick';\nimport './exact';\nimport './explicit.js';\nimport 'package';\nimport '../outside';\nimport './bad\\u002ejs';\nimport('./late');\n";
    let mut f = Fixture::new(&[("main.ts", source), ("pick.ts", b""), ("pick.js", b""),
        ("exact", b"exact captured target"), ("exact.ts", b"alternative candidate"), ("explicit.ts", b""),
        ("late.ts", b""), ("bad.js", b""), ("lib.rs", b"use crate::missing;\n")]);
    f.build(1); let page = f.prepare(1, 1);
    assert_eq!(page.get("retained_links").number(), 7);
    assert_eq!(page.get("parser_diagnostics").number(), 1);
    assert!(!page.get("relationships_complete").flag());
    for (destination, resolution, basis) in [
        ("./pick", "ambiguous", "ambiguous-catalog-target"),
        ("./explicit.js", "missing", "no-catalog-target"),
        ("package", "external", "external-specifier"),
        ("../outside", "unresolved", "unsupported-specifier"),
        ("./bad\\u002ejs", "unresolved", "unsupported-specifier"),
    ] {
        let row = link(&page, b"main.ts", destination);
        assert_eq!(row.get("resolution").text(), resolution);
        assert_eq!(row.get("resolution_basis").text(), basis);
        assert_eq!(row.get("target_file_id"), &Json::Null);
        let id = row.get("link_id").number();
        assert_eq!(f.search.open_import_reader(&f.atlas, owner(72901), 1, id, Side::Target, || false).err(),
            Some(Error::ImportTargetUnavailable));
        let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72902), 1, id, Side::Evidence, || false).unwrap();
        let receipt = json(&receipt);
        assert_eq!(reader.capture().bytes(), source);
        assert_eq!(receipt.get("relationship"), row);
        assert_eq!(receipt.get("initial_original_offset"), row.get("evidence_range").get("start"));
    }
    let exact = link(&page, b"main.ts", "./exact");
    assert_eq!(exact.get("resolution_basis").text(), "exact-relative-path");
    assert_eq!(exact.get("target_path").get("hex").text(), hex(b"exact"));
    assert!(!page.get("links").array().iter().any(|row| row.get("destination").text() == "./late"));
}

#[test]
fn utf16_evidence_and_target_readers_pin_original_bytes_after_source_replacement_and_clear() {
    let prefix = "// \u{1f980} prefix\r\n"; let statement = "import './target';";
    let text = format!("\u{feff}{prefix}{statement}\r\n");
    let source: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let target: Vec<u8> = "\u{feff}export const original = 1;\r\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let expected_start = 2 + prefix.encode_utf16().count() * 2;
    let expected_end = expected_start + statement.encode_utf16().count() * 2;
    let mut f = Fixture::new(&[("main.ts", &source), ("target.ts", &target)]);
    f.build(1); let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    let row = link(&page, b"main.ts", "./target"); let id = row.get("link_id").number();
    assert_eq!(row.get("evidence_range").get("start").number(), expected_start as u64);
    assert_eq!(row.get("evidence_range").get("end").number(), expected_end as u64);
    fs::write(f.root.join("main.ts"), b"import 'replacement';").unwrap();
    fs::write(f.root.join("target.ts"), b"replacement target").unwrap();
    let (mut evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72903), 1, id, Side::Evidence, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("relationship"), row);
    assert_eq!(receipt.get("initial_original_offset").number(), expected_start as u64);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(&source).to_hex());
    assert_eq!(receipt.get("reader").get("capture_sha256"), receipt.get("source_sha256"));
    assert!(!receipt.get("source_reopened").flag());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    let (mut reader, target_receipt) = f.search.open_import_reader(&f.atlas, owner(72904), 1, id, Side::Target, || false).unwrap();
    let target_receipt = json(&target_receipt);
    assert_eq!(target_receipt.get("relationship"), row);
    assert_eq!(target_receipt.get("initial_original_offset").number(), 0);
    assert_eq!(target_receipt.get("source_sha256").text(), Sha256::digest(&target).to_hex());
    assert_eq!(target_receipt.get("relationship").get("source_sha256"), row.get("source_sha256"));
    assert_ne!(evidence.capture().request().file().owner(), reader.capture().request().file().owner());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(f.search.import_generation(), None);
    assert_eq!(f.search.imports_page(&f.atlas, 1, Direction::All, None, 0, 64, || false).err(), Some(Error::MissingImports));
    drop(f.search);
    assert_eq!(json(&evidence.copy_range(expected_start as u64, expected_end as u64, || false).unwrap())
        .get("original_hex").text(), hex(&source[expected_start..expected_end]));
    assert_eq!(json(&reader.copy_range(0, target.len() as u64, || false).unwrap()).get("original_hex").text(), hex(&target));
}

#[test]
fn capture_scope_unavailable_and_uncaptured_members_have_distinct_partial_navigation_states() {
    let source = b"import './b';"; let large = b"x".repeat(40);
    let mut f = Fixture::new(&[("a.ts", source), ("b.ts", &large), ("other.ts", source)]);
    let only_a = AtlasSearchScope::all().with_path(PathKind::File, "a.ts").unwrap();
    f.search.prepare_index_scoped(&f.atlas, 1, only_a, index_options(), || false).unwrap();
    let scoped = f.prepare(1, 1);
    assert!(!scoped.get("relationships_complete").flag());
    assert_eq!(scoped.get("captured_source_files").number(), 1);
    assert_eq!(scoped.get("outside_capture_scope_source_files").number(), 2);
    assert_eq!(link(&scoped, b"a.ts", "./b").get("resolution").text(), "outside-capture-scope");
    let incoming = f.page(1, Direction::Incoming, Some(b"b.ts"), 0, 64);
    assert_eq!(incoming.get("matching_links").number(), 1);
    assert!(!incoming.get("relationships_complete").flag());
    f.search.prepare_index(&f.atlas, 2, AtlasIndexOptions { max_file_bytes: 16, ..index_options() }, || false).unwrap();
    let unavailable = f.prepare(2, 2);
    assert_eq!(unavailable.get("captured_source_files").number(), 2);
    assert_eq!(unavailable.get("capture_unavailable_source_files").number(), 1);
    assert_eq!(unavailable.get("uncaptured_source_files").number(), 0);
    assert_eq!(link(&unavailable, b"a.ts", "./b").get("resolution").text(), "unavailable");
    f.search.prepare_index(&f.atlas, 3, AtlasIndexOptions { max_files: 1, ..index_options() }, || false).unwrap();
    let pending = f.prepare(3, 3);
    assert_eq!(pending.get("captured_source_files").number(), 1);
    assert_eq!(pending.get("uncaptured_source_files").number(), 2);
    assert_eq!(pending.get("capture_unavailable_source_files").number(), 0);
    assert_eq!(link(&pending, b"a.ts", "./b").get("resolution").text(), "uncaptured");
    assert_eq!(f.search.open_import_reader(&f.atlas, owner(72905), 3, 1, Side::Target, || false).err(),
        Some(Error::ImportTargetUnavailable));
}

#[test]
fn source_and_parse_limits_never_turn_unexamined_members_into_proven_absence() {
    let oversized = vec![b' '; MAX_ATLAS_IMPORT_SOURCE_BYTES + 1];
    let source = b"import './a';";
    let mut f = Fixture::new(&[("a.ts", b""), ("b.ts", source), ("bad.ts", b"\xff"), ("huge.ts", &oversized)]);
    f.build(1); let page = f.prepare(1, 1);
    assert!(!page.get("relationships_complete").flag());
    assert_eq!(page.get("scanned_source_files").number(), 2);
    assert_eq!(page.get("unsupported_source_files").number(), 1);
    assert_eq!(page.get("source_limited_source_files").number(), 1);
    assert_eq!(page.get("parsed_source_bytes").number(), source.len() as u64 + 1);
    assert_eq!(page.get("retained_links").number(), 1);
    let mut budget = Fixture::new(&[("a.ts", b""), ("b.ts", source)]);
    budget.build(1); budget.begin(1, 1, 64, 0);
    let first = json(&budget.search.step_imports(&budget.atlas, 1, || false).unwrap());
    coverage(&first);
    assert!(first.get("relationships_in_progress").flag());
    assert_eq!(first.get("last_step_files").number(), 1);
    let stopped = budget.finish(1);
    assert_eq!(stopped.get("stop_reason").text(), "parse-byte-limit");
    assert_eq!(stopped.get("pending_source_files").number(), 1);
    assert_eq!(stopped.get("last_step_files").number(), 0);
    assert_eq!(stopped.get("retained_links").number(), 0);
    assert!(!stopped.get("relationships_complete").flag());
    budget.begin(2, 1, 64, source.len() as u64);
    let exact = budget.finish(2);
    assert!(exact.get("relationships_complete").flag());
    assert_eq!(exact.get("links_seen").number(), 1);
}

#[test]
fn capped_inventory_counts_omitted_occurrences_and_paging_never_rehashes_or_renumbers_rows() {
    let source = "import './dep';\n".repeat(35);
    let mut f = Fixture::new(&[("a.ts", source.as_bytes()), ("b.ts", source.as_bytes()), ("dep.ts", b"")]);
    f.build(1); f.begin(1, 1, 2, 4096);
    let capped = f.finish(1);
    assert_eq!(capped.get("links_seen").number(), 70);
    assert_eq!(capped.get("retained_links").number(), 2);
    assert!(capped.get("truncated").flag()); assert!(!capped.get("relationships_complete").flag());
    assert_eq!(capped.get("pending_source_files").number(), 0);
    assert_eq!(ids(&capped), [1, 2]);
    let empty = f.page(1, Direction::All, None, 2, 64);
    assert!(empty.get("links").array().is_empty());
    assert_eq!(empty.get("next_offset"), &Json::Null);
    let all = f.prepare(2, 1);
    assert!(all.get("relationships_complete").flag());
    assert_eq!(all.get("retained_links").number(), 70);
    assert_eq!(all.get("next_offset").number(), 64);
    let tail = f.page(2, Direction::All, None, 64, 128);
    assert_eq!(ids(&tail), (65..=70).collect::<Vec<u64>>());
    assert_eq!(tail.get("next_offset"), &Json::Null);
    assert_eq!(tail.get("hashed_source_bytes"), all.get("hashed_source_bytes"));
    assert_eq!(tail.get("parsed_source_bytes"), all.get("parsed_source_bytes"));
    assert_eq!(tail.get("source_bytes_read").number(), 0);
}

#[test]
fn independent_documentation_and_search_survive_failed_index_replacement_then_imports_invalidate_on_success() {
    let mut f = Fixture::new(&[("README.md", b"[main](a.ts)"), ("a.ts", b"import './b';"), ("b.ts", b"old target")]);
    f.build(1);
    f.search.begin_relationships(&f.atlas, 1, 1, 64, 1024, || false).unwrap();
    f.search.step_relationships(&f.atlas, 1, || false).unwrap();
    f.prepare(1, 1);
    f.search.search_indexed(&f.atlas, 2, 1, "old target", 10, 1024, || false).unwrap();
    let (reader, _) = f.search.open_import_reader(&f.atlas, owner(72906), 1, 1, Side::Target, || false).unwrap();
    f.begin(2, 1, 64, 1024);
    assert_eq!(f.search.prepare_index(&f.atlas, 3, AtlasIndexOptions { max_files: 0, ..index_options() }, || false).err(),
        Some(Error::InvalidLimits));
    assert_eq!(f.search.prepare_index(&f.atlas, 4, index_options(), || true).err(), Some(Error::Canceled));
    assert_eq!(f.search.clear_index(&f.atlas, 5, || true).err(), Some(Error::Canceled));
    assert_eq!(f.search.index_generation(), Some(1));
    assert_eq!(f.search.import_generation(), Some(1));
    assert_eq!(f.search.pending_import_generation(), Some(2));
    assert_eq!(f.search.relationship_generation(), Some(1));
    assert_eq!(f.page(1, Direction::All, None, 0, 64).get("retained_links").number(), 1);
    let docs = json(&f.search.relationships_page(&f.atlas, 1, AtlasRelationshipDirection::All, None, 0, 64, || false).unwrap());
    assert_eq!(docs.get("schema").text(), "fcb.atlas-links/1");
    assert_eq!(docs.get("retained_links").number(), 1);
    assert_eq!(json(&f.search.page(&f.atlas, 2, 0, 64, || false).unwrap()).get("retained_hits").number(), 1);
    fs::write(f.root.join("b.ts"), b"new target").unwrap();
    f.build(6);
    assert_eq!(f.search.import_generation(), None); assert_eq!(f.search.pending_import_generation(), None);
    assert_eq!(f.search.imports_page(&f.atlas, 1, Direction::All, None, 0, 64, || false).err(), Some(Error::MissingImports));
    assert_eq!(f.search.open_import_reader(&f.atlas, owner(72907), 1, 1, Side::Target, || false).err(), Some(Error::MissingImports));
    assert_eq!(reader.capture().bytes(), b"old target");
    let replacement = f.prepare(3, 6);
    assert_eq!(replacement.get("links").array()[0].get("source_revision").number(), 6);
    assert_eq!(replacement.get("links").array()[0].get("target_sha256").text(), Sha256::digest(b"new target").to_hex());
}

#[test]
fn final_cancellation_stale_requests_and_foreign_namespaces_preserve_the_accepted_inventory() {
    let mut f = Fixture::new(&[("main.ts", b"import 'package';")]);
    f.build(1); f.prepare(1, 1);
    f.begin(2, 1, 64, 1024);
    let mut checks = 0;
    f.search.step_imports(&f.atlas, 2, || { checks += 1; false }).unwrap();
    assert!(checks > 1);
    f.begin(3, 1, 64, 1024);
    let mut canceled_checks = 0;
    assert_eq!(f.search.step_imports(&f.atlas, 3, || { canceled_checks += 1; canceled_checks == checks }).err(), Some(Error::Canceled));
    assert_eq!(f.search.import_generation(), Some(2)); assert_eq!(f.search.pending_import_generation(), None);
    assert_eq!(f.search.begin_imports(&f.atlas, 3, 1, 64, 1024, || false).err(), Some(Error::StaleImports));
    assert_eq!(f.search.begin_imports(&f.atlas, 4, 99, 64, 1024, || false).err(), Some(Error::StaleIndex));
    assert_eq!(f.search.begin_imports(&f.atlas, 5, 1, 0, 1024, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.begin_imports(&f.atlas, 6, 1, 64, MAX_ATLAS_IMPORT_PARSE_BYTES + 1, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.imports_page(&f.atlas, 2, Direction::Incoming, None, 0, 64, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.imports_page(&f.atlas, 2, Direction::Incoming, Some(b"missing.ts"), 0, 64, || false).err(),
        Some(Error::MissingImportPath));
    assert_eq!(f.search.imports_page(&f.atlas, 2, Direction::All, Some(b""), 0, 64, || false).err(), Some(Error::InvalidScope));
    assert_eq!(f.search.imports_page(&f.atlas, 2, Direction::All, None, 2, 64, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(f.search.open_import_reader(&f.atlas, f.owner, 2, 1, Side::Evidence, || false).err(), Some(Error::WrongAtlas));
    assert_eq!(f.search.open_import_reader(&f.atlas, owner(72908), 2, 0, Side::Evidence, || false).err(), Some(Error::MissingImport));
    let other = Fixture::new(&[("main.ts", b"import 'package';")]);
    assert_eq!(f.search.imports_page(&other.atlas, 2, Direction::All, None, 0, 64, || false).err(), Some(Error::WrongAtlas));
    assert!(f.page(2, Direction::All, None, 0, 64).get("relationships_complete").flag());
}

#[test]
fn raw_non_utf8_parent_paths_resolve_only_to_the_exact_captured_catalog_spelling() {
    let source = b"import './dep';"; let target = b"captured raw path target";
    let mut f = Fixture::raw(&[(b"dir-\xff/main.ts", source), (b"dir-\xff/dep.ts", target)]);
    f.build(1); let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    let row = link(&page, b"dir-\xff/main.ts", "./dep");
    assert_eq!(row.get("target_path").get("hex").text(), hex(b"dir-\xff/dep.ts"));
    assert_eq!(row.get("resolution").text(), "captured");
    let incoming = f.page(1, Direction::Incoming, Some(b"dir-\xff/dep.ts"), 0, 64);
    assert_eq!(ids(&incoming), [row.get("link_id").number()]);
    let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72909), 1,
        row.get("link_id").number(), Side::Target, || false).unwrap();
    assert_eq!(reader.capture().bytes(), target);
    assert_eq!(json(&receipt).get("path").get("hex").text(), hex(b"dir-\xff/dep.ts"));
}


#[test]
fn python_relative_imports_share_incoming_navigation_and_exact_retained_readers() {
    let source = b"# captured Python source\nfrom .worker import Tool\nfrom . import helper as alias\nimport external.library\nfrom .ambiguous import Value\nfrom ...outside import Nope\n";
    let worker = b"class Tool:\n    pass\n";
    let helper = b"VALUE = 1\n";
    let mut f = Fixture::new(&[
        ("pkg/__init__.py", b""), ("pkg/main.py", source), ("pkg/worker.py", worker),
        ("pkg/helper.py", helper), ("pkg/ambiguous.py", b"VALUE = 2\n"),
        ("pkg/ambiguous/__init__.py", b"VALUE = 3\n"), ("outside.py", b"SECRET = 4\n"),
    ]);
    f.build(1);
    let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag(), "{page:?}");
    assert_eq!(page.get("retained_links").number(), 5);
    assert_eq!(page.get("resolver").text(), "fcb-closed-catalog-imports-v2");

    let row = link(&page, b"pkg/main.py", ".worker");
    assert_eq!(row.get("language").text(), "python");
    assert_eq!(row.get("lexical_import_kind").text(), "python-from");
    assert_eq!(row.get("resolution").text(), "captured");
    assert_eq!(row.get("resolution_basis").text(), "python-relative-module-candidate");
    assert_eq!(row.get("target_path").get("hex").text(), hex(b"pkg/worker.py"));
    assert_eq!(range(row.get("evidence_range"), source), b"from .worker import Tool");
    assert_eq!(range(row.get("specifier_range"), source), b".worker");
    assert_eq!(row.get("source_sha256").text(), Sha256::digest(source).to_hex());
    let helper_row = link(&page, b"pkg/main.py", ".helper");
    assert_eq!(range(helper_row.get("specifier_range"), source), b"helper",
        "A relative prefix plus an imported name is not a fictional contiguous source slice");
    assert_eq!(helper_row.get("target_path").get("hex").text(), hex(b"pkg/helper.py"));
    let incoming = f.page(1, Direction::Incoming, Some(b"pkg/worker.py"), 0, 64);
    assert_eq!(incoming.get("matching_links").number(), 1);
    assert_eq!(incoming.get("links").array()[0], *row);
    let outgoing = f.page(1, Direction::Outgoing, Some(b"pkg/main.py"), 0, 64);
    assert_eq!(outgoing.get("matching_links").number(), 5);
    for (destination, status) in [
        ("external.library", "external"), (".ambiguous", "ambiguous"), ("...outside", "unresolved")
    ] {
        let refused = link(&page, b"pkg/main.py", destination);
        assert_eq!(refused.get("resolution").text(), status);
        assert_eq!(refused.get("target_file_id"), &Json::Null);
        assert_eq!(f.search.open_import_reader(&f.atlas, owner(72911), 1,
            refused.get("link_id").number(), Side::Target, || false).err(), Some(Error::ImportTargetUnavailable));
    }

    // Navigation follows the index's captured source, even after both live
    // files change and after the relationship inventory itself is retired.
    fs::write(f.root.join("pkg/main.py"), b"import changed\n").unwrap();
    fs::write(f.root.join("pkg/worker.py"), b"class Changed: pass\n").unwrap();
    let id = row.get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72912), 1, id, Side::Evidence, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("relationship"), row);
    assert_eq!(receipt.get("initial_original_offset"), row.get("evidence_range").get("start"));
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    let (target, receipt) = f.search.open_import_reader(&f.atlas, owner(72913), 1, id, Side::Target, || false).unwrap();
    assert!(!json(&receipt).get("source_reopened").flag());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(target.capture().bytes(), worker);
}

#[test]
fn python_import_evidence_keeps_original_utf16_coordinates_and_package_candidates() {
    let statement = "from .tools import Tool";
    let prefix = "# \u{1f40d} Python\r\n";
    let text = format!("\u{feff}{prefix}{statement}\r\n");
    for big_endian in [false, true] {
        let source: Vec<u8> = text.encode_utf16().flat_map(|unit| {
            if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() }
        }).collect();
        let target = b"class Tool:\n    pass\n";
        let mut f = Fixture::new(&[
            ("pkg/__init__.py", b""), ("pkg/main.py", &source), ("pkg/tools/__init__.py", target)
        ]);
        f.build(1);
        let page = f.prepare(1, 1);
        assert!(page.get("relationships_complete").flag());
        let row = link(&page, b"pkg/main.py", ".tools");
        let start = 2 + prefix.encode_utf16().count() * 2;
        let end = start + statement.encode_utf16().count() * 2;
        assert_eq!(row.get("evidence_range").get("start").number(), start as u64);
        assert_eq!(row.get("evidence_range").get("end").number(), end as u64);
        assert_eq!(row.get("target_path").get("hex").text(), hex(b"pkg/tools/__init__.py"));
        let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72914), 1,
            row.get("link_id").number(), Side::Evidence, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        assert_eq!(json(&receipt).get("initial_original_offset").number(), start as u64);
    }
}
