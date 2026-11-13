#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry};
use support::{Json, parse};
use std::{ffi::OsString, fs};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

const STATEMENT: &[u8] = b"from .helper import run as main";
const SOURCE: &[u8] = b"# saved\nfrom .helper import run as main\n";
const TARGET: &[u8] = b"def run():\n    return 7\n";
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
fn error(row: &Json) -> &str { row.get("error").get("code").text() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn saved_import_browsing_enters_annotations_and_restorable_desk_without_live_source_files() {
    let f = Fixture::new(&[entry(b"pkg/helper.py", TARGET), entry(b"pkg/main.py", SOURCE)], true);
    let checkpoint = f.root.join("import-notes.fcbk");
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-imports-build\t1\t7\t64\t4096\n\
         0\tsaved-imports-from\t1\t7\t1\t0\t64\n\
         0\tsaved-importers\t1\t7\t0\t0\t64\n\
         0\tsaved-import-source\t1\t7\t1\n\
         5\tcopy\t1\n\
         5\tannotate\t1\tWhy this import exists\tRetain the exact archived statement\n\
         7\tpin\t1\n\
         8\tsaved-import-target\t1\t7\t1\n\
         9\tread\t2\t0\t64\t4\n\
         9\tsaved-close\t1\n\
         9\trecall\t1\n\
         12\tcopy\t1\n\
         12\tsave\t{}\t1024\n\
         12\tquit\n", f.path.display(), checkpoint.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 15);
    assert_eq!(rows[1].get("saved_import_generation").number(), 7);
    assert_eq!(rows[1].get("last_saved_import_attempt").number(), 7);
    assert_eq!(rows[1].get("accepted_revision").number(), 0);
    assert_eq!(result(&rows[2]).get("direction").text(), "outgoing");
    assert_eq!(result(&rows[3]).get("direction").text(), "incoming");
    assert_eq!(result(&rows[2]).get("links"), result(&rows[3]).get("links"));
    assert_eq!(result(&rows[4]).get("selection_namespace").text(), "saved-import");
    assert_eq!(result(&rows[4]).get("import_generation").number(), 7);
    assert_eq!(result(&rows[4]).get("import_side").text(), "evidence");
    assert_eq!(result(&rows[4]).get("selection").get("start").number(), 8);
    assert_eq!(result(&rows[5]).get("original_hex").text(), hex(STATEMENT));
    assert_eq!(result(&rows[8]).get("import_side").text(), "target");
    assert_eq!(result(&rows[8]).get("selection"), &Json::Null);
    assert_eq!(result(&rows[9]).get("text").text(), std::str::from_utf8(TARGET).unwrap());
    assert_eq!(rows[10].get("saved_import_generation"), &Json::Null);
    assert_eq!(rows[10].get("last_saved_import_attempt"), &Json::Null);
    assert_eq!(result(&rows[12]).get("original_hex").text(), hex(STATEMENT));
    assert!(!f.root.join("pkg/main.py").exists() && !f.root.join("pkg/helper.py").exists());
    fs::rename(&f.path, f.root.join("retired.fcbs")).unwrap();

    let (exit, restored) = invoke(&format!("0\trestore\t{}\n1\trecall\t1\n2\tcopy\t1\n2\tquit\n",
        checkpoint.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&restored[2]).get("original_hex").text(), hex(STATEMENT));
    assert_eq!(result(&restored[3]).get("bookmarks").array()[0].get("body").text(),
        "Retain the exact archived statement");
    assert_eq!(result(&restored[3]).get("initial_source_bytes_read").number(), 0);
    assert_eq!(restored[3].get("saved_token"), &Json::Null);
    assert_eq!(restored[3].get("saved_import_generation"), &Json::Null);
}

#[test]
fn failed_import_builds_and_clears_preserve_generation_without_aliasing_text_docs_or_archive_tokens() {
    let f = Fixture::new(&[entry(b"pkg/helper.py", TARGET), entry(b"pkg/main.py", SOURCE)], true);
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-imports-build\t1\t5\t64\t4096\n\
         0\tsaved-find\t1\t5\t10\tmain\n\
         0\tsaved-links-build\t1\t5\t64\t4096\n\
         0\tsaved-imports-build\t1\t6\t0\t4096\n\
         0\tsaved-imports-page\t1\t5\t0\t64\n\
         0\tsaved-imports-clear\t1\t6\n\
         0\tsaved-imports-clear\t1\t7\n\
         0\tsaved-imports-page\t1\t5\t0\t64\n\
         0\tsaved-imports-build\t1\t8\t64\t4096\n\
         0\tsaved-open\t{}\n\
         0\tsaved-imports-page\t1\t8\t0\t64\n\
         0\tsaved-imports-build\t11\t1\t64\t4096\n\
         0\tquit\n", f.path.display(), f.path.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows.len(), 14);
    assert_eq!(error(&rows[4]), "SAVED_IMPORT_INVALID_LIMITS");
    assert_eq!(rows[4].get("saved_import_generation").number(), 5);
    assert_eq!(rows[4].get("last_saved_import_attempt").number(), 6);
    assert_eq!(rows[4].get("saved_query_generation").number(), 5);
    assert_eq!(rows[4].get("saved_relationship_generation").number(), 5);
    assert_eq!(result(&rows[5]).get("links").array().len(), 1);
    assert_eq!(error(&rows[6]), "SAVED_IMPORT_STALE_GENERATION");
    assert_eq!(rows[7].get("saved_import_generation"), &Json::Null);
    assert_eq!(rows[7].get("last_saved_import_attempt").number(), 7);
    assert_eq!(rows[7].get("saved_query_generation").number(), 5);
    assert_eq!(rows[7].get("saved_relationship_generation").number(), 5);
    assert_eq!(error(&rows[8]), "SAVED_IMPORT_NOT_PREPARED");
    assert_eq!(error(&rows[11]), "DESK_STALE_SAVED_REPOSITORY");
    assert_eq!(rows[12].get("saved_import_generation").number(), 1);
    assert_eq!(rows[12].get("last_saved_import_attempt").number(), 1);
    assert_eq!(rows[12].get("saved_query_generation"), &Json::Null);
    assert_eq!(rows[12].get("saved_relationship_generation"), &Json::Null);
    assert_eq!(rows[13].get("accepted_revision").number(), 0);
}

#[test]
fn partial_import_coverage_and_refused_targets_still_allow_exact_source_evidence() {
    let raw = b"from .blocked import Tool\nimport os\n";
    let f = Fixture::new(&[
        SnapshotEntry { path: b"pkg/blocked.py", observed_bytes: 99, data: SnapshotData::Unavailable("READ_DENIED") },
        entry(b"pkg/main.py", raw),
    ], true);
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-imports-build\t1\t1\t64\t0\n\
         0\tsaved-imports-build\t1\t2\t64\t4096\n\
         0\tsaved-importers\t1\t2\t0\t0\t64\n\
         0\tsaved-import-target\t1\t2\t1\n\
         0\tsaved-import-source\t1\t2\t1\n\
         6\tcopy\t1\n\
         6\tsaved-import-target\t1\t2\t2\n\
         6\tquit\n", f.path.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[1].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert_eq!(result(&rows[1]).get("source_limited_members").number(), 1);
    assert_eq!(result(&rows[1]).get("retained_links").number(), 0);
    assert!(!result(&rows[2]).get("imports_complete").flag());
    assert_eq!(result(&rows[2]).get("unavailable_source_members").number(), 1);
    assert_eq!(result(&rows[3]).get("links").array()[0].get("resolution").text(), "unavailable");
    assert_eq!(error(&rows[4]), "SAVED_IMPORT_TARGET_UNAVAILABLE");
    assert_eq!(rows[4].get("accepted_revision").number(), 0);
    assert_eq!(result(&rows[6]).get("original_hex").text(), hex(b"from .blocked import Tool"));
    assert_eq!(error(&rows[7]), "SAVED_IMPORT_TARGET_UNRESOLVED");
    assert_eq!(rows[7].get("accepted_revision").number(), 6);
    assert_eq!(rows[8].get("saved_import_generation").number(), 2);
}

