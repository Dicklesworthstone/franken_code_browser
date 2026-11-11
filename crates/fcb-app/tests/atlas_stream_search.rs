#![forbid(unsafe_code)]
#![cfg(unix)]

//! Native host → retained catalog → public streaming search → immutable literal
//! activation over real files. Independent byte oracles verify source ranges.
//! This source-only authoring step does not claim strict-RCH/native execution.
mod support;

use std::{ffi::OsStr, fs::{self, File}, io::Write, os::unix::ffi::OsStrExt,
    path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_PARTIAL, host::{HostResponse,
    atlas_session::{AtlasAction, AtlasSession, AtlasSessionOptions},
    atlas_search::{AtlasSearchPathKind, AtlasSearchScope},
    atlas_stream_search::{AtlasStreamSearchError, AtlasStreamSearchOptions, RetainedAtlasStreamSearch,
        MAX_ATLAS_STREAM_SEARCH_HITS, MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES,
        MAX_ATLAS_STREAM_SEARCH_PAGE, MAX_ATLAS_STREAM_SEARCH_READ_CALLS, MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES}}};
use support::{Json, parse};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-native-stream-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); Self(root)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn atlas(&self, max_files: usize) -> AtlasSession {
        static NEXT: AtomicU64 = AtomicU64::new(73_000);
        AtlasSession::open(ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap(), &self.0,
            AtlasSessionOptions { max_files, ..Default::default() }, || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn opts() -> AtlasStreamSearchOptions { AtlasStreamSearchOptions::default() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn bounds(value: &Json) -> (u64, u64) { (value.get("start").number(), value.get("end").number()) }
fn search(service: &mut RetainedAtlasStreamSearch, atlas: &AtlasSession, needle: &str) -> Json {
    json(&service.search(atlas, needle, opts(), || false).unwrap())
}

#[test]
fn file_beyond_both_native_capture_ceilings_is_searchable_with_only_literal_retention() {
    let fixture = Fixture::new();
    let mut file = File::create(fixture.0.join("large.rs")).unwrap();
    let offset = 33 * 1024 * 1024 - 3;
    let mut remaining = offset;
    let block = [b'x'; 16 * 1024];
    while remaining > 0 {
        let count = remaining.min(block.len()); file.write_all(&block[..count]).unwrap(); remaining -= count;
    }
    file.write_all(b"needle\n").unwrap(); drop(file);
    let length = offset + 7;
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    assert_eq!(service.source_bytes_read(), 0, "catalog preparation reads no source payload");
    let response = service.search(&atlas, "needle\n", opts(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let report = json(&response);
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("scope").text(), "catalogued-whole-files");
    assert_eq!(report.get("capture_kind").text(), "literal-witnesses-only");
    assert_eq!(report.get("payload_bytes_read").number(), length as u64);
    assert_eq!(report.get("scanned_input_bytes").number(), length as u64);
    assert!(report.get("peak_input_buffer_bytes").number() <= 16 * 1024);
    assert_eq!(report.get("retained_witness_bytes").number(), 7);
    assert_eq!(service.retained_witness_bytes(), 7);
    assert_eq!(bounds(report.get("hits").array()[0].get("original_range")), (offset as u64, length as u64));
    let io = (service.source_bytes_read(), service.read_calls());
    let hit = json(&service.hit(&atlas, 1, 0).unwrap());
    assert_eq!(hit.get("hit").get("text").text(), "needle\n");
    assert_eq!(hit.get("hit").get("original_hex").text(), "6e6565646c650a");
    assert_eq!(hit.get("additional_source_bytes_read").number(), 0);
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn utf8_both_utf16_orders_overlap_and_raw_paths_preserve_exact_byte_oracles() {
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let fixture = Fixture::new();
        let encode = |text: &str| -> Vec<u8> { match encoding {
            "utf16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            "utf16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => text.as_bytes().to_vec(),
        } };
        let prefix = format!("\u{feff}{}", "x".repeat(if encoding == "utf8" { 16 * 1024 - 4 } else { 8190 }));
        let bytes = encode(&format!("{prefix}🦀a🦀a🦀\r\n"));
        let literal = encode("🦀a🦀");
        let start = encode(&prefix).len() as u64;
        let stride = encode("🦀a").len() as u64;
        let path = b"raw-\xff.rs";
        fs::write(fixture.0.join(OsStr::from_bytes(path)), &bytes).unwrap();
        let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
        let report = search(&mut service, &atlas, "🦀a🦀");
        assert!(report.get("search_complete").flag());
        assert_eq!(report.get("retained_hits").number(), 2);
        assert_eq!(report.get("retained_witness_bytes").number(), literal.len() as u64);
        for index in 0..2 {
            let row = &report.get("hits").array()[index];
            let expected_start = start + stride * index as u64;
            let expected_end = expected_start + literal.len() as u64;
            assert_eq!(row.get("index").number(), index as u64);
            assert_eq!(row.get("occurrence_id").number(), index as u64 + 1);
            assert_eq!(row.get("encoding").text(), encoding);
            assert_eq!(row.get("path").get("hex").text(), hex(path));
            assert_eq!(bounds(row.get("original_range")), (expected_start, expected_end));
            let activated = json(&service.hit(&atlas, 1, index).unwrap());
            let hit = activated.get("hit");
            assert_eq!(hit.get("original_hex").text(), hex(&bytes[expected_start as usize..expected_end as usize]));
            assert_eq!(hit.get("witness_sha256"), row.get("witness_sha256"));
            assert_ne!(hit.get("original_hex").text(), hex(b"wrong witness"), "negative source oracle");
        }
    }
}

#[test]
fn frozen_all_catalog_scope_survives_display_filter_and_live_new_files() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle"); fixture.write("b.md", b"needle");
    fixture.write("node_modules/ignored.js", b"needle");
    let mut atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    fixture.write("later.rs", b"needle");
    atlas.prepare(1, AtlasAction::Scope(AtlasSession::preset_scope("rust").unwrap()), || false).unwrap();
    assert_eq!(atlas.view().file_count(), 1);
    let report = search(&mut service, &atlas, "needle");
    assert_eq!(report.get("catalogued_files").number(), 2);
    assert_eq!(report.get("files_examined").number(), 2);
    assert_eq!(report.get("retained_hits").number(), 2);
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("scope").text(), "catalogued-whole-files");
    assert!(report.get("discovery_policy").text().starts_with("repository-rules-v1:"));
    assert!(report.get("rule_policy").get("scope_complete").flag());
    assert!(report.get("excluded_entries").number() >= 1);
}

fn assert_scope_echo(actual: &Json, expected: &Json) {
    for key in ["search_scope", "scope_extensions", "scope_filenames", "scope_path_kind", "scope_path",
        "scope_path_hex", "case_sensitive", "eligible_files", "excluded_files", "catalogued_files"] {
        assert_eq!(actual.get(key), expected.get(key), "scope changed in {key}");
    }
}

#[test]
fn file_types_intersect_directory_before_io_and_preserve_original_ids_on_page_and_hit() {
    let fixture = Fixture::new(); fixture.write("a-excluded.bin", &vec![b'x'; 1024 * 1024]);
    fixture.write("src/a.RS", b"needle"); fixture.write("src/Makefile", b"needle");
    fixture.write("src/nested/b.rs", b"other"); fixture.write("src/readme.md", b"needle");
    fixture.write("src-old/spill.rs", b"needle");
    let mut atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    // Neither a later display-only filter nor a disappeared excluded file can
    // change the explicitly admitted base-catalog intersection.
    atlas.prepare(1, AtlasAction::Scope(AtlasSession::preset_scope("rust").unwrap()), || false).unwrap();
    fs::remove_file(fixture.0.join("src/readme.md")).unwrap();
    let scope = AtlasSearchScope::from_csv(Some("RS"), Some("Makefile")).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let options = AtlasStreamSearchOptions { max_scan_bytes: 17, max_read_calls: 3, ..opts() };
    let response = service.search_scoped(&atlas, "needle", options, scope, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let report = json(&response);
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("search_scope").text(), "file-types");
    assert_eq!(report.get("scope_extensions").text(), "rs");
    assert_eq!(report.get("scope_filenames").text(), "makefile");
    assert_eq!(report.get("scope_path_kind").text(), "directory");
    assert_eq!(report.get("scope_path").text(), "src");
    assert_eq!(report.get("scope_path_hex").text(), hex(b"src"));
    assert!(report.get("case_sensitive").flag());
    assert_eq!(report.get("catalogued_files").number(), 6);
    assert_eq!(report.get("eligible_files").number(), 3); assert_eq!(report.get("excluded_files").number(), 3);
    assert_eq!(report.get("files_examined").number(), 3); assert_eq!(report.get("files_unexamined").number(), 0);
    assert_eq!(report.get("incomplete_files").number(), 0); assert_eq!(report.get("retained_hits").number(), 2);
    assert_eq!(report.get("payload_bytes_read").number(), 17); assert_eq!(report.get("read_calls").number(), 3);
    for row in report.get("hits").array() {
        let catalog = atlas.atlas().catalog();
        let ordinal = catalog.entries().iter().position(|entry|
            hex(entry.path().raw().as_bytes()) == row.get("path").get("hex").text()).unwrap();
        let file = catalog.file_id(ordinal).unwrap();
        assert_eq!(row.get("file_id").number(), file.get());
        assert_eq!(row.get("node_ordinal").number(), atlas.atlas().node_for_file(file).unwrap().ordinal() as u64);
        assert_eq!(row.get("source_revision").number(), 1 + ordinal as u64);
    }
    let io = (service.source_bytes_read(), service.read_calls());
    fixture.write("src/Makefile", b"changed current contents");
    let page = json(&service.page(&atlas, 1, 0, 32).unwrap());
    let hit = json(&service.hit(&atlas, 1, 0).unwrap());
    assert_scope_echo(&page, &report); assert_scope_echo(&hit, &report);
    assert_eq!(hit.get("hit").get("original_hex").text(), hex(b"needle"));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    assert!(matches!(service.search_scoped(&atlas, "needle",
        AtlasStreamSearchOptions { max_scan_bytes: 0, ..opts() },
        AtlasSearchScope::all().with_case_sensitive(false), || false),
        Err(AtlasStreamSearchError::InvalidLimits)));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    assert_scope_echo(&json(&service.page(&atlas, 1, 0, 32).unwrap()), &report);
}

#[test]
fn exact_file_empty_types_and_empty_visible_region_never_broaden_to_all_files() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle"); fixture.write("b.md", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let file = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "b.md").unwrap();
    let selected = json(&service.search_scoped(&atlas, "needle", opts(), file, || false).unwrap());
    assert_eq!(selected.get("eligible_files").number(), 1); assert_eq!(selected.get("excluded_files").number(), 1);
    assert_eq!(selected.get("hits").array()[0].get("path").get("hex").text(), hex(b"b.md"));
    assert_eq!(service.source_bytes_read(), 6);
    let io = (service.source_bytes_read(), service.read_calls());
    for scope in [AtlasSearchScope::from_csv(Some(""), None).unwrap(),
        AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, "").unwrap()] {
        let response = service.search_scoped(&atlas, "needle", opts(), scope, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_OK);
        let empty = json(&response);
        assert_eq!(empty.get("eligible_files").number(), 0); assert_eq!(empty.get("excluded_files").number(), 2);
        assert_eq!(empty.get("files_examined").number(), 0); assert_eq!(empty.get("files_unexamined").number(), 0);
        assert_eq!(empty.get("payload_bytes_read").number(), 0); assert_eq!(empty.get("read_calls").number(), 0);
        assert!(empty.get("search_complete").flag()); assert!(empty.get("hits").array().is_empty());
        assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    }
    let missing = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, &hex(b"missing.rs")).unwrap();
    let response = service.search_scoped(&atlas, "needle", opts(), missing, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let report = json(&response);
    assert_eq!(report.get("eligible_files").number(), 0); assert_eq!(report.get("files_unexamined").number(), 0);
    assert_eq!(report.get("scope_region_files").number(), 1);
    assert_eq!(report.get("scope_region_catalogued_files").number(), 0);
    assert_eq!(report.get("scope_region_missing_files").number(), 1);
    assert!(!report.get("search_complete").flag()); assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn visible_scope_keeps_raw_path_membership_and_missing_coverage_independent_of_file_types() {
    let fixture = Fixture::new(); let raw = b"raw-\xff.rs";
    fs::write(fixture.0.join(OsStr::from_bytes(raw)), b"needle").unwrap();
    fixture.write("doc.md", b"needle"); fixture.write("other.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut paths = vec![hex(raw), hex(b"doc.md"), hex(b"missing.rs")]; paths.sort();
    let wire = paths.join(",");
    let scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Visible, &wire).unwrap();
    let response = service.search_scoped(&atlas, "needle", opts(), scope, || false).unwrap();
    let report = json(&response);
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    assert_eq!(report.get("scope_path_kind").text(), "visible");
    assert_eq!(report.get("scope_region_files").number(), 3);
    assert_eq!(report.get("scope_region_catalogued_files").number(), 2);
    assert_eq!(report.get("scope_region_missing_files").number(), 1);
    assert_eq!(report.get("scope_region_path_bytes").number(), (raw.len() + b"doc.md".len() + b"missing.rs".len()) as u64);
    assert_eq!(report.get("eligible_files").number(), 1); assert_eq!(report.get("excluded_files").number(), 2);
    assert_eq!(report.get("payload_bytes_read").number(), 6); assert_eq!(report.get("files_unexamined").number(), 0);
    assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("hits").array()[0].get("path").get("hex").text(), hex(raw));
    let page = json(&service.page(&atlas, 1, 0, 32).unwrap());
    let hit = json(&service.hit(&atlas, 1, 0).unwrap());
    for delivered in [&page, &hit] {
        assert_scope_echo(delivered, &report);
        for key in ["scope_region_sha256", "scope_region_files", "scope_region_path_bytes",
            "scope_region_catalogued_files", "scope_region_missing_files"] {
            assert_eq!(delivered.get(key), report.get(key));
        }
    }
    assert_eq!(service.source_bytes_read(), 6);
}

