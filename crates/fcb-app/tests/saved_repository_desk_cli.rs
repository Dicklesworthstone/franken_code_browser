#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, entry};
use support::{Json, parse};
use std::{ffi::OsString, fs::{self, OpenOptions}, io::{self, Seek, SeekFrom, Write}, path::PathBuf};
use fcb::search::snapshot::{SnapshotEntry, SnapshotData};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| {
        let value = parse(line).unwrap(); assert_eq!(value.get("schema").text(), "fcb.desk-stdio/1"); value
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
fn saved_hit_to_preview_bookmark_and_checkpoint_works_without_a_live_source() {
    let raw = b"# Guide\n\nArchived **needle**.\n";
    let f = Fixture::new(&[entry(b"README.md", raw)], true); let checkpoint = f.root.join("desk.fcbk");
    let script = format!("0\tsaved-open\t{}\n0\tsaved-find\t1\t1\t10\tneedle\n0\tsaved-hit\t1\t1\t1\n3\tdoc-prepare\t1\t1\t80\n3\tbookmark\t1\tarchived evidence\n5\tsaved-close\t1\n5\tcopy\t1\n5\tsave\t{}\t1024\n5\tquit\n", f.path.display(), checkpoint.display());
    let (exit, rows) = invoke(&script); assert_eq!(exit, EXIT_OK); assert_eq!(rows.len(), 9);
    assert_eq!(result(&rows[2]).get("source_observation").text(), "verified-saved-member");
    assert!(!result(&rows[2]).get("live_source_reopened").flag());
    assert_eq!(result(&rows[2]).get("member_bytes_read").number(), raw.len() as u64);
    assert_eq!(rows[5].get("saved_token"), &Json::Null);
    assert_eq!(rows[5].get("documents").array().len(), 1);
    assert_eq!(result(&rows[6]).get("original_hex").text(), "6e6565646c65");
    assert_eq!(result(&rows[8]).get("initial_source_bytes_read").number(), 0);
    assert!(!f.root.join("README.md").exists());
    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    let (exit, restored) = invoke(&format!("0\trestore\t{}\n1\tdoc-prepare\t1\t1\t80\n1\tcopy\t1\n1\tquit\n", checkpoint.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&restored[1]).get("headings").array()[0].get("slug").text(), "guide");
    assert_eq!(result(&restored[2]).get("original_hex").text(), "6e6565646c65");
    assert_eq!(result(&restored[3]).get("bookmarks").array()[0].get("label").text(), "archived evidence");
    assert_eq!(restored[3].get("saved_token"), &Json::Null);
}

#[test]
fn failed_archive_replacement_keeps_old_token_query_and_exact_hit() {
    let f = Fixture::new(&[entry(b"a.rs", b"old needle")], true);
    let invalid = f.root.join("invalid.fcbs"); fs::write(&invalid, b"not an archive").unwrap();
    let (exit, rows) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-find\t1\t1\t10\tneedle\n0\tsaved-open\t{}\n0\tsaved-hit\t1\t1\t1\n4\tcopy\t1\n4\tquit\n", f.path.display(), invalid.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[2].get("exit_code").number(), EXIT_ERROR as u64);
    assert_eq!(rows[2].get("saved_token").number(), 1);
    assert_eq!(rows[2].get("saved_query_generation").number(), 1);
    assert_eq!(rows[2].get("accepted_revision").number(), 0);
    assert_eq!(result(&rows[4]).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn replacement_tokens_prevent_aliasing_and_old_readers_still_compare_after_detach() {
    let a = Fixture::new(&[entry(b"same.rs", b"old needle")], true);
    let b = Fixture::new(&[entry(b"same.rs", b"new needle")], true);
    let (exit, rows) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-member\t1\t0\n2\tpin\t1\n3\tsaved-open\t{}\n3\tsaved-member\t1\t0\n3\tsaved-member\t4\t0\n6\tcompare-prepare\t1\t2\t1\n6\tsaved-close\t4\n6\tread\t1\t0\t64\t4\n6\tread\t2\t0\t64\t4\n6\tquit\n", a.path.display(), b.path.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(error(&rows[4]), "DESK_STALE_SAVED_REPOSITORY");
    assert_eq!(rows[4].get("accepted_revision").number(), 3);
    assert_eq!(result(&rows[6]).get("relation").text(), "different");
    assert_eq!(rows[7].get("saved_token"), &Json::Null);
    assert_ne!(rows[7].get("comparison"), &Json::Null);
    assert_eq!(result(&rows[8]).get("text").text(), "old needle");
    assert_eq!(result(&rows[9]).get("text").text(), "new needle");
}

#[test]
fn offline_browsing_does_not_advance_or_replace_pending_live_repository_work() {
    let f = Fixture::new(&[entry(b"README.md", b"# Archived\n\nneedle\n")], true);
    let live = f.root.join("live"); fs::create_dir(&live).unwrap(); fs::write(live.join("a.rs"), b"live needle").unwrap();
    let (exit, rows) = invoke(&format!("0\trepo-open\t{}\n0\trepo-begin\t1\t1\t10\t1024\tneedle\n0\tsaved-open\t{}\n0\tsaved-find\t3\t1\t10\tneedle\n0\tsaved-hit\t3\t1\t1\n5\tdoc-prepare\t1\t1\t80\n5\trepo-progress\t1\t1\n5\trepo-cancel\t1\t1\n5\tquit\n", live.display(), f.path.display()));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[5].get("repository_token").number(), 1);
    assert_eq!(rows[5].get("saved_token").number(), 3);
    assert_eq!(rows[5].get("saved_query_generation").number(), 1);
    assert_eq!(rows[5].get("repository_pending_query_generation").number(), 1);
    assert_eq!(result(&rows[6]).get("step_count").number(), 0);
    assert_eq!(result(&rows[6]).get("source_bytes_read").number(), 0);
    assert_eq!(rows[8].get("documents").array().len(), 1);
    assert!(!rows[8].get("repository_work_pending").flag());
}

#[test]
fn replacing_the_archive_path_does_not_rebind_its_open_handle_or_imports() {
    let f = Fixture::new(&[entry(b"a.rs", b"old needle")], true);
    let other = Fixture::new(&[entry(b"a.rs", b"new bytes")], true);
    struct Replace { bytes: Vec<u8>, flushes: usize, path: PathBuf, moved: PathBuf, replacement: Vec<u8> }
    impl Write for Replace {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 1 { fs::rename(&self.path, &self.moved)?; fs::write(&self.path, &self.replacement)?; }
            Ok(())
        }
    }
    let script = format!("0\tsaved-open\t{}\n0\tsaved-find\t1\t1\t10\tneedle\n0\tsaved-hit\t1\t1\t1\n3\tread\t1\t0\t64\t4\n3\tquit\n", f.path.display());
    let mut input = script.as_bytes(); let mut err = Vec::new();
    let mut out = Replace { bytes: Vec::new(), flushes: 0, path: f.path.clone(), moved: f.root.join("original.fcbs"), replacement: other.bytes };
    assert_eq!(run(&args(), &mut input, &mut out, &mut err, || false), EXIT_OK); assert!(err.is_empty());
    assert_eq!(result(&records(&out.bytes)[3]).get("text").text(), "old needle");
    assert_ne!(fs::read(&f.path).unwrap(), f.bytes);
}

#[test]
fn utf16_hex_queries_and_native_names_reuse_the_same_member_across_actions() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut raw = vec![0xff, 0xfe]; for unit in "banana".encode_utf16() { raw.extend(unit.to_le_bytes()); }
    let f = Fixture::new(&[entry(b"raw-\xff.rs", &raw)], true);
    let path = f.root.join(OsString::from_vec(b"archive\t\xff.fcbs".to_vec())); fs::rename(&f.path, &path).unwrap();
    let encoded = path.as_os_str().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (exit, rows) = invoke(&format!("0\tsaved-open-hex\t{encoded}\n0\tsaved-find-text-hex\t1\t1\t10\t616e61\n0\tsaved-hit\t1\t1\t1\n3\tcopy\t1\n3\tsaved-hit\t1\t1\t2\n5\tcopy\t1\n5\tsaved-clear\t1\t2\n5\tsaved-member\t1\t0\n8\tquit\n"));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(result(&rows[2]).get("selection").get("start").number(), 4);
    assert_eq!(result(&rows[4]).get("selection").get("start").number(), 8);
    assert_eq!(result(&rows[3]).get("original_hex").text(), "61006e006100");
    assert_eq!(result(&rows[5]).get("original_hex").text(), "61006e006100");
    assert!(result(&rows[4]).get("reused_capture").flag());
    assert!(result(&rows[7]).get("reused_capture").flag());
    assert_eq!(result(&rows[8]).get("retained_sources").number(), 1);
    assert_eq!(rows[8].get("saved_query_generation"), &Json::Null);
}

