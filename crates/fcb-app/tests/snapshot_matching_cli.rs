#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production CLI parity across direct, segment, resident-posting and paged
//! searches. These fixtures assert original bytes independently of query text.

mod support;
use std::{ffi::OsString, fs, path::PathBuf, process::Command,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{run, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR, EXIT_CANCELED};
use support::{Json, parse};

#[derive(Clone, Copy, Debug)]
enum Layout { Segments, Postings, Paged }
struct Index { layout: Layout, pin: String }
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-saved-matching-{}-{nonce}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); fs::create_dir(path.join("root")).unwrap(); Self(path)
    }
    fn root(&self) -> PathBuf { self.0.join("root") }
    fn saved(&self) -> PathBuf { self.0.join("source.fcbs") }
    fn index(&self, layout: Layout) -> PathBuf {
        self.0.join(match layout { Layout::Segments => "search.fcbi", Layout::Postings => "search.fcbo", Layout::Paged => "search.fcbd" })
    }
    fn save(&self, extra: &[&str]) -> (u8, Json) {
        let mut args = vec!["snapshot".into(), "save".into(), self.root().into_os_string(),
            "--output".into(), self.saved().into_os_string(), "--json".into()];
        args.extend(extra.iter().map(OsString::from)); invoke(&args, || false)
    }
    fn build(&self, layout: Layout) -> Index {
        let mut args = vec!["snapshot".into(), "index".into(), "build".into(), self.saved().into_os_string(),
            "--output".into(), self.index(layout).into_os_string(), "--json".into()];
        match layout { Layout::Segments => {}, Layout::Postings => args.push("--inverted".into()), Layout::Paged => args.push("--paged".into()) }
        let (exit, receipt) = invoke(&args, || false);
        assert!(matches!(exit, EXIT_OK | EXIT_PARTIAL));
        Index { layout, pin: receipt.get("index_digest").text().to_owned() }
    }
    fn indexes(&self) -> Vec<Index> {
        [Layout::Segments, Layout::Postings, Layout::Paged].into_iter().map(|layout| self.build(layout)).collect()
    }
    fn args(&self, index: Option<&Index>, extra: &[&str]) -> Vec<OsString> {
        let mut args = vec!["snapshot".into()];
        if index.is_some() { args.push("index".into()); }
        args.extend(["search".into(), self.saved().into_os_string(), "--json".into()]);
        if let Some(index) = index {
            args.extend(["--index".into(), self.index(index.layout).into_os_string(), "--index-digest".into(), index.pin.clone().into()]);
            if matches!(index.layout, Layout::Paged) { args.push("--paged".into()); }
        }
        args.extend(extra.iter().map(OsString::from)); args
    }
    fn search(&self, index: Option<&Index>, extra: &[&str]) -> (u8, Json) { invoke(&self.args(index, extra), || false) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn invoke(args: &[OsString], canceled: impl FnMut() -> bool) -> (u8, Json) {
    let mut out = Vec::new(); let mut err = Vec::new();
    let exit = run(args, &mut &b""[..], &mut out, &mut err, canceled);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    let result = parse(&out).unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&out)));
    assert_eq!(result.get("schema").text(), "fcb.cli/1"); (exit, result)
}
fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() {
        let pair = if big_endian { word.to_be_bytes() } else { word.to_le_bytes() };
        bytes.extend_from_slice(&pair);
    }
    bytes
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn rows<'a>(result: &'a Json, path: &str) -> Vec<&'a Json> {
    let path = hex(path.as_bytes());
    result.get("hits").array().iter().filter(|hit| hit.get("path").get("hex").text() == path).collect()
}
fn assert_source(hit: &Json, source: &[u8], start: u64, end: u64, spelling: &str) {
    assert_eq!(hit.get("original_range").get("start").number(), start);
    assert_eq!(hit.get("original_range").get("end").number(), end);
    assert_eq!(hit.get("original_hex").text(), hex(&source[start as usize..end as usize]));
    assert_eq!(hit.get("matched_text").text(), spelling);
    assert_eq!(hit.get("matched_raw_hex"), &Json::Null);
    assert_eq!(hit.get("source_digest").text().len(), 64);
}

