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
    assert_eq!(service.source_bytes_read(), 0, "catalog preparation is metadata-only");
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
    assert_eq!(report.get("discovery_policy").text(), "product-defaults/no-rule-files-v1");
    assert!(report.get("excluded_entries").number() >= 1);
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
