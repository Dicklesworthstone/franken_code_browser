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


#[test]
fn inline_rust_modules_resolve_nested_candidates_from_retained_captures() {
    let source = b"mod outer { mod child; use self::child::Thing; mod inner { use super::child::Thing; } }\n";
    let target = b"pub struct Thing;\n";
    let mut f = Fixture::new(&[("src/lib.rs", source), ("src/outer/child.rs", target),
        ("src/child.rs", b"pub struct WrongModule;\n")]);
    f.build(1);
    fs::write(f.root.join("src/lib.rs"), b"mod replaced;\n").unwrap();
    fs::write(f.root.join("src/outer/child.rs"), b"pub struct Replacement;\n").unwrap();
    let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    assert_eq!(page.get("links_seen").number(), 3);
    assert_eq!(page.get("extractor").text(), "franken-markdown-d6fbd1d5/import-statements-v10");
    assert_eq!(page.get("resolver").text(), "fcb-closed-catalog-imports-v12");
    for (destination, statement, kind, basis) in [
        ("child", b"mod child;".as_slice(), "rust-module", "rust-module-file-candidate"),
        ("self::child::Thing", b"use self::child::Thing;", "rust-use", "rust-use-module-prefix-candidate"),
        ("super::child::Thing", b"use super::child::Thing;", "rust-use", "rust-use-module-prefix-candidate"),
    ] {
        let row = link(&page, b"src/lib.rs", destination);
        assert_eq!(row.get("lexical_import_kind").text(), kind);
        assert_eq!(row.get("resolution").text(), "captured");
        assert_eq!(row.get("resolution_basis").text(), basis);
        assert_eq!(row.get("source_sha256").text(), Sha256::digest(source).to_hex());
        assert_eq!(range(row.get("evidence_range"), source), statement);
        assert_eq!(range(row.get("specifier_range"), source), destination.as_bytes());
        assert_eq!(row.get("target_path").get("hex").text(), hex(b"src/outer/child.rs"));
        assert_eq!(row.get("target_sha256").text(), Sha256::digest(target).to_hex());
    }
    let incoming = f.page(1, Direction::Incoming, Some(b"src/outer/child.rs"), 0, 64);
    assert_eq!(incoming.get("links").array(), page.get("links").array());
    assert_eq!(incoming.get("source_bytes_read").number(), 0);
    let row = link(&page, b"src/lib.rs", "super::child::Thing");
    let id = row.get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(73220), 1, id, Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), row);
    let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(73221), 1, id, Side::Target, || false).unwrap();
    assert_eq!(json(&receipt).get("source_sha256").text(), Sha256::digest(target).to_hex());
    assert!(!json(&receipt).get("source_reopened").flag());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(reader.capture().bytes(), target);
}

#[test]
fn explicit_rust_module_paths_navigate_exact_retained_sources() {
    let statement = b"#[path = \"platform/custom.rs\"] pub(crate) mod child;";
    let source = [b"// captured module\n".as_slice(), statement, b"\n"].concat();
    let target = b"pub const VALUE: usize = 7;\n";
    let mut f = Fixture::raw(&[(b"src/\xff/parent.rs", &source), (b"src/\xff/platform/custom.rs", target)]);
    f.build(1); let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    assert_eq!(page.get("links_seen").number(), 1);
    let row = link(&page, b"src/\xff/parent.rs", "platform/custom.rs");
    assert_eq!(row.get("lexical_import_kind").text(), "rust-path-module");
    assert_eq!(row.get("resolution_basis").text(), "rust-path-attribute-candidate");
    assert_eq!(range(row.get("evidence_range"), &source), statement);
    assert_eq!(range(row.get("specifier_range"), &source), b"platform/custom.rs");
    assert_eq!(row.get("target_path").get("hex").text(), hex(b"src/\xff/platform/custom.rs"));
    let incoming = f.page(1, Direction::Incoming, Some(b"src/\xff/platform/custom.rs"), 0, 64);
    assert_eq!(incoming.get("links").array(), page.get("links").array());
    assert_eq!(incoming.get("source_bytes_read").number(), 0);
    fs::write(f.root.join(OsStr::from_bytes(b"src/\xff/parent.rs")), b"mod replacement;").unwrap();
    fs::write(f.root.join(OsStr::from_bytes(b"src/\xff/platform/custom.rs")), b"replacement").unwrap();
    let id = row.get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(73180), 1, id, Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), row);
    let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(73181), 1, id, Side::Target, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(target).to_hex());
    assert!(!receipt.get("source_reopened").flag()); assert!(!receipt.get("compiler_resolved").flag());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(reader.capture().bytes(), target);
}

#[test]
fn explicit_rust_module_paths_withhold_conditional_missing_and_escaping_targets() {
    let source = b"#[path = \"../outside.rs\"] mod escape;\n\
        #[path = \"missing.rs\"] mod missing;\n\
        #[cfg(test)] #[path = \"child.rs\"] mod conditional;\n\
        #[path = \"child.rs\"] mod supported;\n";
    let mut f = Fixture::new(&[("main.rs", source), ("child.rs", b"pub const VALUE: usize = 7;"),
        ("missing.rs.rs", b"pub const VALUE: usize = 9;")]);
    f.build(1); let page = f.prepare(1, 1);
    for (destination, resolution) in [("../outside.rs", "unresolved"), ("missing.rs", "missing"), ("conditional", "unresolved")] {
        let row = link(&page, b"main.rs", destination);
        assert_eq!(row.get("resolution").text(), resolution);
        assert_eq!(row.get("target_file_id"), &Json::Null);
        assert!(f.search.open_import_reader(&f.atlas, owner(73182), 1,
            row.get("link_id").number(), Side::Target, || false).is_err());
    }
    let supported = link(&page, b"main.rs", "child.rs");
    assert_eq!(supported.get("resolution").text(), "captured");
    assert_eq!(supported.get("resolution_basis").text(), "rust-path-attribute-candidate");
}