#[test]
fn lowercase_hits_keep_actual_utf8_and_both_utf16_spellings_and_overlapping_ranges_offline() {
    let f = Fixture::new();
    let a = b"BaNaNa BANANA".to_vec(); let b = utf16("bAnAnA", false); let c = utf16("BANANA", true);
    for (name, bytes) in [("a.rs", &a), ("b.rs", &b), ("c.rs", &c)] { fs::write(f.root().join(name), bytes).unwrap(); }
    assert_eq!(f.save(&[]).0, EXIT_OK); let indexes = f.indexes();
    fs::rename(f.root(), f.0.join("original-root")).unwrap(); fs::create_dir(f.root()).unwrap();
    fs::write(f.root().join("a.rs"), b"replacement live data must not participate").unwrap();
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, found) = f.search(index, &["--text", "ana", "--ignore-case", "--count-all"]);
        assert_eq!(exit, EXIT_OK); assert!(found.get("workspace_complete").flag());
        assert!(found.get("matches_seen_exact").flag()); assert_eq!(found.get("matches_seen").number(), 8);
        assert_eq!(found.get("matching_files").number(), 3); assert_eq!(found.get("stored_hits").number(), 8);
        assert!(!found.get("truncated").flag()); assert!(!found.get("live_roots_accessed").flag());
        assert_eq!(found.get("mode").text(), "unicode-lowercase-literal"); assert!(!found.get("case_sensitive").flag());
        let hits = rows(&found, "a.rs"); assert_eq!(hits.len(), 4);
        for (hit, start, end, spelling) in [
            (hits[0], 1, 4, "aNa"), (hits[1], 3, 6, "aNa"),
            (hits[2], 8, 11, "ANA"), (hits[3], 10, 13, "ANA")] {
            assert_source(hit, &a, start, end, spelling);
        }
        let hits = rows(&found, "b.rs"); assert_eq!(hits.len(), 2);
        assert_source(hits[0], &b, 4, 10, "AnA"); assert_source(hits[1], &b, 8, 14, "AnA");
        let hits = rows(&found, "c.rs"); assert_eq!(hits.len(), 2);
        assert_source(hits[0], &c, 4, 10, "ANA"); assert_source(hits[1], &c, 8, 14, "ANA");
        assert_eq!(found.get("member_payload_io_scope").text(), "query");
        assert_eq!(found.get("member_payload_bytes_loaded").number(), (a.len() + b.len() + c.len()) as u64);
        assert_eq!(found.get("match_witness_payload_bytes_loaded").number(), (a.len() + b.len() + c.len()) as u64);
        assert_eq!(found.get("match_witness_members_loaded").number(), 3, "one witness load per file, not per hit");
        for (position, hit) in found.get("hits").array().iter().enumerate() {
            assert_eq!(hit.get("hit_id").number(), position as u64 + 1);
        }
    }
}

