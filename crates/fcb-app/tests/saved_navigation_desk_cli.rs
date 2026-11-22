#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Real bounded stdio frames compose saved navigation with retained Desk state.
//! No fixture source path exists; activation must use the verified archive.

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry};
use support::{Json, parse};
use std::{ffi::OsString, fs};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn invoke(script: &str) -> (u8, Vec<Json>) {
    let arguments: Vec<OsString> = vec!["desk".into(), "--stdio".into()];
    let mut input = script.as_bytes();
    let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&arguments, &mut input, &mut output, &mut errors, || false);
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    let rows = output.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| {
        let row = parse(line).unwrap();
        assert_eq!(row.get("schema").text(), "fcb.desk-stdio/1");
        row
    }).collect();
    (exit, rows)
}
fn result(row: &Json) -> &Json { row.get("result") }
fn error(row: &Json) -> &str { row.get("error").get("code").text() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn path_symbol_and_literal_navigation_share_capture_and_preserve_notes_after_detach() {
    let raw = b"fn target() {}\n// target\n";
    let f = Fixture::new(&[entry(b"src/source.rs", raw)], true);
    let checkpoint = f.root.join("navigation.fcbk");
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-paths\t1\t1\tfuzzy\tunicode-lowercase\t8\t1048576\tsource.rs\n\
         0\tsaved-path-hit\t1\t1\t1\n\
         3\tsaved-structure\t1\t1\tsymbols\texact\t8\t1048576\t0\ttarget\n\
         3\tsaved-structure-hit\t1\t1\t1\n\
         5\tcopy\t1\n\
         5\tsaved-find\t1\t1\t8\ttarget\n\
         5\tsaved-hit\t1\t1\t1\n\
         8\tannotate\t1\tTarget declaration\tKeep this archived definition\n\
         9\tsaved-close\t1\n\
         9\tcopy\t1\n\
         9\tsave\t{}\t1024\n\
         9\tquit\n", f.path.display(), checkpoint.display());
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 13);
    assert_eq!(result(&rows[1]).get("member_bytes_read").number(), 0);
    assert_eq!(result(&rows[2]).get("selection_namespace").text(), "saved-paths");
    assert_eq!(result(&rows[2]).get("selection"), &Json::Null);
    assert_eq!(result(&rows[4]).get("selection_namespace").text(), "saved-structure");
    assert_eq!(result(&rows[4]).get("structure").get("evidence_level").text(), "heuristic-outline-candidate");
    assert!(!result(&rows[4]).get("structure").get("semantic_complete").flag());
    assert!(result(&rows[4]).get("reused_capture").flag());
    assert!(result(&rows[7]).get("reused_capture").flag());
    assert_eq!(result(&rows[2]).get("file_id"), result(&rows[7]).get("file_id"));
    assert_eq!(rows[7].get("saved_path_generation").number(), 1);
    assert_eq!(rows[7].get("saved_structure_generation").number(), 1);
    assert_eq!(rows[7].get("saved_query_generation").number(), 1);
    for index in [5, 10] { assert_eq!(result(&rows[index]).get("original_hex").text(), hex(b"target")); }
    assert_eq!(rows[9].get("saved_token"), &Json::Null);
    assert_eq!(result(&rows[12]).get("retained_sources").number(), 1);
    assert!(!f.root.join("src/source.rs").exists());

    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    let (exit, restored) = invoke(&format!("0\trestore\t{}\n1\tcopy\t1\n1\tquit\n", checkpoint.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&restored[1]).get("original_hex").text(), hex(b"target"));
    let note = &result(&restored[2]).get("bookmarks").array()[0];
    assert_eq!(note.get("label").text(), "Target declaration");
    assert_eq!(note.get("body").text(), "Keep this archived definition");
    assert_eq!(result(&restored[2]).get("initial_source_bytes_read").number(), 0);
    assert_eq!(restored[2].get("saved_token"), &Json::Null);
}