#[test]
fn typescript_emitted_extensions_navigate_retained_sources_and_reverse_links() {
    let source = b"import { value } from './child.js';\nexport { value } from './child.js';\n";
    let child = b"export const value: number = 7;\n";
    let mut f = Fixture::raw(&[(b"src/\xff/main.ts", source), (b"src/\xff/child.ts", child)]);
    f.build(1); let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    assert_eq!(page.get("links_seen").number(), 2);
    let rows = page.get("links").array();
    for row in rows {
        assert_eq!(row.get("resolution").text(), "captured");
        assert_eq!(row.get("resolution_basis").text(), "typescript-source-extension-candidate");
        assert_eq!(row.get("target_path").get("hex").text(), hex(b"src/\xff/child.ts"));
        assert_eq!(range(row.get("specifier_range"), source), b"./child.js");
    }
    let incoming = f.page(1, Direction::Incoming, Some(b"src/\xff/child.ts"), 0, 64);
    assert_eq!(incoming.get("links").array(), rows);
    assert_eq!(incoming.get("source_bytes_read").number(), 0);
    fs::write(f.root.join(OsStr::from_bytes(b"src/\xff/main.ts")), b"import './replacement.js';").unwrap();
    fs::write(f.root.join(OsStr::from_bytes(b"src/\xff/child.ts")), b"replacement").unwrap();
    let id = rows[0].get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(73080), 1, id, Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), &rows[0]);
    let (target, receipt) = f.search.open_import_reader(&f.atlas, owner(73081), 1, id, Side::Target, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(child).to_hex());
    assert!(!receipt.get("source_reopened").flag());
    assert!(!receipt.get("compiler_resolved").flag());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(target.capture().bytes(), child);
}

#[test]
fn typescript_candidates_do_not_hide_exact_targets_or_ambiguity() {
    let source = b"import './same.js'; import './exact.js'; import './module.mjs'; import './common.cjs';";
    let mut f = Fixture::new(&[("main.mts", source), ("same.ts", b"export {};"),
        ("same.d.ts", b"export {};"), ("exact.js", b"export {};"), ("exact.ts", b"export {};"),
        ("module.mts", b"export {};"), ("common.cts", b"export {};")]);
    f.build(1); let page = f.prepare(1, 1);
    let ambiguous = link(&page, b"main.mts", "./same.js");
    assert_eq!(ambiguous.get("resolution").text(), "ambiguous");
    assert_eq!(ambiguous.get("target_file_id"), &Json::Null);
    assert!(f.search.open_import_reader(&f.atlas, owner(73082), 1,
        ambiguous.get("link_id").number(), Side::Target, || false).is_err());
    assert_eq!(link(&page, b"main.mts", "./exact.js").get("resolution_basis").text(), "exact-relative-path");
    for (destination, target) in [("./module.mjs", b"module.mts".as_slice()), ("./common.cjs", b"common.cts")] {
        let row = link(&page, b"main.mts", destination);
        assert_eq!(row.get("resolution_basis").text(), "typescript-source-extension-candidate");
        assert_eq!(row.get("target_path").get("hex").text(), hex(target));
    }
}

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
    assert_eq!(page.get("extractor").text(), "franken-markdown-d6fbd1d5/import-statements-v10");
    assert_eq!(page.get("resolver").text(), "fcb-closed-catalog-imports-v12");
    assert_eq!(page.get("extraction_policy").text(), "rust-inline-modules-file-includes-use-tree-js-literal-loads-python-cpp-go-java-v10");
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
    assert!(page.get("parsed_source_bytes").number() + page.get("parsed_module_bytes").number()
        <= page.get("max_parse_bytes").number());
    assert!(page.get("retained_links").number() <= page.get("links_seen").number());
}

#[test]
fn java_import_navigation_keeps_exact_evidence_and_retained_type_candidates() {
    let source = b"package app;\nimport example.Tool;\nimport example.Tool.Nested;\nimport static example.Tool.VALUE;\nimport static example.Tool.*;\nimport example.*;\nimport missing.Absent;\nimport dup.Value;\nclass Main { String text = \"import hidden.Fake;\"; }\n";
    let target = b"package example; public class Tool { public static final int VALUE = 7; public class Nested {} }\n";
    let mut f = Fixture::new(&[("src/main/java/app/Main.java", source),
        ("src/main/java/example/Tool.java", target),
        ("dup/Value.java", b"package dup; class Value {}\n"),
        ("src/main/java/dup/Value.java", b"package dup; class Value {}\n")]);
    f.build(1);
    fs::write(f.root.join("src/main/java/app/Main.java"), b"class Replacement {}\n").unwrap();
    fs::write(f.root.join("src/main/java/example/Tool.java"), b"class Replacement {}\n").unwrap();
    let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    assert_eq!(page.get("links_seen").number(), 7);
    let simple = link(&page, b"src/main/java/app/Main.java", "example.Tool");
    assert_eq!(simple.get("language").text(), "java");
    assert_eq!(simple.get("lexical_import_kind").text(), "java-import");
    assert_eq!(simple.get("resolution_basis").text(), "java-source-file-candidate");
    assert_eq!(range(simple.get("evidence_range"), source), b"import example.Tool;");
    assert_eq!(range(simple.get("specifier_range"), source), b"example.Tool");
    for destination in ["example.Tool", "example.Tool.Nested", "example.Tool.VALUE", "example.Tool.*"] {
        let row = link(&page, b"src/main/java/app/Main.java", destination);
        assert_eq!(row.get("resolution").text(), "captured");
        assert_eq!(row.get("target_path").get("hex").text(), hex(b"src/main/java/example/Tool.java"));
        assert_eq!(row.get("resolution_basis").text(), if destination == "example.Tool.Nested" {
            "java-enclosing-type-candidate"
        } else { "java-source-file-candidate" });
        if destination == "example.Tool.VALUE" || destination == "example.Tool.*" {
            assert_eq!(row.get("lexical_import_kind").text(), "java-static-import");
        }
    }
    for (destination, resolution, basis) in [
        ("example.*", "unresolved", "unresolved-java-wildcard"),
        ("missing.Absent", "missing", "no-catalog-target"),
        ("dup.Value", "ambiguous", "ambiguous-catalog-target"),
    ] {
        let row = link(&page, b"src/main/java/app/Main.java", destination);
        assert_eq!(row.get("resolution").text(), resolution);
        assert_eq!(row.get("resolution_basis").text(), basis);
        assert_eq!(row.get("target_file_id"), &Json::Null);
    }
    let incoming = f.page(1, Direction::Incoming, Some(b"src/main/java/example/Tool.java"), 0, 64);
    assert_eq!(incoming.get("matching_links").number(), 4);
    let id = simple.get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72990), 1, id, Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), simple);
    assert_eq!(json(&receipt).get("initial_original_offset").number(), simple.get("evidence_range").get("start").number());
    let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72991), 1, id, Side::Target, || false).unwrap();
    assert_eq!(json(&receipt).get("source_sha256").text(), Sha256::digest(target).to_hex());
    assert_eq!(json(&receipt).get("additional_source_bytes_read").number(), 0);
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(reader.capture().bytes(), target);
}