#[test]
fn exact_counts_are_independent_of_retention_and_count_only_positive_and_zero_exit_correctly() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"banana banana").unwrap();
    fs::write(f.root().join("b"), b"banana").unwrap(); fs::write(f.root().join("c"), b"unrelated").unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK); let indexes = f.indexes();
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, counted) = f.search(index, &["--text", "ana", "--count-all", "--limit", "1"]);
        assert_eq!(exit, EXIT_OK, "a complete explicitly requested count is successful despite capped rows");
        assert_eq!(counted.get("matches_seen").number(), 6); assert!(counted.get("matches_seen_exact").flag());
        assert!(counted.get("workspace_complete").flag()); assert!(counted.get("truncated").flag());
        assert_eq!(counted.get("stored_hits").number(), 1); assert_eq!(counted.get("matching_files").number(), 2);
        assert_eq!(counted.get("retained_matching_files").number(), 1);
        assert_eq!(counted.get("match_witness_members_loaded").number(), 1);
        let (exit, bounded) = f.search(index, &["--text", "ana", "--limit", "1"]);
        assert_eq!(exit, EXIT_PARTIAL); assert!(!bounded.get("matches_seen_exact").flag());
        assert!(!bounded.get("count_all_matches").flag()); assert_eq!(bounded.get("matches_seen").number(), 2);
        for mode in [["--text", "ana"], ["--raw-hex", "616e61"]] {
            let (exit, only_count) = f.search(index, &[mode[0], mode[1], "--count-all", "--limit", "0"]);
            assert_eq!(exit, EXIT_OK); assert_eq!(only_count.get("matches_seen").number(), 6);
            assert_eq!(only_count.get("matching_files").number(), 2); assert!(only_count.get("matches_seen_exact").flag());
            assert!(only_count.get("truncated").flag()); assert!(only_count.get("hits").array().is_empty());
            assert_eq!(only_count.get("match_witness_payload_bytes_loaded").number(), 0);
            assert_eq!(only_count.get("match_witness_members_loaded").number(), 0);
        }
        let (exit, absent) = f.search(index, &["--text", "absent", "--count-all", "--limit", "0"]);
        assert_eq!(exit, EXIT_NO_MATCH); assert_eq!(absent.get("matches_seen").number(), 0);
        assert!(absent.get("matches_seen_exact").flag()); assert!(!absent.get("truncated").flag());
        if index.is_some() {
            assert!(counted.get("index_used").flag()); assert_eq!(counted.get("index_eliminated_files").number(), 1);
            assert_eq!(counted.get("member_payload_bytes_loaded").number(), 19);
        }
    }
}

#[test]
fn every_index_layout_bypasses_incompatible_case_sensitive_candidates_without_false_negatives() {
    let f = Fixture::new();
    for (path, body) in [("a", "BEACON"), ("b", "beacon"), ("c", "BeAcOn"), ("d", "unrelated")] {
        fs::write(f.root().join(path), body).unwrap();
    }
    assert_eq!(f.save(&[]).0, EXIT_OK);
    for index in f.indexes() {
        let (exit, exact) = f.search(Some(&index), &["--text", "beacon", "--count-all"]);
        assert_eq!(exit, EXIT_OK); assert_eq!(exact.get("matches_seen").number(), 1);
        assert!(exact.get("index_used").flag()); assert_eq!(exact.get("index_eliminated_files").number(), 3);
        let (exit, folded) = f.search(Some(&index), &["--text", "beacon", "--ignore-case", "--count-all"]);
        assert_eq!(exit, EXIT_OK); assert_eq!(folded.get("matches_seen").number(), 3);
        assert!(!folded.get("index_used").flag());
        assert_eq!(folded.get("candidate_strategy").text(), "direct-verified-member-scan");
        assert_eq!(folded.get("index_policy").text(), "direct-verified-member-scan");
        assert_eq!(folded.get("index_bypass_reason").text(), "incompatible-normalization");
        assert_eq!(folded.get("index_eliminated_files").number(), 0);
        assert_eq!(folded.get("posting_entries_visited").number(), 0);
        assert_eq!(folded.get("loaded_members").number(), 4);
        assert_eq!(folded.get("member_payload_bytes_loaded").number(), 27);
        if matches!(index.layout, Layout::Paged) {
            assert_eq!(folded.get("index_page_bytes_read").number(), 0);
            assert_eq!(folded.get("index_page_loads").number(), 0);
        }
        assert_eq!(rows(&folded, "a")[0].get("matched_text").text(), "BEACON");
        assert_eq!(rows(&folded, "c")[0].get("matched_text").text(), "BeAcOn");
    }
}

