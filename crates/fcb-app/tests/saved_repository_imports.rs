#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production saved-archive import extraction, reverse navigation and exact
//! reader ownership. Authored code-first; independent batch verification owns
//! execution. No Python/JavaScript/Rust project execution supplies the oracle.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::search::{imports::MAX_IMPORT_SOURCE_BYTES, snapshot::{SnapshotData, SnapshotEntry}};
use fcb::store::Sha256;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, reader::ReaderSession, saved_repository::{SavedRepositorySession,
    imports::{SavedImportDirection as Direction, SavedImportSide as Side, SavedImportError as Error,
        MAX_SAVED_IMPORTS, MAX_SAVED_IMPORT_SOURCE_BYTES},
    relationships::RelationshipDirection}};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn reads(saved: &mut SavedRepositorySession) -> (u64, u64) {
    let info = json(&saved.info(|| false).unwrap());
    (info.get("member_bytes_read").number(), info.get("member_read_calls").number())
}
fn prepare(saved: &mut SavedRepositorySession, generation: u64) -> Json {
    json(&saved.prepare_imports(generation, 64, 1024 * 1024, || false).unwrap())
}
fn copy_all(reader: &mut ReaderSession) -> Json {
    json(&reader.copy_range(0, reader.capture().bytes().len() as u64, || false).unwrap())
}
fn range<'a>(bytes: &'a [u8], value: &Json) -> &'a [u8] {
    &bytes[value.get("start").number() as usize..value.get("end").number() as usize]
}
fn link<'a>(page: &'a Json, path: &[u8], destination: &str) -> &'a Json {
    let path = hex(path);
    page.get("links").array().iter().find(|link| link.get("source_path").get("hex").text() == path
        && link.get("destination").text() == destination).unwrap()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() { bytes.extend_from_slice(&if big { word.to_be_bytes() } else { word.to_le_bytes() }); }
    bytes
}

