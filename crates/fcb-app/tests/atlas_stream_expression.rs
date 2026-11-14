#![forbid(unsafe_code)]
#![cfg(unix)]

//! Native-host expression requests use the shared parser and streaming engine
//! over real files. The independent JSON/byte oracles check predicate evidence,
//! scopes, counts and immutable hit activation, not a fabricated native result.
//! Code-first fixtures: strict-RCH execution and physical-Mac proof are pending.
mod support;

use std::{cell::Cell, ffi::OsStr, fs, os::unix::ffi::OsStrExt, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse,
    atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{AtlasSearchPathKind, AtlasSearchScope},
    atlas_stream_search::{AtlasStreamSearchError, AtlasStreamSearchOptions, RetainedAtlasStreamSearch}}};
use support::{Json, parse};

const CHUNK: usize = 16 * 1024;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-host-stream-expression-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn atlas(&self) -> AtlasSession {
        static NEXT: AtomicU64 = AtomicU64::new(79_000);
        AtlasSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(),
            &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
// Retain fixture inputs for the independent batch-verification/replay lane.
fn opts() -> AtlasStreamSearchOptions { AtlasStreamSearchOptions::default() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(hit: &Json) -> (u64, u64) {
    (hit.get("original_range").get("start").number(), hit.get("original_range").get("end").number())
}
fn encoded(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "utf16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => text.as_bytes().to_vec(),
    }
}
fn run(service: &mut RetainedAtlasStreamSearch, atlas: &AtlasSession, query: &str,
    options: AtlasStreamSearchOptions, scope: AtlasSearchScope) -> HostResponse {
    service.search_scoped_expression_with_progress(atlas, query, options, scope,
        || false, || false, |_| {}).unwrap()
}
fn policy(receipt: &Json, query: &str, primary: &str) {
    assert_eq!(receipt.get("mode").text(), "decoded-text-expression");
    assert_eq!(receipt.get("expression").text(), query);
    assert_eq!(receipt.get("needle").text(), primary);
    assert_eq!(receipt.get("primary_needle").text(), primary);
    assert_eq!(receipt.get("query_syntax").text(), "fcb-bounded-expression/1");
    assert_eq!(receipt.get("predicate_scope").text(), "same-observed-stream");
    assert_eq!(receipt.get("path_filter_domain").text(), "root-relative-utf8-or-escaped-search-key");
    assert_eq!(receipt.get("capture_kind").text(), "literal-witnesses-only");
    assert!(receipt.get("count_all_matches").flag());
    assert_eq!(receipt.get("eligible_files").number(), receipt.get("expression_scope_files").number()
        + receipt.get("expression_excluded_files").number());
}
fn coverage(receipt: &Json) {
    assert_eq!(receipt.get("files_examined").number() + receipt.get("files_unexamined").number(),
        receipt.get("expression_scope_files").number());
    assert_eq!(receipt.get("eligible_files").number() + receipt.get("excluded_files").number(),
        receipt.get("catalogued_files").number());
    assert_eq!(receipt.get("truncated").flag(),
        receipt.get("matches_seen").number() > receipt.get("retained_hits").number());
    assert!(receipt.get("scanned_input_bytes").number() <= receipt.get("payload_bytes_read").number());
    assert!(receipt.get("peak_input_buffer_bytes").number() <= CHUNK as u64);
}
fn window(receipt: &Json, start_after: u64, skipped: u64, more: bool) {
    assert_eq!(receipt.get("page_start_after").number(), start_after);
    assert_eq!(receipt.get("skipped_matches").number(), skipped);
    assert_eq!(receipt.get("page_has_more_matches").flag(), more);
}
fn echo(receipt: &Json, original: &Json) {
    for key in ["owner", "source_manifest", "layout_revision", "query_generation", "root_path", "mode",
        "needle", "expression", "primary_needle", "query_syntax", "predicate_scope", "path_filter_domain",
        "eligible_files", "excluded_files", "expression_scope_files", "expression_excluded_files",
        "search_scope", "scope_extensions", "scope_filenames", "scope_path_kind", "scope_path_hex",
        "case_sensitive", "count_all_matches", "page_start_after"] {
        assert_eq!(receipt.get(key), original.get(key), "immutable query field changed: {key}");
    }
}

#[test]
fn late_predicates_qualify_whole_files_and_count_beyond_retained_rows() {
    let fixture = Fixture::new();
    let mut accepted = b"banana ".to_vec();
    accepted.resize(2 * CHUNK, b'x'); accepted.extend_from_slice(b" required");
    let mut rejected = b"banana required ".to_vec();
    rejected.resize(2 * CHUNK, b'x'); rejected.extend_from_slice(b" forbidden");
    let later = b"banana banana required";
    let absent = b"absent required";
    fixture.write("a.rs", &accepted); fixture.write("b.rs", &rejected);
    fixture.write("c.rs", later); fixture.write("d.rs", absent);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "ana required -forbidden";
    let mut progress = Vec::new();
    let response = service.search_scoped_expression_with_progress(&atlas, query,
        AtlasStreamSearchOptions { max_matches: 1, ..opts() }, AtlasSearchScope::all(),
        || false, || false, |response| progress.push(json(response))).unwrap();
    let report = json(&response);
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "retained rows are truncated independently of coverage");
    policy(&report, query, "ana"); coverage(&report);
    assert!(report.get("search_complete").flag()); assert!(report.get("match_count_complete").flag());
    assert!(report.get("truncated").flag()); assert_eq!(report.get("stop_reason"), &Json::Null);
    assert_eq!(report.get("matches_seen").number(), 6);
    assert_eq!(report.get("matching_files").number(), 2);
    assert_eq!(report.get("retained_matching_files").number(), 1);
    assert_eq!(report.get("retained_hits").number(), 1);
    assert_eq!(report.get("files_examined").number(), 4);
    assert_eq!(report.get("incomplete_files").number(), 0);
    assert_eq!(report.get("payload_bytes_read").number(),
        (accepted.len() + rejected.len() + later.len() + absent.len()) as u64);
    assert_eq!(bounds(&report.get("hits").array()[0]), (1, 4));
    assert!(!progress.is_empty());
    for frame in &progress {
        policy(frame, query, "ana"); coverage(frame); echo(frame, &report);
        assert!(!frame.get("terminal").flag()); assert!(!frame.get("selection_available").flag());
        if frame.get("files_examined").number() == 0 {
            assert_eq!(frame.get("matches_seen").number(), 0);
            assert!(frame.get("hits").array().is_empty());
        }
        assert!(report.get("hits").array().starts_with(frame.get("hits").array()));
    }
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    fixture.write("a.rs", b"new live bytes without the previous expression");
    let page = json(&service.page(&atlas, generation, 0, 32).unwrap());
    let witness = json(&service.hit(&atlas, generation, 0).unwrap());
    echo(&page, &report); echo(&witness, &report);
    assert_eq!(witness.get("hit").get("text").text(), "ana");
    assert_eq!(witness.get("hit").get("original_hex").text(), hex(&accepted[1..4]));
    assert_eq!(witness.get("additional_source_bytes_read").number(), 0);
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn expression_metadata_intersects_native_scope_before_any_source_quota() {
    let fixture = Fixture::new(); let accepted = b"banana required"; let rejected = b"banana required forbidden";
    fixture.write("a.py", &vec![0xff; 2 * CHUNK]); fixture.write("other/wrong.rs", &[0xff]);
    fixture.write("src/a.rs", accepted); fixture.write("src/b.rs", rejected);
    fixture.write("src/skip.rs", &[0xff]); fixture.write("src/readme.md", &[0xff]);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "ana required -forbidden path:src/ -path:skip lang:rust";
    let scope = AtlasSearchScope::from_csv(Some("rs,md"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let response = run(&mut service, &atlas, query, AtlasStreamSearchOptions {
        max_scan_bytes: (accepted.len() + rejected.len()) as u64, max_read_calls: 2, ..opts() }, scope);
    assert_eq!(response.exit_code(), EXIT_OK);
    let report = json(&response); policy(&report, query, "ana"); coverage(&report);
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("catalogued_files").number(), 6);
    assert_eq!(report.get("eligible_files").number(), 4);
    assert_eq!(report.get("excluded_files").number(), 2);
    assert_eq!(report.get("expression_scope_files").number(), 2);
    assert_eq!(report.get("expression_excluded_files").number(), 2);
    assert_eq!(report.get("files_examined").number(), 2);
    assert_eq!(report.get("incomplete_files").number(), 0);
    assert_eq!(report.get("payload_bytes_read").number(), (accepted.len() + rejected.len()) as u64);
    assert_eq!(report.get("read_calls").number(), 2);
    assert_eq!(report.get("matches_seen").number(), 2);
    for row in report.get("hits").array() {
        assert_eq!(row.get("path").get("hex").text(), hex(b"src/a.rs"));
        let catalog = atlas.atlas().catalog();
        let ordinal = catalog.entries().iter().position(|entry| entry.path().raw().as_bytes() == b"src/a.rs").unwrap();
        assert_eq!(row.get("file_id").number(), catalog.file_id(ordinal).unwrap().get());
        assert_eq!(row.get("source_revision").number(), 1 + ordinal as u64);
    }
}

#[test]
fn empty_expression_scope_does_not_broaden_or_hide_missing_visible_membership() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle"); fixture.write("b.md", b"needle");
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "needle path:absent/";
    let report = json(&run(&mut service, &atlas, query, opts(), AtlasSearchScope::all()));
    policy(&report, query, "needle"); coverage(&report);
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("eligible_files").number(), 2);
    assert_eq!(report.get("expression_excluded_files").number(), 2);
    assert_eq!(report.get("expression_scope_files").number(), 0);
    assert_eq!(report.get("files_examined").number(), 0);
    assert_eq!(report.get("payload_bytes_read").number(), 0);
    assert_eq!(report.get("read_calls").number(), 0);
    let paths = format!("{},{}", hex(b"a.rs"), hex(b"missing.rs"));
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, &paths).unwrap();
    let response = run(&mut service, &atlas, query, opts(), scope);
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let missing = json(&response); policy(&missing, query, "needle"); coverage(&missing);
    assert!(!missing.get("search_complete").flag());
    assert_eq!(missing.get("expression_scope_files").number(), 0);
    assert_eq!(missing.get("scope_region_missing_files").number(), 1);
    assert_eq!((service.source_bytes_read(), service.read_calls()), (0, 0));
}

