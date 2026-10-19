#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{ffi::OsString, fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_ERROR};
use fcb::ui::reading_panes::desk::MAX_ANNOTATION_BODY_BYTES;

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-desk-notes-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn invoke(text: &str) -> (u8, Vec<Json>, Vec<u8>) {
    let mut input = text.as_bytes(); let mut output = Vec::new(); let mut error = Vec::new();
    let args = [OsString::from("desk"), OsString::from("--stdio")];
    let exit = run(&args, &mut input, &mut output, &mut error, || false);
    let records = output.split(|b| *b == b'\n').filter(|line| !line.is_empty())
        .map(|line| parse(line).unwrap()).collect();
    (exit, records, error)
}
fn hex(text: &str) -> String { text.bytes().map(|b| format!("{b:02x}")).collect() }

#[test]
fn notes_are_editable_durable_and_keep_original_source_after_path_reuse() {
    let root = fixture(); let path = root.join("source.rs"); let saved = root.join("notes.fcbdesk");
    fs::write(&path, b"before\nexact\n").unwrap();
    let label = "🧭 Reading rationale";
    let body = "First line\n\tSecond line\0with NUL and \\";
    let edited = "Revised\nKeep this exact range: <not executable>";
    let commands = format!(
        "0\topen\t{}\n1\tselect\t1\t7\t12\n2\tannotate-hex\t1\t{}\t{}\n3\tedit-bookmark-hex\t1\t{}\t{}\n4\tsave\t{}\t13\n4\tquit\n",
        path.display(), hex(label), hex(body), hex(label), hex(edited), saved.display());
    let (exit, records, errors) = invoke(&commands);
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty()); assert_eq!(records.len(), 6);
    let created = &records[2].get("result").get("bookmarks").array()[0];
    assert_eq!(created.get("label").text(), label);
    assert_eq!(created.get("body").text(), body);
    assert_eq!(created.get("update_sequence").number(), 1);
    let changed = &records[3].get("result").get("bookmarks").array()[0];
    assert_eq!(changed.get("body").text(), edited);
    assert_eq!(changed.get("update_sequence").number(), 2);
    assert_eq!(changed.get("selection").get("start").number(), 7);
    assert_eq!(records[4].get("effect").text(), "complete-file-and-parent-sync-requested");

    fs::rename(&path, root.join("original.rs")).unwrap();
    fs::write(&path, b"unrelated replacement").unwrap();
    let commands = format!("0\trestore\t{}\n1\trecall\t1\n2\tcopy\t1\n2\tstate\n2\tforget\t1\n5\tquit\n", saved.display());
    let (exit, records, errors) = invoke(&commands);
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty());
    let restored = &records[0].get("result").get("bookmarks").array()[0];
    assert_eq!(restored.get("body").text(), edited);
    assert_eq!(restored.get("update_sequence").number(), 2);
    assert_eq!(records[2].get("result").get("original_hex").text(), "6578616374");
    assert_eq!(records[3].get("result").get("initial_source_bytes_read").number(), 0);
    assert!(records[5].get("result").get("bookmarks").array().is_empty());
    assert_eq!(fs::read(&path).unwrap(), b"unrelated replacement");
}

#[test]
fn empty_body_can_turn_a_note_into_a_relabelled_bookmark() {
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"capture").unwrap();
    let commands = format!(
        "0\topen\t{}\n1\tannotate\t1\tTitle\tNote body\n2\tedit-bookmark\t1\tRenamed\t\n3\tedit-bookmark-hex\t1\t4c616e646d61726b\t\n4\tquit\n", path.display());
    let (exit, records, errors) = invoke(&commands);
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty());
    let marks = records[4].get("result").get("bookmarks").array();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].get("bookmark").number(), 1);
    assert_eq!(marks[0].get("label").text(), "Landmark");
    assert_eq!(marks[0].get("body").text(), "");
    assert_eq!(marks[0].get("update_sequence").number(), 3);
}

#[test]
fn malformed_or_oversized_note_text_never_replaces_accepted_personal_state() {
    let root = fixture(); let path = root.join("a.rs"); fs::write(&path, b"capture").unwrap();
    let commands = format!(
        "0\topen\t{}\n1\tannotate\t1\tOriginal\tKeep me\n2\tedit-bookmark-hex\t1\t53686f756c64206661696c\tff\n2\tedit-bookmark-hex\t1\t53686f756c64206661696c\txyz\n2\tedit-bookmark\t1\tToo big\t{}\n2\tstate\n2\tquit\n",
        path.display(), "x".repeat(MAX_ANNOTATION_BODY_BYTES + 1));
    let (exit, records, errors) = invoke(&commands);
    assert_eq!(exit, EXIT_ERROR); assert!(errors.is_empty()); assert_eq!(records.len(), 7);
    assert_eq!(records[2].get("error").get("code").text(), "DESK_INVALID_TEXT");
    assert_eq!(records[3].get("error").get("code").text(), "DESK_INVALID_HEX");
    assert_eq!(records[4].get("error").get("code").text(), "DESK_INVALID_BODY");
    assert_eq!(records[5].get("accepted_revision").number(), 2);
    let mark = &records[5].get("result").get("bookmarks").array()[0];
    assert_eq!(mark.get("body").text(), "Keep me");
    assert_eq!(mark.get("label").text(), "Original");
    assert_eq!(mark.get("update_sequence").number(), 1);
}