#[test]
fn java_import_limits_and_unsupported_escapes_preserve_coverage() {
    let source = b"import example.Tool;\nimport example.Other;\nclass Main {}\n";
    let mut f = Fixture::new(&[("src/main/java/app/Main.java", source),
        ("src/main/java/example/Tool.java", b"package example; class Tool {}\n"),
        ("src/main/java/example/Other.java", b"package example; class Other {}\n"),
        ("Bad.java", b"import example.\\u0054ool;\nclass Bad {}\n")]);
    f.build(1);
    let page = f.prepare(1, 1);
    assert!(!page.get("relationships_complete").flag());
    assert_eq!(page.get("unsupported_source_files").number(), 1);
    assert_eq!(page.get("links_seen").number(), 2);
    let bounded = f.begin(2, 1, 1, MAX_ATLAS_IMPORT_PARSE_BYTES);
    coverage(&bounded);
    let limited = f.finish(2);
    assert!(!limited.get("relationships_complete").flag());
    assert!(limited.get("truncated").flag());
    assert_eq!(limited.get("retained_links").number(), 1);
}

#[test]
fn go_imports_use_captured_modules_and_retain_package_candidate_readers() {
    let module = b"module example.local/project\n\ngo 1.24\n";
    let source = b"package main\r\nimport (\r\n util \"example.local/project/pkg\"\r\n _ `example.local/project/pkg`\r\n \"fmt\"\r\n \"example.local/project-other/pkg\"\r\n \"example.local/project/nested/pkg\"\r\n \"example.local/project/missing\"\r\n)\r\n";
    let target = b"package pkg\nconst Value = 7\n";
    let mut f = Fixture::new(&[("go.mod", module), ("cmd/app/main.go", source), ("pkg/a.go", target),
        ("pkg/a_test.go", b"package pkg\n"), ("pkg/_hidden.go", b"package pkg\n"), ("pkg/z.go", b"package pkg\n"),
        ("nested/go.mod", b"module example.local/nested\n"), ("nested/pkg/a.go", b"package pkg\n")]);
    f.build(1);
    // Resolution must use the captured module even when preparation happens
    // after a working-tree edit to both metadata and the destination.
    fs::write(f.root.join("go.mod"), b"module changed.local/elsewhere\n").unwrap();
    fs::write(f.root.join("pkg/a.go"), b"package replacement\n").unwrap();
    let result = f.prepare(1, 1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("links_seen").number(), 6);
    assert_eq!(result.get("parsed_module_bytes").number(), module.len() as u64);
    let row = link(&result, b"cmd/app/main.go", "example.local/project/pkg");
    assert_eq!(row.get("language").text(), "go");
    assert_eq!(row.get("lexical_import_kind").text(), "go-import");
    assert_eq!(row.get("resolution_basis").text(), "go-module-package-file-candidate");
    assert_eq!(row.get("target_path").get("hex").text(), hex(b"pkg/a.go"));
    assert_eq!(range(row.get("evidence_range"), source), b"util \"example.local/project/pkg\"");
    assert_eq!(range(row.get("specifier_range"), source), b"example.local/project/pkg");
    let incoming = f.page(1, Direction::Incoming, Some(b"pkg/a.go"), 0, 64);
    assert_eq!(incoming.get("matching_links").number(), 2);
    assert_eq!(incoming.get("parsed_module_bytes"), result.get("parsed_module_bytes"));
    assert_eq!(f.page(1, Direction::Incoming, Some(b"pkg/z.go"), 0, 64).get("matching_links").number(), 0);
    for (destination, resolution, basis) in [
        ("fmt", "external", "external-specifier"),
        ("example.local/project-other/pkg", "external", "external-specifier"),
        ("example.local/project/nested/pkg", "unresolved", "go-nested-module-boundary"),
        ("example.local/project/missing", "missing", "no-catalog-target"),
    ] {
        let edge = link(&result, b"cmd/app/main.go", destination);
        assert_eq!(edge.get("resolution").text(), resolution);
        assert_eq!(edge.get("resolution_basis").text(), basis);
        assert_eq!(edge.get("target_file_id"), &Json::Null);
    }
    let id = row.get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72980), 1, id, Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), row);
    let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72981), 1, id, Side::Target, || false).unwrap();
    assert_eq!(json(&receipt).get("source_sha256").text(), Sha256::digest(target).to_hex());
    assert_eq!(json(&receipt).get("additional_source_bytes_read").number(), 0);
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(reader.capture().bytes(), target);
}

#[test]
fn go_module_context_respects_capture_scope_nearest_boundary_and_parse_budget() {
    let source = b"package main\nimport \"example.local/project/pkg\"\n";
    let module = b"module example.local/project\n";
    let mut f = Fixture::new(&[("go.mod", module), ("main.go", source), ("pkg/a.go", b"package pkg\n")]);
    f.search.prepare_index_scoped(&f.atlas, 1, AtlasSearchScope::from_csv(Some("go"), None).unwrap(),
        index_options(), || false).unwrap();
    let scoped = f.prepare(1, 1);
    let edge = link(&scoped, b"main.go", "example.local/project/pkg");
    assert_eq!(edge.get("resolution_basis").text(), "go-module-context-unavailable");
    assert_eq!(edge.get("target_file_id"), &Json::Null);
    assert_eq!(scoped.get("parsed_module_bytes").number(), 0);
    f.build(2);
    f.begin(2, 2, 64, source.len() as u64);
    let capped = f.finish(2);
    assert_eq!(capped.get("stop_reason").text(), "parse-byte-limit");
    assert_eq!(capped.get("parsed_module_bytes").number(), 0);
    assert_eq!(link(&capped, b"main.go", "example.local/project/pkg").get("resolution_basis").text(),
        "go-module-context-unavailable");
    f.begin(3, 2, 64, (source.len() + module.len() + b"package pkg\n".len()) as u64);
    let exact = f.finish(3);
    assert!(exact.get("relationships_complete").flag());
    assert_eq!(link(&exact, b"main.go", "example.local/project/pkg").get("resolution").text(), "captured");
    let mut nested = Fixture::new(&[("go.mod", module), ("nested/go.mod", b"not a module directive\n"),
        ("nested/main.go", source), ("pkg/a.go", b"package pkg\n")]);
    nested.build(1);
    let page = nested.prepare(1, 1);
    assert_eq!(link(&page, b"nested/main.go", "example.local/project/pkg").get("resolution_basis").text(),
        "go-module-context-unavailable");
}