#[test]
fn scoped_progress_stop_and_cancellation_keep_the_frozen_eligible_universe_and_previous_result() {
    use std::cell::Cell;
    let fixture = Fixture::new(); fixture.write("a-excluded.rs", &vec![b'x'; 1024 * 1024]);
    fixture.write("src/a.rs", b"needle"); fixture.write("src/z.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let stop = Cell::new(false); let mut frames = Vec::new();
    let response = service.search_scoped_with_progress(&atlas, "needle", opts(), scope.clone(),
        || false, || stop.get(), |response| {
            let frame = json(response);
            assert_eq!(frame.get("eligible_files").number(), 2); assert_eq!(frame.get("excluded_files").number(), 1);
            assert_eq!(frame.get("files_examined").number() + frame.get("files_unexamined").number(), 2);
            if frame.get("retained_hits").number() == 1 { stop.set(true); }
            frames.push(frame);
        }).unwrap();
    let terminal = json(&response);
    assert!(stop.get()); assert_eq!(frames.len(), 2); assert_eq!(response.exit_code(), EXIT_PARTIAL);
    assert_eq!(terminal.get("stop_reason").text(), "user-stopped");
    assert_eq!(terminal.get("files_examined").number(), 1); assert_eq!(terminal.get("files_unexamined").number(), 1);
    assert_eq!(terminal.get("payload_bytes_read").number(), 6);
    for frame in &frames { assert_scope_echo(frame, &terminal); }
    let accepted = service.accepted_generation().unwrap();
    let io = (service.source_bytes_read(), service.read_calls());
    let cancel = Cell::new(false); let mut spent = (0, 0);
    let result = service.search_scoped_with_progress(&atlas, "needle", opts(), scope,
        || cancel.get(), || cancel.get(), |response| {
            let frame = json(response);
            if frame.get("retained_hits").number() == 1 {
                spent = (frame.get("payload_bytes_read").number(), frame.get("read_calls").number());
                cancel.set(true);
            }
        });
    assert!(matches!(result, Err(AtlasStreamSearchError::Canceled)));
    assert_eq!(service.accepted_generation(), Some(accepted));
    assert_eq!((service.source_bytes_read(), service.read_calls()), (io.0 + spent.0, io.1 + spent.1));
    let retained = json(&service.page(&atlas, accepted, 0, 32).unwrap());
    assert_scope_echo(&retained, &terminal); assert_eq!(retained.get("hits"), terminal.get("hits"));
}

#[test]
fn byte_and_read_call_caps_are_global_and_do_not_turn_unexamined_files_into_absence() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"nothing"); fixture.write("b.rs", b"needle"); fixture.write("c.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let bytes = service.search(&atlas, "needle", AtlasStreamSearchOptions { max_scan_bytes: 8, ..opts() }, || false).unwrap();
    assert_eq!(bytes.exit_code(), EXIT_PARTIAL);
    let report = json(&bytes);
    assert_eq!(report.get("payload_bytes_read").number(), 8);
    assert_eq!(report.get("files_examined").number(), 2);
    assert_eq!(report.get("files_unexamined").number(), 1);
    assert_eq!(report.get("stop_reason").text(), "byte-limit");
    assert!(!report.get("truncated").flag(), "a scan cap is not result-buffer overflow");
    assert!(!report.get("search_complete").flag());
    let report = json(&service.search(&atlas, "needle", AtlasStreamSearchOptions { max_read_calls: 1, ..opts() }, || false).unwrap());
    assert_eq!(report.get("read_calls").number(), 1);
    assert_eq!(report.get("files_examined").number(), 1);
    assert_eq!(report.get("files_unexamined").number(), 2);
    assert_eq!(report.get("stop_reason").text(), "read-call-limit");
}

