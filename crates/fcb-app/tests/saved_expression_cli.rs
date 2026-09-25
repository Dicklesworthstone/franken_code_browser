#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
#[path = "support/saved_desk.rs"] mod saved_fixture;
use support::{parse, Json};
use saved_fixture::{entry, Fixture};
use std::{ffi::OsString, fs::{self, OpenOptions}, io::{self, Seek, SeekFrom, Write}, path::PathBuf};
use fcb_app::{run, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL};

fn args() -> Vec<OsString> { vec!["desk".into(), "--stdio".into()] }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|b| *b == b'\n').filter(|s| !s.is_empty()).map(|s| parse(s).unwrap()).collect()
}
fn invoke(script: &str) -> (u8, Vec<Json>, Vec<u8>) {
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args(), &mut input, &mut output, &mut errors, || false);
    (exit, records(&output), errors)
}

#[test]
fn expression_to_selection_bookmark_and_checkpoint_is_an_offline_workflow() {
    let f = Fixture::new(&[entry(b"src/a.rs", b"fn needle() {}\n// required"),
        entry(b"src/b.rs", b"needle required forbidden"), entry(b"src/c.py", b"needle required")], true);
    let checkpoint = f.root.join("reading.fcbk");
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t100\t10000\tneedle required -forbidden lang:rust\n0\tsaved-expression-page\t1\t1\t0\t10\n0\tsaved-expression-hit\t1\t1\t1\n4\tbookmark\t1\tpredicate evidence\n5\tsaved-close\t1\n5\tsave\t{}\t1000\n5\tcopy\t1\n5\tquit\n", f.path.display(), checkpoint.display());
    let (exit, rows, errors) = invoke(&script);
    assert_eq!(exit, EXIT_OK); assert!(errors.is_empty()); assert_eq!(rows.len(), 9);
    assert_eq!(rows[1].get("result").get("retained_hits").number(), 1);
    assert_eq!(rows[3].get("result").get("selection_namespace").text(), "saved-expression");
    assert_eq!(rows[3].get("result").get("expression_hit_id").number(), 1);
    assert_eq!(rows[7].get("result").get("original_hex").text(), "6e6565646c65");
    let (exit, reopened, _) = invoke(&format!("0\trestore\t{}\n1\tcopy\t1\n1\tquit\n", checkpoint.display()));
    assert_eq!(exit, EXIT_OK); assert_eq!(reopened[1].get("result").get("original_hex").text(), "6e6565646c65");
    assert_eq!(reopened[2].get("result").get("bookmarks").array()[0].get("label").text(), "predicate evidence");
}

#[test]
fn failed_replacement_and_delayed_clear_preserve_the_accepted_expression() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required")], true);
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t10\t10000\tneedle required\n0\tsaved-expression\t1\t2\t10\t10000\t\"unterminated\n0\tsaved-expression\t1\t2\t10\t10000\tneedle\n0\tsaved-expression-page\t1\t1\t0\t10\n0\tsaved-expression\t1\t3\t10\t10000\tneedle -required\n0\tsaved-expression-clear\t1\t1\n0\tsaved-expression-page\t1\t3\t0\t10\n0\tsaved-expression-clear\t1\t3\n0\tsaved-expression-page\t1\t3\t0\t10\n0\tsaved-expression\t1\t3\t10\t10000\tneedle\n0\tquit\n", f.path.display());
    let (exit, rows, _) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[2].get("saved_expression_generation").number(), 1);
    assert_eq!(rows[2].get("last_saved_expression_attempt").number(), 2);
    assert_eq!(rows[2].get("error").get("code").text(), "QUERY_SYNTAX_ERROR");
    assert_eq!(rows[3].get("error").get("code").text(), "EXPRESSION_STALE_QUERY");
    assert_eq!(rows[4].get("result").get("retained_hits").number(), 1);
    assert_eq!(rows[6].get("saved_expression_generation").number(), 3);
    assert_eq!(rows[7].get("result").get("retained_hits").number(), 0);
    assert!(rows[7].get("result").get("complete").flag());
    assert_eq!(rows[9].get("error").get("code").text(), "DESK_NO_SAVED_EXPRESSION");
    assert_eq!(rows[10].get("error").get("code").text(), "EXPRESSION_STALE_QUERY");
}