fn atlas_parcel(atlas: &AtlasSession, path: &[u8]) -> Json {
    let index = atlas.atlas().index().unwrap();
    let node = index.find_path(path).unwrap();
    let rect = index.bounds_in(node, index.root_node()).unwrap();
    parse(format!("{{\"x\":\"{}\",\"y\":\"{}\",\"w\":\"{}\",\"h\":\"{}\"}}",
        rect.min_x(), rect.min_y(), rect.size().width(), rect.size().height()).as_bytes()).unwrap()
}

#[test]
fn direct_import_geometry_is_catalog_bound_and_keeps_reader_occurrence_identity() {
    use fcb_app::host::atlas_session::AtlasAction;
    let mut f = Fixture::new(&[("src/main.js", b"import './child.js';\nimport './child.js';\nimport 'external';\n"),
        ("src/child.js", b"export const child = 1;\n"), ("README.md", b"# Guide\n")]);
    f.build(1); f.prepare(1, 1);
    let before = f.page(1, Direction::Outgoing, Some(b"src/main.js"), 0, 64);
    assert_eq!(before.get("geometry_projection").text(), "retained-atlas-parcels-v1");
    assert_eq!(before.get("filter_parcel"), &atlas_parcel(&f.atlas, b"src/main.js"));
    let rows = before.get("links").array();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get("source_parcel"), before.get("filter_parcel"));
    assert_eq!(rows[0].get("target_parcel"), &atlas_parcel(&f.atlas, b"src/child.js"));
    assert_eq!(rows[1].get("target_parcel"), rows[0].get("target_parcel"));
    assert_eq!(rows[2].get("target_parcel"), &Json::Null);
    f.atlas.prepare(1, AtlasAction::Scope(AtlasSession::preset_scope("markdown").unwrap()), || false).unwrap();
    fs::write(f.root.join("src/main.js"), b"replacement without imports").unwrap();
    assert_eq!(f.page(1, Direction::Outgoing, Some(b"src/main.js"), 0, 64), before);
    let (_, receipt) = f.search.open_import_reader(&f.atlas, owner(72990), 1,
        rows[1].get("link_id").number(), Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), &rows[1]);
    assert_eq!(json(&receipt).get("additional_source_bytes_read").number(), 0);
}

#[test]
fn cpp_includes_keep_exact_catalog_targets_and_retained_reader_evidence() {
    let source = b"// captured C++\r\n# include \"detail/value.h\" // evidence\r\n#include <vector>\r\n#include HEADER_NAME\r\n";
    let header = b"#pragma once\nstruct Value {};\n";
    let mut f = Fixture::new(&[("src/main.cpp", source), ("src/detail/value.h", header),
        ("include/detail/value.h", b"// another include root is not guessed\n")]);
    f.build(1);
    let result = f.prepare(1, 1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("links_seen").number(), 3);
    let quoted = link(&result, b"src/main.cpp", "detail/value.h");
    assert_eq!(quoted.get("language").text(), "cpp");
    assert_eq!(quoted.get("lexical_import_kind").text(), "cpp-quoted-include");
    assert_eq!(quoted.get("resolution_basis").text(), "cpp-source-relative-include-candidate");
    assert_eq!(quoted.get("target_path").get("hex").text(), hex(b"src/detail/value.h"));
    assert_eq!(range(quoted.get("evidence_range"), source), b"# include \"detail/value.h\"");
    assert_eq!(range(quoted.get("specifier_range"), source), b"detail/value.h");
    let id = quoted.get("link_id").number();
    let incoming = f.page(1, Direction::Incoming, Some(b"src/detail/value.h"), 0, 64);
    assert_eq!(ids(&incoming), [id]);
    assert_eq!(f.page(1, Direction::Incoming, Some(b"include/detail/value.h"), 0, 64)
        .get("matching_links").number(), 0);
    assert_eq!(f.page(1, Direction::Outgoing, Some(b"src/main.cpp"), 0, 64)
        .get("matching_links").number(), 3);
    for (destination, kind, resolution, basis) in [
        ("vector", "cpp-angle-include", "external", "external-specifier"),
        ("HEADER_NAME", "cpp-computed-include", "unresolved", "unresolved-cpp-include"),
    ] {
        let row = link(&result, b"src/main.cpp", destination);
        assert_eq!(row.get("lexical_import_kind").text(), kind);
        assert_eq!(row.get("resolution").text(), resolution);
        assert_eq!(row.get("resolution_basis").text(), basis);
        assert_eq!(row.get("target_path"), &Json::Null);
    }
    fs::write(f.root.join("src/main.cpp"), b"#include \"changed.h\"\n").unwrap();
    fs::write(f.root.join("src/detail/value.h"), b"replacement header").unwrap();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72960), 1, id, Side::Evidence, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("relationship"), quoted);
    assert_eq!(receipt.get("initial_original_offset"), quoted.get("evidence_range").get("start"));
    assert_eq!(evidence.capture().bytes(), source);
    let (target, receipt) = f.search.open_import_reader(&f.atlas, owner(72961), 1, id, Side::Target, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(header).to_hex());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    assert!(!receipt.get("source_reopened").flag());
    assert!(!receipt.get("compiler_resolved").flag());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(target.capture().bytes(), header);
    assert_eq!(evidence.capture().bytes(), source);
}

