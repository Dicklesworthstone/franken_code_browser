#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};
fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-desk-repo-cli-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root
}
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let args: Vec<OsString> = vec!["desk".into(), "--stdio".into()];
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args, &mut input, &mut output, &mut errors, || false);
    assert!(errors.is_empty());
    let rows = output.split(|b| *b == b'\n').filter(|b| !b.is_empty()).map(|b| parse(b).unwrap()).collect();
    (exit, rows)
}

#[test]
fn repository_search_hit_pin_bookmark_save_restore_is_one_complete_workflow() {
    let root = fixture(); let source = root.join("a.rs"); fs::write(&source, b"old needle").unwrap();
    let saved = root.join("desk.fcbk");
    let script = format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t1024\tneedle\n0\trepo-hit\t1\t1\t1\n3\tpin\t1\n4\tbookmark\t1\tevidence\n5\trepo-close\t1\n5\tcopy\t1\n5\tsave\t{}\t100\n5\tquit\n", root.display(), saved.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 9);
    assert_eq!(rows[0].get("repository_token").number(), 1); assert_eq!(rows[0].get("accepted_revision").number(), 0);
    assert_eq!(rows[1].get("repository_query_generation").number(), 1);
    assert_eq!(rows[2].get("accepted_revision").number(), 3);
    assert!(!rows[2].get("result").get("source_reopened").flag());
    assert_eq!(rows[6].get("result").get("original_hex").text(), "6e6565646c65");
    assert_eq!(rows[7].get("result").get("source_bytes").number(), 10);
    fs::rename(&source, root.join("moved.rs")).unwrap();
    let (exit, restored) = invoke(&format!("0\trestore\t{}\n1\tview\t1\n1\tquit\n", saved.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(restored[1].get("result").get("text").text(), "needle");
    assert_eq!(restored[0].get("result").get("bookmarks").array()[0].get("label").text(), "evidence");
}

#[test]
fn stale_repository_tokens_cannot_activate_same_numbered_hits_from_a_new_root() {
    let first = fixture(); let second = fixture();
    fs::write(first.join("a.rs"), b"first needle").unwrap(); fs::write(second.join("b.rs"), b"second needle").unwrap();
    let script = format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t1024\tneedle\n0\trepo-hit\t1\t1\t1\n3\tpin\t1\n4\trepo-open\t{}\n4\trepo-find\t5\t1\t10\t1024\tneedle\n4\trepo-hit\t1\t1\t1\n4\trepo-hit\t5\t1\t1\n8\tread\t1\t0\t100\t10\n8\tread\t2\t0\t100\t10\n8\tquit\n", first.display(), second.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[6].get("error").get("code").text(), "DESK_STALE_REPOSITORY");
    assert_eq!(rows[6].get("accepted_revision").number(), 4);
    assert_eq!(rows[8].get("result").get("text").text(), "first needle");
    assert_eq!(rows[9].get("result").get("text").text(), "second needle");
}

#[test]
fn failed_repository_open_keeps_old_root_query_and_reader_usable() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let missing = root.join("missing");
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t100\tneedle\n0\trepo-open\t{}\n0\trepo-hit\t1\t1\t1\n4\tcopy\t1\n4\tquit\n", root.display(), missing.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[2].get("repository_token").number(), 1);
    assert_eq!(rows[2].get("repository_query_generation").number(), 1);
    assert_eq!(rows[4].get("result").get("original_hex").text(), "6e6565646c65");
}

#[test]
fn paged_hits_reuse_one_import_and_preserve_original_offsets() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle x needle").unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t1024\tneedle\n0\trepo-page\t1\t1\t1\t1\n0\trepo-hit\t1\t1\t2\n4\trepo-hit\t1\t1\t1\n5\tstate\n5\tquit\n", root.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[2].get("result").get("hits").array()[0].get("hit_id").number(), 2);
    assert_eq!(rows[3].get("result").get("original_range").get("start").number(), 9);
    assert!(rows[4].get("result").get("reused_source").flag());
    assert_eq!(rows[5].get("result").get("retained_sources").number(), 1);
    assert_eq!(rows[5].get("result").get("initial_source_bytes_read").number(), 0);
}

#[test]
fn clearing_repository_results_does_not_clear_local_reader_query_or_bookmark() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t10\t100\tneedle\n0\trepo-hit\t1\t1\t1\n3\tfind\t1\t10\t10\t100\tneedle\n3\tbookmark\t1\tkeep\n5\trepo-clear\t1\t2\n5\thit\t1\t10\t0\n7\tcopy\t1\n7\tquit\n", root.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[5].get("accepted_query_generation").number(), 10);
    assert_eq!(rows[7].get("result").get("original_hex").text(), "6e6565646c65");
    assert_eq!(rows[8].get("result").get("bookmarks").array()[0].get("label").text(), "keep");
}

#[test]
fn partial_repository_query_is_not_promoted_to_complete_by_opening_a_hit() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle needle needle").unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-find\t1\t1\t1\t100\tneedle\n0\trepo-page\t1\t1\t0\t10\n0\trepo-hit\t1\t1\t1\n4\tcopy\t1\n4\tquit\n", root.display()));
    assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(rows[1].get("exit_code").number(), EXIT_PARTIAL as u64);
    assert!(!rows[2].get("result").get("search_complete").flag());
    assert_eq!(rows[4].get("result").get("original_hex").text(), "6e6565646c65");
}

#[test]
fn native_root_and_source_names_and_hex_text_are_not_shell_interpreted() {
    use std::os::unix::ffi::{OsStringExt, OsStrExt};
    let parent = fixture(); let root = parent.join(OsString::from_vec(b"repo\t\xff".to_vec())); fs::create_dir(&root).unwrap();
    fs::write(root.join(OsString::from_vec(b"a\n\xff.rs".to_vec())), b"a\tb").unwrap();
    let path_hex = root.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (exit, rows) = invoke(&format!("0\trepo-open-hex\t{path_hex}\n0\trepo-find-text-hex\t1\t1\t10\t100\t610962\n0\trepo-hit\t1\t1\t1\n3\tcopy\t1\n3\tquit\n"));
    assert_eq!(exit, EXIT_OK); assert_eq!(rows[3].get("result").get("original_hex").text(), "610962");
}

#[test]
fn invalid_commands_and_stale_desk_revision_cannot_switch_repository() {
    let root = fixture(); fs::write(root.join("a.rs"), b"needle").unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-hit\t1\t1\t1\n0\trepo-open\t{}\n1\trepo-close\t2\n0\trepo-find\t2\t1\t0\t100\tneedle\n0\trepo-info\t2\n0\tquit\n", root.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[0].get("error").get("code").text(), "DESK_NO_REPOSITORY");
    assert_eq!(rows[2].get("error").get("code").text(), "DESK_STALE_REVISION");
    assert_eq!(rows[4].get("repository_token").number(), 2);
}