#[test]
fn utf8_and_bom_utf16_predicates_cross_chunks_with_original_byte_witnesses() {
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let fixture = Fixture::new();
        let prefix = format!("\u{feff}{}", "x".repeat(if encoding == "utf8" { CHUNK - 4 } else { CHUNK / 2 - 2 }));
        let mut text = format!("{prefix}🦀a🦀a🦀");
        let width = if encoding == "utf8" { 1 } else { 2 };
        let filler = (2 * CHUNK - 2 - encoded(&text, encoding).len()) / width;
        text.push_str(&" ".repeat(filler)); text.push_str("req uired");
        let source = encoded(&text, encoding);
        let raw_path = b"raw-\xff.rs";
        fs::write(fixture.0.join(OsStr::from_bytes(raw_path)), &source).unwrap();
        let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
        let query = "\"🦀a🦀\" +\"req uired\" -\"for bidden\" lang:rust";
        let response = run(&mut service, &atlas, query, opts(), AtlasSearchScope::all());
        assert_eq!(response.exit_code(), EXIT_OK);
        let report = json(&response); policy(&report, query, "🦀a🦀"); coverage(&report);
        assert!(report.get("search_complete").flag());
        assert_eq!(report.get("matches_seen").number(), 2);
        assert_eq!(report.get("payload_bytes_read").number(), source.len() as u64);
        let base = encoded(&prefix, encoding).len();
        let stride = encoded("🦀a", encoding).len();
        let length = encoded("🦀a🦀", encoding).len();
        let generation = report.get("query_generation").number();
        for (index, row) in report.get("hits").array().iter().enumerate() {
            let start = base + index * stride; let end = start + length;
            assert_eq!(bounds(row), (start as u64, end as u64));
            assert_eq!(row.get("encoding").text(), encoding);
            assert_eq!(row.get("path").get("hex").text(), hex(raw_path));
            assert_eq!(row.get("occurrence_id").number(), index as u64 + 1);
            let witness = json(&service.hit(&atlas, generation, index).unwrap());
            policy(&witness, query, "🦀a🦀");
            assert_eq!(witness.get("hit").get("text").text(), "🦀a🦀");
            assert_eq!(witness.get("hit").get("original_hex").text(), hex(&source[start..end]));
        }
        assert_eq!(service.source_bytes_read(), source.len() as u64);
    }
}