#[test]
fn cpp_translation_phase_refusal_is_partial_coverage_without_false_edges() {
    let continued = b"#define MULTILINE(x) \\\n    x\n#include \"header.h\"\n";
    let mut f = Fixture::new(&[("bad.cpp", continued), ("good.c", b"#include \"header.h\"\n"),
        ("header.h", b"#pragma once\n")]);
    f.build(1);
    let result = f.prepare(1, 1);
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("source_files").number(), 3);
    assert_eq!(result.get("unsupported_source_files").number(), 1);
    assert_eq!(result.get("scanned_source_files").number(), 2);
    assert_eq!(result.get("pending_source_files").number(), 0);
    assert_eq!(result.get("links_seen").number(), 1);
    assert_eq!(link(&result, b"good.c", "header.h").get("resolution").text(), "captured");
    assert!(f.page(1, Direction::Outgoing, Some(b"bad.cpp"), 0, 64).get("links").array().is_empty());
}

#[test]
fn rust_use_tree_leaves_keep_independent_occurrences_and_exact_statement_evidence() {
    let statement = b"use crate::shared::{self, Thing as Alias, nested::{Alpha, Beta}, *};";
    let shared = b"pub struct Thing;";
    let nested = b"pub struct Alpha; pub struct Beta;";
    let mut f = Fixture::new(&[("src/lib.rs", statement), ("src/shared.rs", shared),
        ("src/shared/nested.rs", nested)]);
    f.build(1);
    let result = f.prepare(1, 1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("links_seen").number(), 5);
    let rows = result.get("links").array();
    for (index, (destination, evidence)) in [
        ("crate::shared", b"self".as_slice()),
        ("crate::shared::Thing", b"Thing".as_slice()),
        ("crate::shared::nested::Alpha", b"Alpha".as_slice()),
        ("crate::shared::nested::Beta", b"Beta".as_slice()),
        ("crate::shared::*", b"*".as_slice()),
    ].into_iter().enumerate() {
        let row = &rows[index];
        assert_eq!(row.get("destination").text(), destination);
        assert_eq!(row.get("source_link_id").number(), index as u64 + 1);
        assert_eq!(range(row.get("evidence_range"), statement), statement);
        assert_eq!(range(row.get("specifier_range"), statement), evidence);
        let (path, bytes) = if matches!(index, 2 | 3) {
            (b"src/shared/nested.rs".as_slice(), nested.as_slice())
        } else { (b"src/shared.rs".as_slice(), shared.as_slice()) };
        assert_eq!(row.get("resolution").text(), "captured");
        assert_eq!(row.get("resolution_basis").text(), if matches!(index, 0 | 4) {
            "rust-use-module-file-candidate"
        } else { "rust-use-module-prefix-candidate" });
        assert_eq!(row.get("target_path").get("hex").text(), hex(path));
        assert_eq!(row.get("target_sha256").text(), Sha256::digest(bytes).to_hex());
        assert_eq!(row.get("target_bytes").number(), bytes.len() as u64);
        assert_eq!(row.get("target_revision").number(), 1);
        let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72910 + index as u64),
            1, row.get("link_id").number(), Side::Evidence, || false).unwrap();
        assert_eq!(reader.capture().bytes(), statement);
        assert_eq!(json(&receipt).get("relationship"), row);
    }
    let incoming = f.page(1, Direction::Incoming, Some(b"src/shared.rs"), 0, 2);
    assert_eq!(incoming.get("matching_links").number(), 3);
    assert_eq!(ids(&incoming), [1, 2]);
    assert_eq!(incoming.get("next_offset").number(), 2);
    let tail = f.page(1, Direction::Incoming, Some(b"src/shared.rs"), 2, 2);
    assert_eq!(ids(&tail), [5]);
    assert_eq!(tail.get("next_offset"), &Json::Null);
    assert_eq!(ids(&f.page(1, Direction::Incoming, Some(b"src/shared/nested.rs"), 0, 64)), [3, 4]);

    // A prefix candidate opens the captured containing module at byte zero;
    // changing the live source cannot turn this into an item-definition claim.
    fs::write(f.root.join("src/lib.rs"), b"use changed::Thing;").unwrap();
    fs::write(f.root.join("src/shared.rs"), b"pub struct Changed;").unwrap();
    let (target, receipt) = f.search.open_import_reader(&f.atlas, owner(72920), 1, 2, Side::Target, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("relationship"), &rows[1]);
    assert_eq!(receipt.get("initial_original_offset").number(), 0);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(shared).to_hex());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    assert!(!receipt.get("source_reopened").flag());
    assert!(!receipt.get("compiler_resolved").flag());
    assert_eq!(target.capture().bytes(), shared);
    f.begin(2, 1, 2, MAX_ATLAS_IMPORT_PARSE_BYTES);
    let limited = f.finish(2);
    assert_eq!(limited.get("retained_links").number(), 2);
    assert_eq!(limited.get("links_seen").number(), 5);
    assert!(limited.get("truncated").flag());
    assert!(!limited.get("relationships_complete").flag());
    assert_eq!(f.search.open_import_reader(&f.atlas, owner(72921), 1, 2, Side::Target, || false).err(),
        Some(Error::StaleImports));
    assert_eq!(target.capture().bytes(), shared, "an independent reader survives projection replacement");
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
    assert_eq!(usage.get("resolution").text(), "captured");
    assert_eq!(usage.get("resolution_basis").text(), "rust-use-module-prefix-candidate");
    assert_eq!(usage.get("target_file_id"), module.get("target_file_id"));
    assert_eq!(usage.get("target_sha256").text(), Sha256::digest(worker).to_hex());
    assert_eq!(range(usage.get("evidence_range"), rust), b"use crate::worker::Thing;");
    let import = link(&result, b"web/main.ts", "./dep");
    assert_eq!(range(import.get("evidence_range"), javascript), b"import { value } from './dep';");
    assert_eq!(range(import.get("specifier_range"), javascript), b"./dep");
    assert_eq!(import.get("language").text(), "typescript");
    assert_eq!(import.get("resolution_basis").text(), "unique-relative-module-candidate");
    let incoming = f.page(1, Direction::Incoming, Some(b"src/worker.rs"), 0, 1);
    assert_eq!(incoming.get("matching_links").number(), 3); assert_eq!(ids(&incoming), [1]);
    assert_eq!(incoming.get("next_offset").number(), 1);
    let second = f.page(1, Direction::Incoming, Some(b"src/worker.rs"), 1, 1);
    assert_eq!(ids(&second), [2]); assert_eq!(second.get("next_offset").number(), 2);
    assert_eq!(second.get("links").array()[0], *usage);
    let third = f.page(1, Direction::Incoming, Some(b"src/worker.rs"), 2, 1);
    assert_eq!(ids(&third), [3]); assert_eq!(third.get("next_offset"), &Json::Null);
    assert_eq!(third.get("links").array()[0].get("source_link_id").number(), 3);
    assert_eq!(second.get("hashed_source_bytes"), result.get("hashed_source_bytes"));
    assert_eq!(ids(&f.page(1, Direction::Outgoing, Some(b"web/main.ts"), 0, 64)), [4, 5]);
    assert_eq!(ids(&f.page(1, Direction::All, Some(b"web/dep.ts"), 0, 64)), [4, 5]);
}