#[test]
fn raw_path_hex_and_later_utf16_occurrences_keep_exact_source_ranges() {
    let mut raw = vec![0xff, 0xfe];
    raw.extend("needle needle needle\n".encode_utf16().flat_map(u16::to_le_bytes));
    let name = b"raw-\xff.rs";
    let f = Fixture::new(&[entry(name, &raw)], true);
    let script = format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-paths-hex\t1\t1\texact\tsensitive\t8\t1048576\t{}\n\
         0\tsaved-path-hit\t1\t1\t1\n\
         3\tsaved-structure-text-hex\t1\t1\toccurrences\texact\t1\t1048576\t0\t{}\n\
         3\tsaved-structure-page\t1\t1\t0\t1\n\
         3\tsaved-structure-hit\t1\t1\t1\n\
         6\tcopy\t1\n\
         6\tsaved-structure\t1\t2\toccurrences\texact\t2\t1048576\t2\tneedle\n\
         6\tsaved-structure-hit\t1\t1\t1\n\
         6\tsaved-structure-hit\t1\t2\t1\n\
         10\tcopy\t1\n\
         10\tquit\n", f.path.display(), hex(name), hex(b"needle"));
    let (exit, rows) = invoke(&script);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows.len(), 12);
    assert_eq!(result(&rows[1]).get("hits").array()[0].get("path").get("hex").text(), hex(name));
    assert_eq!(result(&rows[1]).get("member_bytes_read").number(), 0);
    assert_eq!(rows[3].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert_eq!(result(&rows[3]).get("next_match_offset").number(), 1);
    assert!(result(&rows[3]).get("includes_comments_and_literals").flag());
    assert_eq!(result(&rows[4]).get("hits"), result(&rows[3]).get("hits"));
    assert_eq!(result(&rows[5]).get("selection").get("start").number(), 2);
    assert_eq!(result(&rows[5]).get("selection").get("end").number(), 14);
    assert_eq!(result(&rows[6]).get("original_hex").text(), hex(&raw[2..14]));
    assert!(result(&rows[7]).get("complete").flag());
    assert_eq!(result(&rows[7]).get("skipped_matches").number(), 2);
    assert_eq!(result(&rows[7]).get("next_match_offset"), &Json::Null);
    assert_eq!(error(&rows[8]), "SAVED_SESSION_STALE_QUERY");
    assert_eq!(rows[8].get("accepted_revision").number(), 6);
    assert_eq!(result(&rows[9]).get("structure_hit_id").number(), 1);
    assert_eq!(result(&rows[9]).get("selection").get("start").number(), 30);
    assert_eq!(result(&rows[9]).get("selection").get("end").number(), 42);
    assert_eq!(result(&rows[10]).get("original_hex").text(), hex(&raw[30..42]));
    assert!(result(&rows[5]).get("reused_capture").flag() && result(&rows[9]).get("reused_capture").flag());
    assert_eq!(rows[10].get("saved_path_generation").number(), 1);
    assert_eq!(rows[10].get("saved_structure_generation").number(), 2);
    assert_eq!(result(&rows[11]).get("retained_sources").number(), 1);
}

#[test]
fn saved_heading_activation_keeps_qualified_original_bytes_after_archive_close() {
    let raw = b"\xef\xbb\xbf# Guide\n\nArchived text.\n";
    let f = Fixture::new(&[entry(b"README.md", raw)], true);
    let (exit, rows) = invoke(&format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-structure\t1\t1\theadings\texact\t8\t1048576\t0\tGuide\n\
         0\tsaved-structure-hit\t1\t1\t1\n\
         3\tsaved-close\t1\n\
         3\tcopy\t1\n\
         3\tquit\n", f.path.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 6);
    let hit = &result(&rows[1]).get("hits").array()[0];
    assert_eq!(hit.get("structure").get("heading_slug").text(), "guide");
    let selected = result(&rows[2]).get("selection");
    assert_eq!(selected, hit.get("original_range"));
    assert_eq!(selected.get("start").number(), 3);
    let end = selected.get("end").number() as usize;
    assert!(raw[3..end].starts_with(b"# Guide"));
    assert_eq!(result(&rows[2]).get("structure").get("evidence_level").text(), "fmd-heading");
    assert_eq!(result(&rows[4]).get("original_hex").text(), hex(&raw[3..end]));
    assert_eq!(rows[3].get("saved_structure_generation"), &Json::Null);
    assert!(!result(&rows[2]).get("live_source_reopened").flag());
}

#[test]
fn failed_attempts_clears_and_replacement_keep_query_namespaces_separate() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"fn target() {}\n")], true);
    let replacement = Fixture::new(&[entry(b"src/a.rs", b"fn changed() {}\n")], true);
    let (exit, rows) = invoke(&format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-find\t1\t1\t8\ttarget\n\
         0\tsaved-paths\t1\t1\tfuzzy\tsensitive\t8\t1048576\ta.rs\n\
         0\tsaved-structure\t1\t1\tsymbols\texact\t8\t1048576\t0\ttarget\n\
         0\tsaved-paths\t1\t2\tfuzzy\tsensitive\t257\t1048576\ta.rs\n\
         0\tsaved-structure\t1\t2\toccurrences\tprefix\t8\t1048576\t0\ttarget\n\
         0\tsaved-path-clear\t1\t3\n\
         0\tsaved-path-hit\t1\t1\t1\n\
         0\tsaved-structure-hit\t1\t1\t1\n\
         9\tsaved-paths\t1\t2\tfuzzy\tsensitive\t8\t1048576\ta.rs\n\
         9\tsaved-structure\t1\t1\tsymbols\texact\t8\t1048576\t0\ttarget\n\
         9\tsaved-open\t{}\n\
         9\tsaved-structure-hit\t1\t1\t1\n\
         9\tread\t1\t0\t64\t4\n\
         9\tsaved-paths\t12\t1\tfuzzy\tsensitive\t8\t1048576\ta.rs\n\
         9\tsaved-path-hit\t12\t1\t1\n\
         16\tquit\n", f.path.display(), replacement.path.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows.len(), 17);
    for index in [4, 5] { assert_eq!(rows[index].get("exit_code").number(), EXIT_ERROR as u64); }
    assert_eq!(rows[5].get("saved_path_generation").number(), 1);
    assert_eq!(rows[5].get("last_saved_path_attempt").number(), 2);
    assert_eq!(rows[5].get("saved_structure_generation").number(), 1);
    assert_eq!(rows[5].get("last_saved_structure_attempt").number(), 2);
    assert_eq!(rows[5].get("saved_query_generation").number(), 1);
    assert_eq!(rows[6].get("saved_path_generation"), &Json::Null);
    assert_eq!(rows[6].get("last_saved_path_attempt").number(), 3);
    assert_eq!(error(&rows[7]), "SAVED_SESSION_NO_QUERY");
    assert_eq!(rows[8].get("accepted_revision").number(), 9);
    for index in [9, 10] { assert_eq!(error(&rows[index]), "SAVED_SESSION_STALE_QUERY"); }
    assert_eq!(rows[11].get("saved_token").number(), 12);
    assert_eq!(rows[11].get("last_saved_path_attempt").number(), 0);
    assert_eq!(rows[11].get("last_saved_structure_attempt").number(), 0);
    assert_eq!(error(&rows[12]), "DESK_STALE_SAVED_REPOSITORY");
    assert_eq!(result(&rows[13]).get("text").text(), "fn target() {}\n");
    assert!(!result(&rows[15]).get("reused_capture").flag());
    assert_ne!(result(&rows[8]).get("file_id"), result(&rows[15]).get("file_id"));
}