#[test]
fn exact_capacity_and_cross_file_lookahead_distinguish_complete_from_truncated_counts() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle"); fixture.write("b.rs", b"nothing");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let options = AtlasStreamSearchOptions { max_matches: 1, ..opts() };
    let complete = json(&service.search(&atlas, "needle", options, || false).unwrap());
    assert!(complete.get("search_complete").flag());
    assert!(!complete.get("truncated").flag());
    assert_eq!(complete.get("matches_seen").number(), 1);
    fixture.write("b.rs", b"needle");
    let partial = json(&service.search(&atlas, "needle", options, || false).unwrap());
    assert!(!partial.get("search_complete").flag());
    assert!(partial.get("truncated").flag());
    assert_eq!(partial.get("retained_hits").number(), 1);
    assert_eq!(partial.get("matches_seen").number(), 2);
    assert_eq!(partial.get("stop_reason").text(), "match-limit");
    assert!(partial.get("hits").array()[0].get("source_revision").number()
        > complete.get("hits").array()[0].get("source_revision").number());
}

#[test]
fn unavailable_and_malformed_files_remain_explicit_and_late_invalid_text_discards_its_hits() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"gone");
    let mut malformed = b"needle".to_vec(); malformed.resize(16 * 1024, b'x'); malformed.push(0xff);
    fixture.write("b.rs", &malformed); fixture.write("c.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    fs::remove_file(fixture.0.join("a.rs")).unwrap();
    let report = search(&mut service, &atlas, "needle");
    assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("files_examined").number(), 3);
    assert_eq!(report.get("incomplete_files").number(), 2);
    assert_eq!(report.get("unavailable_files").number(), 1);
    assert_eq!(report.get("unsupported_files").number(), 1);
    assert_eq!(report.get("failed_files").number(), 0);
    assert_eq!(report.get("retained_hits").number(), 1);
    assert_eq!(report.get("hits").array()[0].get("path").get("hex").text(), hex(b"c.rs"));
    assert_eq!(report.get("payload_bytes_read").number(), (malformed.len() + 6) as u64);
}