#[test]
fn lowercase_primary_expansion_keeps_spelling_and_expression_policy_on_later_pages() {
    let fixture = Fixture::new(); let source = format!("{}READY", "İ ".repeat(40));
    fixture.write("a.rs", source.as_bytes());
    fixture.write("b.rs", &encoded("\u{feff}İ READY FORBIDDEN", "utf16le"));
    fixture.write("c.rs", "ß READY".as_bytes());
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "i ready -forbidden";
    let scope = AtlasSearchScope::all().with_case_sensitive(false);
    let report = json(&run(&mut service, &atlas, query, opts(), scope.clone()));
    policy(&report, query, "i"); coverage(&report);
    assert!(!report.get("case_sensitive").flag()); assert!(report.get("search_complete").flag());
    assert_eq!(report.get("matches_seen").number(), 40);
    assert_eq!(report.get("retained_hits").number(), 40);
    assert_eq!(report.get("retained_witness_bytes").number(), 80);
    assert_eq!(report.get("next_offset").number(), 32);
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    fixture.write("a.rs", b"changed");
    let later = json(&service.page(&atlas, generation, 32, 32).unwrap());
    echo(&later, &report); assert_eq!(later.get("page_count").number(), 8);
    assert_eq!(later.get("next_offset"), &Json::Null);
    for (position, row) in later.get("hits").array().iter().enumerate() {
        let index = 32 + position;
        assert_eq!(row.get("index").number(), index as u64);
        assert_eq!(bounds(row), ((index * 3) as u64, (index * 3 + 2) as u64));
        let witness = json(&service.hit(&atlas, generation, index).unwrap());
        echo(&witness, &report);
        assert_eq!(witness.get("hit").get("text").text(), "İ");
        assert_eq!(witness.get("hit").get("original_hex").text(), "c4b0");
    }
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    let full_fold = json(&run(&mut service, &atlas, "ss ready", opts(), scope));
    assert!(full_fold.get("search_complete").flag());
    assert_eq!(full_fold.get("matches_seen").number(), 0, "Unicode lowercase does not claim full case folding");
}

