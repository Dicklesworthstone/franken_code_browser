#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use support::{parse, Json};
use saved_fixture::{Fixture, entry};
use std::{ffi::OsString, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture(bytes: &[u8]) -> (PathBuf, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-code-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); let path = root.join("source.rs"); fs::write(&path, bytes).unwrap(); (root, path)
}
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let args: Vec<OsString> = vec!["desk".into(), "--stdio".into()];
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args, &mut input, &mut output, &mut errors, || false);
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    let rows = output.split(|b| *b == b'\n').filter(|s| !s.is_empty()).map(|line| {
        let row = parse(line).unwrap(); assert_eq!(row.get("schema").text(), "fcb.desk-stdio/1"); row
    }).collect(); (exit, rows)
}
fn result(row: &Json) -> &Json { row.get("result") }
fn error(row: &Json) -> &str { row.get("error").get("code").text() }

#[test]
fn declaration_to_reference_to_bookmark_checkpoint_is_one_source_workflow() {
    let (root, path) = fixture(b"fn alpha() {}\nfn beta() { alpha(); }\n// alpha\n"); let checkpoint = root.join("desk.fcbk");
    let script = format!("0\topen\t{}\n1\tcode-outline\t1\t1\tauto\n1\tcode-find\t1\t1\texact\t0\t10\tbeta\n1\tcode-select\t1\t1\t2\tname\n4\tcopy\t1\n4\tcode-symbol-references\t1\t1\t1\t1\t10\n4\tcode-ref-select\t1\t1\t2\n7\tbookmark\t1\tuse evidence\n8\tsave\t{}\t1024\n8\tquit\n", path.display(), checkpoint.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 10);
    assert_eq!(result(&rows[2]).get("symbols").array()[0].get("symbol_id").number(), 2);
    assert_eq!(result(&rows[4]).get("original_hex").text(), "62657461");
    assert_eq!(result(&rows[5]).get("retained_references").number(), 3);
    assert!(!result(&rows[5]).get("compiler_resolved").flag());
    assert_eq!(rows[9].get("code_outlines").array().len(), 1); assert_eq!(rows[9].get("code_references").array().len(), 1);
    fs::rename(&path, root.join("moved.rs")).unwrap();
    let (exit, reopened) = invoke(&format!("0\trestore\t{}\n1\tcopy\t1\n1\tcode-outline\t1\t1\tauto\n1\tquit\n", checkpoint.display()));
    assert_eq!(exit, EXIT_OK); assert!(reopened[0].get("code_outlines").array().is_empty());
    assert!(reopened[0].get("code_references").array().is_empty());
    assert_eq!(result(&reopened[1]).get("original_hex").text(), "616c706861");
    assert_eq!(result(&reopened[2]).get("retained_symbols").number(), 2);
    assert_eq!(result(&reopened[3]).get("bookmarks").array()[0].get("label").text(), "use evidence");
    assert_eq!(result(&reopened[3]).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn code_navigation_does_not_advance_pending_live_repository_work() {
    let (root, _) = fixture(b"fn alpha() {}\nalpha();\n");
    let script = format!("0\trepo-open\t{}\n0\trepo-begin\t1\t1\t10\t4096\talpha\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tsource.rs\n0\trepo-path-open\t1\t1\t1\n4\tcode-outline\t1\t1\tauto\n4\tcode-symbol-references\t1\t1\t1\t1\t10\n4\trepo-progress\t1\t1\n4\trepo-cancel\t1\t1\n4\tcode-ref-select\t1\t1\t2\n9\tcopy\t1\n9\tquit\n", root.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[5].get("repository_pending_query_generation").number(), 1);
    assert_eq!(result(&rows[6]).get("step_count").number(), 0); assert_eq!(result(&rows[6]).get("source_bytes_read").number(), 0);
    assert_eq!(result(&rows[9]).get("original_hex").text(), "616c706861");
    assert!(!rows[10].get("repository_work_pending").flag());
}

#[test]
fn archived_source_outline_and_references_remain_after_archive_detachment() {
    let f = Fixture::new(&[entry(b"source.rs", b"fn target() {}\ntarget();\n")], true);
    let script = format!("0\tsaved-open\t{}\n0\tsaved-find\t1\t1\t10\ttarget\n0\tsaved-hit\t1\t1\t2\n3\tcode-outline\t1\t1\tauto\n3\tcode-symbol-references\t1\t1\t1\t1\t10\n3\tsaved-close\t1\n3\tcode-ref-select\t1\t1\t2\n7\tcopy\t1\n7\tquit\n", f.path.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[5].get("saved_token"), &Json::Null);
    assert_eq!(rows[8].get("code_outlines").array().len(), 1); assert_eq!(rows[8].get("code_references").array().len(), 1);
    assert_eq!(result(&rows[7]).get("original_hex").text(), "746172676574");
    assert_eq!(result(&rows[8]).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn failed_replacement_and_outline_clear_preserve_independent_reference_results() {
    let (_, path) = fixture(b"fn target() {}\ntarget();\n");
    let script = format!("0\topen\t{}\n1\tcode-outline\t1\t10\tauto\n1\tcode-outline\t1\t11\tauto\t0\n1\tcode-outline\t1\t11\tauto\n1\tcode-symbols\t1\t10\t0\t10\n1\tcode-references\t1\t20\t10\ttarget\n1\tcode-references\t1\t21\t10\ttwo words\n1\tcode-ref-page\t1\t20\t0\t10\n1\tcode-clear\t1\t10\n1\tcode-ref-select\t1\t20\t2\n10\tcopy\t1\n10\tquit\n", path.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error(&rows[2]), "SYMBOL_INVALID_LIMITS"); assert_eq!(error(&rows[3]), "DESK_CODE_STALE_GENERATION");
    assert_eq!(rows[3].get("last_code_outline_attempt").number(), 11);
    assert_eq!(result(&rows[4]).get("retained_symbols").number(), 1);
    assert_eq!(error(&rows[6]), "REFERENCE_INVALID_NAME"); assert_eq!(rows[6].get("last_code_reference_attempt").number(), 21);
    assert_eq!(result(&rows[7]).get("retained_references").number(), 2);
    assert!(rows[8].get("code_outlines").array().is_empty());
    assert_eq!(rows[8].get("code_references").array()[0].get("generation").number(), 20);
    assert_eq!(result(&rows[10]).get("original_hex").text(), "746172676574");
}

#[test]
fn duplicate_panes_select_independently_and_close_retires_only_its_code_index() {
    let (_, path) = fixture(b"fn alpha() {}\nfn beta() {}\n");
    let script = format!("0\topen\t{}\n1\tduplicate\t1\n2\tcode-outline\t1\t1\tauto\n2\tcode-outline\t2\t2\tauto\n2\tcode-select\t1\t1\t1\tname\n5\tcode-select\t2\t2\t2\tname\n6\tcopy\t1\n6\tcopy\t2\n6\tclose\t1\n9\tcode-symbols\t2\t2\t0\t10\n9\tquit\n", path.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[6]).get("original_hex").text(), "616c706861");
    assert_eq!(result(&rows[7]).get("original_hex").text(), "62657461");
    assert_eq!(rows[8].get("code_outlines").array().len(), 1);
    assert_eq!(rows[8].get("code_outlines").array()[0].get("pane").number(), 2);
    assert_eq!(result(&rows[9]).get("retained_symbols").number(), 2);
    assert_eq!(result(&rows[10]).get("retained_sources").number(), 1);
}

#[test]
fn retargeting_a_reused_pane_cannot_recycle_analysis_generations() {
    let (root, old) = fixture(b"fn original() {}\n"); let new = root.join("new.rs"); fs::write(&new, b"fn replacement() {}\n").unwrap();
    let script = format!("0\topen\t{}\n1\tcode-outline\t1\t1\tauto\n1\tcode-references\t1\t1\t10\toriginal\n1\topen\t{}\n4\tcode-outline\t1\t1\tauto\n4\tcode-references\t1\t1\t10\treplacement\n4\tcode-outline\t1\t2\tauto\n4\tcode-select\t1\t1\t1\tname\n4\tcode-select\t1\t2\t1\tname\n9\tcopy\t1\n9\tquit\n", old.display(), new.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert!(rows[3].get("code_outlines").array().is_empty()); assert!(rows[3].get("code_references").array().is_empty());
    for at in [4, 5, 7] { assert_eq!(error(&rows[at]), "DESK_CODE_STALE_GENERATION"); }
    assert_eq!(rows[7].get("accepted_revision").number(), 4);
    assert_eq!(result(&rows[9]).get("original_hex").text(), "7265706c6163656d656e74");
}

#[test]
fn utf16_and_native_hex_names_round_trip_through_code_selection() {
    use std::os::unix::ffi::{OsStringExt, OsStrExt};
    let (root, _) = fixture(b""); let path = root.join(OsString::from_vec(b"raw-\xff\t.RS".to_vec()));
    let bytes: Vec<u8> = "\u{feff}// 🦀\r\nfn target() {}\r\ntarget(); targetx();\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(&path, &bytes).unwrap();
    let path_hex = path.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (exit, rows) = invoke(&format!("0\topen-hex\t{path_hex}\n1\tcode-outline\t1\t1\tauto\n1\tcode-references-hex\t1\t1\t10\t746172676574\n1\tcode-ref-select\t1\t1\t2\n4\tcopy\t1\n4\tquit\n"));
    assert_eq!(exit, EXIT_OK); assert_eq!(result(&rows[1]).get("language").text(), "rust");
    assert_eq!(result(&rows[2]).get("retained_references").number(), 2);
    assert_eq!(result(&rows[4]).get("original_hex").text(), "740061007200670065007400");
}

#[test]
fn partial_outline_and_reference_counts_never_turn_into_complete_negatives() {
    let (_, path) = fixture(b"fn alpha() {}\nfn beta() {}\nalpha(); alpha();\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tcode-outline\t1\t1\tauto\t1\n1\tcode-find\t1\t1\texact\t0\t10\tbeta\n1\tcode-references\t1\t1\t1\talpha\n1\tcode-references\t1\t2\t0\tbeta\n1\tcode-references\t1\t3\t0\tabsent\n1\tquit\n", path.display()));
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result(&rows[2]).get("matched_symbols").number(), 0);
    assert!(result(&rows[2]).get("output_limited").flag()); assert!(!result(&rows[2]).get("semantic_complete").flag());
    assert_eq!(result(&rows[3]).get("matches_counted").number(), 2); assert!(!result(&rows[3]).get("count_complete").flag());
    assert_eq!(result(&rows[4]).get("retained_references").number(), 0); assert_eq!(result(&rows[4]).get("matches_counted").number(), 1);
    assert!(!result(&rows[4]).get("search_complete").flag()); assert!(result(&rows[5]).get("search_complete").flag());
}

#[test]
fn outline_guard_does_not_disable_reference_search_or_the_original_reader() {
    let mut bytes = b"target ".to_vec(); bytes.resize(65537, b' '); let (_, path) = fixture(&bytes);
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tcode-outline\t1\t1\tauto\n1\tcode-references\t1\t1\t10\ttarget\n1\tcode-ref-select\t1\t1\t1\n4\tcopy\t1\n4\tquit\n", path.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[1]), "SYMBOL_SOURCE_LIMIT");
    assert!(rows[1].get("code_outlines").array().is_empty()); assert_eq!(result(&rows[2]).get("retained_references").number(), 1);
    assert_eq!(result(&rows[4]).get("original_hex").text(), "746172676574");
    assert_eq!(result(&rows[5]).get("retained_source_bytes").number(), bytes.len() as u64);
}

#[test]
fn paged_symbol_ids_remain_stable_across_noncontiguous_filtered_views() {
    let source: String = (0..80).map(|i| format!("fn item_{i}() {{}}\n")).collect(); let (_, path) = fixture(source.as_bytes());
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tcode-outline\t1\t1\tauto\n1\tcode-symbols\t1\t1\t64\t128\n1\tcode-find\t1\t1\tprefix\t0\t128\titem_7\n1\tcode-select\t1\t1\t80\tname\n5\tcopy\t1\n5\tquit\n", path.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(result(&rows[1]).get("next_offset").number(), 64);
    assert_eq!(result(&rows[2]).get("symbols").array().len(), 16);
    assert_eq!(result(&rows[2]).get("symbols").array()[0].get("symbol_id").number(), 65);
    assert_eq!(result(&rows[3]).get("matched_symbols").number(), 11);
    assert_eq!(result(&rows[5]).get("original_hex").text(), "6974656d5f3739");
}