#[test]
fn rust_root_and_module_ambiguity_preserve_evidence_and_never_guess_a_target() {
    let root = b"use crate::choice::Item;\nuse crate::missing;\nuse external::Thing;\n";
    let consumer = b"use crate::choice;";
    let nested = b"mod inner { use super::choice; }";
    let conditional = b"#[cfg(test)] mod inner { use super::choice; }";
    let local = b"fn helper() { use super::choice; }";
    let overridden = b"#[path = \"actual.rs\"] mod moved; use crate::choice;";
    let mut f = Fixture::new(&[("src/lib.rs", root), ("src/main.rs", b""),
        ("src/choice.rs", b"pub struct Item;"), ("src/choice/mod.rs", b"pub struct Item;"),
        ("src/consumer.rs", consumer), ("src/nested.rs", nested),
        ("src/conditional.rs", conditional), ("src/local.rs", local),
        ("src/overridden.rs", overridden), ("src/bin/tool.rs", consumer)]);
    f.build(1);
    let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag(), "the lexical inventory can be complete while targets are unknown");
    let cases: &[(&[u8], &[u8], &str, &str, &str)] = &[
        (b"src/lib.rs", root, "crate::choice::Item", "ambiguous", "ambiguous-catalog-target"),
        (b"src/lib.rs", root, "crate::missing", "missing", "no-catalog-target"),
        (b"src/lib.rs", root, "external::Thing", "unresolved", "unresolved-rust-use"),
        (b"src/consumer.rs", consumer, "crate::choice", "ambiguous", "ambiguous-rust-crate-root"),
        (b"src/nested.rs", nested, "super::choice", "ambiguous", "ambiguous-rust-crate-root"),
        (b"src/conditional.rs", conditional, "super::choice", "unresolved", "unresolved-rust-use"),
        (b"src/local.rs", local, "super::choice", "unresolved", "unresolved-rust-use"),
        (b"src/overridden.rs", overridden, "crate::choice", "unresolved", "unresolved-rust-use"),
        (b"src/bin/tool.rs", consumer, "crate::choice", "unresolved", "unresolved-rust-use"),
    ];
    for (index, &(path, source, destination, resolution, basis)) in cases.iter().enumerate() {
        let row = link(&page, path, destination);
        assert_eq!(row.get("resolution").text(), resolution);
        assert_eq!(row.get("resolution_basis").text(), basis);
        for field in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
            assert_eq!(row.get(field), &Json::Null);
        }
        assert_eq!(range(row.get("evidence_range"), source), format!("use {destination};").as_bytes());
        assert_eq!(range(row.get("specifier_range"), source), destination.as_bytes());
        let id = row.get("link_id").number();
        assert_eq!(f.search.open_import_reader(&f.atlas, owner(72930 + index as u64),
            1, id, Side::Target, || false).err(), Some(Error::ImportTargetUnavailable));
        let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(72940 + index as u64),
            1, id, Side::Evidence, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        assert_eq!(json(&receipt).get("relationship"), row);
    }
    for target in [b"src/choice.rs".as_slice(), b"src/choice/mod.rs"] {
        let incoming = f.page(1, Direction::Incoming, Some(target), 0, 64);
        assert_eq!(incoming.get("matching_links").number(), 0);
        assert!(incoming.get("links").array().is_empty(), "ambiguous files must not acquire reverse links");
    }
}