#[test]
fn byte_and_read_call_limits_cannot_qualify_the_unobserved_suffix() {
    let fixture = Fixture::new(); let first = b"needle required";
    fixture.write("a.rs", first);
    let mut unfinished = first.to_vec(); unfinished.resize(2 * CHUNK, b'x');
    unfinished.extend_from_slice(b" forbidden");
    fixture.write("b.rs", &unfinished); fixture.write("c.rs", first);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "needle required -forbidden";
    for (options, reason) in [
        (AtlasStreamSearchOptions { max_scan_bytes: first.len() as u64 + CHUNK as u64, ..opts() }, "byte-limit"),
        (AtlasStreamSearchOptions { max_read_calls: 2, ..opts() }, "read-call-limit"),
    ] {
        let response = run(&mut service, &atlas, query, options, AtlasSearchScope::all());
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let report = json(&response); policy(&report, query, "needle"); coverage(&report);
        assert!(!report.get("search_complete").flag()); assert!(!report.get("match_count_complete").flag());
        assert_eq!(report.get("stop_reason").text(), reason);
        assert_eq!(report.get("files_examined").number(), 2);
        assert_eq!(report.get("files_unexamined").number(), 1);
        assert_eq!(report.get("incomplete_files").number(), 1);
        assert_eq!(report.get("matches_seen").number(), 1);
        assert_eq!(report.get("retained_hits").number(), 1);
        assert_eq!(report.get("retained_witness_bytes").number(), 6);
        assert_eq!(report.get("payload_bytes_read").number(), first.len() as u64 + CHUNK as u64);
        assert_eq!(report.get("hits").array()[0].get("path").get("hex").text(), hex(b"a.rs"));
    }
}

#[test]
fn late_malformed_text_invalidates_all_expression_candidates_for_that_file() {
    let fixture = Fixture::new(); let mut malformed = b"needle required".to_vec();
    malformed.resize(2 * CHUNK, b'x'); malformed.push(0xff);
    fixture.write("a.rs", &malformed); fixture.write("b.rs", b"needle required");
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "needle required -forbidden";
    let report = json(&run(&mut service, &atlas, query,
        AtlasStreamSearchOptions { max_matches: 1, ..opts() }, AtlasSearchScope::all()));
    policy(&report, query, "needle"); coverage(&report);
    assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("unsupported_files").number(), 1);
    assert_eq!(report.get("incomplete_files").number(), 1);
    assert_eq!(report.get("matches_seen").number(), 1);
    assert_eq!(report.get("retained_hits").number(), 1);
    assert_eq!(report.get("hits").array()[0].get("path").get("hex").text(), hex(b"b.rs"));
}