#[test]
fn archive_replacement_rejects_old_tokens_without_retargeting_old_readers() {
    let a = Fixture::new(&[entry(b"a.rs", b"needle required")], true);
    let b = Fixture::new(&[entry(b"a.rs", b"other required")], true);
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t10\t1000\tneedle required\n0\tsaved-expression-hit\t1\t1\t1\n3\tsaved-open\t{}\n3\tsaved-expression-hit\t1\t1\t1\n3\tsaved-expression-page\t4\t1\t0\t10\n3\tcopy\t1\n3\tsaved-expression\t4\t1\t10\t1000\tother required\n3\tquit\n", a.path.display(), b.path.display());
    let (exit, rows, _) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[4].get("error").get("code").text(), "DESK_STALE_SAVED_REPOSITORY");
    assert_eq!(rows[5].get("error").get("code").text(), "DESK_NO_SAVED_EXPRESSION");
    assert_eq!(rows[6].get("result").get("original_hex").text(), "6e6565646c65");
    assert_eq!(rows[7].get("saved_token").number(), 4);
    assert_eq!(rows[7].get("result").get("retained_hits").number(), 1);
}

#[test]
fn terminal_work_and_result_limits_remain_partial_even_with_zero_visible_hits() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required needle")], true);
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t10\t1\tneedle -forbidden\n0\tsaved-expression\t1\t2\t0\t10000\tneedle\n0\tsaved-expression\t1\t3\t1\t10000\tneedle\n0\tsaved-expression\t1\t4\t10\t10000\tneedle required\n0\tquit\n", f.path.display());
    let (exit, rows, _) = invoke(&script); assert_eq!(exit, EXIT_PARTIAL);
    assert_eq!(rows[1].get("result").get("state").text(), "work-limit");
    assert_eq!(rows[1].get("result").get("retained_hits").number(), 0);
    assert!(!rows[1].get("result").get("matches_seen_exact").flag());
    assert!(rows[2].get("result").get("truncated").flag());
    assert_eq!(rows[2].get("result").get("retained_hits").number(), 0);
    assert_eq!(rows[3].get("result").get("retained_hits").number(), 1);
    assert!(rows[4].get("result").get("complete").flag());
}

#[test]
fn literal_index_and_expression_generations_are_independent() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required"), entry(b"b.rs", b"needle required forbidden")], true);
    let (index, pin) = f.index();
    let script = format!("0\tsaved-open\t{}\n0\tsaved-index-attach\t1\t1\t{}\t{}\n0\tsaved-find\t1\t1\t10\tneedle\n0\tsaved-expression\t1\t1\t10\t10000\tneedle required -forbidden\n0\tsaved-clear\t1\t2\n0\tsaved-expression-page\t1\t1\t0\t10\n0\tsaved-index-detach\t1\t2\n0\tsaved-expression-hit\t1\t1\t1\n8\tcopy\t1\n8\tquit\n", f.path.display(), index.display(), pin.to_hex());
    let (exit, rows, _) = invoke(&script); assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[3].get("saved_query_generation").number(), 1);
    assert_eq!(rows[3].get("saved_expression_generation").number(), 1);
    assert_eq!(rows[3].get("saved_index_generation").number(), 1);
    assert!(!rows[3].get("result").get("index_used").flag());
    assert_eq!(rows[5].get("result").get("retained_hits").number(), 1);
    assert_eq!(rows[8].get("result").get("original_hex").text(), "6e6565646c65");
}