#[test]
fn work_limits_missing_sources_and_extra_frame_fields_remain_explicit() {
    let f = Fixture::new(&[entry(b"available.rs", b"fn target() {}\n"),
        SnapshotEntry { path: b"missing.rs", observed_bytes: 200,
            data: SnapshotData::Unavailable("READ_DENIED") }], false);
    let (exit, rows) = invoke(&format!(
        "0\tsaved-open\t{}\n\
         0\tsaved-paths\t1\t1\tfuzzy\tsensitive\t0\t1048576\trs\n\
         0\tsaved-paths\t1\t2\tfuzzy\tsensitive\t2\t0\trs\n\
         0\tsaved-structure\t1\t1\tsymbols\texact\t8\t0\t0\ttarget\n\
         0\tsaved-structure\t1\t2\tsymbols\texact\t8\t1048576\t0\ttarget\n\
         0\tsaved-structure-hit\t1\t2\t1\n\
         6\tsaved-paths\t1\t3\texact\tsensitive\t2\t1048576\tmissing.rs\n\
         6\tsaved-info\t1\n\
         6\tsaved-path-hit\t1\t3\t1\n\
         6\tsaved-info\t1\n\
         6\tsaved-structure\t1\t3\tsymbols\texact\t8\t1048576\t0\ttarget\textra\n\
         6\tquit\n", f.path.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows.len(), 12);
    let paths = result(&rows[1]);
    assert_eq!(rows[1].get("exit_code").number(), EXIT_OK as u64);
    assert!(paths.get("search_complete").flag() && paths.get("top_k_complete").flag());
    assert!(!paths.get("discovery_complete").flag());
    assert_eq!(paths.get("matches_seen").number(), 2);
    assert!(paths.get("hits").array().is_empty() && paths.get("truncated").flag());
    assert_eq!(result(&rows[2]).get("state").text(), "work-limit");
    assert_eq!(result(&rows[2]).get("work_units").number(), 0);
    for index in [2, 3, 4] { assert_eq!(rows[index].get("exit_code").number(), EXIT_PARTIAL as u64); }
    assert_eq!(result(&rows[3]).get("member_bytes_read").number(), 0);
    assert!(!result(&rows[4]).get("complete").flag());
    assert_eq!(result(&rows[4]).get("unavailable_files").number(), 1);
    assert_eq!(rows[5].get("exit_code").number(), EXIT_OK as u64);
    assert!(!result(&rows[6]).get("hits").array()[0].get("captured").flag());
    assert_eq!(error(&rows[8]), "SNAPSHOT_MEMBER_UNAVAILABLE");
    assert_eq!(rows[8].get("accepted_revision").number(), 6);
    assert_eq!(result(&rows[7]).get("member_bytes_read"), result(&rows[9]).get("member_bytes_read"));
    assert_eq!(error(&rows[10]), "DESK_FIELD_LIMIT");
    assert_eq!(rows[10].get("last_saved_structure_attempt").number(), 2);
    assert_eq!(rows[11].get("accepted_revision").number(), 6);
}