#[test]
fn aggregate_unsupported_count_survives_bounded_diagnostics_and_partial_discovery_is_distinct() {
    let fixture = Fixture::new();
    for i in 0..40 { fixture.write(&format!("bad-{i:02}.bin"), &[0xff]); }
    fixture.write("valid.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = search(&mut service, &atlas, "needle");
    assert_eq!(report.get("unsupported_files").number(), 40);
    assert_eq!(report.get("incomplete_files").number(), 40);
    assert_eq!(report.get("diagnostics").array().len(), 32);
    assert!(report.get("diagnostics_truncated").flag());
    assert_eq!(report.get("retained_hits").number(), 1);
    let partial = Fixture::new(); partial.write("a.rs", b"needle"); partial.write("b.rs", b"needle");
    let atlas = partial.atlas(1); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = search(&mut service, &atlas, "needle");
    assert!(!report.get("discovery_complete").flag());
    assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("files_unexamined").number(), 0);
    assert_eq!(report.get("incomplete_files").number(), 0);
    assert_eq!(report.get("stop_reason"), &Json::Null);
}

#[test]
fn pages_and_exact_witnesses_survive_live_path_replacement_without_any_source_io() {
    let fixture = Fixture::new(); fixture.write("a.rs", "needle ".repeat(70).as_bytes());
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = search(&mut service, &atlas, "needle");
    assert_eq!(report.get("page_count").number(), MAX_ATLAS_STREAM_SEARCH_PAGE as u64);
    assert_eq!(report.get("next_offset").number(), 32);
    assert_eq!(report.get("retained_witness_bytes").number(), 6, "one witness per matching file, not per occurrence");
    let io = (service.source_bytes_read(), service.read_calls());
    fs::rename(fixture.0.join("a.rs"), fixture.0.join("old.rs")).unwrap(); fixture.write("a.rs", b"changed now");
    let page = json(&service.page(&atlas, 1, 32, 32).unwrap());
    assert_eq!(page.get("page_offset").number(), 32);
    assert_eq!(page.get("hits").array()[0].get("index").number(), 32);
    assert_eq!(page.get("next_offset").number(), 64);
    let last = json(&service.page(&atlas, 1, 64, 32).unwrap());
    assert_eq!(last.get("page_count").number(), 6);
    assert_eq!(last.get("next_offset"), &Json::Null);
    let witness = json(&service.hit(&atlas, 1, 69).unwrap());
    assert_eq!(witness.get("hit").get("original_hex").text(), hex(b"needle"));
    assert_eq!(witness.get("hit").get("occurrence_id").number(), 70);
    assert_eq!(bounds(witness.get("hit").get("original_range")), (69 * 7, 69 * 7 + 6));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    assert!(matches!(service.page(&atlas, 1, 0, MAX_ATLAS_STREAM_SEARCH_PAGE + 1), Err(AtlasStreamSearchError::InvalidLimits)));
    assert!(matches!(service.hit(&atlas, 1, 70), Err(AtlasStreamSearchError::MissingHit)));
}

#[test]
fn failed_and_early_canceled_replacements_consume_identity_but_preserve_accepted_evidence() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"old needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let first = search(&mut service, &atlas, "needle");
    let old_revision = first.get("hits").array()[0].get("source_revision").number();
    let io = (service.source_bytes_read(), service.read_calls());
    assert!(matches!(service.search(&atlas, "old", opts(), || true), Err(AtlasStreamSearchError::Canceled)));
    assert_eq!(service.last_attempted_generation(), 2);
    assert!(matches!(service.search(&atlas, "", opts(), || false), Err(AtlasStreamSearchError::InvalidLimits)));
    assert_eq!(service.last_attempted_generation(), 3);
    assert_eq!(service.accepted_generation(), Some(1));
    assert!(service.hit(&atlas, 1, 0).is_ok());
    assert!(matches!(service.hit(&atlas, 2, 0), Err(AtlasStreamSearchError::StaleQuery)));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    let next = search(&mut service, &atlas, "old");
    assert_eq!(next.get("query_generation").number(), 4);
    assert!(next.get("hits").array()[0].get("source_revision").number() > old_revision + 2);
    assert!(matches!(service.hit(&atlas, 1, 0), Err(AtlasStreamSearchError::StaleQuery)));
    service.clear(&atlas).unwrap();
    assert_eq!(service.accepted_generation(), None);
    assert_eq!(service.retained_witness_bytes(), 0);
    assert!(matches!(service.hit(&atlas, 4, 0), Err(AtlasStreamSearchError::MissingQuery)));
}

#[test]
fn cancellation_at_final_delivery_gate_keeps_old_result_and_charges_completed_source_reads() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut baseline_polls = 0;
    service.search(&atlas, "needle", opts(), || { baseline_polls += 1; false }).unwrap();
    assert!(baseline_polls > 10);
    let old_bytes = service.source_bytes_read();
    let mut polls = 0;
    let result = service.search(&atlas, "needle", opts(), || { polls += 1; polls == baseline_polls });
    assert!(matches!(result, Err(AtlasStreamSearchError::Canceled)));
    assert_eq!(polls, baseline_polls);
    assert_eq!(service.source_bytes_read(), old_bytes + 6);
    assert_eq!(service.accepted_generation(), Some(1));
    assert_eq!(service.last_attempted_generation(), 2);
    assert!(service.hit(&atlas, 1, 0).is_ok());
}

#[test]
fn canceled_mid_scan_charges_actual_io_and_limits_are_refused_before_new_reads() {
    let fixture = Fixture::new(); fixture.write("large.rs", &vec![b'x'; 2 * 1024 * 1024]);
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut polls = 0;
    assert!(matches!(service.search(&atlas, "needle", opts(), || { polls += 1; polls == 100 }), Err(AtlasStreamSearchError::Canceled)));
    assert!(service.source_bytes_read() > 0 && service.source_bytes_read() < 2 * 1024 * 1024);
    assert!(service.read_calls() > 0);
    assert_eq!(service.accepted_generation(), None);
    let io = (service.source_bytes_read(), service.read_calls());
    for options in [AtlasStreamSearchOptions { max_matches: 0, ..opts() },
        AtlasStreamSearchOptions { max_matches: MAX_ATLAS_STREAM_SEARCH_HITS + 1, ..opts() },
        AtlasStreamSearchOptions { max_scan_bytes: 0, ..opts() },
        AtlasStreamSearchOptions { max_scan_bytes: MAX_ATLAS_STREAM_SEARCH_SCAN_BYTES + 1, ..opts() },
        AtlasStreamSearchOptions { max_read_calls: 0, ..opts() },
        AtlasStreamSearchOptions { max_read_calls: MAX_ATLAS_STREAM_SEARCH_READ_CALLS + 1, ..opts() }] {
        assert!(matches!(service.search(&atlas, "needle", options, || false), Err(AtlasStreamSearchError::InvalidLimits)));
    }
    assert!(matches!(service.search(&atlas, &"x".repeat(MAX_ATLAS_STREAM_SEARCH_NEEDLE_BYTES + 1), opts(), || false),
        Err(AtlasStreamSearchError::InvalidLimits)));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn wrong_atlas_and_revoked_grants_cannot_deliver_even_retained_literal_evidence() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle");
    let atlas = fixture.atlas(64); let other = fixture.atlas(64);
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    search(&mut service, &atlas, "needle");
    assert!(matches!(service.page(&other, 1, 0, 1), Err(AtlasStreamSearchError::WrongAtlas)));
    assert!(matches!(service.hit(&other, 1, 0), Err(AtlasStreamSearchError::WrongAtlas)));
    atlas.atlas().catalog().grant().revoke();
    assert!(service.hit(&atlas, 1, 0).is_err());
    assert!(service.page(&atlas, 1, 0, 1).is_err());
    assert!(service.search(&atlas, "needle", opts(), || false).is_err());
}


