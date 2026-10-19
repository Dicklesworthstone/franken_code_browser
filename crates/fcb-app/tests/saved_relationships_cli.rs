#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry};
use support::{Json, parse};
use std::{ffi::OsString, fs};
use fcb::search::snapshot::{SnapshotEntry, SnapshotData};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

const DOC: &[u8] = b"---\ntitle: Guide\n---\n\n# Guide\n\nSee [engine](src/lib.rs#entry).\n";
const SOURCE: &[u8] = b"fn entry() { /* exact saved code */ }\n";
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let args = [OsString::from("desk"), OsString::from("--stdio")];
    let mut input = script.as_bytes(); let mut out = Vec::new(); let mut error = Vec::new();
    let exit = run(&args, &mut input, &mut out, &mut error, || false);
    assert!(error.is_empty(), "{}", String::from_utf8_lossy(&error));
    let rows = out.split(|b| *b == b'\n').filter(|row| !row.is_empty())
        .map(|row| parse(row).unwrap()).collect();
    (exit, rows)
}
fn result(row: &Json) -> &Json { row.get("result") }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn source_to_document_to_source_navigation_can_be_annotated_and_saved_without_live_paths() {
    let f = Fixture::new(&[entry(b"README.md", DOC), entry(b"src/lib.rs", SOURCE)], true);
    let checkpoint = f.root.join("documentation-notes.fcbdesk");
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-links-build\t1\t1\t64\t4096\n\
         0\tsaved-backlinks\t1\t1\t1\t0\t64\n\
         0\tsaved-link-source\t1\t1\t1\n\
         4\tcopy\t1\n\
         4\tannotate\t1\tDocumentation for entry\tWhy this code exists\n\
         6\tpin\t1\n\
         7\tsaved-link-target\t1\t1\t1\n\
         8\tread\t2\t0\t64\t4\n\
         8\tsaved-close\t1\n\
         8\trecall\t1\n\
         11\tcopy\t1\n\
         11\tsave\t{}\t1024\n\
         11\tquit\n", f.path.display(), checkpoint.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 14);
    assert_eq!(rows[1].get("saved_relationship_generation").number(), 1);
    assert_eq!(result(&rows[2]).get("links").array().len(), 1);
    assert_eq!(result(&rows[2]).get("evidence_precision").text(), "whole-captured-markdown-document");
    assert_eq!(result(&rows[3]).get("selection_namespace").text(), "saved-relationship");
    assert_eq!(result(&rows[3]).get("relationship_side").text(), "evidence");
    assert_eq!(result(&rows[3]).get("selection").get("start").number(), 0);
    assert_eq!(result(&rows[3]).get("selection").get("end").number(), DOC.len() as u64);
    assert_eq!(result(&rows[4]).get("original_hex").text(), hex(DOC));
    assert_eq!(result(&rows[7]).get("fragment_resolution").text(), "unresolved");
    assert_eq!(result(&rows[7]).get("selection"), &Json::Null);
    assert_eq!(result(&rows[8]).get("text").text(), std::str::from_utf8(SOURCE).unwrap());
    assert_eq!(rows[9].get("saved_relationship_generation"), &Json::Null);
    assert_eq!(result(&rows[11]).get("original_hex").text(), hex(DOC));
    assert!(!f.root.join("README.md").exists() && !f.root.join("src/lib.rs").exists());
    fs::rename(&f.path, f.root.join("retired.fcbs")).unwrap();

    let (exit, restored) = invoke(&format!("0\trestore\t{}\n1\trecall\t1\n2\tcopy\t1\n2\tquit\n", checkpoint.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&restored[2]).get("original_hex").text(), hex(DOC));
    assert_eq!(result(&restored[3]).get("bookmarks").array()[0].get("body").text(), "Why this code exists");
    assert_eq!(result(&restored[3]).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn omitted_documents_keep_backlink_results_explicitly_partial_even_when_navigation_succeeds() {
    let f = Fixture::new(&[
        entry(b"README.md", DOC),
        SnapshotEntry { path: b"other.md", observed_bytes: 100, data: SnapshotData::Unavailable("READ_DENIED") },
        entry(b"src/lib.rs", SOURCE),
    ], true);
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-links-build\t1\t1\t64\t0\n\
         0\tsaved-links-build\t1\t2\t64\t4096\n\
         0\tsaved-backlinks\t1\t2\t2\t0\t64\n\
         0\tsaved-link-target\t1\t2\t1\n\
         5\tread\t1\t0\t64\t4\n\
         5\tquit\n", f.path.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!result(&rows[1]).get("relationships_complete").flag());
    assert_eq!(result(&rows[1]).get("source_limited_members").number(), 1);
    assert_eq!(result(&rows[1]).get("retained_links").number(), 0);
    assert!(!result(&rows[2]).get("relationships_complete").flag());
    assert_eq!(result(&rows[2]).get("unavailable_markdown_members").number(), 1);
    assert_eq!(result(&rows[3]).get("links").array().len(), 1);
    assert!(!result(&rows[3]).get("relationships_complete").flag());
    assert_eq!(rows[4].get("exit_code").number(), EXIT_OK as u64);
    assert_eq!(result(&rows[5]).get("text").text(), std::str::from_utf8(SOURCE).unwrap());
}

#[test]
fn relationship_generations_tokens_and_failed_replacements_are_independent_of_text_queries() {
    let f = Fixture::new(&[entry(b"README.md", DOC), entry(b"src/lib.rs", SOURCE)], true);
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-links-build\t1\t5\t64\t4096\n\
         0\tsaved-find\t1\t5\t10\tentry\n\
         0\tsaved-links-clear\t1\t5\n\
         0\tsaved-links-page\t1\t5\t0\t64\n\
         0\tsaved-links-build\t1\t6\t0\t4096\n\
         0\tsaved-links-from\t1\t5\t0\t0\t64\n\
         0\tsaved-open\t{}\n\
         0\tsaved-links-page\t1\t5\t0\t64\n\
         0\tsaved-links-build\t8\t1\t64\t4096\n\
         0\tquit\n", f.path.display(), f.path.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[2].get("saved_query_generation").number(), 5);
    assert_eq!(rows[3].get("exit_code").number(), EXIT_ERROR as u64);
    assert_eq!(rows[4].get("saved_relationship_generation").number(), 5);
    assert_eq!(result(&rows[4]).get("links").array().len(), 1);
    assert_eq!(rows[5].get("exit_code").number(), EXIT_ERROR as u64);
    assert_eq!(result(&rows[6]).get("links").array().len(), 1);
    assert_eq!(rows[8].get("error").get("code").text(), "DESK_STALE_SAVED_REPOSITORY");
    assert_eq!(rows[9].get("saved_relationship_generation").number(), 1);
    assert_eq!(rows[9].get("saved_query_generation"), &Json::Null);
    assert_eq!(rows[10].get("accepted_revision").number(), 0);
}