#[test]
fn stop_keeps_only_finalized_files_and_hard_cancellation_preserves_accepted_history() {
    let fixture = Fixture::new(); let first = b"needle required";
    fixture.write("a.rs", first);
    let mut active = first.to_vec(); active.resize(5 * 1024 * 1024, b'x'); active.extend_from_slice(b" forbidden");
    fixture.write("b.rs", &active); fixture.write("c.rs", first);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "needle required -forbidden";
    let stop = Cell::new(false); let mut previous = None;
    let response = service.search_scoped_expression_with_progress(&atlas, query, opts(), AtlasSearchScope::all(),
        || false, || stop.get(), |response| {
            let frame = json(response); policy(&frame, query, "needle"); coverage(&frame);
            if frame.get("payload_bytes_read").number() > first.len() as u64 {
                assert_eq!(frame.get("files_examined").number(), 1);
                assert_eq!(frame.get("matches_seen").number(), 1);
                stop.set(true);
            }
            previous = Some(frame);
        }).unwrap();
    assert!(stop.get(), "the fixture must enter the unfinished-file progress route");
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let report = json(&response); let previous = previous.unwrap();
    policy(&report, query, "needle"); coverage(&report);
    assert_eq!(report.get("stop_reason").text(), "user-stopped");
    assert_eq!(report.get("files_examined").number(), 1);
    assert_eq!(report.get("files_unexamined").number(), 2);
    assert_eq!(report.get("incomplete_files").number(), 0);
    assert_eq!(report.get("matches_seen").number(), 1);
    assert_eq!(report.get("hits"), previous.get("hits"));
    for key in ["payload_bytes_read", "scanned_input_bytes", "read_calls"] {
        assert_eq!(report.get(key), previous.get(key), "Stop must spend no more source work");
    }
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    let cancel = Cell::new(false); stop.set(false);
    let mut canceled_bytes = 0; let mut canceled_calls = 0;
    let rejected = service.search_scoped_expression_with_progress(&atlas, query, opts(), AtlasSearchScope::all(),
        || cancel.get(), || stop.get(), |response| {
            let frame = json(response);
            if frame.get("payload_bytes_read").number() > first.len() as u64 {
                canceled_bytes = frame.get("payload_bytes_read").number();
                canceled_calls = frame.get("read_calls").number();
                cancel.set(true); stop.set(true);
            }
        });
    assert!(cancel.get()); assert!(matches!(rejected, Err(AtlasStreamSearchError::Canceled)));
    assert_eq!(service.accepted_generation(), Some(generation));
    assert_eq!(service.source_bytes_read(), io.0 + canceled_bytes);
    assert_eq!(service.read_calls(), io.1 + canceled_calls);
    let page = json(&service.page(&atlas, generation, 0, 32).unwrap());
    echo(&page, &report); assert_eq!(page.get("hits"), report.get("hits"));
    let witness = json(&service.hit(&atlas, generation, 0).unwrap());
    assert_eq!(witness.get("hit").get("original_hex").text(), hex(b"needle"));
}