#[test]
fn scalar_lowercase_expansions_copy_complete_original_units_without_claiming_casefold_or_normalization() {
    let f = Fixture::new(); let original = "İ i\u{307} K K k ß ss é e\u{301}";
    fs::write(f.root().join("a.rs"), original).unwrap(); assert_eq!(f.save(&[]).0, EXIT_OK);
    let indexes = f.indexes();
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, expanded) = f.search(index, &["--text", "i", "--ignore-case", "--count-all"]);
        assert_eq!(exit, EXIT_OK); assert_eq!(expanded.get("matches_seen").number(), 2);
        assert_source(&expanded.get("hits").array()[0], original.as_bytes(), 0, 2, "İ");
        assert_source(&expanded.get("hits").array()[1], original.as_bytes(), 3, 4, "i");
        assert!(!expanded.get("full_case_folding").flag()); assert!(!expanded.get("canonical_normalization").flag());
        let (_, dot) = f.search(index, &["--text", "\u{307}", "--ignore-case", "--count-all"]);
        assert_eq!(dot.get("matches_seen").number(), 2);
        assert_source(&dot.get("hits").array()[0], original.as_bytes(), 0, 2, "İ");
        assert_source(&dot.get("hits").array()[1], original.as_bytes(), 4, 6, "\u{307}");
        let (_, kelvin) = f.search(index, &["--text", "k", "--ignore-case", "--count-all"]);
        assert_eq!(kelvin.get("matches_seen").number(), 3);
        assert_eq!(kelvin.get("hits").array()[0].get("matched_text").text(), "K");
        let (_, sharp_s) = f.search(index, &["--text", "ss", "--ignore-case", "--count-all"]);
        assert_eq!(sharp_s.get("matches_seen").number(), 1, "scalar lowercase must not turn ß into ss");
        assert_eq!(sharp_s.get("hits").array()[0].get("matched_text").text(), "ss");
        let (_, composed) = f.search(index, &["--text", "é", "--ignore-case", "--count-all"]);
        assert_eq!(composed.get("matches_seen").number(), 1, "decomposed text is not silently normalized");
    }
}

#[test]
fn source_witnesses_preserve_interior_boms_without_reintroducing_encoding_headers() {
    let f = Fixture::new();
    let mut utf8 = vec![0xef, 0xbb, 0xbf]; utf8.extend_from_slice("x\u{feff}y".as_bytes());
    let little = utf16("x\u{feff}y", false); let big = utf16("x\u{feff}y", true);
    for (name, bytes) in [("a.rs", &utf8), ("b.rs", &little), ("c.rs", &big)] {
        fs::write(f.root().join(name), bytes).unwrap();
    }
    assert_eq!(f.save(&[]).0, EXIT_OK); let indexes = f.indexes();
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, found) = f.search(index, &["--text", "\u{feff}", "--count-all"]);
        assert_eq!(exit, EXIT_OK); assert_eq!(found.get("matches_seen").number(), 3);
        assert_source(rows(&found, "a.rs")[0], &utf8, 4, 7, "\u{feff}");
        assert_source(rows(&found, "b.rs")[0], &little, 4, 6, "\u{feff}");
        assert_source(rows(&found, "c.rs")[0], &big, 4, 6, "\u{feff}");
    }
}