#[test]
fn catalog_ambiguity_external_names_and_unsupported_specifiers_never_open_guessed_targets() {
    let source = b"import './pick';\nimport './exact';\nimport './explicit.js';\nimport 'package';\nimport '../outside';\nimport './bad\\u002ejs';\nimport('./late');\nimport(computed);\n";
    let mut f = Fixture::new(&[("main.ts", source), ("pick.ts", b""), ("pick.js", b""),
        ("exact", b"exact captured target"), ("exact.ts", b"alternative candidate"), ("explicit.ts", b""),
        ("late.ts", b""), ("bad.js", b""), ("lib.rs", b"use crate::missing;\n")]);
    f.build(1); let page = f.prepare(1, 1);
    assert_eq!(page.get("retained_links").number(), 8);
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
    let dynamic = link(&page, b"main.ts", "./late");
    assert_eq!(dynamic.get("lexical_import_kind").text(), "javascript-dynamic-import");
    assert_eq!(dynamic.get("resolution").text(), "captured");
    assert_eq!(dynamic.get("target_path").get("hex").text(), hex(b"late.ts"));
    assert_eq!(range(dynamic.get("evidence_range"), source), b"import('./late')");
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
    f.search.prepare_index(&f.atlas, 1, AtlasIndexOptions { max_file_bytes: 2 * 1024 * 1024,
        ..index_options() }, || false).unwrap();
    let page = f.prepare(1, 1);
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
fn python_absolute_imports_navigate_conventional_roots_without_reopening_live_sources() {
    let source = b"import helper as local\nfrom library.worker import Task\nimport conflict.item\nimport external.library\n";
    let worker = b"class Task:\n    pass\n";
    let mut f = Fixture::new(&[
        ("tools/main.py", source), ("helper.py", b"VALUE = 1\n"), ("src/library/worker.py", worker),
        ("conflict/item.py", b"VALUE = 2\n"), ("src/conflict/item.py", b"VALUE = 3\n"),
        ("packages/service/project/__init__.py", b""),
        ("packages/service/project/main.py", b"import project.worker\n"),
        ("packages/service/project/worker.py", b"VALUE = 4\n"),
    ]);
    f.build(1);
    let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    assert_eq!(page.get("retained_links").number(), 5);
    for (path, destination, target) in [
        (b"tools/main.py".as_slice(), "helper", b"helper.py".as_slice()),
        (b"tools/main.py".as_slice(), "library.worker", b"src/library/worker.py".as_slice()),
        (b"packages/service/project/main.py".as_slice(), "project.worker",
            b"packages/service/project/worker.py".as_slice()),
    ] {
        let row = link(&page, path, destination);
        assert_eq!(row.get("resolution").text(), "captured");
        assert_eq!(row.get("resolution_basis").text(), "python-absolute-module-candidate");
        assert_eq!(row.get("target_path").get("hex").text(), hex(target));
        let incoming = f.page(1, Direction::Incoming, Some(target), 0, 64);
        assert_eq!(incoming.get("matching_links").number(), 1);
        assert_eq!(&incoming.get("links").array()[0], row);
    }
    let row = link(&page, b"tools/main.py", "library.worker");
    assert_eq!(range(row.get("evidence_range"), source), b"from library.worker import Task");
    assert_eq!(range(row.get("specifier_range"), source), b"library.worker");
    for (destination, resolution) in [("conflict.item", "ambiguous"), ("external.library", "external")] {
        let refused = link(&page, b"tools/main.py", destination);
        assert_eq!(refused.get("resolution").text(), resolution);
        assert_eq!(refused.get("target_file_id"), &Json::Null);
        assert_eq!(f.search.open_import_reader(&f.atlas, owner(72980), 1,
            refused.get("link_id").number(), Side::Target, || false).err(), Some(Error::ImportTargetUnavailable));
    }
    for path in [b"conflict/item.py".as_slice(), b"src/conflict/item.py"] {
        assert_eq!(f.page(1, Direction::Incoming, Some(path), 0, 64).get("matching_links").number(), 0);
    }
    fs::write(f.root.join("tools/main.py"), b"import changed\n").unwrap();
    fs::write(f.root.join("src/library/worker.py"), b"class Changed: pass\n").unwrap();
    let id = row.get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72981), 1, id, Side::Evidence, || false).unwrap();
    assert_eq!(json(&receipt).get("relationship"), row);
    assert_eq!(json(&receipt).get("additional_source_bytes_read").number(), 0);
    let (target, receipt) = f.search.open_import_reader(&f.atlas, owner(72982), 1, id, Side::Target, || false).unwrap();
    assert!(!json(&receipt).get("source_reopened").flag());
    assert_eq!(json(&receipt).get("additional_source_bytes_read").number(), 0);
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(target.capture().bytes(), worker);
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
    assert_eq!(page.get("resolver").text(), "fcb-closed-catalog-imports-v12");

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

#[test]
fn javascript_literal_loads_navigate_incoming_edges_and_keep_original_captures() {
    let source = b"const first = require('./child');\nconst later = () => import('./child');\nrequire('node:fs');\nimport(computed);\n";
    let child = b"module.exports = { retained: true };\n";
    let mut f = Fixture::new(&[("src/main.cjs", source), ("src/child.cjs", child)]);
    f.build(1);
    let result = f.prepare(1, 1);
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("parser_diagnostics").number(), 1);
    assert_eq!(result.get("links_seen").number(), 3);
    let rows = result.get("links").array();
    for (i, kind) in ["javascript-require", "javascript-dynamic-import"].into_iter().enumerate() {
        assert_eq!(rows[i].get("lexical_import_kind").text(), kind);
        assert_eq!(rows[i].get("destination").text(), "./child");
        assert_eq!(rows[i].get("resolution").text(), "captured");
        assert_eq!(rows[i].get("resolution_basis").text(), "unique-relative-module-candidate");
        assert_eq!(rows[i].get("target_path").get("hex").text(), hex(b"src/child.cjs"));
        assert_eq!(range(rows[i].get("specifier_range"), source), b"./child");
    }
    assert_eq!(range(rows[0].get("evidence_range"), source), b"require('./child')");
    assert_eq!(range(rows[1].get("evidence_range"), source), b"import('./child')");
    assert_eq!(rows[2].get("resolution").text(), "external");
    assert_eq!(rows[2].get("target_file_id"), &Json::Null);
    let incoming = f.page(1, Direction::Incoming, Some(b"src/child.cjs"), 0, 64);
    assert_eq!(incoming.get("links").array(), &rows[..2]);
    assert_eq!(incoming.get("source_bytes_read").number(), 0);
    fs::write(f.root.join("src/main.cjs"), b"require('./replacement');").unwrap();
    fs::write(f.root.join("src/child.cjs"), b"replacement").unwrap();
    let id = rows[1].get("link_id").number();
    let (evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(72980), 1, id, Side::Evidence, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("relationship"), &rows[1]);
    assert_eq!(receipt.get("initial_original_offset"), rows[1].get("evidence_range").get("start"));
    let (target, receipt) = f.search.open_import_reader(&f.atlas, owner(72981), 1, id, Side::Target, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_sha256").text(), Sha256::digest(child).to_hex());
    assert_eq!(receipt.get("additional_source_bytes_read").number(), 0);
    assert!(!receipt.get("source_reopened").flag()); assert!(!receipt.get("compiler_resolved").flag());
    f.search.clear_index(&f.atlas, 2, || false).unwrap();
    assert_eq!(evidence.capture().bytes(), source);
    assert_eq!(target.capture().bytes(), child);
}

#[test]
fn javascript_literal_loads_preserve_ambiguity_and_complete_empty_code_boundaries() {
    let source = b"require('./same'); import('./same');";
    let mut f = Fixture::new(&[("main.cjs", source), ("same.js", b"export {};"),
        ("same.cjs", b"module.exports = {};"),
        ("markup.jsx", b"const view = <div>require('./false') import('./false')</div>;")]);
    f.build(1); let result = f.prepare(1, 1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("links_seen").number(), 2);
    for row in result.get("links").array() {
        assert_eq!(row.get("resolution").text(), "ambiguous");
        assert_eq!(row.get("target_file_id"), &Json::Null);
    }
    assert_eq!(f.page(1, Direction::Incoming, Some(b"same.js"), 0, 64).get("matching_links").number(), 0);
    assert_eq!(f.page(1, Direction::Outgoing, Some(b"markup.jsx"), 0, 64).get("matching_links").number(), 0);
}