#[test]
fn malformed_expression_errors_do_no_io_and_do_not_destroy_previous_evidence() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle required");
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "needle required";
    let report = json(&run(&mut service, &atlas, query, opts(), AtlasSearchScope::all()));
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    let too_long = "x".repeat(1025);
    for (raw, code) in [("", "QUERY_EMPTY"), ("-forbidden", "QUERY_EMPTY"),
        ("path:src/", "QUERY_EMPTY"), ("\"unfinished", "QUERY_SYNTAX_ERROR"),
        ("regex:.*", "QUERY_REGEX_UNQUALIFIED"), (too_long.as_str(), "QUERY_NEEDLE_TOO_LONG")] {
        let attempt = service.last_attempted_generation();
        let error = service.search_scoped_expression_with_progress(&atlas, raw, opts(), AtlasSearchScope::all(),
            || false, || false, |_| panic!("invalid expression published progress")).err().unwrap();
        assert_eq!(error.to_string(), code);
        assert_eq!(service.last_attempted_generation(), attempt + 1);
        assert_eq!(service.accepted_generation(), Some(generation));
        assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
        assert_eq!(json(&service.page(&atlas, generation, 0, 32).unwrap()).get("hits"), report.get("hits"));
    }
    fixture.write("a.rs", b"current live replacement");
    let witness = json(&service.hit(&atlas, generation, 0).unwrap());
    policy(&witness, query, "needle");
    assert_eq!(witness.get("hit").get("text").text(), "needle");
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn query_syntax_is_explicit_and_literal_entry_points_preserve_operator_text() {
    let fixture = Fixture::new(); let source = b"needle -forbidden"; fixture.write("a.rs", source);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let literal = json(&service.search(&atlas, "needle -forbidden", opts(), || false).unwrap());
    assert_eq!(literal.get("mode").text(), "exact-decoded-literal");
    assert_eq!(literal.get("matches_seen").number(), 1);
    assert_eq!(bounds(&literal.get("hits").array()[0]), (0, source.len() as u64));
    let Json::Object(fields) = &literal else { panic!("expected literal object") };
    assert!(!fields.contains_key("expression")); assert!(!fields.contains_key("primary_needle"));
    let filtered = json(&run(&mut service, &atlas, "needle -forbidden", opts(), AtlasSearchScope::all()));
    assert!(filtered.get("search_complete").flag());
    assert_eq!(filtered.get("matches_seen").number(), 0);
    let quoted = "\"needle -forbidden\"";
    let preserved = json(&run(&mut service, &atlas, quoted, opts(), AtlasSearchScope::all()));
    policy(&preserved, quoted, "needle -forbidden");
    assert_eq!(preserved.get("matches_seen").number(), 1);
    assert_eq!(preserved.get("hits").array()[0].get("original_range"),
        literal.get("hits").array()[0].get("original_range"));
}

