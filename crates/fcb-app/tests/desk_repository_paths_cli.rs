#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn fixture(text: &[u8]) -> (PathBuf, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-path-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); let path = root.join("README.md"); fs::write(&path, text).unwrap(); (root, path)
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|b| !b.is_empty()).map(|line| {
        let row = parse(line).unwrap(); assert_eq!(row.get("schema").text(), "fcb.desk-stdio/1"); row
    }).collect()
}
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let mut input = script.as_bytes(); let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(&args(), &mut input, &mut out, &mut err, || false);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err)); (exit, records(&out))
}
fn result(row: &Json) -> &Json { row.get("result") }
fn error(row: &Json) -> &str { row.get("error").get("code").text() }

#[test]
fn readme_path_to_preview_bookmark_and_checkpoint_is_a_complete_cli_route() {
    let (root, source) = fixture(b"# Intro\n\n## Usage\n\nRead this.\n"); let saved = root.with_extension("fcbk");
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-path-open\t1\t1\t1\n3\tdoc-prepare\t1\t1\t80\n3\tdoc-heading\t1\t1\tusage\n5\tbookmark\t1\tusage evidence\n6\tsave\t{}\t1024\n6\tquit\n", root.display(), saved.display()));
    assert_eq!(exit, EXIT_OK); assert!(!result(&rows[1]).get("source_payload_read").flag());
    assert_eq!(result(&rows[1]).get("hits").array()[0].get("file_id").number(), 1);
    assert_eq!(result(&rows[2]).get("source_observation").text(), "new-capture-after-path-selection");
    assert_eq!(rows[2].get("accepted_revision").number(), 3);
    assert_eq!(rows[7].get("repository_query_generation"), &Json::Null);
    assert_eq!(rows[7].get("repository_path_generation").number(), 1);
    fs::rename(&source, root.join("moved.md")).unwrap();
    let (exit, restored) = invoke(&format!("0\trestore\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tcopy\t1\n1\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&restored[1]).get("headings").array()[1].get("slug").text(), "usage");
    assert!(result(&restored[2]).get("original_hex").text().contains("5573616765"));
    assert_eq!(result(&restored[3]).get("bookmarks").array()[0].get("label").text(), "usage evidence");
    assert_eq!(result(&restored[3]).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn path_open_gets_current_bytes_while_content_hit_keeps_its_old_capture() {
    let (root, path) = fixture(b"old needle");
    struct ChangeAfterLookup { bytes: Vec<u8>, count: usize, path: PathBuf }
    impl Write for ChangeAfterLookup {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.count += 1; if self.count == 3 { fs::write(&self.path, b"new bytes")?; } Ok(())
        }
    }
    let script = format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t1024\tneedle\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-path-open\t1\t1\t1\n4\tpin\t1\n5\trepo-hit\t1\t1\t1\n6\tview\t1\n6\tread\t2\t0\t64\t4\n6\tquit\n", root.display());
    let mut input = script.as_bytes(); let mut err = Vec::new(); let mut out = ChangeAfterLookup { bytes: Vec::new(), count: 0, path };
    assert_eq!(run(&args(), &mut input, &mut out, &mut err, || false), EXIT_OK); assert!(err.is_empty());
    let rows = records(&out.bytes);
    assert_eq!(result(&rows[6]).get("text").text(), "new bytes");
    assert_eq!(result(&rows[7]).get("text").text(), "old needle");
    assert_eq!(rows[8].get("repository_path_generation").number(), 1);
    assert_eq!(rows[8].get("repository_query_generation").number(), 1);
}

#[test]
fn path_navigation_and_document_work_do_not_advance_pending_text_search() {
    let (root, _) = fixture(b"# Title\n\nneedle\n");
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-begin\t1\t1\t10\t1024\tneedle\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-path-open\t1\t1\t1\n4\tdoc-prepare\t1\t1\t80\n4\trepo-progress\t1\t1\n4\trepo-cancel\t1\t1\n4\tquit\n", root.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[4].get("repository_pending_query_generation").number(), 1);
    assert_eq!(result(&rows[5]).get("step_count").number(), 0);
    assert_eq!(result(&rows[5]).get("source_bytes_read").number(), 0);
    assert_eq!(rows[7].get("documents").array().len(), 1);
    assert!(!rows[7].get("repository_work_pending").flag());
}

#[test]
fn replacement_root_tokens_reject_stale_filename_activation() {
    let (first, _) = fixture(b"# Old\n"); let (second, _) = fixture(b"# New\n");
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-open\t{}\n0\trepo-path-open\t1\t1\t1\n0\trepo-paths\t3\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-path-open\t3\t1\t1\n6\tdoc-prepare\t1\t1\t80\n6\tquit\n", first.display(), second.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[3]), "DESK_STALE_REPOSITORY");
    assert_eq!(rows[3].get("accepted_revision").number(), 0);
    assert_eq!(rows[2].get("repository_path_generation"), &Json::Null);
    assert_eq!(result(&rows[6]).get("headings").array()[0].get("slug").text(), "new");
}

#[test]
fn raw_filename_hex_queries_open_native_identity_not_escaped_display_text() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let (root, source) = fixture(b"# Raw\n"); let raw = b"README\t\xff.md";
    fs::rename(source, root.join(OsString::from_vec(raw.to_vec()))).unwrap();
    let query: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-paths-hex\t1\t1\texact\tsensitive\t10\t{}\n0\trepo-path-open\t1\t1\t1\n3\tdoc-prepare\t1\t1\t80\n3\tquit\n", root.display(), query));
    assert_eq!(exit, EXIT_OK); assert_eq!(result(&rows[1]).get("query_hex").text(), query);
    assert_eq!(result(&rows[3]).get("headings").array()[0].get("slug").text(), "raw");
}

#[test]
fn path_modes_case_policy_and_top_k_coverage_are_exposed_without_source_reads() {
    let (root, _) = fixture(b"x"); fs::write(root.join("readme.md"), b"y").unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-paths\t1\t2\tprefix\tunicode-lowercase\t1\tread\n0\trepo-path-page\t1\t2\t0\t1\n0\trepo-paths\t1\t3\tfuzzy\tsensitive\t10\tRDM\n0\tquit\n", root.display()));
    assert_eq!(exit, EXIT_PARTIAL); assert_eq!(result(&rows[1]).get("hits").array().len(), 1);
    assert!(!result(&rows[2]).get("search_complete").flag()); assert!(result(&rows[2]).get("truncated").flag());
    assert_eq!(result(&rows[3]).get("hits").array().len(), 1);
    assert_eq!(result(&rows[4]).get("mode").text(), "fuzzy");
    assert_eq!(result(&rows[4]).get("hits").array().len(), 1);
    for i in 1..=4 { assert!(!result(&rows[i]).get("source_payload_read").flag()); }
    assert_eq!(result(&rows[5]).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn invalid_modes_generations_and_file_ids_leave_current_path_query_usable() {
    let (root, _) = fixture(b"readable");
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-paths\t1\t2\twrong\tsensitive\t10\tREADME.md\n0\trepo-paths\t1\t2\texact\twrong\t10\tREADME.md\n0\trepo-path-open\t1\t2\t1\n0\trepo-path-open\t1\t1\t0\n0\trepo-path-select\t1\t1\t1\n0\trepo-path-open\t1\t1\t1\n8\trepo-path-clear\t1\t2\n8\tview\t1\n8\tquit\n", root.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[2]), "DESK_PATH_MODE"); assert_eq!(error(&rows[3]), "DESK_PATH_CASE");
    assert_eq!(error(&rows[4]), "ATLAS_PATH_STALE_QUERY"); assert_eq!(error(&rows[5]), "DESK_PATH_FILE_ID");
    assert_eq!(rows[6].get("accepted_revision").number(), 0);
    assert_eq!(rows[8].get("repository_path_generation"), &Json::Null);
    assert_eq!(result(&rows[9]).get("text").text(), "readable");
}

#[test]
fn filename_lookup_handles_unreadable_size_without_falsely_opening_truncated_source() {
    let (root, path) = fixture(b""); fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(4 * 1024 * 1024 + 1).unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-paths\t1\t1\texact\tsensitive\t10\tREADME.md\n0\trepo-path-open\t1\t1\t1\n0\tstate\n0\tquit\n", root.display()));
    assert_eq!(exit, EXIT_ERROR); assert!(result(&rows[1]).get("search_complete").flag());
    assert_eq!(result(&rows[1]).get("hits").array().len(), 1); assert_eq!(error(&rows[2]), "CLI_INPUT_LIMIT");
    assert_eq!(result(&rows[3]).get("initial_source_bytes_read").number(), 0);
    assert!(result(&rows[3]).get("panes").array().is_empty());
}
