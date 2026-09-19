#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, io::{self, Write}, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR};

fn fixture(bytes: &[u8]) -> (PathBuf, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-doc-cli-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); let source = root.join("README.md"); fs::write(&source, bytes).unwrap();
    (root, source)
}
fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|b| !b.is_empty()).map(|b| {
        let value = parse(b).unwrap(); assert_eq!(value.get("schema").text(), "fcb.desk-stdio/1"); value
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
fn headings_split_history_and_bookmark_use_the_same_original_selection() {
    let raw = b"# One\n\nfirst\n\n## Two\n\nsecond\n";
    let (_, source) = fixture(raw);
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tdoc-headings\t1\t1\t0\t8\n1\tdoc-heading\t1\t1\ttwo\n4\tdoc-split\t1\t1\t8\n4\tbookmark\t1\twhy this section\n6\tback\n7\tforward\n8\tdoc-sync\t1\t1\t8\n8\tquit\n", source.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 10);
    assert_eq!(result(&rows[2]).get("headings").array()[1].get("slug").text(), "two");
    let selection = result(&rows[3]).get("panes").array()[0].get("selection");
    assert!(selection.get("start").number() > 0);
    let split = result(&rows[4]); assert_eq!(split.get("synchronization").text(), "enclosing-source-region");
    assert_eq!(split.get("source").get("selection"), selection);
    assert_eq!(split.get("source").get("source_revision"), split.get("preview").get("source_revision"));
    assert_eq!(result(&rows[5]).get("bookmarks").array()[0].get("selection"), selection);
    assert_eq!(result(&rows[6]).get("panes").array()[0].get("offset").number(), 0);
    assert_eq!(result(&rows[7]).get("panes").array()[0].get("selection"), selection);
    assert_eq!(rows[9].get("documents").array().len(), 1);
    assert_eq!(result(&rows[9]).get("initial_source_bytes_read").number(), raw.len() as u64);
}

#[test]
fn rendered_copy_and_original_markdown_copy_are_distinct_and_selection_is_navigable() {
    let (_, source) = fixture(b"Hello **world**.\r\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tdoc-copy\t1\t1\t6\t11\trendered\n1\tdoc-copy\t1\t1\t6\t11\tmarkdown\n1\tdoc-select\t1\t1\t6\t11\n5\tcopy\t1\n5\tquit\n", source.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[2]).get("text").text(), "world");
    assert_eq!(result(&rows[2]).get("copy_domain").text(), "rendered-text-utf8");
    assert!(!result(&rows[2]).get("clipboard_written").flag());
    assert!(result(&rows[3]).get("original_hex").text().contains("2a2a776f726c642a2a"));
    assert_eq!(result(&rows[5]).get("original_hex"), result(&rows[3]).get("original_hex"));
    assert_eq!(rows[4].get("accepted_revision").number(), 5);
    assert_eq!(result(&rows[6]).get("history").array().len(), 2);
}

#[test]
fn reflow_preserves_source_anchor_and_rejects_old_rendered_coordinates() {
    let (_, source) = fixture(b"one two three four five six seven eight nine ten\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tselect\t1\t0\t3\n3\tdoc-prepare\t1\t2\t8\n3\tdoc-select\t1\t1\t0\t3\n3\tdoc-window\t1\t2\t0\t8\n3\tdoc-sync\t1\t2\t8\n3\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[4]), "DOCUMENT_READ_STALE_GENERATION");
    assert_eq!(result(&rows[3]).get("width_columns").number(), 8);
    assert_eq!(rows[7].get("accepted_revision").number(), 3);
    assert_eq!(result(&rows[7]).get("panes").array()[0].get("selection").get("end").number(), 3);
    assert_eq!(rows[7].get("documents").array()[0].get("document_generation").number(), 2);
    assert_eq!(result(&rows[6]).get("original_anchor").number(), 0);
}

#[test]
fn refused_reflow_and_stale_clear_leave_the_accepted_layout_available() {
    let (_, source) = fixture(b"# Title\n\nbody\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tdoc-prepare\t1\t2\t1\n1\tdoc-prepare\t1\t2\t80\n1\tdoc-clear\t1\t2\n1\tdoc-window\t1\t1\t0\t8\n1\tdoc-clear\t1\t1\n1\tdoc-prepare\t1\t1\t80\n1\tdoc-prepare\t1\t3\t80\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[2]), "DOCUMENT_READ_INVALID_LIMITS");
    assert_eq!(error(&rows[3]), "DOCUMENT_READ_STALE_GENERATION");
    assert_eq!(error(&rows[4]), "DOCUMENT_READ_STALE_GENERATION");
    assert_eq!(result(&rows[5]).get("document_generation").number(), 1);
    assert_eq!(rows[2].get("last_document_attempt").number(), 2);
    assert!(rows[6].get("documents").array().is_empty());
    assert_eq!(error(&rows[7]), "DOCUMENT_READ_STALE_GENERATION");
    assert_eq!(rows[9].get("documents").array()[0].get("document_generation").number(), 3);
}

#[test]
fn duplicate_panes_keep_independent_previews_and_close_retires_only_one() {
    let (_, source) = fixture(b"# One\n\nfirst paragraph\n\n## Two\n\nsecond paragraph\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tduplicate\t1\n3\tdoc-prepare\t2\t2\t8\n3\tdoc-heading\t2\t2\ttwo\n5\tdoc-window\t1\t1\t0\t8\n5\tclose\t2\n7\tquit\n", source.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows[4].get("documents").array().len(), 2);
    let panes = result(&rows[4]).get("panes").array();
    assert_eq!(panes[0].get("offset").number(), 0); assert!(panes[1].get("offset").number() > 0);
    assert_eq!(result(&rows[3]).get("width_columns").number(), 8);
    assert_eq!(result(&rows[5]).get("width_columns").number(), 80);
    assert_eq!(rows[6].get("documents").array().len(), 1);
    assert_eq!(rows[6].get("documents").array()[0].get("pane").number(), 1);
}

#[test]
fn source_replacement_and_checkpoint_restore_retire_derived_layouts_not_sources() {
    let (root, source) = fixture(b"# Old\n\noriginal\n");
    let other = root.join("other.md"); fs::write(&other, b"# New\n").unwrap(); let saved = root.join("desk.fcbk");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tsave\t{}\t1024\n1\topen\t{}\n4\tdoc-prepare\t1\t2\t80\n4\trestore\t{}\n6\tdoc-window\t1\t2\t0\t8\n6\tdoc-prepare\t2\t3\t80\n6\tdoc-heading\t2\t3\told\n9\tquit\n", source.display(), saved.display(), other.display(), saved.display()));
    assert_eq!(exit, EXIT_ERROR); assert!(rows[3].get("documents").array().is_empty());
    assert!(rows[5].get("documents").array().is_empty()); assert_eq!(error(&rows[6]), "DESK_MISSING_PANE");
    assert_eq!(result(&rows[7]).get("headings").array()[0].get("slug").text(), "old");
    assert_eq!(rows[9].get("documents").array()[0].get("pane").number(), 2);
    assert_eq!(rows[9].get("accepted_revision").number(), 9);
}

#[test]
fn utf8_bom_and_scalar_boundaries_are_not_rendered_byte_offsets() {
    let (_, source) = fixture("\u{feff}Hello 😀.\r\n".as_bytes());
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tdoc-sync\t1\t1\t8\n1\tdoc-copy\t1\t1\t7\t10\trendered\n1\tdoc-copy\t1\t1\t6\t10\trendered\n1\tdoc-select\t1\t1\t6\t10\n6\tdoc-sync\t1\t1\t8\n6\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(result(&rows[1]).get("parser_source_base").number(), 3);
    assert_eq!(error(&rows[2]), "DOCUMENT_READ_INVALID_RANGE"); assert_eq!(error(&rows[3]), "DOCUMENT_READ_INVALID_RANGE");
    assert_eq!(result(&rows[4]).get("text").text(), "😀");
    assert!(result(&rows[5]).get("panes").array()[0].get("selection").get("start").number() >= 3);
    assert!(!result(&rows[6]).get("flow_lines").array().is_empty());
}

#[test]
fn unsupported_preview_bytes_still_support_exact_original_copy() {
    for bytes in [b"\xffinvalid".as_slice(), b"\xff\xfe#\0 \0A\0\n\0"] {
        let (_, source) = fixture(bytes);
        let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tselect\t1\t0\t{}\n3\tcopy\t1\n3\tquit\n", source.display(), bytes.len()));
        assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[1]), "DOCUMENT_READ_INVALID_UTF8");
        assert!(rows[4].get("documents").array().is_empty());
        let expected: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(result(&rows[3]).get("original_hex").text(), expected);
    }
}

#[test]
fn repository_hit_preview_uses_captured_bytes_after_live_source_rename() {
    let (root, source) = fixture(b"# Evidence\n\nneedle\n");
    struct MoveAfterSearch { bytes: Vec<u8>, flushes: usize, source: PathBuf, moved: PathBuf }
    impl Write for MoveAfterSearch {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 2 { fs::rename(&self.source, &self.moved)?; fs::write(&self.source, b"# Replacement\n")?; }
            Ok(())
        }
    }
    let script = format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t1024\tneedle\n0\trepo-hit\t1\t1\t1\n3\tdoc-prepare\t1\t1\t80\n3\tdoc-heading\t1\t1\tevidence\n5\trepo-close\t1\n5\tdoc-window\t1\t1\t0\t8\n5\tquit\n", root.display());
    let mut input = script.as_bytes(); let mut errors = Vec::new();
    let mut out = MoveAfterSearch { bytes: Vec::new(), flushes: 0, source: source.clone(), moved: root.join("moved.md") };
    assert_eq!(run(&args(), &mut input, &mut out, &mut errors, || false), EXIT_OK);
    let rows = records(&out.bytes); assert!(errors.is_empty());
    assert_eq!(result(&rows[3]).get("headings").array()[0].get("slug").text(), "evidence");
    assert_eq!(rows[7].get("repository_token"), &Json::Null);
    assert_eq!(rows[7].get("documents").array().len(), 1);
    assert_eq!(result(&rows[7]).get("initial_source_bytes_read").number(), 0);
    assert_eq!(fs::read(&source).unwrap(), b"# Replacement\n");
}