#[test]
fn metadata_pages_do_not_load_sources_and_unavailable_is_not_empty() {
    let f = Fixture::new(&[entry(b"a", b""), SnapshotEntry { path: b"b", observed_bytes: 10,
        data: SnapshotData::Unavailable("READ_DENIED") }, entry(b"c", b"needle")], true);
    let (exit, rows) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-members\t1\t0\t2\n0\tsaved-info\t1\n0\tsaved-member\t1\t1\n0\tsaved-member\t1\t0\n5\tview\t1\n5\tquit\n", f.path.display()));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(result(&rows[1]).get("next_offset").number(), 2);
    assert!(!result(&rows[1]).get("members").array()[1].get("captured").flag());
    assert_eq!(result(&rows[2]).get("member_bytes_read").number(), 0);
    assert_eq!(error(&rows[3]), "SNAPSHOT_MEMBER_UNAVAILABLE");
    assert_eq!(rows[3].get("accepted_revision").number(), 0);
    assert_eq!(result(&rows[5]).get("text").text(), "");
    assert_eq!(result(&rows[6]).get("retained_sources").number(), 1);
}

#[test]
fn partial_search_remains_partial_even_when_a_verified_hit_opens_successfully() {
    let f = Fixture::new(&[entry(b"a", b"needle needle")], false);
    let (exit, rows) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-find\t1\t1\t1\tneedle\n0\tsaved-hit\t1\t1\t1\n3\tcopy\t1\n3\tquit\n", f.path.display()));
    assert_eq!(exit, EXIT_PARTIAL);
    assert!(!result(&rows[1]).get("search_complete").flag());
    assert!(result(&rows[1]).get("truncated").flag());
    assert_eq!(rows[2].get("exit_code").number(), EXIT_OK as u64);
    assert_eq!(result(&rows[3]).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn archive_damage_blocks_new_import_but_preserves_the_prior_reader_and_query() {
    let raw = b"unique needle source"; let f = Fixture::new(&[entry(b"a", raw)], true);
    let offset = f.bytes.windows(raw.len()).position(|v| v == raw).unwrap() as u64;
    struct Damage { bytes: Vec<u8>, flushes: usize, path: PathBuf, offset: u64 }
    impl Write for Damage {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 {
                let mut file = OpenOptions::new().write(true).open(&self.path)?;
                file.seek(SeekFrom::Start(self.offset))?; file.write_all(b"X")?; file.flush()?;
            }
            Ok(())
        }
    }
    let script = format!("0\tsaved-open\t{}\n0\tsaved-find\t1\t1\t10\tneedle\n0\tsaved-hit\t1\t1\t1\n3\tsaved-hit\t1\t1\t1\n3\tread\t1\t0\t64\t4\n3\tcopy\t1\n3\tquit\n", f.path.display());
    let mut input = script.as_bytes(); let mut err = Vec::new();
    let mut out = Damage { bytes: Vec::new(), flushes: 0, path: f.path, offset };
    assert_eq!(run(&args(), &mut input, &mut out, &mut err, || false), EXIT_ERROR); assert!(err.is_empty());
    let rows = records(&out.bytes);
    assert_eq!(rows[3].get("exit_code").number(), EXIT_ERROR as u64);
    assert_eq!(rows[3].get("accepted_revision").number(), 3);
    assert_eq!(rows[3].get("saved_query_generation").number(), 1);
    assert_eq!(result(&rows[4]).get("text").text().as_bytes(), raw);
    assert_eq!(result(&rows[5]).get("original_hex").text(), "6e6565646c65");
}

#[test]
fn trusted_disk_index_attachment_and_failed_replacement_keep_query_identity() {
    let f = Fixture::new(&[entry(b"a", b"needle"), entry(b"b", b"unrelated")], true);
    let (path, pin) = f.index(); let wrong = "00".repeat(32);
    let (exit, rows) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-index-attach\t1\t1\t{}\t{}\n0\tsaved-find\t1\t1\t10\tneedle\n0\tsaved-index-attach\t1\t2\t{}\t{}\n0\tsaved-index-detach\t1\t3\n0\tsaved-hit\t1\t1\t1\n6\tcopy\t1\n6\tquit\n", f.path.display(), path.display(), pin.to_hex(), path.display(), wrong));
    assert_eq!(exit, EXIT_ERROR);
    assert_eq!(result(&rows[1]).get("index_page_loads").number(), 0);
    assert_eq!(result(&rows[2]).get("member_bytes_read").number(), 6);
    assert_eq!(result(&rows[2]).get("index_eliminated_files").number(), 1);
    assert_eq!(rows[3].get("exit_code").number(), EXIT_ERROR as u64);
    assert_eq!(rows[3].get("saved_index_generation").number(), 1);
    assert_eq!(rows[4].get("saved_index_generation"), &Json::Null);
    assert_eq!(rows[4].get("saved_query_generation").number(), 1);
    assert_eq!(result(&rows[6]).get("original_hex").text(), "6e6565646c65");
}