#[test]
fn unavailable_and_malformed_saved_members_keep_count_only_results_partial() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"NEEDLE needle").unwrap();
    fs::write(f.root().join("b"), [0, 0xff, 1, 2]).unwrap();
    fs::write(f.root().join("c"), vec![b'x'; 128]).unwrap();
    assert_eq!(f.save(&["--max-file-bytes", "64"]).0, EXIT_PARTIAL);
    let indexes = f.indexes();
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        let (exit, result) = f.search(index, &["--text", "needle", "--ignore-case", "--count-all", "--limit", "0"]);
        assert_eq!(exit, EXIT_PARTIAL); assert!(!result.get("workspace_complete").flag());
        assert!(!result.get("matches_seen_exact").flag()); assert_eq!(result.get("matches_seen").number(), 2);
        assert_eq!(result.get("unsupported_text_files_count").number(), 1);
        assert_eq!(result.get("unavailable_files_count").number(), 1);
        assert!(result.get("hits").array().is_empty());
        let (exit, absent) = f.search(index, &["--text", "absent", "--ignore-case", "--count-all", "--limit", "0"]);
        assert_eq!(exit, EXIT_PARTIAL, "unknown source cannot prove no match");
        assert_eq!(absent.get("matches_seen").number(), 0); assert!(!absent.get("matches_seen_exact").flag());
    }
}

#[test]
fn unsupported_flag_combinations_fail_before_io_and_literal_flag_values_stay_literal() {
    for tail in [
        vec!["search", "missing.fcbs", "--raw-hex", "61", "--ignore-case"],
        vec!["search", "missing.fcbs", "--query", "needle path:src", "--text", "needle", "--ignore-case"],
        vec!["search", "missing.fcbs", "--query", "needle", "--count-all", "--raw-hex", "61", "--ignore-case"],
        vec!["structure", "missing.fcbs", "--mode", "occurrences", "--name", "needle", "--ignore-case"],
        vec!["structure", "missing.fcbs", "--mode", "symbols", "--name", "needle", "--count-all"],
        vec!["save", "missing-root", "--output", "unused.fcbs", "--count-all"],
        vec!["index", "search", "missing.fcbs", "--index", "missing.fcbi", "--index-digest",
            "0000000000000000000000000000000000000000000000000000000000000000", "--raw-hex", "61", "--ignore-case"],
    ] {
        let mut args = vec![OsString::from("snapshot")]; args.extend(tail.iter().map(OsString::from)); args.push("--json".into());
        let (exit, result) = invoke(&args, || false);
        assert_eq!(exit, EXIT_ERROR); assert_eq!(result.get("error").get("code").text(), "CLI_INCOMPATIBLE_OPTIONS");
    }
    let f = Fixture::new(); fs::write(f.root().join("a"), b"--ignore-case --count-all").unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK); let indexes = f.indexes();
    for index in std::iter::once(None).chain(indexes.iter().map(Some)) {
        for flag in ["--ignore-case", "--count-all"] {
            let (exit, value) = f.search(index, &["--text", flag]);
            assert_eq!(exit, EXIT_OK); assert!(value.get("case_sensitive").flag()); assert!(!value.get("count_all_matches").flag());
            assert_eq!(value.get("hits").array()[0].get("matched_text").text(), flag);
            let (exit, duplicate) = f.search(index, &["--text", "x", flag, flag]);
            assert_eq!(exit, EXIT_ERROR); assert_eq!(duplicate.get("error").get("code").text(), "CLI_DUPLICATE_OPTION");
        }
    }
}

#[test]
fn ignore_case_keeps_pin_checks_and_reports_unread_posting_pages_truthfully() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"BEACON").unwrap(); assert_eq!(f.save(&[]).0, EXIT_OK);
    let indexes = f.indexes();
    for index in &indexes {
        let mut bad = f.args(Some(index), &["--text", "beacon", "--ignore-case", "--count-all"]);
        let pin = bad.iter().position(|arg| arg == "--index-digest").unwrap() + 1;
        bad[pin] = "00".repeat(32).into();
        let (exit, result) = invoke(&bad, || false); assert_eq!(exit, EXIT_ERROR);
        assert!(!result.get("complete").flag());
    }
    let paged = indexes.iter().find(|index| matches!(index.layout, Layout::Paged)).unwrap();
    let mut bytes = fs::read(f.index(paged.layout)).unwrap();
    let body = 32 + u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    bytes[body] ^= 1; fs::write(f.index(paged.layout), bytes).unwrap();
    let (exit, direct) = f.search(Some(paged), &["--text", "beacon", "--ignore-case", "--count-all"]);
    assert_eq!(exit, EXIT_OK); assert_eq!(direct.get("matches_seen").number(), 1);
    assert_eq!(direct.get("index_page_bytes_read").number(), 0); assert!(!direct.get("index_used").flag());
    assert!(!direct.get("index_body_verified_on_open").flag());
    let (exit, exact) = f.search(Some(paged), &["--text", "BEACON", "--count-all"]);
    assert_eq!(exit, EXIT_ERROR); assert_eq!(exact.get("error").get("code").text(), "PAGED_INDEX_CHANGED_PAGE");
}