#[test]
fn utf16_and_multiline_quoted_phrases_round_trip_through_hex_requests() {
    let mut bytes = vec![0xfe, 0xff]; bytes.extend("😀 a\nb\nrequired".encode_utf16().flat_map(u16::to_be_bytes));
    let f = Fixture::new(&[entry(b"raw-\xff.rs", &bytes)], true);
    let expression = "\"a\nb\" required lang:rs".as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (exit, rows, _) = invoke(&format!("0\tsaved-open\t{}\n0\tsaved-expression-hex\t1\t1\t10\t10000\t{}\n0\tsaved-expression-hit\t1\t1\t1\n3\tcopy\t1\n3\tquit\n", f.path.display(), expression));
    assert_eq!(exit, EXIT_OK);
    assert_eq!(rows[3].get("result").get("original_hex").text(), "0061000a0062");
}

#[test]
fn failed_archive_replacement_preserves_expression_and_invalid_ids_do_not_navigate() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required")], true); let missing = f.root.join("absent.fcbs");
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t10\t1000\tneedle required\n0\tsaved-open\t{}\n0\tsaved-expression-hit\t1\t1\t0\n0\tsaved-expression-hit\t1\t1\t18446744073709551615\n0\tsaved-expression-hit\t1\t1\t1\n6\tcopy\t1\n6\tquit\n", f.path.display(), missing.display());
    let (exit, rows, _) = invoke(&script); assert_eq!(exit, EXIT_ERROR);
    assert_eq!(rows[2].get("saved_expression_generation").number(), 1);
    assert_eq!(rows[3].get("accepted_revision").number(), 0); assert_eq!(rows[4].get("accepted_revision").number(), 0);
    assert_eq!(rows[6].get("result").get("original_hex").text(), "6e6565646c65");
}

#[test]
fn archive_damage_after_query_preserves_metadata_but_refuses_new_activation() {
    let f = Fixture::new(&[entry(b"a.rs", b"needle required")], true);
    let at = f.bytes.windows(15).position(|s| s == b"needle required").unwrap();
    struct Damage { bytes: Vec<u8>, path: PathBuf, at: u64, flushes: usize }
    impl Write for Damage {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 2 { let mut f = OpenOptions::new().write(true).open(&self.path)?;
                f.seek(SeekFrom::Start(self.at))?; f.write_all(b"CHANGED!")?; f.flush()?; }
            Ok(())
        }
    }
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t10\t1000\tneedle required\n0\tsaved-expression-hit\t1\t1\t1\n0\tsaved-expression-page\t1\t1\t0\t10\n0\tquit\n", f.path.display());
    let mut input = script.as_bytes(); let mut out = Damage { bytes: Vec::new(), path: f.path.clone(), at: at as u64 + 7, flushes: 0 };
    assert_eq!(run(&args(), &mut input, &mut out, &mut Vec::new(), || false), EXIT_ERROR);
    let rows = records(&out.bytes); assert_eq!(rows[2].get("accepted_revision").number(), 0);
    assert!(!rows[2].get("error").get("code").text().is_empty());
    assert_eq!(rows[3].get("result").get("retained_hits").number(), 1);
}

#[test]
fn broken_expression_delivery_stops_before_following_navigation_or_export() {
    struct Broken { flushes: usize }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { if self.flushes == 0 { Ok(bytes.len()) } else { Err(io::ErrorKind::BrokenPipe.into()) } }
        fn flush(&mut self) -> io::Result<()> { self.flushes += 1; Ok(()) }
    }
    let f = Fixture::new(&[entry(b"a.rs", b"needle required")], true); let never = f.root.join("never.fcbk");
    let script = format!("0\tsaved-open\t{}\n0\tsaved-expression\t1\t1\t10\t1000\tneedle required\n0\tsaved-expression-hit\t1\t1\t1\n3\tsave\t{}\t1000\n", f.path.display(), never.display());
    let mut input = script.as_bytes(); let mut errors = Vec::new();
    assert_eq!(run(&args(), &mut input, &mut Broken { flushes: 0 }, &mut errors, || false), EXIT_ERROR);
    assert!(!never.exists()); assert!(String::from_utf8(errors).unwrap().contains("DESK_OUTPUT_INTERRUPTED"));
    assert!(fs::metadata(&f.path).is_ok());
}