#[test]
fn saved_rust_use_tree_pages_preserve_leaf_identity_and_original_utf16_evidence() {
    let statement = "use crate::shared::{self, Thing as Alias, nested::{Alpha, Beta}, *};";
    for big in [false, true] {
        let source = utf16(statement, big);
        let shared = b"pub struct Thing;";
        let nested = b"pub struct Alpha; pub struct Beta;";
        let f = Fixture::new(&[entry(b"src/lib.rs", &source), entry(b"src/shared.rs", shared),
            entry(b"src/shared/nested.rs", nested)], true);
        let mut saved = f.open(57801 + u64::from(big));
        let result = prepare(&mut saved, 1);
        assert!(result.get("imports_complete").flag());
        assert_eq!(result.get("links_seen").number(), 5);
        assert_eq!(result.get("resolver").text(), "fcb-closed-catalog-imports-v3");
        let rows = result.get("links").array();
        for (index, (destination, spelling)) in [
            ("crate::shared", "self"), ("crate::shared::Thing", "Thing"),
            ("crate::shared::nested::Alpha", "Alpha"), ("crate::shared::nested::Beta", "Beta"),
            ("crate::shared::*", "*"),
        ].into_iter().enumerate() {
            let row = &rows[index];
            assert_eq!(row.get("destination").text(), destination);
            assert_eq!(row.get("source_link_id").number(), index as u64 + 1);
            assert_eq!(range(&source, row.get("evidence_range")), &source[2..]);
            assert_eq!(range(&source, row.get("specifier_range")), &utf16(spelling, big)[2..]);
            let (member, path, bytes) = if matches!(index, 2 | 3) {
                (2, b"src/shared/nested.rs".as_slice(), nested.as_slice())
            } else { (1, b"src/shared.rs".as_slice(), shared.as_slice()) };
            assert_eq!(row.get("resolution").text(), "captured");
            assert_eq!(row.get("resolution_basis").text(), if matches!(index, 0 | 4) {
                "rust-use-module-file-candidate"
            } else { "rust-use-module-prefix-candidate" });
            assert_eq!(row.get("target_member").number(), member);
            assert_eq!(row.get("target_file_id").number(), member + 1);
            assert_eq!(row.get("target_revision").number(), member + 1);
            assert_eq!(row.get("target_path").get("hex").text(), hex(path));
            assert_eq!(row.get("target_digest").text(), Sha256::digest(bytes).to_hex());
            assert_eq!(row.get("target_bytes").number(), bytes.len() as u64);
        }
        let before = reads(&mut saved);
        let page = json(&saved.import_page(1, Some(0), Direction::Outgoing, 2, 2, || false).unwrap());
        assert_eq!(page.get("matching_links").number(), 5);
        assert_eq!(page.get("next_offset").number(), 4);
        assert_eq!(page.get("links").array()[0], rows[2]);
        assert_eq!(page.get("links").array()[1], rows[3]);
        let incoming = json(&saved.import_page(1, Some(1), Direction::Incoming, 0, 2, || false).unwrap());
        assert_eq!(incoming.get("matching_links").number(), 3);
        assert_eq!(incoming.get("links").array(), &rows[..2]);
        assert_eq!(incoming.get("next_offset").number(), 2);
        let tail = json(&saved.import_page(1, Some(1), Direction::Incoming, 2, 2, || false).unwrap());
        assert_eq!(tail.get("links").array(), &rows[4..]);
        assert_eq!(tail.get("next_offset"), &Json::Null);
        let nested_page = json(&saved.import_page(1, Some(2), Direction::Incoming, 0, 64, || false).unwrap());
        assert_eq!(nested_page.get("links").array(), &rows[2..4]);
        assert_eq!(reads(&mut saved), before, "paging use leaves must not reread archive members");

        let reader_owner = owner(57810 + u64::from(big));
        let (mut target, receipt) = saved.open_import_reader(reader_owner, 1, 2, Side::Target, || false).unwrap();
        let receipt = json(&receipt);
        assert_eq!(receipt.get("relationship"), &rows[1]);
        assert_eq!(receipt.get("selection"), &Json::Null, "a containing module is not an item definition");
        assert_eq!(receipt.get("offset").number(), 0);
        assert_eq!(receipt.get("source_digest").text(), Sha256::digest(shared).to_hex());
        assert_eq!(receipt.get("member_bytes_read").number(), shared.len() as u64);
        assert!(!receipt.get("live_source_reopened").flag());
        assert!(!receipt.get("compiler_resolved").flag());
        prepare(&mut saved, 2);
        let before = reads(&mut saved);
        assert_eq!(saved.open_import_reader(reader_owner, 1, 2, Side::Target, || false).err(),
            Some(Error::StaleGeneration));
        assert_eq!(reads(&mut saved), before, "stale selections fail before archive I/O");
        drop(saved);
        fs::rename(&f.path, f.root.join("retired.fcbs")).unwrap();
        assert_eq!(copy_all(&mut target).get("original_hex").text(), hex(shared));
    }
}

