#![forbid(unsafe_code)]

mod support;
use std::ffi::OsString;
use fcb_app::{run, EXIT_ERROR, EXIT_OK, EXIT_PARTIAL};
use support::{Json, parse};

fn args() -> Vec<OsString> { ["desk", "--stdio"].map(OsString::from).to_vec() }
fn records(bytes: &[u8]) -> Vec<Json> {
    bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).map(|line| {
        let value = parse(line).unwrap();
        assert_eq!(value.get("schema").text(), "fcb.desk-stdio/1"); value
    }).collect()
}
fn invoke(script: &str) -> (u8, Vec<Json>) {
    let mut input = script.as_bytes(); let mut output = Vec::new(); let mut errors = Vec::new();
    let exit = run(&args(), &mut input, &mut output, &mut errors, || false);
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    (exit, records(&output))
}

#[test]
fn option_grammar_hex_numeric_domains_and_field_limits_are_strict_and_recoverable() {
    let script = concat!(
        "0\tfind-page\t1\t1\tregex\tsensitive\tbounded\t0\t2\t100\tfoo\n",
        "0\tfind-page\t1\t1\tliteral\tfold\tbounded\t0\t2\t100\tfoo\n",
        "0\tfind-page\t1\t1\tliteral\tsensitive\tall\t0\t2\t100\tfoo\n",
        "0\tfind-page\t1\t1\tliteral\tsensitive\tbounded\t01\t2\t100\tfoo\n",
        "0\tfind-page\t1\t1\tliteral\tsensitive\tbounded\t18446744073709551616\t2\t100\tfoo\n",
        "0\tfind-page-text-hex\t1\t1\tliteral\tsensitive\tbounded\t0\t2\t100\tff\n",
        "0\tfind-page-text-hex\t1\t1\tliteral\tsensitive\tbounded\t0\t2\t100\txy\n",
        "0\tfind-page\t1\t1\tliteral\tsensitive\tbounded\t0\t2\t100\tfoo\textra\n",
        "0\tfind\t1\t1\t1\t100\tfoo\textra\textra\textra\textra\n",
        "0\trepo-begin-indexed-expression\t1\t1\t1\tsensitive\tbounded\t1\t100\tall\t-\tfoo\textra\n",
        "0\tquit\n",
    );
    let (exit, rows) = invoke(script);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(rows.len(), 11);
    let expected = ["DESK_FIND_MODE", "DESK_FIND_CASE", "DESK_FIND_COUNT",
        "DESK_INVALID_NUMBER", "DESK_INVALID_NUMBER", "DESK_INVALID_TEXT",
        "DESK_INVALID_HEX", "DESK_FIELD_LIMIT", "DESK_FIELD_LIMIT", "DESK_FIELD_LIMIT"];
    for (row, expected) in rows.iter().zip(expected) {
        assert_eq!(row.get("error").get("code").text(), expected);
        assert_eq!(row.get("accepted_revision").number(), 0);
    }
    assert!(rows[10].get("result").get("panes").array().is_empty());
}

#[test]
fn help_documents_find_modes_windows_and_incomplete_count_semantics() {
    let mut output = Vec::new(); let mut errors = Vec::new();
    let help: Vec<OsString> = ["desk", "--help"].map(OsString::from).to_vec();
    assert_eq!(run(&help, &mut b"".as_slice(), &mut output, &mut errors, || false), EXIT_OK);
    let text = String::from_utf8(output).unwrap();
    for expected in ["find-page-text-hex", "START_AFTER", "count-all", "unicode-lowercase",
        "expression", "identifier", "match", "No page reopens a source path"] {
        assert!(text.contains(expected), "missing {expected}");
    }
}