fn assert_progress_prefix(value: &Json, previous: Option<&Json>) {
    assert_eq!(value.get("command").text(), "progress");
    assert!(!value.get("terminal").flag());
    assert!(!value.get("selection_available").flag());
    assert!(!value.get("search_complete").flag());
    assert!(!value.get("truncated").flag());
    assert_eq!(value.get("stop_reason"), &Json::Null);
    assert_eq!(value.get("page_offset").number(), 0);
    assert_eq!(value.get("page_count").number(), value.get("retained_hits").number().min(32));
    assert_eq!(value.get("hits").array().len() as u64, value.get("page_count").number());
    assert_eq!(value.get("files_examined").number() + value.get("files_unexamined").number(),
        value.get("catalogued_files").number());
    if value.get("retained_hits").number() > 32 {
        assert_eq!(value.get("next_offset").number(), 32);
    } else {
        assert_eq!(value.get("next_offset"), &Json::Null);
    }
    if let Some(previous) = previous {
        for key in ["owner", "source_manifest", "layout_revision", "root_path", "query_generation",
            "needle", "limits", "discovery_complete", "catalogued_files", "discovery_policy", "excluded_entries"] {
            assert_eq!(value.get(key), previous.get(key), "progress cannot change {key}");
        }
        assert_eq!(value.get("progress_sequence").number(), previous.get("progress_sequence").number() + 1);
        for key in ["files_examined", "incomplete_files", "unavailable_files", "unsupported_files", "failed_files",
            "matching_files", "retained_hits", "matches_seen", "payload_bytes_read", "scanned_input_bytes",
            "read_calls", "interrupted_read_calls", "peak_input_buffer_bytes", "retained_witness_bytes"] {
            assert!(value.get(key).number() >= previous.get(key).number(), "{key} must be cumulative");
        }
        assert!(value.get("files_unexamined").number() <= previous.get("files_unexamined").number());
        assert!(value.get("hits").array().starts_with(previous.get("hits").array()));
        assert!(value.get("diagnostics").array().starts_with(previous.get("diagnostics").array()));
    } else {
        assert_eq!(value.get("progress_sequence").number(), 1);
        assert_eq!(value.get("payload_bytes_read").number(), 0);
        assert_eq!(value.get("retained_hits").number(), 0);
    }
}