#[test]
fn shared_language_extractors_publish_exact_occurrences_and_metadata_only_reverse_pages() {
    let js = b"import item from './util';\nexport { item } from './util';\n";
    let python = b"from . import helper as kept\nfrom .helper import Value\n";
    let rust = b"mod worker;\nuse crate::worker::Thing;\n";
    let util = b"export const item = 1;\n";
    let helper = b"Value = 1\n";
    let worker = b"pub struct Thing;\n";
    let f = Fixture::new(&[entry(b"js/main.ts", js), entry(b"js/util.ts", util),
        entry(b"pkg/helper.py", helper), entry(b"pkg/main.py", python),
        entry(b"rust/lib.rs", rust), entry(b"rust/worker.rs", worker)], true);
    let mut saved = f.open(57401);
    let response = saved.prepare_imports(7, 64, 1024 * 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let result = json(&response);
    assert_eq!(result.get("command").text(), "imports-prepare");
    assert_eq!(result.get("import_generation").number(), 7);
    assert_eq!(result.get("projection").text(), "lexical-source-imports");
    assert_eq!(result.get("evidence_precision").text(), "exact-import-source-range");
    assert_eq!(result.get("resolver").text(), "fcb-closed-catalog-imports-v3");
    assert_eq!(result.get("extraction_policy").text(), "rust-use-tree-js-static-and-python-import-v3");
    assert_eq!(result.get("completeness_scope").text(), "parsed-source-imports-in-saved-manifest");
    assert!(!result.get("compiler_resolved").flag());
    assert!(!result.get("semantic_complete").flag());
    assert!(result.get("imports_complete").flag());
    assert_eq!(result.get("source_members").number(), 6);
    assert_eq!(result.get("scanned_source_members").number(), 6);
    assert_eq!(result.get("retained_links").number(), 6);
    assert_eq!(result.get("links_seen").number(), 6);
    assert_eq!(result.get("member_bytes_read").number(), (js.len() + python.len() + rust.len() + util.len() + helper.len() + worker.len()) as u64);
    assert_eq!(result.get("peak_source_bytes").number(), js.len().max(python.len()).max(rust.len()) as u64);
    assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
    let links = result.get("links").array();
    for (index, link) in links.iter().enumerate() {
        assert_eq!(link.get("link_id").number(), index as u64 + 1);
        assert_eq!(link.get("source_link_id").number(), (index % 2 + 1) as u64);
        assert_eq!(link.get("source_file_id").number(), link.get("source_member").number() + 1);
        assert_eq!(link.get("source_revision"), link.get("source_file_id"));
        let source = match index { 0 | 1 => js.as_slice(), 2 | 3 => python.as_slice(), _ => rust.as_slice() };
        assert_eq!(link.get("source_digest").text(), Sha256::digest(source).to_hex());
        assert_eq!(link.get("source_bytes").number(), source.len() as u64);
        let evidence = link.get("evidence_range");
        let specifier = link.get("specifier_range");
        assert!(specifier.get("start").number() >= evidence.get("start").number());
        assert!(specifier.get("end").number() <= evidence.get("end").number());
        assert!(!range(source, evidence).is_empty());
    }
    assert_eq!(range(js, links[0].get("evidence_range")), b"import item from './util';");
    assert_eq!(range(python, links[2].get("evidence_range")), b"from . import helper as kept");
    assert_eq!(range(python, links[2].get("specifier_range")), b"helper");
    assert_eq!(links[2].get("destination").text(), ".helper");
    assert_eq!(range(rust, links[4].get("evidence_range")), b"mod worker;");
    assert_eq!(links[0].get("resolution").text(), "captured");
    assert_eq!(links[0].get("target_member").number(), 1);
    assert_eq!(links[2].get("target_member").number(), 2);
    assert_eq!(links[4].get("target_member").number(), 5);
    assert_eq!(links[5].get("resolution").text(), "captured");
    assert_eq!(links[5].get("resolution_basis").text(), "rust-use-module-prefix-candidate");
    assert_eq!(links[5].get("target_member").number(), 5);
    assert_eq!(links[5].get("target_digest").text(), Sha256::digest(worker).to_hex());

    let before = reads(&mut saved);
    let first = json(&saved.import_page(7, Some(2), Direction::Incoming, 0, 1, || false).unwrap());
    assert_eq!(first.get("matching_links").number(), 2);
    assert_eq!(first.get("links").array()[0].get("link_id").number(), 3);
    assert_eq!(first.get("next_offset").number(), 1);
    let last = json(&saved.import_page(7, Some(2), Direction::Incoming, 1, 1, || false).unwrap());
    assert_eq!(last.get("links").array()[0].get("link_id").number(), 4);
    assert_eq!(last.get("next_offset"), &Json::Null);
    let outgoing = json(&saved.import_page(7, Some(3), Direction::Outgoing, 0, 64, || false).unwrap());
    assert_eq!(outgoing.get("matching_links").number(), 2);
    assert_eq!(outgoing.get("links").array()[0].get("link_id").number(), 3);
    let either = json(&saved.import_page(7, Some(5), Direction::All, 0, 64, || false).unwrap());
    assert_eq!(either.get("matching_links").number(), 2);
    assert_eq!(either.get("links").array(), &links[4..]);
    assert_eq!(reads(&mut saved), before, "pages must not load or reparse any source");
}

#[test]
fn saved_rust_root_and_module_ambiguity_keep_evidence_without_target_authority() {
    let root = b"use crate::choice::Item;\nuse crate::missing;\nuse external::Thing;\n";
    let consumer = b"use crate::choice;";
    let nested = b"mod inner { use super::choice; }";
    let overridden = b"#[path = \"actual.rs\"] mod moved; use crate::choice;";
    let f = Fixture::new(&[entry(b"src/bin/tool.rs", consumer),
        entry(b"src/choice.rs", b"pub struct Item;"), entry(b"src/choice/mod.rs", b"pub struct Item;"),
        entry(b"src/consumer.rs", consumer), entry(b"src/lib.rs", root), entry(b"src/main.rs", b""),
        entry(b"src/nested.rs", nested), entry(b"src/overridden.rs", overridden)], true);
    let mut saved = f.open(57820);
    let page = prepare(&mut saved, 1);
    assert!(page.get("imports_complete").flag(), "unresolved navigation is distinct from incomplete extraction");
    let cases: &[(&[u8], &[u8], &str, &str, &str)] = &[
        (b"src/lib.rs", root, "crate::choice::Item", "ambiguous", "ambiguous-catalog-target"),
        (b"src/lib.rs", root, "crate::missing", "missing", "no-catalog-target"),
        (b"src/lib.rs", root, "external::Thing", "unresolved", "unresolved-rust-use"),
        (b"src/consumer.rs", consumer, "crate::choice", "ambiguous", "ambiguous-rust-crate-root"),
        (b"src/nested.rs", nested, "super::choice", "unresolved", "unresolved-rust-use"),
        (b"src/overridden.rs", overridden, "crate::choice", "unresolved", "unresolved-rust-use"),
        (b"src/bin/tool.rs", consumer, "crate::choice", "unresolved", "unresolved-rust-use"),
    ];
    for (index, &(path, source, destination, resolution, basis)) in cases.iter().enumerate() {
        let row = link(&page, path, destination);
        assert_eq!(row.get("resolution").text(), resolution);
        assert_eq!(row.get("resolution_basis").text(), basis);
        for field in ["target_member", "target_file_id", "target_path", "target_revision", "target_digest", "target_bytes"] {
            assert_eq!(row.get(field), &Json::Null);
        }
        assert_eq!(range(source, row.get("evidence_range")), format!("use {destination};").as_bytes());
        assert_eq!(range(source, row.get("specifier_range")), destination.as_bytes());
        let id = row.get("link_id").number();
        let before = reads(&mut saved);
        assert_eq!(saved.open_import_reader(owner(57830 + index as u64), 1, id, Side::Target, || false).err(),
            Some(Error::UnresolvedTarget));
        assert_eq!(reads(&mut saved), before, "unknown targets fail before archive I/O");
        let (reader, receipt) = saved.open_import_reader(owner(57840 + index as u64),
            1, id, Side::Evidence, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        let receipt = json(&receipt);
        assert_eq!(receipt.get("relationship"), row);
        assert_eq!(receipt.get("selection"), row.get("evidence_range"));
    }
    let before = reads(&mut saved);
    for member in [1, 2] {
        let incoming = json(&saved.import_page(1, Some(member), Direction::Incoming, 0, 64, || false).unwrap());
        assert_eq!(incoming.get("matching_links").number(), 0);
        assert!(incoming.get("links").array().is_empty(), "ambiguous files must not acquire reverse links");
    }
    assert_eq!(reads(&mut saved), before);
}

#[test]
fn target_outcomes_keep_ambiguity_unavailable_missing_external_and_unsupported_distinct() {
    let source = b"from .ambiguous import Thing\nfrom .blocked import Thing\nfrom .missing import Thing\nimport os\nfrom ...outside import Thing\n";
    let f = Fixture::new(&[entry(b"pkg/ambiguous.py", b""), entry(b"pkg/ambiguous/__init__.py", b""),
        SnapshotEntry { path: b"pkg/blocked.py", observed_bytes: 77, data: SnapshotData::Unavailable("READ_DENIED") },
        entry(b"pkg/main.py", source)], true);
    let mut saved = f.open(57410);
    let result = prepare(&mut saved, 1);
    let links = result.get("links").array();
    assert_eq!(links.len(), 5);
    for (link, expected) in links.iter().zip(["ambiguous", "unavailable", "missing", "external", "unresolved"]) {
        assert_eq!(link.get("resolution").text(), expected);
        assert_eq!(link.get("target_digest"), &Json::Null);
        assert_eq!(link.get("target_revision"), &Json::Null);
        assert_eq!(link.get("target_bytes"), &Json::Null);
    }
    assert_eq!(links[1].get("target_member").number(), 2);
    assert_eq!(links[1].get("target_file_id").number(), 3);
    assert_eq!(links[1].get("target_path").get("hex").text(), hex(b"pkg/blocked.py"));
    for index in [0, 2, 3, 4] { assert_eq!(links[index].get("target_member"), &Json::Null); }
    assert!(!result.get("imports_complete").flag(), "an unavailable supported source may contain imports");
    assert_eq!(result.get("unavailable_source_members").number(), 1);
    let before = reads(&mut saved);
    for (id, error) in [(1, Error::UnresolvedTarget), (2, Error::TargetUnavailable),
        (3, Error::UnresolvedTarget), (4, Error::UnresolvedTarget), (5, Error::UnresolvedTarget)] {
        assert_eq!(saved.open_import_reader(owner(57411), 1, id, Side::Target, || false).err(), Some(error));
    }
    assert_eq!(reads(&mut saved), before, "unresolved activation must fail before archive I/O");
    let incoming = json(&saved.import_page(1, Some(2), Direction::Incoming, 0, 10, || false).unwrap());
    assert_eq!(incoming.get("matching_links").number(), 1, "an unavailable target still has catalog identity");
    assert_eq!(incoming.get("links").array()[0].get("link_id").number(), 2);
}

#[test]
fn utf16_original_ranges_raw_parent_names_and_independent_readers_survive_archive_detachment() {
    for big in [false, true] {
        let source = utf16("# \u{1f600}\r\nfrom . import helper as chosen\r\n", big);
        let target = b"Value = 1\r\n";
        let f = Fixture::new(&[entry(b"pkg/\xff/helper.py", target), entry(b"pkg/\xff/main.pyi", &source)], true);
        let base = 57420 + u64::from(big) * 10;
        let mut saved = f.open(base);
        let result = prepare(&mut saved, 1);
        let link = &result.get("links").array()[0];
        assert_eq!(link.get("source_path").get("hex").text(), hex(b"pkg/\xff/main.pyi"));
        assert_eq!(link.get("evidence_range").get("start").number(), 14);
        assert_eq!(link.get("evidence_range").get("end").number(), 74);
        assert_eq!(link.get("specifier_range").get("start").number(), 42);
        assert_eq!(link.get("specifier_range").get("end").number(), 54);
        assert_eq!(link.get("resolution").text(), "captured");
        assert_eq!(link.get("resolution_basis").text(), "python-relative-module-candidate");
        let (mut evidence, response) = saved.open_import_reader(owner(base + 1), 1, 1, Side::Evidence, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("command").text(), "imports-open-reader");
        assert_eq!(receipt.get("relationship"), link);
        assert_eq!(receipt.get("selection"), link.get("evidence_range"));
        assert_eq!(receipt.get("offset").number(), 14);
        assert_eq!(receipt.get("source_digest").text(), Sha256::digest(&source).to_hex());
        assert_eq!(receipt.get("reader_owner").number(), base + 1);
        assert_eq!(receipt.get("reader").get("owner").number(), base + 1);
        assert_eq!(receipt.get("reader").get("capture_origin").text(), "host-supplied");
        assert!(!receipt.get("live_source_reopened").flag());
        assert_eq!(receipt.get("member_bytes_read").number(), source.len() as u64);
        let (mut reader, response) = saved.open_import_reader(owner(base + 2), 1, 1, Side::Target, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("member").number(), 0);
        assert_eq!(receipt.get("source_digest").text(), Sha256::digest(target).to_hex());
        assert_eq!(receipt.get("selection"), &Json::Null);
        assert_eq!(receipt.get("offset").number(), 0);
        assert_eq!(receipt.get("member_bytes_read").number(), target.len() as u64);
        saved.clear_imports(2, || false).unwrap();
        drop(saved);
        fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
        assert_eq!(copy_all(&mut evidence).get("original_hex").text(), hex(&source));
        assert_eq!(json(&evidence.copy_range(42, 54, || false).unwrap()).get("original_hex").text(), hex(&utf16("helper", big)[2..]));
        assert_eq!(copy_all(&mut reader).get("original_hex").text(), hex(target));
        assert!(!f.root.join("pkg").exists(), "archive member labels never become live-path grants");
    }
}

#[test]
fn explicit_javascript_targets_preserve_binary_bytes_and_opened_archive_identity() {
    let source = b"import data from '../raw.bin';\n";
    let target = b"\0\xffcaptured\r\nbytes";
    let f = Fixture::new(&[entry(b"pkg/main.ts", source), entry(b"raw.bin", target)], true);
    let replacement = Fixture::new(&[entry(b"pkg/main.ts", b"import data from '../other.bin';\n"),
        entry(b"raw.bin", b"replacement")], true);
    let mut saved = f.open(57441);
    prepare(&mut saved, 1);
    fs::rename(&f.path, f.root.join("retained.fcbs")).unwrap();
    fs::write(&f.path, &replacement.bytes).unwrap();
    let (mut reader, response) = saved.open_import_reader(owner(57442), 1, 1, Side::Target, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("source_digest").text(), Sha256::digest(target).to_hex());
    assert_eq!(receipt.get("relationship").get("resolution_basis").text(), "exact-relative-path");
    assert_eq!(receipt.get("path").get("hex").text(), hex(b"raw.bin"));
    drop(saved);
    assert_eq!(copy_all(&mut reader).get("original_hex").text(), hex(target));
}

#[test]
fn unavailable_unsupported_and_source_limited_members_partition_coverage_without_payload_retention() {
    let source = b"from .b import Value\n";
    let oversized = vec![b' '; MAX_IMPORT_SOURCE_BYTES + 1];
    let f = Fixture::new(&[entry(b"a.py", source), entry(b"b.py", b""), entry(b"c.py", &[0xff, 0, 1]),
        entry(b"d.py", &oversized),
        SnapshotEntry { path: b"e.py", observed_bytes: 50, data: SnapshotData::Unavailable("READ_DENIED") },
        entry(b"f.PYI", b""), entry(b"g.txt", b"import not_python")], true);
    let mut saved = f.open(57451);
    let response = saved.prepare_imports(1, 20, 1024 * 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let result = json(&response);
    assert_eq!(result.get("source_members").number(), 6);
    assert_eq!(result.get("scanned_source_members").number(), 3);
    assert_eq!(result.get("unsupported_source_members").number(), 1);
    assert_eq!(result.get("source_limited_members").number(), 1);
    assert_eq!(result.get("unavailable_source_members").number(), 1);
    assert_eq!(result.get("member_bytes_read").number(), source.len() as u64 + 3);
    assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
    assert!(!result.get("imports_complete").flag());
    let probe = json(&saved.prepare_imports(2, 20, 0, || false).unwrap());
    assert_eq!(probe.get("member_bytes_read").number(), 0);
    assert_eq!(probe.get("retained_links").number(), 0);
    assert_eq!(probe.get("scanned_source_members").number(), 2, "empty captured sources remain verifiable");
    assert_eq!(probe.get("source_limited_members").number(), 3);
    assert_eq!(probe.get("unavailable_source_members").number(), 1);
    assert!(!probe.get("imports_complete").flag());
}

#[test]
fn retained_cap_keeps_seen_counts_and_parser_or_discovery_gaps_cannot_claim_complete_absence() {
    let f = Fixture::new(&[entry(b"a.py", b"import a, b, c\n"), entry(b"b.py", b"from .a import Thing\n")], false);
    let mut saved = f.open(57461);
    let result = json(&saved.prepare_imports(1, 1, 1024, || false).unwrap());
    assert_eq!(result.get("links_seen").number(), 4, "other admitted sources are still examined after the display cap");
    assert_eq!(result.get("retained_links").number(), 1);
    assert!(result.get("truncated").flag());
    assert!(!result.get("imports_complete").flag());
    let page = json(&saved.import_page(1, None, Direction::All, 1, 64, || false).unwrap());
    assert!(page.get("links").array().is_empty());
    assert_eq!(page.get("next_offset"), &Json::Null);
    let complete_rows = prepare(&mut saved, 2);
    assert_eq!(complete_rows.get("retained_links").number(), 4);
    assert!(!complete_rows.get("truncated").flag());
    assert!(!complete_rows.get("imports_complete").flag(), "partial discovery remains independent of extraction");
    let malformed = Fixture::new(&[entry(b"a.py", b"import good,\n")], true);
    let mut saved = malformed.open(57462);
    let result = prepare(&mut saved, 1);
    assert_eq!(result.get("retained_links").number(), 0);
    assert_eq!(result.get("scanned_source_members").number(), 1);
    assert_eq!(result.get("diagnostic_source_members").number(), 1);
    assert_eq!(result.get("parser_diagnostics").number(), 1);
    assert!(!result.get("imports_complete").flag(), "unsupported statement coverage is not a complete empty inventory");
}

#[test]
fn invalid_identity_filters_and_limits_fail_before_archive_reads() {
    let f = Fixture::new(&[entry(b"a.py", b"from .b import Value\n"), entry(b"b.py", b"Value = 1\n")], true);
    let mut saved = f.open(57471);
    assert_eq!(saved.import_page(1, None, Direction::All, 0, 64, || false).err(), Some(Error::MissingImports));
    prepare(&mut saved, 1);
    let before = reads(&mut saved);
    for (generation, id, side, expected) in [(2, 1, Side::Evidence, Error::StaleGeneration),
        (1, 0, Side::Evidence, Error::MissingImport), (1, 2, Side::Target, Error::MissingImport)] {
        assert_eq!(saved.open_import_reader(owner(57472), generation, id, side, || false).err(), Some(expected));
    }
    assert_eq!(saved.open_import_reader(owner(57471), 1, 1, Side::Target, || false).err(), Some(Error::OwnerMismatch));
    assert!(saved.open_import_reader(owner(57472), 1, 1, Side::Target, || true).err().unwrap().is_canceled());
    for (member, direction, start, limit) in [(None, Direction::Incoming, 0, 10),
        (Some(99), Direction::Outgoing, 0, 10), (None, Direction::All, 2, 10),
        (None, Direction::All, 0, 0), (None, Direction::All, 0, 129)] {
        assert_eq!(saved.import_page(1, member, direction, start, limit, || false).err(), Some(Error::InvalidLimits));
    }
    for (generation, limit, bytes) in [(2, 0, 1024), (3, MAX_SAVED_IMPORTS + 1, 1024),
        (4, 20, MAX_SAVED_IMPORT_SOURCE_BYTES + 1)] {
        assert_eq!(saved.prepare_imports(generation, limit, bytes, || false).err(), Some(Error::InvalidLimits));
    }
    assert_eq!(saved.import_generation(), Some(1));
    assert_eq!(saved.last_import_attempt(), 4);
    assert_eq!(reads(&mut saved), before);
}

#[test]
fn failed_and_final_checkpoint_canceled_replacements_preserve_imports_and_other_projections() {
    let source = b"from .b import Value\n";
    let f = Fixture::new(&[entry(b"README.md", b"[code](a.py)"), entry(b"a.py", source), entry(b"b.py", b"Value = 1\n")], true);
    let mut probe = f.open(57482);
    prepare(&mut probe, 1);
    let mut polls = 0;
    probe.prepare_imports(2, 64, 1024 * 1024, || { polls += 1; false }).unwrap();
    assert!(polls > 1);
    let mut saved = f.open(57481);
    saved.search(1, "Value", 20, || false).unwrap();
    saved.prepare_relationships(1, 20, 1024, || false).unwrap();
    prepare(&mut saved, 1);
    let old = json(&saved.import_page(1, None, Direction::All, 0, 64, || false).unwrap());
    let mut seen = 0;
    let canceled = saved.prepare_imports(2, 64, 1024 * 1024, || { seen += 1; seen == polls });
    assert!(matches!(canceled, Err(error) if error.is_canceled()));
    assert_eq!(saved.import_generation(), Some(1));
    assert_eq!(json(&saved.import_page(1, None, Direction::All, 0, 64, || false).unwrap()), old);
    assert_eq!(saved.prepare_imports(2, 64, 1024, || false).err(), Some(Error::StaleGeneration));
    assert_eq!(saved.prepare_imports(3, 0, 1024, || false).err(), Some(Error::InvalidLimits));
    assert!(saved.clear_imports(4, || true).err().unwrap().is_canceled());
    assert_eq!(saved.import_generation(), Some(1));
    assert_eq!(saved.accepted_generation(), Some(1));
    assert_eq!(saved.relationship_generation(), Some(1));
    saved.results(1, 0, 20, || false).unwrap();
    saved.relationship_page(1, None, RelationshipDirection::All, 0, 20, || false).unwrap();
    let info = json(&saved.info(|| false).unwrap());
    assert_eq!(info.get("accepted_import_generation").number(), 1);
    assert_eq!(info.get("last_import_attempt").number(), 4);
    let clear = json(&saved.clear_imports(5, || false).unwrap());
    assert_eq!(clear.get("accepted_import_generation"), &Json::Null);
    assert_eq!(clear.get("last_import_attempt").number(), 5);
    assert_eq!(saved.import_generation(), None);
    assert_eq!(saved.relationship_generation(), Some(1));
    assert_eq!(saved.accepted_generation(), Some(1));
    assert_eq!(saved.import_page(1, None, Direction::All, 0, 20, || false).err(), Some(Error::MissingImports));
}

#[test]
fn corrupt_archive_members_cannot_replace_accepted_metadata_or_reopen_retained_targets() {
    let source = b"from .helper import Value\n";
    let target = b"captured original target\n";
    let f = Fixture::new(&[entry(b"pkg/helper.py", target), entry(b"pkg/main.py", source)], true);
    let mut saved = f.open(57491);
    prepare(&mut saved, 1);
    let old = json(&saved.import_page(1, None, Direction::All, 0, 64, || false).unwrap());
    let (mut retained, _) = saved.open_import_reader(owner(57492), 1, 1, Side::Target, || false).unwrap();
    let at = f.bytes.windows(target.len()).position(|bytes| bytes == target).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap();
    writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.prepare_imports(2, 64, 1024, || false).is_err());
    assert_eq!(saved.import_generation(), Some(1));
    assert_eq!(json(&saved.import_page(1, None, Direction::All, 0, 64, || false).unwrap()), old);
    assert!(saved.open_import_reader(owner(57493), 1, 1, Side::Target, || false).is_err());
    assert_eq!(copy_all(&mut retained).get("original_hex").text(), hex(target));
}

#[test]
fn owner_qualified_archives_with_equal_generations_never_share_import_authority() {
    let first = Fixture::new(&[entry(b"a.py", b"from .b import Value\n"), entry(b"b.py", b"Value = 1\n")], true);
    let second = Fixture::new(&[entry(b"a.py", b"from .c import Value\n"), entry(b"c.py", b"Value = 2\n")], true);
    let mut left = first.open(57501); let mut right = second.open(57502);
    let l = prepare(&mut left, 1); let r = prepare(&mut right, 1);
    assert_eq!(l.get("import_generation"), r.get("import_generation"));
    assert_eq!(l.get("links").array()[0].get("link_id"), r.get("links").array()[0].get("link_id"));
    assert_ne!(l.get("owner"), r.get("owner"));
    assert_ne!(l.get("archive_digest"), r.get("archive_digest"));
    let (mut left_reader, left_receipt) = left.open_import_reader(owner(57503), 1, 1, Side::Target, || false).unwrap();
    let (mut right_reader, right_receipt) = right.open_import_reader(owner(57504), 1, 1, Side::Target, || false).unwrap();
    assert_eq!(json(&left_receipt).get("owner").number(), 57501);
    assert_eq!(json(&right_receipt).get("owner").number(), 57502);
    assert_eq!(copy_all(&mut left_reader).get("original_hex").text(), hex(b"Value = 1\n"));
    assert_eq!(copy_all(&mut right_reader).get("original_hex").text(), hex(b"Value = 2\n"));
}