#[test]
fn ambiguous_upstream_javascript_regex_context_stays_partial_without_false_imports() {
    let ambiguous = b"if (ready) /require('fake-package')/.test(text);";
    let mut f = Fixture::new(&[("ambiguous.js", ambiguous),
        ("real.cjs", b"require('./child');"), ("child.cjs", b"module.exports = 1;")]);
    f.build(1); let result = f.prepare(1, 1);
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("unsupported_source_files").number(), 1);
    assert_eq!(result.get("links_seen").number(), 1);
    assert_eq!(result.get("links").array()[0].get("destination").text(), "./child");
    assert_eq!(result.get("links").array()[0].get("resolution").text(), "captured");
    assert_eq!(f.page(1, Direction::Outgoing, Some(b"ambiguous.js"), 0, 64).get("matching_links").number(), 0);
}

#[test]
fn rust_literal_includes_connect_project_backlinks_to_retained_docs() {
    let source = b"#![doc = include_str!(\"../docs/intro.md\")]\n";
    let text = b"# a captured guide\n";
    let mut f = Fixture::new(&[("src/lib.rs", source), ("docs/intro.md", text)]);
    f.build(1);
    let page = f.prepare(1, 1);
    let row = link(&page, b"src/lib.rs", "../docs/intro.md");
    assert_eq!(row.get("lexical_import_kind").text(), "rust-include-str");
    assert_eq!(row.get("resolution_basis").text(), "rust-include-file-candidate");
    assert_eq!(range(row.get("evidence_range"), source), br#"include_str!("../docs/intro.md")"#);
    assert_eq!(range(row.get("specifier_range"), source), b"../docs/intro.md");
    let incoming = f.page(1, Direction::Incoming, Some(b"docs/intro.md"), 0, 64);
    assert_eq!(incoming.get("links").array(), page.get("links").array());
    fs::write(f.root.join("docs/intro.md"), b"changed live").unwrap();
    let (reader, receipt) = f.search.open_import_reader(&f.atlas, owner(73189), 1,
        row.get("link_id").number(), Side::Target, || false).unwrap();
    assert!(!json(&receipt).get("source_reopened").flag());
    assert_eq!(reader.capture().bytes(), text);
}


#[test]
fn large_import_sources_keep_late_ranges_reverse_links_and_independent_readers() {
    let prefix = "// retained import source \u{1f980}\r\n".repeat(6000);
    let statement = "import './target';";
    for encoding in 0..3 {
        let encode = |text: &str, bom: bool| -> Vec<u8> {
            if encoding == 0 {
                let mut bytes = if bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
                bytes.extend_from_slice(text.as_bytes()); return bytes;
            }
            let mut bytes = if !bom { Vec::new() } else if encoding == 1 { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if encoding == 1 { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            bytes
        };
        let source = encode(&format!("{prefix}{statement}\r\n"), true);
        assert!(source.len() > 128 * 1024 && source.len() <= MAX_ATLAS_IMPORT_SOURCE_BYTES);
        let target = b"export const original = 1;\n";
        let start = encode(&prefix, true).len();
        let expected = encode(statement, false);
        let mut f = Fixture::new(&[("main.ts", &source), ("target.ts", target)]);
        f.build(1); let page = f.prepare(1, 1);
        assert!(page.get("relationships_complete").flag());
        assert_eq!(page.get("source_limited_source_files").number(), 0);
        let row = link(&page, b"main.ts", "./target");
        let id = row.get("link_id").number();
        assert_eq!(row.get("source_bytes").number(), source.len() as u64);
        assert_eq!(row.get("evidence_range").get("start").number(), start as u64);
        assert_eq!(range(row.get("evidence_range"), &source), expected);
        assert_eq!(range(row.get("specifier_range"), &source), encode("./target", false));
        let incoming = f.page(1, Direction::Incoming, Some(b"target.ts"), 0, 64);
        assert_eq!(incoming.get("links").array(), page.get("links").array());
        f.begin(2, 1, 64, MAX_ATLAS_IMPORT_PARSE_BYTES);
        let mut polls = 0;
        assert!(matches!(f.search.step_imports(&f.atlas, 2, || {
            polls += 1; polls == 8
        }), Err(Error::Canceled)));
        assert_eq!(f.search.import_generation(), Some(1));
        assert_eq!(f.page(1, Direction::All, None, 0, 64).get("links"), page.get("links"));
        fs::write(f.root.join("main.ts"), b"import 'replacement';").unwrap();
        fs::write(f.root.join("target.ts"), b"replacement target").unwrap();
        let (mut evidence, receipt) = f.search.open_import_reader(&f.atlas, owner(73910 + encoding * 2),
            1, id, Side::Evidence, || false).unwrap();
        assert_eq!(json(&receipt).get("initial_original_offset").number(), start as u64);
        assert_eq!(json(&receipt).get("additional_source_bytes_read").number(), 0);
        let (mut reader, _) = f.search.open_import_reader(&f.atlas, owner(73911 + encoding * 2),
            1, id, Side::Target, || false).unwrap();
        f.search.clear_index(&f.atlas, 2, || false).unwrap(); drop(f.search);
        assert_eq!(json(&evidence.copy_range(start as u64, (start + expected.len()) as u64,
            || false).unwrap()).get("original_hex").text(), hex(&expected));
        assert_eq!(json(&reader.copy_range(0, target.len() as u64, || false).unwrap())
            .get("original_hex").text(), hex(target));
    }
}

#[test]
fn maximum_import_source_reaches_the_final_statement_in_a_captured_project() {
    let tail = b"*/\nimport './target';\n";
    let mut source = b"/*".to_vec();
    source.resize(MAX_ATLAS_IMPORT_SOURCE_BYTES - tail.len(), b'x');
    source.extend_from_slice(tail);
    let mut f = Fixture::new(&[("main.ts", &source), ("target.ts", b"")]);
    f.build(1); let page = f.prepare(1, 1);
    assert!(page.get("relationships_complete").flag());
    assert_eq!(page.get("parsed_source_bytes").number(), MAX_ATLAS_IMPORT_SOURCE_BYTES as u64);
    let row = link(&page, b"main.ts", "./target");
    assert_eq!(range(row.get("evidence_range"), &source), b"import './target';");
    assert_eq!(row.get("resolution").text(), "captured");
    assert!(row.get("evidence_range").get("start").number() > 128 * 1024);
}