#[test]
fn cancellation_withdraws_a_running_count_instead_of_publishing_an_exact_partial_total() {
    let f = Fixture::new();
    for name in ["a", "b", "c"] { fs::write(f.root().join(name), "BANANA ".repeat(4096)).unwrap(); }
    assert_eq!(f.save(&[]).0, EXIT_OK); let index = f.build(Layout::Paged);
    let args = f.args(Some(&index), &["--text", "ana", "--ignore-case", "--count-all", "--limit", "0"]);
    let mut polls = 0usize;
    let (exit, complete) = invoke(&args, || { polls += 1; false });
    assert_eq!(exit, EXIT_OK); assert_eq!(complete.get("matches_seen").number(), 3 * 4096 * 2);
    assert!(polls > 20);
    let cutoff = polls / 2; let mut seen = 0usize;
    let (exit, canceled) = invoke(&args, || { seen += 1; seen >= cutoff });
    assert_eq!(exit, EXIT_CANCELED); assert!(!canceled.get("complete").flag());
    assert_eq!(canceled.get("effect").text(), "none");
    if let Json::Object(fields) = canceled { assert!(!fields.contains_key("hits")); assert!(!fields.contains_key("matches_seen_exact")); }
    else { panic!("expected error object"); }
}

#[test]
fn actual_binary_counts_saved_sources_and_prints_meaningful_human_totals_after_live_roots_move() {
    let f = Fixture::new(); fs::write(f.root().join("a"), b"BEACON beacon").unwrap();
    assert_eq!(f.save(&[]).0, EXIT_OK); let index = f.build(Layout::Paged);
    fs::rename(f.root(), f.0.join("moved-root")).unwrap();
    for route in [None, Some(&index)] {
        let output = Command::new(env!("CARGO_BIN_EXE_fcb"))
            .args(f.args(route, &["--text", "beacon", "--ignore-case", "--count-all", "--limit", "0"])).output().unwrap();
        assert_eq!(output.status.code(), Some(EXIT_OK as i32)); assert!(output.stderr.is_empty());
        let result = parse(&output.stdout).unwrap(); assert_eq!(result.get("matches_seen").number(), 2);
        assert!(result.get("matches_seen_exact").flag()); assert!(result.get("hits").array().is_empty());
        let mut args = f.args(route, &["--text", "beacon", "--ignore-case", "--count-all", "--limit", "0"]);
        args.retain(|arg| arg != "--json");
        let human = Command::new(env!("CARGO_BIN_EXE_fcb")).args(args).output().unwrap();
        assert_eq!(human.status.code(), Some(EXIT_OK as i32)); assert!(human.stderr.is_empty());
        let text = String::from_utf8(human.stdout).unwrap();
        assert!(text.contains("Matches seen: 2 (exact); retained hits: 0; matching files: 1."));
        let absent = Command::new(env!("CARGO_BIN_EXE_fcb"))
            .args(f.args(route, &["--text", "absent", "--ignore-case", "--count-all", "--limit", "0"])).output().unwrap();
        assert_eq!(absent.status.code(), Some(EXIT_NO_MATCH as i32));
    }
}