#[test]
fn progress_publishes_bounded_append_only_rows_before_terminal_inventory_and_preserves_paging() {
    let fixture = Fixture::new();
    for n in 0..70 { fixture.write(&format!("a-{n:02}.rs"), b"needle"); }
    fixture.write("z.rs", &vec![b'x'; 8 * 1024 * 1024]);
    let atlas = fixture.atlas(100); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut observations = Vec::new();
    let response = service.search_with_progress(&atlas, "needle", opts(), || false, || false, |response| {
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let value = json(response);
        assert_progress_prefix(&value, observations.last());
        observations.push(value);
    }).unwrap();
    let terminal = json(&response);
    assert_eq!(response.exit_code(), EXIT_OK);
    assert!(terminal.get("terminal").flag());
    assert!(terminal.get("selection_available").flag());
    assert!(terminal.get("search_complete").flag());
    assert_eq!(terminal.get("retained_hits").number(), 70);
    assert!(observations.iter().any(|value| value.get("retained_hits").number() == 1
        && value.get("files_unexamined").number() > 0), "first finalized match is visible before later files");
    assert!(observations.iter().any(|value| value.get("retained_hits").number() == 70
        && value.get("payload_bytes_read").number() > 70 * 6), "active-file I/O advances independently of rows");
    assert!(observations.len() <= 36, "publication is bounded by prefix growth and 256-quanta cadence");
    for value in &observations {
        assert!(terminal.get("hits").array().starts_with(value.get("hits").array()));
        assert!(terminal.get("payload_bytes_read").number() >= value.get("payload_bytes_read").number());
    }
    assert_eq!(service.source_bytes_read(), 70 * 6 + 8 * 1024 * 1024);
    let io = (service.source_bytes_read(), service.read_calls());
    let page = json(&service.page(&atlas, 1, 32, 32).unwrap());
    assert!(page.get("terminal").flag());
    assert!(page.get("selection_available").flag());
    assert_eq!(page.get("hits").array()[0].get("index").number(), 32);
    assert_eq!(page.get("retained_hits").number(), 70);
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn stop_during_an_active_file_accepts_only_finalized_witnesses_with_exact_spent_io() {
    use std::cell::Cell;
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle");
    let mut active = b"needle".to_vec(); active.resize(8 * 1024 * 1024, b'x');
    fixture.write("b.rs", &active); fixture.write("c.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let stop = Cell::new(false); let mut last = None;
    let response = service.search_with_progress(&atlas, "needle", opts(), || false, || stop.get(), |response| {
        let value = json(response);
        assert_progress_prefix(&value, last.as_ref());
        if value.get("payload_bytes_read").number() > 6 { stop.set(true); }
        last = Some(value);
    }).unwrap();
    let terminal = json(&response); let previous = last.unwrap();
    assert!(stop.get());
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    assert_eq!(terminal.get("stop_reason").text(), "user-stopped");
    assert!(terminal.get("terminal").flag());
    assert!(terminal.get("selection_available").flag());
    assert!(!terminal.get("search_complete").flag());
    assert!(!terminal.get("truncated").flag());
    assert_eq!(terminal.get("files_examined").number(), 1);
    assert_eq!(terminal.get("files_unexamined").number(), 2);
    assert_eq!(terminal.get("incomplete_files").number(), 0, "unfinished active file stays unexamined");
    assert_eq!(terminal.get("retained_hits").number(), 1);
    assert_eq!(terminal.get("matches_seen").number(), 1);
    assert_eq!(terminal.get("retained_witness_bytes").number(), 6);
    assert_eq!(terminal.get("hits"), previous.get("hits"));
    for key in ["payload_bytes_read", "scanned_input_bytes", "read_calls", "interrupted_read_calls"] {
        assert_eq!(terminal.get(key), previous.get(key), "Stop performs no extra scanner work");
    }
    assert_eq!(service.source_bytes_read(), 6 + 4 * 1024 * 1024);
    assert_eq!(service.read_calls(), 257);
    assert_eq!(service.accepted_generation(), Some(1));
    fixture.write("a.rs", b"different now");
    let io = (service.source_bytes_read(), service.read_calls());
    let hit = json(&service.hit(&atlas, 1, 0).unwrap());
    assert_eq!(hit.get("hit").get("original_hex").text(), hex(b"needle"));
    let page = json(&service.page(&atlas, 1, 0, 32).unwrap());
    assert_eq!(page.get("hits"), terminal.get("hits"));
    assert_eq!(page.get("stop_reason").text(), "user-stopped");
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
    assert!(matches!(service.hit(&atlas, 1, 1), Err(AtlasStreamSearchError::MissingHit)));
}

#[test]
fn cancellation_after_progress_wins_over_stop_and_keeps_old_generation_and_spent_reads() {
    use std::cell::Cell;
    let fixture = Fixture::new(); fixture.write("a.rs", b"old needle");
    fixture.write("b.rs", &vec![b'x'; 8 * 1024 * 1024]);
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let old = search(&mut service, &atlas, "old");
    let before = (service.source_bytes_read(), service.read_calls());
    let cancel = Cell::new(false); let stop = Cell::new(false); let mut spent = None;
    let result = service.search_with_progress(&atlas, "needle", opts(), || cancel.get(), || stop.get(), |response| {
        let value = json(response);
        if value.get("payload_bytes_read").number() > 10 {
            spent = Some((value.get("payload_bytes_read").number(), value.get("read_calls").number()));
            cancel.set(true); stop.set(true);
        }
    });
    assert!(matches!(result, Err(AtlasStreamSearchError::Canceled)));
    let spent = spent.unwrap();
    assert_eq!((service.source_bytes_read(), service.read_calls()), (before.0 + spent.0, before.1 + spent.1));
    assert_eq!(service.accepted_generation(), Some(1));
    assert_eq!(service.last_attempted_generation(), 2);
    let old_page = json(&service.page(&atlas, 1, 0, 32).unwrap());
    assert_eq!(old_page.get("hits"), old.get("hits"));
    assert_eq!(json(&service.hit(&atlas, 1, 0).unwrap()).get("hit").get("original_hex").text(), hex(b"old"));
    assert!(matches!(service.page(&atlas, 2, 0, 32), Err(AtlasStreamSearchError::StaleQuery)));
    let next = search(&mut service, &atlas, "needle");
    assert_eq!(next.get("query_generation").number(), 3);
    assert!(next.get("hits").array()[0].get("source_revision").number()
        > old.get("hits").array()[0].get("source_revision").number() + 2,
        "discarded observations cannot reuse source identity");
}

#[test]
fn a_progress_callback_can_stop_before_first_io_and_empty_catalog_still_finishes_complete() {
    use std::cell::Cell;
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let stop = Cell::new(false); let mut callbacks = 0;
    let response = service.search_with_progress(&atlas, "needle", opts(), || false, || stop.get(), |response| {
        callbacks += 1; let value = json(response); assert_progress_prefix(&value, None); stop.set(true);
    }).unwrap();
    let report = json(&response);
    assert_eq!(callbacks, 1);
    assert_eq!(report.get("stop_reason").text(), "user-stopped");
    assert_eq!(report.get("files_examined").number(), 0);
    assert_eq!(report.get("files_unexamined").number(), 1);
    assert_eq!(report.get("payload_bytes_read").number(), 0);
    assert_eq!(report.get("read_calls").number(), 0);
    assert_eq!(service.accepted_generation(), Some(1));
    assert_eq!(service.source_bytes_read(), 0);

    let empty = Fixture::new(); let atlas = empty.atlas(64);
    let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = json(&service.search_with_progress(&atlas, "needle", opts(), || false, || true,
        |_| panic!("an already complete empty catalog has no pending progress")).unwrap());
    assert!(report.get("search_complete").flag());
    assert!(report.get("terminal").flag());
    assert_eq!(report.get("stop_reason"), &Json::Null);
}

#[test]
fn malformed_active_text_never_retracts_an_earlier_progress_witness_or_exposes_tentative_matches() {
    let fixture = Fixture::new();
    let mut utf16 = vec![0xff, 0xfe];
    utf16.extend("needle\n".encode_utf16().flat_map(u16::to_le_bytes));
    fixture.write("a.rs", &utf16);
    let mut malformed = b"needle".to_vec(); malformed.resize(5 * 1024 * 1024, b'x'); malformed.push(0xff);
    fixture.write("b.rs", &malformed);
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut observations = Vec::new();
    let terminal = json(&service.search_with_progress(&atlas, "needle", opts(), || false, || false, |response| {
        let value = json(response);
        assert_progress_prefix(&value, observations.last());
        assert!(value.get("retained_hits").number() <= 1);
        observations.push(value);
    }).unwrap());
    assert!(!terminal.get("search_complete").flag());
    assert_eq!(terminal.get("unsupported_files").number(), 1);
    assert_eq!(terminal.get("retained_hits").number(), 1);
    assert_eq!(terminal.get("hits").array()[0].get("encoding").text(), "utf16le");
    assert!(observations.iter().any(|value| value.get("files_examined").number() == 1
        && value.get("payload_bytes_read").number() > utf16.len() as u64));
    for value in observations { assert!(terminal.get("hits").array().starts_with(value.get("hits").array())); }
    let hit = json(&service.hit(&atlas, 1, 0).unwrap());
    assert_eq!(hit.get("hit").get("original_hex").text(),
        hex(&"needle".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()));
    assert_eq!(service.source_bytes_read(), (utf16.len() + malformed.len()) as u64);
}

#[test]
fn negative_file_progress_has_bounded_cadence_and_terminal_limits_remain_explicit() {
    let fixture = Fixture::new(); fixture.write("a.rs", &vec![b'x'; 20 * 1024 * 1024]);
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let mut callbacks = 0; let mut previous = None;
    let report = json(&service.search_with_progress(&atlas, "needle", opts(), || false, || false, |response| {
        let value = json(response); callbacks += 1;
        assert_progress_prefix(&value, previous.as_ref());
        assert_eq!(value.get("files_examined").number(), 0);
        previous = Some(value);
    }).unwrap());
    assert!((4..=6).contains(&callbacks), "1280 read quanta must not produce 1280 callback documents");
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("retained_hits").number(), 0);
    assert_eq!(report.get("payload_bytes_read").number(), 20 * 1024 * 1024);
    let report = json(&service.search_with_progress(&atlas, "needle",
        AtlasStreamSearchOptions { max_scan_bytes: 1024, ..opts() }, || false, || false, |_| {}).unwrap());
    assert_eq!(report.get("stop_reason").text(), "byte-limit");
    assert!(!report.get("search_complete").flag());
    assert!(report.get("terminal").flag());
    assert!(report.get("selection_available").flag());
}

#[test]
fn revocation_from_progress_and_invalid_admission_cannot_publish_a_new_inventory() {
    let fixture = Fixture::new(); fixture.write("a.rs", b"needle");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    search(&mut service, &atlas, "needle");
    let before = (service.source_bytes_read(), service.read_calls());
    assert!(matches!(service.search_with_progress(&atlas, "", opts(), || false, || false,
        |_| panic!("invalid query must not publish progress")), Err(AtlasStreamSearchError::InvalidLimits)));
    assert_eq!(service.accepted_generation(), Some(1));
    assert_eq!(before, (service.source_bytes_read(), service.read_calls()));
    let result = service.search_with_progress(&atlas, "needle", opts(), || false, || false,
        |_| atlas.atlas().catalog().grant().revoke());
    assert!(result.is_err());
    assert_eq!(service.accepted_generation(), Some(1));
    assert_eq!(service.last_attempted_generation(), 3);
    assert_eq!(before, (service.source_bytes_read(), service.read_calls()));
    assert!(service.page(&atlas, 1, 0, 32).is_err(), "retained evidence cannot bypass revoked authority");
}

#[test]
fn counted_search_finishes_later_files_with_bounded_witnesses_and_stable_page_counts() {
    let fixture = Fixture::new();
    fixture.write("a.rs", b"banana banana");
    let le: Vec<u8> = "\u{feff}banana".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let be: Vec<u8> = "\u{feff}banana banana".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fixture.write("b.rs", &le); fixture.write("c.rs", &be); fixture.write("d.rs", b"absent");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let options = AtlasStreamSearchOptions { max_matches: 1, ..opts() };
    let legacy = json(&service.search(&atlas, "ana", options, || false).unwrap());
    assert!(!legacy.get("count_all_matches").flag());
    assert!(!legacy.get("match_count_complete").flag());
    assert_eq!(legacy.get("matches_seen").number(), 2);
    assert_eq!(legacy.get("stop_reason").text(), "match-limit");

    let response = service.search_counted(&atlas, "ana", options, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL, "omitted rows remain explicit in the exit code");
    let report = json(&response);
    assert!(report.get("search_complete").flag());
    assert!(report.get("match_count_complete").flag());
    assert!(report.get("count_all_matches").flag());
    assert!(report.get("truncated").flag());
    assert_eq!(report.get("stop_reason"), &Json::Null);
    assert_eq!(report.get("matches_seen").number(), 10);
    assert_eq!(report.get("matching_files").number(), 3);
    assert_eq!(report.get("retained_matching_files").number(), 1);
    assert_eq!(report.get("retained_hits").number(), 1);
    assert_eq!(report.get("files_examined").number(), 4);
    assert_eq!(report.get("files_unexamined").number(), 0);
    assert_eq!(report.get("payload_bytes_read").number(), (13 + le.len() + be.len() + 6) as u64);
    assert!(report.get("peak_input_buffer_bytes").number() <= 16 * 1024);
    assert_eq!(service.retained_witness_bytes(), 3);
    let generation = report.get("query_generation").number();
    let io = (service.source_bytes_read(), service.read_calls());
    fixture.write("a.rs", b"changed after search");
    let page = json(&service.page(&atlas, generation, 0, 1).unwrap());
    for key in ["count_all_matches", "match_count_complete", "search_complete", "truncated",
        "matches_seen", "matching_files", "retained_matching_files", "retained_hits"] {
        assert_eq!(page.get(key), report.get(key), "{key}");
    }
    let hit = json(&service.hit(&atlas, generation, 0).unwrap());
    assert!(hit.get("count_all_matches").flag()); assert!(hit.get("match_count_complete").flag());
    assert_eq!(hit.get("retained_matching_files").number(), 1);
    assert_eq!(hit.get("hit").get("original_hex").text(), hex(b"ana"));
    assert_eq!(bounds(hit.get("hit").get("original_range")), (1, 4));
    assert!(matches!(service.hit(&atlas, generation, 1), Err(AtlasStreamSearchError::MissingHit)));
    assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
}

#[test]
fn counted_search_keeps_scope_and_work_limits_separate_from_row_omissions() {
    let fixture = Fixture::new();
    fixture.write("a-excluded.rs", &vec![b'a'; 1024 * 1024]);
    fixture.write("src/a.rs", b"aaaaaaaaaa"); fixture.write("src/b.rs", b"aaaaa");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    for (options, expected_matches, expected_bytes, stop) in [
        (AtlasStreamSearchOptions { max_matches: 1, max_scan_bytes: 5, ..opts() }, 4, 5, "byte-limit"),
        (AtlasStreamSearchOptions { max_matches: 1, max_read_calls: 1, ..opts() }, 9, 10, "read-call-limit"),
    ] {
        let response = service.search_scoped_counted(&atlas, "aa", options, scope.clone(), || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let report = json(&response);
        assert!(report.get("count_all_matches").flag()); assert!(report.get("truncated").flag());
        assert!(!report.get("search_complete").flag()); assert!(!report.get("match_count_complete").flag());
        assert_eq!(report.get("matches_seen").number(), expected_matches);
        assert_eq!(report.get("payload_bytes_read").number(), expected_bytes);
        assert_eq!(report.get("stop_reason").text(), stop);
        assert_eq!(report.get("eligible_files").number(), 2); assert_eq!(report.get("excluded_files").number(), 1);
        assert_eq!(report.get("files_unexamined").number(), 1);
        assert_eq!(report.get("matching_files").number(), 1);
        assert_eq!(report.get("retained_matching_files").number(), 1);
    }
    let complete = json(&service.search_scoped_counted(&atlas, "aa",
        AtlasStreamSearchOptions { max_matches: 1, ..opts() }, scope, || false).unwrap());
    assert!(complete.get("match_count_complete").flag()); assert!(complete.get("truncated").flag());
    assert_eq!(complete.get("matches_seen").number(), 13);
    assert_eq!(complete.get("matching_files").number(), 2);
    assert_eq!(complete.get("payload_bytes_read").number(), 15);
}

#[test]
fn counted_progress_stop_and_cancellation_keep_finalized_counts_after_retention_fills() {
    use std::cell::Cell;
    let fixture = Fixture::new();
    fixture.write("a.rs", b"banana banana"); fixture.write("b.rs", b"banana");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let options = AtlasStreamSearchOptions { max_matches: 1, ..opts() };
    let stop = Cell::new(false); let mut frames = Vec::new();
    let response = service.search_counted_with_progress(&atlas, "ana", options,
        || false, || stop.get(), |response| {
            let frame = json(response);
            assert!(frame.get("count_all_matches").flag());
            assert!(!frame.get("match_count_complete").flag());
            assert!(frame.get("retained_hits").number() <= 1);
            if frame.get("matches_seen").number() == 4 { stop.set(true); }
            frames.push(frame);
        }).unwrap();
    assert!(stop.get()); assert_eq!(frames.len(), 2); assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let report = json(&response);
    assert_eq!(report.get("stop_reason").text(), "user-stopped");
    assert_eq!(report.get("matches_seen").number(), 4);
    assert_eq!(report.get("files_examined").number(), 1);
    assert_eq!(report.get("files_unexamined").number(), 1);
    assert!(report.get("truncated").flag()); assert!(!report.get("match_count_complete").flag());
    let generation = report.get("query_generation").number();
    let cancel = Cell::new(false);
    let rejected = service.search_counted_with_progress(&atlas, "ana", options,
        || cancel.get(), || false, |response| {
            if json(response).get("matches_seen").number() == 4 { cancel.set(true); }
        });
    assert!(cancel.get()); assert!(matches!(rejected, Err(AtlasStreamSearchError::Canceled)));
    assert_eq!(service.accepted_generation(), Some(generation));
    let retained = json(&service.page(&atlas, generation, 0, 1).unwrap());
    assert_eq!(retained.get("matches_seen"), report.get("matches_seen"));
    assert_eq!(retained.get("hits"), report.get("hits"));
}

#[test]
fn lowercase_workspace_scope_counts_and_original_witnesses_survive_row_limits() {
    let fixture = Fixture::new();
    fixture.write("src/a.rs", "KK".as_bytes());
    let utf16: Vec<u8> = "\u{feff}kK".encode_utf16().flat_map(u16::to_be_bytes).collect();
    fixture.write("src/b.rs", &utf16);
    fixture.write("src/ignored.md", b"KKKKKK");
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap().with_case_sensitive(false);
    for (generation, retained) in [(1, 1), (2, 4)] {
        let report = json(&service.search_scoped_counted(&atlas, "k",
            AtlasStreamSearchOptions { max_matches: retained, ..opts() }, scope.clone(), || false).unwrap());
        assert_eq!(report.get("query_generation").number(), generation);
        assert_eq!(report.get("mode").text(), "unicode-lowercase-literal");
        assert!(!report.get("case_sensitive").flag());
        assert_eq!(report.get("eligible_files").number(), 2);
        assert_eq!(report.get("excluded_files").number(), 1);
        assert_eq!(report.get("payload_bytes_read").number(), 10);
        assert_eq!(report.get("matches_seen").number(), 4);
        assert_eq!(report.get("matching_files").number(), 2);
        assert_eq!(report.get("retained_matching_files").number(), if retained == 1 { 1 } else { 2 });
        assert_eq!(report.get("retained_hits").number(), retained as u64);
        assert_eq!(report.get("truncated").flag(), retained < 4);
        assert!(report.get("match_count_complete").flag());
        let expected: [(&str, &[u8]); 4] = [("K", "K".as_bytes()), ("K", b"K"), ("k", &[0, 0x6b]), ("K", &[0x21, 0x2a])];
        let mut bytes = 0;
        let io = (service.source_bytes_read(), service.read_calls());
        for (position, &(spelling, original)) in expected.iter().take(retained).enumerate() {
            let selected = json(&service.hit(&atlas, generation, position).unwrap());
            assert_scope_echo(&selected, &report);
            assert_eq!(selected.get("mode"), report.get("mode"));
            assert_eq!(selected.get("hit").get("text").text(), spelling);
            assert_eq!(selected.get("hit").get("original_hex").text(), hex(original));
            assert_eq!(selected.get("hit").get("witness_sha256"), report.get("hits").array()[position].get("witness_sha256"));
            bytes += original.len();
        }
        assert_eq!(report.get("retained_witness_bytes").number(), bytes as u64);
        assert_eq!(service.retained_witness_bytes(), bytes);
        assert_eq!(io, (service.source_bytes_read(), service.read_calls()));
        assert_eq!(json(&service.page(&atlas, generation, 0, 32).unwrap()).get("hits"), report.get("hits"));
    }
}

#[test]
fn lowercase_workspace_progress_and_partial_counts_preserve_accepted_witnesses() {
    let fixture = Fixture::new();
    fixture.write("a.rs", "KK".as_bytes());
    fixture.write("b.rs", "kK".as_bytes());
    let atlas = fixture.atlas(64); let mut service = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let scope = AtlasSearchScope::all().with_case_sensitive(false);
    let mut progress = Vec::new();
    let complete = json(&service.search_scoped_counted_with_progress(&atlas, "k",
        AtlasStreamSearchOptions { max_matches: 1, ..opts() }, scope.clone(), || false, || false,
        |response| progress.push(json(response))).unwrap());
    assert_eq!(complete.get("matches_seen").number(), 4);
    assert!(complete.get("match_count_complete").flag());
    assert!(progress.len() >= 2);
    for preview in &progress {
        assert_scope_echo(preview, &complete);
        assert_eq!(preview.get("mode"), complete.get("mode"));
        assert!(!preview.get("match_count_complete").flag());
        assert!(!preview.get("selection_available").flag());
        assert!(preview.get("matches_seen").number() <= 4);
        if !preview.get("hits").array().is_empty() {
            assert_eq!(preview.get("hits").array()[0], complete.get("hits").array()[0]);
            assert_eq!(preview.get("retained_witness_bytes").number(), 3);
        }
    }
    let partial = json(&service.search_scoped_counted(&atlas, "k",
        AtlasStreamSearchOptions { max_scan_bytes: 3, ..opts() }, scope.clone(), || false).unwrap());
    assert_eq!(partial.get("stop_reason").text(), "byte-limit");
    assert!(!partial.get("match_count_complete").flag());
    assert_eq!(partial.get("matches_seen").number(), 1);
    assert_eq!(partial.get("payload_bytes_read").number(), 3);
    let accepted = json(&service.hit(&atlas, 2, 0).unwrap());
    assert_eq!(service.search_scoped_counted(&atlas, "K", opts(), scope, || true).err(), Some(AtlasStreamSearchError::Canceled));
    fixture.write("a.rs", b"changed");
    let witness = json(&service.hit(&atlas, 2, 0).unwrap());
    assert_eq!(witness.get("hit"), accepted.get("hit"));
    assert_eq!(witness.get("hit").get("text").text(), "K");
    assert_eq!(witness.get("hit").get("original_hex").text(), "e284aa");
}