#[test]
fn bad_pages_stale_desk_revision_and_invalid_commands_do_not_discard_preview() {
    let (_, source) = fixture(b"# Title\n");
    let (exit, rows) = invoke(&format!("0\topen\t{}\n1\tdoc-prepare\t1\t1\t80\n0\tdoc-heading\t1\t1\ttitle\n1\tdoc-window\t1\t1\t0\t129\n1\tdoc-window\t1\t1\t999999\t1\n1\tdoc-copy\t1\t1\t0\t1\tother\n1\tdoc-missing\t1\n1\tdoc-headings\t1\t1\t0\t1\n1\tquit\n", source.display()));
    assert_eq!(exit, EXIT_ERROR); assert_eq!(error(&rows[2]), "DESK_STALE_REVISION");
    assert_eq!(error(&rows[3]), "DOCUMENT_READ_INVALID_LIMITS");
    assert_eq!(error(&rows[4]), "DOCUMENT_READ_INVALID_RANGE");
    assert_eq!(error(&rows[5]), "DESK_DOCUMENT_COPY_MODE");
    assert_eq!(error(&rows[6]), "DESK_DOCUMENT_COMMAND_SYNTAX");
    assert_eq!(result(&rows[7]).get("headings").array()[0].get("slug").text(), "title");
    assert_eq!(rows[8].get("accepted_revision").number(), 1);
    assert_eq!(rows[8].get("documents").array().len(), 1);
}