#[test]
fn expression_window_skips_only_qualified_files_and_counts_past_retention() {
    let fixture = Fixture::new(); let missing_required = b"banana";
    let mut rejected = b"banana banana banana required ".to_vec();
    rejected.resize(2 * CHUNK, b'x'); rejected.extend_from_slice(b" forbidden");
    let skipped_file = b"banana required"; let retained_file = b"banana banana required";
    let counted_file = b"banana required";
    fixture.write("a.rs", missing_required); fixture.write("b.rs", &rejected);
    fixture.write("c.rs", skipped_file); fixture.write("d.rs", retained_file);
    fixture.write("e.rs", counted_file);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "ana required -forbidden"; let mut progress = Vec::new();
    let response = service.search_scoped_expression_page_with_progress(&atlas, query,
        AtlasStreamSearchOptions { max_matches: 2, ..opts() }, AtlasSearchScope::all(), 3,
        || false, || false, |response| progress.push(json(response))).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "the complete scan still omits prefix and later rows");
    let report = json(&response); policy(&report, query, "ana"); coverage(&report); window(&report, 3, 3, true);
    assert!(report.get("search_complete").flag()); assert!(report.get("match_count_complete").flag());
    assert_eq!(report.get("stop_reason"), &Json::Null);
    assert_eq!(report.get("matches_seen").number(), 8);
    assert_eq!(report.get("matching_files").number(), 3);
    assert_eq!(report.get("retained_matching_files").number(), 1);
    assert_eq!(report.get("retained_hits").number(), 2);
    assert_eq!(report.get("retained_witness_bytes").number(), 3);
    assert_eq!(report.get("files_examined").number(), 5);
    assert_eq!(report.get("incomplete_files").number(), 0);
    assert_eq!(report.get("payload_bytes_read").number(),
        (missing_required.len() + rejected.len() + skipped_file.len() + retained_file.len() + counted_file.len()) as u64);
    for (index, row) in report.get("hits").array().iter().enumerate() {
        assert_eq!(row.get("path").get("hex").text(), hex(b"d.rs"));
        assert_eq!(row.get("occurrence_id").number(), index as u64 + 2);
        assert_eq!(bounds(row), [(3, 6), (8, 11)][index]);
    }
    assert!(progress.iter().any(|frame| frame.get("files_examined").number() == 4));
    for frame in &progress {
        policy(frame, query, "ana"); coverage(frame); echo(frame, &report);
        let examined = frame.get("files_examined").number() as usize;
        assert_eq!(frame.get("matches_seen").number(), [0, 0, 0, 2, 6, 8][examined]);
        window(frame, 3, [0, 0, 0, 2, 3, 3][examined], examined >= 4);
        assert!(report.get("hits").array().starts_with(frame.get("hits").array()));
        assert!(!frame.get("selection_available").flag());
    }
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    fixture.write("d.rs", b"later source replacement");
    let page = json(&service.page(&atlas, generation, 0, 32).unwrap());
    echo(&page, &report); window(&page, 3, 3, true);
    assert_eq!(page.get("hits"), report.get("hits"));
    for index in 0..2 {
        let witness = json(&service.hit(&atlas, generation, index).unwrap());
        echo(&witness, &report);
        assert_eq!(witness.get("hit").get("text").text(), "ana");
        assert_eq!(witness.get("hit").get("original_hex").text(), "616e61");
    }
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn expression_window_metadata_exclusions_spend_neither_offsets_nor_io_budgets() {
    let fixture = Fixture::new(); let admitted = b"banana required";
    let excluded = b"banana required ".repeat(CHUNK);
    for path in ["a.py", "other/a.rs", "src/a_drop.rs", "src/readme.md"] {
        fixture.write(path, &excluded);
    }
    fixture.write("src/b.rs", admitted); fixture.write("src/c.rs", admitted);
    let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let query = "ana required path:src/ -path:drop lang:rust";
    let scope = AtlasSearchScope::from_csv(Some("rs,md"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let response = service.search_scoped_expression_page_with_progress(&atlas, query,
        AtlasStreamSearchOptions { max_matches: 1, max_scan_bytes: 2 * admitted.len() as u64,
            max_read_calls: 2 }, scope, 3, || false, || false, |_| {}).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "skipped prefix rows remain explicitly omitted");
    let report = json(&response); policy(&report, query, "ana"); coverage(&report); window(&report, 3, 3, false);
    assert!(report.get("search_complete").flag()); assert!(report.get("match_count_complete").flag());
    assert_eq!(report.get("stop_reason"), &Json::Null);
    assert_eq!(report.get("catalogued_files").number(), 6);
    assert_eq!(report.get("eligible_files").number(), 4);
    assert_eq!(report.get("excluded_files").number(), 2);
    assert_eq!(report.get("expression_scope_files").number(), 2);
    assert_eq!(report.get("expression_excluded_files").number(), 2);
    assert_eq!(report.get("files_examined").number(), 2);
    assert_eq!(report.get("matches_seen").number(), 4);
    assert_eq!(report.get("matching_files").number(), 2);
    assert_eq!(report.get("retained_matching_files").number(), 1);
    assert_eq!(report.get("retained_hits").number(), 1);
    assert_eq!(report.get("payload_bytes_read").number(), 2 * admitted.len() as u64);
    assert_eq!(report.get("read_calls").number(), 2);
    let row = &report.get("hits").array()[0];
    assert_eq!(row.get("path").get("hex").text(), hex(b"src/c.rs"));
    assert_eq!(row.get("occurrence_id").number(), 2); assert_eq!(bounds(row), (3, 6));
    let catalog = atlas.atlas().catalog();
    let ordinal = catalog.entries().iter().position(|entry| entry.path().raw().as_bytes() == b"src/c.rs").unwrap();
    assert_eq!(row.get("file_id").number(), catalog.file_id(ordinal).unwrap().get());
    assert_eq!(row.get("source_revision").number(), 1 + ordinal as u64);
    assert_eq!((service.source_bytes_read(), service.read_calls()), (2 * admitted.len() as u64, 2));
}

#[test]
fn incomplete_expression_window_keeps_qualified_prefix_and_fresh_utf16_rescan_selects_exact_witness() {
    for encoding in ["utf16le", "utf16be"] {
        let fixture = Fixture::new(); let first = "İ İ READY".as_bytes();
        let mut rejected = first.to_vec(); rejected.resize(2 * CHUNK, b'x');
        rejected.extend_from_slice(b" FORBIDDEN");
        let last = encoded("\u{feff}İ İ READY", encoding);
        fixture.write("a.rs", first); fixture.write("b.rs", &rejected); fixture.write("c.rs", &last);
        let atlas = fixture.atlas(); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
        let query = "i ready -forbidden";
        let scope = AtlasSearchScope::all().with_case_sensitive(false);
        let partial_response = service.search_scoped_expression_page_with_progress(&atlas, query,
            AtlasStreamSearchOptions { max_matches: 1, max_scan_bytes: first.len() as u64 + CHUNK as u64,
                ..opts() }, scope.clone(), 3, || false, || false, |_| {}).unwrap();
        assert_eq!(partial_response.exit_code(), EXIT_PARTIAL);
        let partial = json(&partial_response); policy(&partial, query, "i"); coverage(&partial);
        window(&partial, 3, 2, false);
        assert_eq!(partial.get("stop_reason").text(), "byte-limit");
        assert!(!partial.get("search_complete").flag()); assert!(!partial.get("match_count_complete").flag());
        assert_eq!(partial.get("matches_seen").number(), 2);
        assert_eq!(partial.get("matching_files").number(), 1);
        assert_eq!(partial.get("retained_hits").number(), 0);
        assert_eq!(partial.get("retained_witness_bytes").number(), 0);
        assert_eq!(partial.get("files_examined").number(), 2);
        assert_eq!(partial.get("files_unexamined").number(), 1);
        assert_eq!(partial.get("incomplete_files").number(), 1);
        assert_eq!(partial.get("payload_bytes_read").number(), first.len() as u64 + CHUNK as u64);
        let old_generation = partial.get("query_generation").number();
        let io = (service.source_bytes_read(), service.read_calls());
        let response = service.search_scoped_expression_page_with_progress(&atlas, query,
            AtlasStreamSearchOptions { max_matches: 1, ..opts() }, scope, 3,
            || false, || false, |_| {}).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL, "the complete later window omits its prefix");
        let report = json(&response); policy(&report, query, "i"); coverage(&report); window(&report, 3, 3, false);
        assert!(report.get("search_complete").flag()); assert!(report.get("match_count_complete").flag());
        assert_eq!(report.get("stop_reason"), &Json::Null);
        assert_eq!(report.get("matches_seen").number(), 4);
        assert_eq!(report.get("matching_files").number(), 2);
        assert_eq!(report.get("retained_matching_files").number(), 1);
        assert_eq!(report.get("retained_hits").number(), 1);
        assert_eq!(report.get("retained_witness_bytes").number(), 2);
        assert_eq!(report.get("files_examined").number(), 3);
        assert_eq!(report.get("incomplete_files").number(), 0);
        assert_eq!(report.get("payload_bytes_read").number(), (first.len() + rejected.len() + last.len()) as u64);
        let generation = report.get("query_generation").number();
        assert_eq!(generation, old_generation + 1);
        assert!(matches!(service.page(&atlas, old_generation, 0, 32), Err(AtlasStreamSearchError::StaleQuery)));
        assert!(matches!(service.hit(&atlas, old_generation, 0), Err(AtlasStreamSearchError::StaleQuery)));
        let row = &report.get("hits").array()[0];
        assert_eq!(row.get("path").get("hex").text(), hex(b"c.rs"));
        assert_eq!(row.get("occurrence_id").number(), 2); assert_eq!(bounds(row), (6, 8));
        assert_eq!(row.get("encoding").text(), encoding);
        let catalog = atlas.atlas().catalog();
        let ordinal = catalog.entries().iter().position(|entry| entry.path().raw().as_bytes() == b"c.rs").unwrap();
        assert_eq!(row.get("source_revision").number(), 1 + atlas.atlas().file_count() as u64 + ordinal as u64);
        assert_eq!(service.source_bytes_read(), io.0 + report.get("payload_bytes_read").number());
        assert_eq!(service.read_calls(), io.1 + report.get("read_calls").number());
        let frozen_io = (service.source_bytes_read(), service.read_calls());
        fixture.write("c.rs", b"different live bytes");
        let page = json(&service.page(&atlas, generation, 0, 32).unwrap());
        echo(&page, &report); window(&page, 3, 3, false);
        assert_eq!(page.get("hits"), report.get("hits"));
        let witness = json(&service.hit(&atlas, generation, 0).unwrap());
        echo(&witness, &report);
        assert_eq!(witness.get("hit").get("text").text(), "İ");
        assert_eq!(witness.get("hit").get("original_hex").text(), hex(&last[6..8]));
        assert_eq!(witness.get("additional_source_bytes_read").number(), 0);
        assert_eq!(frozen_io, (service.source_bytes_read(), service.read_calls()));
    }
}