#[cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod native {
    use super::*;
    use std::{fs, io::{self, Write}, path::PathBuf, sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH}};

    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-desk-find-options-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); path
    }
    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

    #[test]
    fn find_windows_activate_exact_spelling_and_restore_offline_with_personal_notes() {
        let root = fixture(); let source = root.join("source.rs"); let saved = root.join("reading.fcbk");
        let original = b"Anchor anchor\nfoo foobar foo\nrequired phrase";
        fs::write(&source, original).unwrap();
        struct MovingOutput { bytes: Vec<u8>, source: PathBuf, moved: PathBuf, flushes: usize }
        impl Write for MovingOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                if self.flushes == 1 { fs::rename(&self.source, &self.moved)?; }
                Ok(())
            }
        }
        let expression = hex(b"anchor \"required phrase\" -missing");
        let script = format!(
            "0\topen\t{}\n\
             1\tfind-page\t1\t1\tliteral\tunicode-lowercase\tcount-all\t0\t1\t1000\tanchor\n\
             1\thit\t1\t1\t0\n\
             3\tcopy\t1\n\
             3\tannotate\t1\twhy\tretain this explanation\n\
             5\tfind-page-text-hex\t1\t2\texpression\tunicode-lowercase\tcount-all\t1\t1\t1000\t{expression}\n\
             5\thit\t1\t2\t0\n\
             7\tcopy\t1\n\
             7\tsave\t{}\t1000\n\
             7\tquit\n", source.display(), saved.display());
        let mut input = script.as_bytes(); let mut errors = Vec::new();
        let mut output = MovingOutput { bytes: Vec::new(), source: source.clone(),
            moved: root.join("moved.rs"), flushes: 0 };
        let exit = run(&args(), &mut input, &mut output, &mut errors, || false);
        assert_eq!(exit, EXIT_PARTIAL); assert!(errors.is_empty()); assert!(!source.exists());
        let first = records(&output.bytes);
        assert_eq!(first.len(), 10);
        let page = first[1].get("result");
        assert_eq!(page.get("matches_seen").number(), 2);
        assert!(page.get("match_count_complete").flag()); assert!(page.get("has_more_matches").flag());
        assert_eq!(first[3].get("result").get("original_hex").text(), hex(b"Anchor"));
        let second_page = first[5].get("result");
        assert!(second_page.get("expression_qualified").flag());
        assert_eq!(second_page.get("hits").array()[0].get("hit").number(), 0);
        assert_eq!(second_page.get("hits").array()[0].get("occurrence_id").number(), 2);
        assert!(!second_page.get("has_more_matches").flag());
        assert_eq!(first[7].get("result").get("original_hex").text(), hex(b"anchor"));
        assert_eq!(first[9].get("result").get("initial_source_bytes_read").number(), original.len() as u64);

        let (exit, restored) = invoke(&format!(
            "0\trestore\t{}\n\
             1\tfind-page\t1\t1\tidentifier\tsensitive\tcount-all\t1\t1\t1000\tfoo\n\
             1\thit\t1\t1\t0\n\
             3\tcopy\t1\n\
             3\tfind-page\t1\t2\tliteral\tunicode-lowercase\tcount-all\t0\t0\t1000\tanchor\n\
             3\tstate\n\
             3\tquit\n", saved.display()));
        assert_eq!(exit, EXIT_PARTIAL);
        assert_eq!(restored[1].get("result").get("matches_seen").number(), 2);
        assert_eq!(restored[1].get("result").get("hits").array()[0].get("occurrence_id").number(), 2);
        assert_eq!(restored[3].get("result").get("original_hex").text(), hex(b"foo"));
        assert!(restored[4].get("result").get("hits").array().is_empty());
        assert!(restored[4].get("result").get("match_count_complete").flag());
        assert_eq!(restored[4].get("result").get("matches_seen").number(), 2);
        let state = restored[5].get("result");
        assert_eq!(state.get("initial_source_bytes_read").number(), 0);
        assert_eq!(state.get("bookmarks").array()[0].get("body").text(), "retain this explanation");
        for row in &restored {
            assert_eq!(row.get("result").get("additional_source_bytes_read").number(), 0);
        }
    }

    #[test]
    fn failed_mode_replacements_preserve_legacy_find_until_a_new_page_is_accepted() {
        let root = fixture(); let source = root.join("source.rs");
        fs::write(&source, b"first second").unwrap();
        let script = format!(
            "0\topen\t{}\n\
             1\tfind\t1\t1\t10\t100\tfirst\n\
             1\tfind-page\t1\t2\texpression\tsensitive\tbounded\t0\t10\t100\tsecond path:source.rs\n\
             1\tfind-page\t1\t3\tidentifier\tunicode-lowercase\tbounded\t0\t10\t100\tsecond\n\
             1\tfind-page-text-hex\t1\t4\tliteral\tsensitive\tbounded\t0\t10\t100\tff\n\
             1\tfind-page\t1\t4\tliteral\tsensitive\tbounded\t0\t10\t100\tsecond\n\
             1\thit\t1\t1\t0\n\
             1\thit\t1\t4\t0\n\
             8\tcopy\t1\n\
             8\tquit\n", source.display());
        let (exit, rows) = invoke(&script);
        assert_eq!(exit, EXIT_ERROR); assert_eq!(rows.len(), 10);
        assert_eq!(rows[2].get("error").get("code").text(), "DESK_EXPRESSION_SCOPE_UNSUPPORTED");
        assert_eq!(rows[3].get("error").get("code").text(), "DESK_INVALID_LIMITS");
        assert_eq!(rows[4].get("error").get("code").text(), "DESK_INVALID_TEXT");
        for row in &rows[1..5] { assert_eq!(row.get("accepted_query_generation").number(), 1); }
        assert_eq!(rows[5].get("accepted_query_generation").number(), 4);
        assert_eq!(rows[6].get("error").get("code").text(), "DESK_STALE_QUERY");
        assert_eq!(rows[8].get("result").get("original_hex").text(), hex(b"second"));
    }

    #[test]
    fn encoded_multiline_literal_uses_one_field_and_copies_the_exact_source_range() {
        let root = fixture(); let source = root.join("source.rs");
        fs::write(&source, b"prefix a\tb\nc suffix").unwrap();
        let (exit, rows) = invoke(&format!(
            "0\topen\t{}\n\
             1\tfind-page-text-hex\t1\t1\tliteral\tsensitive\tbounded\t0\t1\t100\t{}\n\
             1\thit\t1\t1\t0\n\
             3\tcopy\t1\n\
             3\tquit\n", source.display(), hex(b"a\tb\nc")));
        assert_eq!(exit, EXIT_OK);
        assert_eq!(rows[3].get("result").get("original_hex").text(), hex(b"a\tb\nc"));
    }
}
